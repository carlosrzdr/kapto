// SPDX-License-Identifier: Apache-2.0

//! The synthetic camera: JPEG frames of a moving picture, as a robot's
//! camera sends them compressed. A second of frames, as many as the rate up
//! to [`MOST_FRAMES`], is encoded once when the generator starts and played
//! in a loop, so sending a frame costs no more than sending its bytes; each
//! stream starts at a frame of its own.
//!
//! The picture is a colour gradient sweeping across and a bright square
//! circling the middle, so consecutive frames differ as a camera's do,
//! with a grain over every pixel, so they compress to a real scene's sizes
//! (about 360 KB at 1920x1080) rather than a flat picture's.
//!
//! Raw frames are the same pictures' pixels, uncompressed, as a simulator
//! or a depth camera sends them; fewer of them loop, [`RAW_FRAMES`], as
//! each is megabytes.

use std::io;

use jpeg_encoder::{ColorType, Encoder};

/// The most frames encoded, a second's worth at 120 fps.
pub const MOST_FRAMES: usize = 120;

/// The most raw frames kept: ten at 1920x1080 hold 62 MB.
pub const RAW_FRAMES: usize = 10;

/// JPEG quality, as a camera's driver commonly sends.
const QUALITY: u8 = 80;

/// How far a pixel's grain goes either way.
const GRAIN: i8 = 12;

/// A loop of encoded frames.
#[derive(Debug)]
pub struct Frames {
    frames: Vec<Vec<u8>>,
}

impl Frames {
    /// Encodes a second of `width` × `height` frames at `rate_hz`.
    ///
    /// # Errors
    ///
    /// If a frame can't be encoded, e.g. a size JPEG doesn't take.
    pub fn new(width: u16, height: u16, rate_hz: u32) -> io::Result<Self> {
        let count = usize::try_from(rate_hz)
            .unwrap_or(MOST_FRAMES)
            .clamp(1, MOST_FRAMES);
        let encode = |frame: usize| {
            #[allow(clippy::cast_precision_loss, reason = "at most 120 frames")]
            let turn = frame as f64 / count as f64;
            let mut jpeg = Vec::new();
            Encoder::new(&mut jpeg, QUALITY)
                .encode(&picture(width, height, turn), width, height, ColorType::Rgb)
                .map_err(io::Error::other)?;
            Ok(jpeg)
        };
        // Encoded on every core at once: a second of 1080p frames takes
        // seconds on one, which the generator would wait before sending.
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        let mut frames: Vec<io::Result<Vec<u8>>> = Vec::new();
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|worker| {
                    scope.spawn(move || {
                        (worker..count)
                            .step_by(threads)
                            .map(|frame| (frame, encode(frame)))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let mut encoded: Vec<_> = workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap_or_default())
                .collect();
            encoded.sort_by_key(|(frame, _)| *frame);
            frames = encoded.into_iter().map(|(_, jpeg)| jpeg).collect();
        });
        if frames.len() != count {
            return Err(io::Error::other("a thread encoding frames stopped"));
        }
        Ok(Self {
            frames: frames.into_iter().collect::<io::Result<_>>()?,
        })
    }

    /// `width` × `height` frames of pixels in `rgb8`, row after row with
    /// no padding: as many as a second at `rate_hz` up to [`RAW_FRAMES`].
    #[must_use]
    pub fn raw(width: u16, height: u16, rate_hz: u32) -> Self {
        let count = usize::try_from(rate_hz)
            .unwrap_or(RAW_FRAMES)
            .clamp(1, RAW_FRAMES);
        #[allow(clippy::cast_precision_loss, reason = "at most ten frames")]
        let frames = (0..count)
            .map(|frame| picture(width, height, frame as f64 / count as f64))
            .collect();
        Self { frames }
    }

    /// The frame stream `stream` sends at `tick`.
    #[must_use]
    pub fn frame(&self, stream: u16, tick: u64) -> &[u8] {
        let count = self.frames.len() as u64;
        let at = (tick + u64::from(stream) * 7) % count.max(1);
        usize::try_from(at)
            .ok()
            .and_then(|at| self.frames.get(at))
            .map_or(&[], Vec::as_slice)
    }

