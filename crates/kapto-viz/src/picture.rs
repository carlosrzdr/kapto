// SPDX-License-Identifier: Apache-2.0

//! A camera's picture, taken out of a frame as recorded, in a form a
//! browser draws: a ROS 2 `sensor_msgs/msg/CompressedImage`'s or a TCP
//! camera's JPEG or PNG as it is, and a raw picture (a ROS 2
//! `sensor_msgs/msg/Image`, or a TCP camera's pixels) encoded as JPEG
//! here, never in capture.
//!
//! A frame that can't be shown says why ([`Unshown`]), so the live view can
//! name it rather than show nothing.

use std::fmt;

use jpeg_encoder::{ColorType, Encoder};
use kapto_config::{CameraDeclaration, FramingDeclaration, PixelEncoding, StreamDeclaration};

/// How raw pictures are encoded: good enough to watch, about a tenth of
/// the pixels' size.
const QUALITY: u8 = 80;

/// How a camera's frames are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraKind {
    /// A ROS 2 `sensor_msgs/msg/CompressedImage`, as CDR.
    CompressedImage,
    /// A ROS 2 `sensor_msgs/msg/Image`, as CDR.
    Image,
    /// A TCP camera's JPEG pictures, each a message.
    Jpeg {
        /// The bytes before the picture: the framing's header, which a
        /// length-prefixed message is stored with.
        skip: usize,
    },
    /// A TCP camera's raw pictures of a fixed size, each a message.
    Raw {
        /// Pixels per row.
        width: u32,
        /// Rows.
        height: u32,
        /// How each pixel is laid out.
        encoding: PixelEncoding,
        /// The bytes before the pixels, as for [`CameraKind::Jpeg`].
        skip: usize,
    },
}

impl CameraKind {
    /// How `declaration`'s frames are laid out, if it is a camera.
    #[must_use]
    pub fn of(declaration: &StreamDeclaration) -> Option<Self> {
        if let Some(camera) = declaration.camera {
            // A delimiter isn't part of the message; a length prefix is.
            let skip = match declaration.tcp.as_ref().map(|tcp| &tcp.framing) {
                Some(FramingDeclaration::LengthPrefix { header_bytes, .. }) => {
                    usize::from(*header_bytes)
                }
                _ => 0,
            };
            return Some(match camera {
                CameraDeclaration::Jpeg => Self::Jpeg { skip },
                CameraDeclaration::Raw {
                    width,
                    height,
                    encoding,
                } => Self::Raw {
                    width,
                    height,
                    encoding,
                    skip,
                },
            });
        }
        match declaration.ros2.as_ref()?.message_type.as_str() {
            "sensor_msgs/msg/CompressedImage" => Some(Self::CompressedImage),
            "sensor_msgs/msg/Image" => Some(Self::Image),
            _ => None,
        }
    }
}

/// What a browser decodes a picture as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFormat {
    /// `image/jpeg`.
    Jpeg,
    /// `image/png`.
    Png,
}

/// A picture a browser can draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Picture<'a> {
    /// How it is encoded.
    pub format: PictureFormat,
    /// Its width and height in pixels, if known without decoding it: a
    /// raw picture's, not a compressed one's.
    pub size: Option<(u32, u32)>,
    /// The encoded picture.
    pub bytes: &'a [u8],
}

/// Why a frame isn't shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unshown {
    /// The frame isn't the message its stream declares: cut short, or not
    /// CDR.
    Unreadable(&'static str),
    /// A compressed picture neither JPEG nor PNG, which a browser can't be
    /// counted on to draw; its `format` as the message names it.
    Format(String),
    /// A raw picture of a pixel encoding the bridge doesn't encode.
    Encoding(String),
    /// A raw picture with fewer bytes than its size says.
    Short {
        /// Bytes its size needs.
        needed: u64,
        /// Bytes it has.
        has: u64,
    },
    /// A raw picture wider or taller than JPEG holds (65 535).
    TooLarge(u32, u32),
}

impl fmt::Display for Unshown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "a frame that can't be read: {why}"),
            Self::Format(format) => {
                write!(
                    f,
                    "pictures in `{format}`, which the live view can't draw: only JPEG and PNG"
                )
            }
            Self::Encoding(encoding) => write!(
                f,
                "raw pictures in `{encoding}`, which the live view can't show: only rgb8, bgr8, rgba8, bgra8 and mono8"
            ),
            Self::Short { needed, has } => {
                write!(
                    f,
                    "a raw picture of {has} bytes where its size needs {needed}"
                )
            }
            Self::TooLarge(width, height) => {
                write!(
                    f,
                    "a raw picture of {width}x{height}, larger than JPEG holds"
                )
            }
        }
    }
}

/// Takes pictures out of frames, keeping its buffers between frames.
#[derive(Debug, Default)]
pub struct Pictures {
    /// The newest raw picture encoded as JPEG.
    jpeg: Vec<u8>,
    /// The newest raw picture's rows without their padding.
    rows: Vec<u8>,
}

