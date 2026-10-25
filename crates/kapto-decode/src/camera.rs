// SPDX-License-Identifier: Apache-2.0

//! A camera's frame, read as recorded: what kind of camera sent it, and the
//! picture in it. A ROS 2 `sensor_msgs/msg/CompressedImage` or a TCP
//! camera's JPEG holds a compressed picture, told JPEG or PNG by its first
//! bytes; a ROS 2 `sensor_msgs/msg/Image` or a TCP camera's pixels hold a
//! raw one. Reading never copies: a [`Frame`] borrows the message.

use std::fmt;

/// How a raw frame's pixels are laid out, named as ROS 2 names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelEncoding {
    /// `rgb8`: red, green and blue, a byte each.
    Rgb8,
    /// `bgr8`: blue, green and red, a byte each.
    Bgr8,
    /// `rgba8`: red, green, blue and alpha, a byte each.
    Rgba8,
    /// `bgra8`: blue, green, red and alpha, a byte each.
    Bgra8,
    /// `mono8`: grey, a byte.
    Mono8,
}

impl PixelEncoding {
    /// Every encoding, in the order they are documented.
    pub const ALL: [Self; 5] = [
        Self::Rgb8,
        Self::Bgr8,
        Self::Rgba8,
        Self::Bgra8,
        Self::Mono8,
    ];

    /// Its name, as `streams.toml` and ROS 2 write it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rgb8 => "rgb8",
            Self::Bgr8 => "bgr8",
            Self::Rgba8 => "rgba8",
            Self::Bgra8 => "bgra8",
            Self::Mono8 => "mono8",
        }
    }

    /// The encoding named `name`, if it is one of these.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|encoding| encoding.name() == name)
    }

    /// Bytes per pixel.
    #[must_use]
    pub fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Rgb8 | Self::Bgr8 => 3,
            Self::Rgba8 | Self::Bgra8 => 4,
            Self::Mono8 => 1,
        }
    }
}

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

/// How a compressed picture is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureFormat {
    /// `image/jpeg`.
    Jpeg,
    /// `image/png`.
    Png,
}

/// The picture in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame<'a> {
    /// A compressed picture, as it is.
    Compressed {
        /// How it is encoded, by its first bytes.
        format: PictureFormat,
        /// The encoded picture.
        data: &'a [u8],
    },
    /// A raw picture: `height` rows of `step` bytes, each starting with
    /// `width` pixels in `encoding`.
    Raw {
        /// The pixels, row after row.
        data: &'a [u8],
        /// Pixels per row.
        width: u32,
        /// Rows.
        height: u32,
        /// Bytes from one row's start to the next's.
        step: u64,
        /// How each pixel is laid out, as named.
        encoding: &'a str,
        /// Whether a pixel of more than one byte stores it most significant
        /// byte first.
        big_endian: bool,
    },
}

/// Why a frame holds no picture of its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// It isn't the message its camera sends: cut short, or not CDR.
    Unreadable(&'static str),
    /// A compressed picture neither JPEG nor PNG; its `format` as the
    /// message names it.
    Format(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(why) => write!(f, "a frame that can't be read: {why}"),
            Self::Format(format) => write!(f, "pictures in `{format}`, neither JPEG nor PNG"),
        }
    }
}

/// The picture in `bytes`, a frame of a camera of `kind`.
///
/// # Errors
///
/// Why it holds none.
pub fn frame<'a>(kind: &CameraKind, bytes: &'a [u8]) -> Result<Frame<'a>, FrameError> {
    let after = |skip: usize| {
        bytes
            .get(skip..)
            .ok_or(FrameError::Unreadable("shorter than its framing's header"))
    };
    match *kind {
        CameraKind::Jpeg { skip } => compressed(after(skip)?, "jpeg"),
        CameraKind::CompressedImage => {
            let mut cdr = Cdr::new(bytes)?;
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
        } => Ok(Frame::Raw {
            data: after(skip)?,
            width,
            height,
            step: u64::from(width) * encoding.bytes_per_pixel(),
            encoding: encoding.name(),
            big_endian: false,
        }),
        CameraKind::Image => {
            let mut cdr = Cdr::new(bytes)?;
            cdr.header()?;
            let height = cdr.u32()?;
            let width = cdr.u32()?;
            let encoding = cdr.string()?;
            let big_endian = cdr.u8()? != 0;
            let step = cdr.u32()?;
            let data = cdr.bytes()?;
            Ok(Frame::Raw {
                data,
                width,
                height,
                step: u64::from(step),
                encoding,
                big_endian,
            })
        }
    }
}