    /// The largest frame, in bytes: the `max_message_bytes` a stream of
    /// them needs.
    #[must_use]
    pub fn largest(&self) -> usize {
        self.frames.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// The picture `turn` (0 to 1) of the way through the loop, as RGB.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pixel values within 0–255 and coordinates within the frame"
)]
fn picture(width: u16, height: u16, turn: f64) -> Vec<u8> {
    use std::f64::consts::TAU;
    let (w, h) = (f64::from(width), f64::from(height));
    let side = w.min(h) / 5.0;
    let centre = (
        w / 2.0 + (turn * TAU).cos() * w / 4.0,
        h / 2.0 + (turn * TAU).sin() * h / 4.0,
    );
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    let frame = (turn * 1e6) as u32;
    for row in 0..height {
        for column in 0..width {
            let (x, y) = (f64::from(column), f64::from(row));
            let inside = (x - centre.0).abs() < side / 2.0 && (y - centre.1).abs() < side / 2.0;
            let base = if inside {
                [250, 240, 90]
            } else {
                let sweep = ((x / w + turn).fract() * 255.0) as u8;
                [sweep, (y / h * 255.0) as u8, 255 - sweep]
            };
            let grain = noise(u32::from(column), u32::from(row), frame);
            rgb.extend(base.map(|value| value.saturating_add_signed(grain)));
        }
    }
    rgb
}

/// The grain of `(x, y)` in `frame`: a pixel's own variation, between
/// -`GRAIN` and `GRAIN`, as a real scene's detail and a sensor's noise give
/// it, so frames compress to a real camera's sizes rather than a flat
/// picture's.
fn noise(x: u32, y: u32, frame: u32) -> i8 {
    let mut hash =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ frame.wrapping_mul(0xC2B2_AE3D);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2C1B_3C6D);
    hash ^= hash >> 12;
    let spread = u32::from(GRAIN.unsigned_abs()) * 2 + 1;
    i8::try_from(hash % spread).unwrap_or(0) - GRAIN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_compress_to_a_real_cameras_sizes_not_a_flat_pictures() {
        // About 57 KB at 640x480, 360 KB at 1920x1080: a busy scene's JPEG
        // at quality 80, where the picture alone would be a few KB.
        let frames = Frames::new(640, 480, 2).unwrap();
        assert!(
            (40_000..80_000).contains(&frames.largest()),
            "{} bytes",
            frames.largest()
        );
    }

    #[test]
    fn a_second_of_frames_is_encoded_and_played_in_a_loop() {
        let frames = Frames::new(64, 48, 30).unwrap();
        assert_eq!(frames.frames.len(), 30);
        assert_eq!(frames.frame(0, 0), frames.frame(0, 30));
        assert_ne!(frames.frame(0, 0), frames.frame(0, 1), "the picture moves");
        assert_ne!(frames.frame(0, 0), frames.frame(1, 0), "streams differ");
        assert_eq!(
            Frames::new(16, 16, 1_000).unwrap().frames.len(),
            MOST_FRAMES
        );
    }

    #[test]
    fn raw_frames_are_each_pixel_in_rgb8_and_fewer_loop() {
        let frames = Frames::raw(64, 48, 60);
        assert_eq!(frames.frames.len(), RAW_FRAMES);
        assert_eq!(frames.largest(), 64 * 48 * 3);
        assert_ne!(frames.frame(0, 0), frames.frame(0, 1), "the picture moves");
        assert_eq!(Frames::raw(64, 48, 2).frames.len(), 2);
    }

    #[test]
    fn each_frame_is_a_jpeg_of_its_size_as_another_decoder_reads_it() {
        let frames = Frames::new(64, 48, 4).unwrap();
        for frame in &frames.frames {
            let mut decoder = jpeg_decoder::Decoder::new(frame.as_slice());
            let pixels = decoder.decode().unwrap();
            let info = decoder.info().unwrap();
            assert_eq!((info.width, info.height), (64, 48));
            assert_eq!(pixels.len(), 64 * 48 * 3);
        }
        assert!(frames.largest() > 0);
    }
}
