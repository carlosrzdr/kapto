// SPDX-License-Identifier: Apache-2.0

//! The `camera` table of a TCP stream whose messages are a camera's frames,
//! so the live view can show them: JPEG pictures, or raw ones of a fixed
//! layout.
//!
//! ```toml
//! [streams.wrist_camera]
//! max_message_bytes = 921600
//! tcp = { listen = "0.0.0.0:7010", framing = { kind = "fixed", size = 921600 } }
//! camera = { format = "raw", width = 640, height = 480, encoding = "rgb8" }
//! ```
//!
//! A ROS 2 camera needs no table: its type says so.

use toml_edit::TableLike;

use super::{MAX_MESSAGE_BYTES, Reader};

/// The ROS 2 message types whose messages are a camera's frames.
pub const CAMERA_MESSAGE_TYPES: &[&str] =
    &["sensor_msgs/msg/CompressedImage", "sensor_msgs/msg/Image"];

/// What a TCP camera's frames are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraDeclaration {
    /// `format = "jpeg"`: each message is a JPEG picture.
    Jpeg,
    /// `format = "raw"`: each message is the pixels, row after row, without
    /// padding.
    Raw {
        /// Pixels per row.
        width: u32,
        /// Rows.
        height: u32,
        /// How each pixel is laid out.
        encoding: PixelEncoding,
    },
}

impl CameraDeclaration {
    /// The bytes of one frame, for a raw camera.
    #[must_use]
    pub fn frame_bytes(&self) -> Option<u64> {
        match *self {
            Self::Jpeg => None,
            Self::Raw {
                width,
                height,
                encoding,
            } => Some(u64::from(width) * u64::from(height) * encoding.bytes_per_pixel()),
        }
    }
}

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
    /// Its name in `streams.toml`.
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

pub(super) const CAMERA_KEYS: &[&str] = &["format", "width", "height", "encoding"];
const RAW_KEYS: &[&str] = &["width", "height", "encoding"];
/// The widest or tallest frame: 16K.
const MAX_SIDE: i64 = 16_384;

impl Reader<'_> {
    pub(super) fn camera(
        &mut self,
        table: &dyn TableLike,
        path: &str,
    ) -> Option<CameraDeclaration> {
        let path = format!("{path}.camera");
        self.unknown_keys(table, &path, CAMERA_KEYS);
        let format = self.choice(table, &path, "format", &[("jpeg", false), ("raw", true)]);
        let Some(raw) = format else {
            if table.get("format").is_none() {
                self.invalid(table, "format", format!("`{path}` needs `format`"));
            }
            return None;
        };
        if !raw {
            for key in RAW_KEYS.iter().filter(|key| table.get(key).is_some()) {
                let detail = format!("`{path}.{key}` is for raw frames; a JPEG says its own");
                self.invalid(table, key, detail);
            }
            return Some(CameraDeclaration::Jpeg);
        }
        let width = self.integer(table, &path, "width", 1..=MAX_SIDE);
        let height = self.integer(table, &path, "height", 1..=MAX_SIDE);
        let encodings = [
            PixelEncoding::Rgb8,
            PixelEncoding::Bgr8,
            PixelEncoding::Rgba8,
            PixelEncoding::Bgra8,
            PixelEncoding::Mono8,
        ]
        .map(|encoding| (encoding.name(), encoding));
        let encoding = self.choice(table, &path, "encoding", &encodings);
        for key in RAW_KEYS.iter().filter(|key| table.get(key).is_none()) {
            self.invalid(
                table,
                "format",
                format!("`{path}` of raw frames needs `{key}`"),
            );
        }
        let camera = CameraDeclaration::Raw {
            width: u32::try_from(width?).ok()?,
            height: u32::try_from(height?).ok()?,
            encoding: encoding?,
        };
        let bytes = camera.frame_bytes().unwrap_or(0);
        if bytes > u64::from(MAX_MESSAGE_BYTES) {
            let detail = format!(
                "`{path}` frames are {bytes} bytes, more than the {MAX_MESSAGE_BYTES} a message may be"
            );
            self.invalid(table, "width", detail);
            return None;
        }
        Some(camera)
    }
}