/// A compressed picture, told JPEG or PNG by its first bytes rather than
/// the `format` its message names, which cameras fill in loosely.
fn compressed<'a>(data: &'a [u8], format: &str) -> Result<Frame<'a>, FrameError> {
    let format = if data.starts_with(&[0xff, 0xd8, 0xff]) {
        PictureFormat::Jpeg
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        PictureFormat::Png
    } else {
        return Err(FrameError::Format(format.to_owned()));
    };
    Ok(Frame::Compressed { format, data })
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
    fn new(message: &'a [u8]) -> Result<Self, FrameError> {
        let little_endian = match message.get(..2) {
            Some([0, 0]) => false,
            Some([0, 1]) => true,
            Some(_) => return Err(FrameError::Unreadable("not plain CDR")),
            None => return Err(FrameError::Unreadable("shorter than its header")),
        };
        Ok(Self {
            bytes: message
                .get(4..)
                .ok_or(FrameError::Unreadable("shorter than its header"))?,
            at: 0,
            little_endian,
        })
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], FrameError> {
        let end = self
            .at
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(FrameError::Unreadable("cut short"))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, FrameError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, FrameError> {
        self.at = self.at.next_multiple_of(4);
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| FrameError::Unreadable("cut short"))?;
        Ok(if self.little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    /// A string, without its terminating zero byte.
    fn string(&mut self) -> Result<&'a str, FrameError> {
        let text = self.bytes()?;
        let text = text.strip_suffix(&[0]).unwrap_or(text);
        std::str::from_utf8(text).map_err(|_| FrameError::Unreadable("a string that isn't UTF-8"))
    }

    /// A sequence of bytes.
    fn bytes(&mut self) -> Result<&'a [u8], FrameError> {
        let length = self.u32()?;
        self.take(usize::try_from(length).map_err(|_| FrameError::Unreadable("cut short"))?)
    }

    /// A `std_msgs/msg/Header`: its stamp and frame ID, skipped.
    fn header(&mut self) -> Result<(), FrameError> {
        self.u32()?;
        self.u32()?;
        self.string()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `sensor_msgs/msg/CompressedImage` in little-endian CDR, of
    /// `format` holding `data`.
    fn compressed_image(format: &str, data: &[u8]) -> Vec<u8> {
        let mut message = vec![0, 1, 0, 0];
        let string = |message: &mut Vec<u8>, text: &[u8]| {
            while !(message.len() - 4).is_multiple_of(4) {
                message.push(0);
            }
            let length = u32::try_from(text.len()).unwrap();
            message.extend(length.to_le_bytes());
            message.extend(text);
        };
        message.extend([0; 8]);
        string(&mut message, b"camera\0");
        string(&mut message, format.as_bytes());
        string(&mut message, data);
        message
    }

    #[test]
    fn a_compressed_picture_is_told_by_its_first_bytes_whatever_its_message_names() {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 1, 2];
        let png = b"\x89PNG\r\n\x1a\n...";
        assert_eq!(
            frame(
                &CameraKind::CompressedImage,
                &compressed_image("png", &jpeg)
            ),
            Ok(Frame::Compressed {
                format: PictureFormat::Jpeg,
                data: &jpeg
            })
        );
        assert_eq!(
            frame(&CameraKind::Jpeg { skip: 0 }, png),
            Ok(Frame::Compressed {
                format: PictureFormat::Png,
                data: png
            })
        );
        assert_eq!(
            frame(
                &CameraKind::CompressedImage,
                &compressed_image("tiff", b"II*\0")
            ),
            Err(FrameError::Format("tiff".to_owned()))
        );
    }

    #[test]
    fn a_frame_too_short_for_its_kind_says_so() {
        let mut header = vec![0, 0, 0, 4];
        header.extend(jpeg_after_header());
        assert!(matches!(
            frame(&CameraKind::Jpeg { skip: 4 }, &header),
            Ok(Frame::Compressed { .. })
        ));
        assert_eq!(
            frame(&CameraKind::Jpeg { skip: 8 }, &[0; 4]),
            Err(FrameError::Unreadable("shorter than its framing's header"))
        );
        let cut = compressed_image("jpeg", &jpeg_after_header());
        assert_eq!(
            frame(&CameraKind::CompressedImage, &cut[..cut.len() - 1]),
            Err(FrameError::Unreadable("cut short"))
        );
        assert_eq!(
            frame(&CameraKind::Image, &[7, 7, 0, 0]),
            Err(FrameError::Unreadable("not plain CDR"))
        );
    }

    #[test]
    fn a_raw_frame_is_its_pixels_after_its_header() {
        let kind = CameraKind::Raw {
            width: 2,
            height: 1,
            encoding: PixelEncoding::Rgb8,
            skip: 2,
        };
        assert_eq!(
            frame(&kind, &[9, 9, 1, 2, 3, 4, 5, 6]),
            Ok(Frame::Raw {
                data: &[1, 2, 3, 4, 5, 6],
                width: 2,
                height: 1,
                step: 6,
                encoding: "rgb8",
                big_endian: false,
            })
        );
        assert_eq!(PixelEncoding::named("bgra8"), Some(PixelEncoding::Bgra8));
        assert_eq!(PixelEncoding::named("16UC1"), None);
    }

    fn jpeg_after_header() -> Vec<u8> {
        vec![0xff, 0xd8, 0xff, 0xdb]
    }
}
