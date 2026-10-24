// SPDX-License-Identifier: Apache-2.0

//! The pictures the bridge keeps of each camera, so a page looking back
//! sees what the cameras saw then: fewer than were shown, at most
//! `viewer.camera_history_fps` a second per camera, made smaller, for as
//! long as the rest of the history within a budget of bytes for every
//! camera together, the oldest going first.
//!
//! A picture is made smaller by the JPEG decoder's own scaling, which
//! decodes at a half, a quarter or an eighth of the size for a fraction of
//! the work of a full decode, then encoded as JPEG again.

use std::collections::{BTreeMap, VecDeque};
use std::io::Cursor;
use std::time::Duration;

use jpeg_decoder::{Decoder, PixelFormat};
use jpeg_encoder::{ColorType, Encoder};
use tungstenite::Bytes;

/// How the pictures kept are encoded: good enough to see what happened.
const QUALITY: u8 = 75;

/// What the bridge keeps of cameras' pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CameraHistorySettings {
    /// At least this long between two pictures kept of one camera.
    pub every: Duration,
    /// How wide, at most, a picture kept is, in pixels.
    pub width: u32,
    /// How many bytes of pictures are kept, every camera together.
    pub bytes: usize,
    /// How long a picture is kept, in nanoseconds.
    pub age_ns: i64,
}

impl Default for CameraHistorySettings {
    fn default() -> Self {
        Self {
            every: Duration::from_millis(200),
            width: 640,
            bytes: 128 * 1024 * 1024,
            age_ns: 600_000_000_000,
        }
    }
}

/// One picture kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    /// The camera's position among the streams.
    pub stream: u16,
    /// When Kapto received its frame, in UTC nanoseconds.
    pub received_utc_ns: i64,
    /// Its width in pixels, as kept.
    pub width: u32,
    /// Its height in pixels, as kept.
    pub height: u32,
    /// The picture, as JPEG.
    pub jpeg: Bytes,
}

/// The pictures kept of every camera, oldest first.
#[derive(Debug)]
pub struct CameraHistory {
    settings: CameraHistorySettings,
    kept: VecDeque<Kept>,
    bytes: usize,
    /// When the newest picture kept of each camera was received.
    newest: BTreeMap<u16, i64>,
}

impl CameraHistory {
    /// Keeping nothing yet, by `settings`.
    #[must_use]
    pub fn new(settings: CameraHistorySettings) -> Self {
        Self {
            settings,
            kept: VecDeque::new(),
            bytes: 0,
            newest: BTreeMap::new(),
        }
    }

    /// The settings it keeps pictures by.
    #[must_use]
    pub fn settings(&self) -> CameraHistorySettings {
        self.settings
    }

    /// Whether a picture of camera `stream` received at `received_utc_ns`
    /// is to be kept: the first, or one at least a setting's interval after
    /// the last kept.
    #[must_use]
    pub fn due(&self, stream: u16, received_utc_ns: i64) -> bool {
        let every = i64::try_from(self.settings.every.as_nanos()).unwrap_or(i64::MAX);
        self.newest
            .get(&stream)
            .is_none_or(|newest| received_utc_ns.saturating_sub(*newest) >= every)
    }

    /// Keeps `picture`, then lets go of the oldest pictures until what is
    /// kept is younger than the setting's age, counted from this one, and
    /// within its bytes.
    pub fn keep(&mut self, picture: Kept) {
        self.newest.insert(picture.stream, picture.received_utc_ns);
        let oldest_kept = picture.received_utc_ns.saturating_sub(self.settings.age_ns);
        self.bytes += picture.jpeg.len();
        self.kept.push_back(picture);
        while let Some(oldest) = self.kept.front() {
            if oldest.received_utc_ns >= oldest_kept && self.bytes <= self.settings.bytes {
                break;
            }
            self.bytes -= oldest.jpeg.len();
            self.kept.pop_front();
        }
    }

    /// The picture kept of camera `stream` received nearest `at_utc_ns`.
    #[must_use]
    pub fn nearest(&self, stream: u16, at_utc_ns: i64) -> Option<&Kept> {
        self.kept
            .iter()
            .filter(|kept| kept.stream == stream)
            .min_by_key(|kept| kept.received_utc_ns.abs_diff(at_utc_ns))
    }

