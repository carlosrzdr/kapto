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
pub use kapto_decode::camera::{CameraKind, PictureFormat};
use kapto_decode::camera::{Frame, FrameError, frame};

/// How raw pictures are encoded: good enough to watch, about a tenth of
/// the pixels' size.
const QUALITY: u8 = 80;

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

impl From<FrameError> for Unshown {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::Unreadable(why) => Self::Unreadable(why),
            FrameError::Format(format) => Self::Format(format),
        }
    }
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
        frame_bytes: &'a [u8],
    ) -> Result<Picture<'a>, Unshown> {
        match frame(kind, frame_bytes)? {
            Frame::Compressed { format, data } => Ok(Picture {
                format,
                size: None,
                bytes: data,
            }),
            Frame::Raw {
                data,
                width,
                height,
                step,
                encoding,
            } => self.encode(data, (width, height, step), encoding),
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