impl Pictures {
    /// The picture in `frame`, a message of a camera of `kind`.
    ///
    /// # Errors
    ///
    /// Why it can't be shown.
    pub fn picture<'a>(
        &'a mut self,
        kind: &CameraKind,
        frame: &'a [u8],
    ) -> Result<Picture<'a>, Unshown> {
        let after = |skip: usize| {
            frame
                .get(skip..)
                .ok_or(Unshown::Unreadable("shorter than its framing's header"))
        };
        match *kind {
            CameraKind::Jpeg { skip } => compressed(after(skip)?, "jpeg"),
            CameraKind::CompressedImage => {
                let mut cdr = Cdr::new(frame)?;
                cdr.header()?;
                let format = cdr.string()?;
                let data = cdr.bytes()?;
                compressed(data, format)
            }
            CameraKind::Raw {
                width,
                height,
                encoding,
                skip,
            } => {
                let step = u64::from(width) * encoding.bytes_per_pixel();
                self.encode(after(skip)?, (width, height, step), encoding.name())
            }
            CameraKind::Image => {
                let mut cdr = Cdr::new(frame)?;
                cdr.header()?;
                let height = cdr.u32()?;
                let width = cdr.u32()?;
                let encoding = cdr.string()?;
                cdr.u8()?;
                let step = cdr.u32()?;
                let data = cdr.bytes()?;
                self.encode(data, (width, height, u64::from(step)), encoding)
            }
        }
    }

    /// Encodes the raw picture in `data`, `height` rows of `step` bytes
    /// each starting with `width` pixels in `encoding`, as JPEG.
    fn encode(
        &mut self,
        data: &[u8],
        (width, height, step): (u32, u32, u64),
        encoding: &str,
    ) -> Result<Picture<'_>, Unshown> {
        let (color, pixel_bytes) = match encoding {
            "rgb8" => (ColorType::Rgb, 3),
            "bgr8" => (ColorType::Bgr, 3),
            "rgba8" => (ColorType::Rgba, 4),
            "bgra8" => (ColorType::Bgra, 4),
            "mono8" => (ColorType::Luma, 1),
            other => return Err(Unshown::Encoding(other.to_owned())),
        };
        let (Ok(jpeg_width), Ok(jpeg_height)) = (u16::try_from(width), u16::try_from(height))
        else {
            return Err(Unshown::TooLarge(width, height));
        };
        if width == 0 || height == 0 {
            return Err(Unshown::Unreadable("a picture without pixels"));
        }
        let row = u64::from(width) * pixel_bytes;
        if step < row {
            return Err(Unshown::Unreadable("rows shorter than their pixels"));
        }
        // The last row needn't carry its padding.
        let needed = step * u64::from(height - 1) + row;
        let has = data.len() as u64;
        if has < needed {
            return Err(Unshown::Short { needed, has });
        }
        // Rows padded to `step` are packed first: the encoder takes them
        // back to back.
        let pixels = if step == row {
            &data[..to_usize(needed)]
        } else {
            let (step, row) = (to_usize(step), to_usize(row));
            self.rows.clear();
            for start in (0..usize::from(jpeg_height)).map(|line| line * step) {
                self.rows.extend_from_slice(&data[start..start + row]);
            }
            &self.rows
        };
        self.jpeg.clear();
        Encoder::new(&mut self.jpeg, QUALITY)
            .encode(pixels, jpeg_width, jpeg_height, color)
            .map_err(|_| Unshown::TooLarge(width, height))?;
        Ok(Picture {
            format: PictureFormat::Jpeg,
            size: Some((width, height)),
            bytes: &self.jpeg,
        })
    }
}

fn to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A compressed picture, told JPEG or PNG by its first bytes rather than
/// the `format` its message names, which cameras fill in loosely.
fn compressed<'a>(data: &'a [u8], format: &str) -> Result<Picture<'a>, Unshown> {
    let format = if data.starts_with(&[0xff, 0xd8, 0xff]) {
        PictureFormat::Jpeg
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        PictureFormat::Png
    } else {
        return Err(Unshown::Format(format.to_owned()));
    };
    Ok(Picture {
        format,
        size: None,
        bytes: data,
    })
}

/// Reads a message serialised as CDR, field by field: just what a camera's
/// two message types need.
struct Cdr<'a> {
    /// The message after its 4-byte encapsulation header, from which
    /// alignment counts.
    bytes: &'a [u8],
    at: usize,
    little_endian: bool,
}

impl<'a> Cdr<'a> {
    fn new(message: &'a [u8]) -> Result<Self, Unshown> {
        let little_endian = match message.get(..2) {
            Some([0, 0]) => false,
            Some([0, 1]) => true,
            Some(_) => return Err(Unshown::Unreadable("not plain CDR")),
            None => return Err(Unshown::Unreadable("shorter than its header")),
        };
        Ok(Self {
            bytes: message
                .get(4..)
                .ok_or(Unshown::Unreadable("shorter than its header"))?,
            at: 0,
            little_endian,
        })
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], Unshown> {
        let end = self
            .at
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Unshown::Unreadable("cut short"))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, Unshown> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, Unshown> {
        self.at = self.at.next_multiple_of(4);
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| Unshown::Unreadable("cut short"))?;
        Ok(if self.little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    /// A string, without its terminating zero byte.
    fn string(&mut self) -> Result<&'a str, Unshown> {
        let text = self.bytes()?;
        let text = text.strip_suffix(&[0]).unwrap_or(text);
        std::str::from_utf8(text).map_err(|_| Unshown::Unreadable("a string that isn't UTF-8"))
    }

    /// A sequence of bytes.
    fn bytes(&mut self) -> Result<&'a [u8], Unshown> {
        let length = self.u32()?;
        self.take(usize::try_from(length).map_err(|_| Unshown::Unreadable("cut short"))?)
    }

    /// A `std_msgs/msg/Header`: its stamp and frame ID, skipped.
    fn header(&mut self) -> Result<(), Unshown> {
        self.u32()?;
        self.u32()?;
        self.string()?;
        Ok(())
    }
}