    /// The bytes of pictures kept.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// `jpeg` made no wider than `width`, as JPEG, with its size: decoded at
/// the largest of a half, a quarter or an eighth of its size that fits, or
/// whole if it already does. `None` for a picture that can't be decoded, or
/// whose pixels aren't grey or RGB.
#[must_use]
pub fn smaller(jpeg: &[u8], width: u32) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = Decoder::new(Cursor::new(jpeg));
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    let (whole_width, whole_height) = (u32::from(info.width), u32::from(info.height));
    let divisor = [1, 2, 4, 8]
        .into_iter()
        .find(|divisor| whole_width.div_ceil(*divisor) <= width)
        .unwrap_or(8);
    let wanted = (
        u16::try_from(whole_width.div_ceil(divisor)).ok()?,
        u16::try_from(whole_height.div_ceil(divisor)).ok()?,
    );
    let (scaled_width, scaled_height) = decoder.scale(wanted.0, wanted.1).ok()?;
    let pixels = decoder.decode().ok()?;
    let color = match decoder.info()?.pixel_format {
        PixelFormat::L8 => ColorType::Luma,
        PixelFormat::RGB24 => ColorType::Rgb,
        PixelFormat::L16 | PixelFormat::CMYK32 => return None,
    };
    let mut out = Vec::new();
    Encoder::new(&mut out, QUALITY)
        .encode(&pixels, scaled_width, scaled_height, color)
        .ok()?;
    Some((u32::from(scaled_width), u32::from(scaled_height), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;

    fn picture(stream: u16, at_s: f64, bytes: usize) -> Kept {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "test times are a few seconds"
        )]
        let received_utc_ns = (at_s * 1e9) as i64;
        Kept {
            stream,
            received_utc_ns,
            width: 640,
            height: 360,
            jpeg: Bytes::from(vec![0; bytes]),
        }
    }

    #[test]
    fn one_picture_per_interval_is_kept_per_camera() {
        let history = |every_ms| {
            CameraHistory::new(CameraHistorySettings {
                every: Duration::from_millis(every_ms),
                ..CameraHistorySettings::default()
            })
        };
        let mut five_a_second = history(200);
        let mut kept = Vec::new();
        // Two cameras at 60 a second for a second.
        for frame in 0..60 {
            for stream in [1, 2] {
                let at = f64::from(frame) / 60.0;
                let picture = picture(stream, at, 10);
                if five_a_second.due(stream, picture.received_utc_ns) {
                    kept.push(stream);
                    five_a_second.keep(picture);
                }
            }
        }
        assert_eq!(kept.iter().filter(|stream| **stream == 1).count(), 5);
        assert_eq!(kept.iter().filter(|stream| **stream == 2).count(), 5);
    }

    #[test]
    fn the_oldest_go_first_by_age_and_by_size() {
        let mut history = CameraHistory::new(CameraHistorySettings {
            every: Duration::ZERO,
            width: 640,
            bytes: 300,
            age_ns: 10 * SECOND,
        });
        for second in 0..5 {
            history.keep(picture(1, f64::from(second), 100));
        }
        // Over the bytes: only the newest three.
        assert_eq!(history.bytes(), 300);
        assert_eq!(history.nearest(1, 0).unwrap().received_utc_ns, 2 * SECOND);
        // One 20 s later: the others are older than the age.
        history.keep(picture(1, 24.0, 100));
        assert_eq!(history.bytes(), 100);
        assert_eq!(history.nearest(1, 0).unwrap().received_utc_ns, 24 * SECOND);
    }

    #[test]
    fn the_nearest_picture_of_the_camera_asked_for() {
        let mut history = CameraHistory::new(CameraHistorySettings::default());
        for second in [1.0, 2.0, 3.0] {
            history.keep(picture(1, second, 10));
            history.keep(picture(2, second + 0.4, 10));
        }
        let nearest = |stream, at_s: f64| {
            #[allow(clippy::cast_possible_truncation, reason = "test times")]
            let at = (at_s * 1e9) as i64;
            history.nearest(stream, at).map(|kept| kept.received_utc_ns)
        };
        assert_eq!(nearest(1, 2.4), Some(2 * SECOND));
        assert_eq!(nearest(2, 2.4), Some(2_400_000_000));
        assert_eq!(nearest(1, 99.0), Some(3 * SECOND));
        assert_eq!(nearest(3, 2.0), None);
    }

    /// A JPEG of `width` by `height`, a gradient.
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let pixels: Vec<u8> = (0..u32::from(width) * u32::from(height))
            .flat_map(|at| {
                let shade = u8::try_from(at % 256).unwrap_or(0);
                [shade, shade / 2, 255 - shade]
            })
            .collect();
        let mut out = Vec::new();
        Encoder::new(&mut out, 90)
            .encode(&pixels, width, height, ColorType::Rgb)
            .unwrap();
        out
    }

    #[test]
    fn a_picture_is_made_no_wider_than_asked_by_the_decoders_own_scaling() {
        let full = jpeg(1920, 1080);
        // A half is 960, too wide: a quarter it is.
        let (width, height, smaller_jpeg) = smaller(&full, 640).unwrap();
        assert_eq!((width, height), (480, 270));
        assert!(
            smaller_jpeg.len() < full.len() / 4,
            "{} of {}",
            smaller_jpeg.len(),
            full.len()
        );
        let mut decoder = Decoder::new(Cursor::new(&smaller_jpeg));
        decoder.read_info().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (480, 270));
        // Already narrow enough: kept at its size.
        assert_eq!(
            smaller(&jpeg(320, 240), 640).map(|(w, h, _)| (w, h)),
            Some((320, 240))
        );
        // Not a JPEG.
        assert_eq!(smaller(b"not a picture", 640), None);
    }
}
