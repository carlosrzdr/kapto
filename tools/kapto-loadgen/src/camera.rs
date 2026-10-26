// SPDX-License-Identifier: Apache-2.0

//! The synthetic camera: JPEG frames of a moving picture, as a robot's
//! camera sends them compressed. A second of frames, as many as the rate up
//! to [`MOST_FRAMES`], is encoded once when the generator starts and played
//! in a loop, so sending a frame costs no more than sending its bytes; each
//! stream starts at a frame of its own.
//!
//! The picture is a wrist camera's view of the sample arm's cell, drawn
//! plainly: the table with the red cube and the bin swaying past as the
//! arm moves, and the gripper's fingers opening and closing in front, so
//! consecutive frames differ as a camera's do, with a grain over every
//! pixel, so they compress to a real scene's sizes (about 360 KB at
//! 1920x1080) rather than a flat picture's.
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
    // The camera sways over the table as the arm moves, and the gripper's
    // fingers, fixed to the camera, open and close.
    let sway = (0.06 * (turn * TAU).cos() * w, 0.04 * (turn * TAU).sin() * h);
    let opening = 0.5 + 0.5 * (turn * TAU).sin();
    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    let frame = (turn * 1e6) as u32;
    for row in 0..height {
        for column in 0..width {
            let (x, y) = (f64::from(column), f64::from(row));
            let base = wrist_view(
                x / w,
                y / h,
                (x + sway.0) / h,
                (y + sway.1) / h,
                w / h,
                opening,
            );
            // Darker towards the corners, as a small lens is.
            let (dx, dy) = (x / w - 0.5, y / h - 0.5);
            let light = 1.0 - 0.9 * (dx * dx + dy * dy);
            let grain = noise(u32::from(column), u32::from(row), frame);
            rgb.extend(
                base.map(|value| ((f64::from(value) * light) as u8).saturating_add_signed(grain)),
            );
        }
    }
    rgb
}

/// The colour a wrist camera sees at `(u, v)` of its frame (each 0 to 1):
/// the gripper's fingers `opening` (0 to 1) apart in front, then on the
/// table at `(tx, ty)` (in frame heights, the camera's sway added) the red
/// cube, its shadow and the bin, with the wall beyond the table's edge.
/// `aspect` is the frame's width over its height.
fn wrist_view(u: f64, v: f64, tx: f64, ty: f64, aspect: f64, opening: f64) -> [u8; 3] {
    // The fingers, from the bottom edge, and the palm between them.
    let finger = 0.07;
    let gap = 0.12 + 0.1 * opening;
    let left = 0.5 - gap;
    let right = 0.5 + gap;
    if v > 0.92 && (0.2..0.8).contains(&u) {
        return [55, 58, 64];
    }
    if v > 0.7 && ((left - finger..left).contains(&u) || (right..right + finger).contains(&u)) {
        let edge = (u - (left - finger)).abs() < 0.008 || (u - (right + finger)).abs() < 0.008;
        return if edge {
            [190, 196, 204]
        } else {
            [140, 146, 154]
        };
    }
    // The red cube: its top face, its front face below, and its shadow.
    let cube = (0.42 * aspect, 0.55);
    let half = 0.09;
    let (cx, cy) = (tx - cube.0, ty - cube.1);
    if cx.abs() < half && cy.abs() < half {
        return [205, 42, 38];
    }
    if cx.abs() < half && (half..half + 0.05).contains(&cy) {
        return [140, 26, 24];
    }
    let shadowed = (cx - 0.04).abs() < half && (half..half + 0.09).contains(&(cy - 0.02));
    // The bin: a rim around a dark inside.
    let bin = (0.78 * aspect, 0.42);
    let (bx, by) = ((tx - bin.0) / 0.2, (ty - bin.1) / 0.14);
    let reach = bx * bx + by * by;
    if reach < 0.62 {
        return [28, 30, 34];
    }
    if reach < 1.0 {
        return [72, 78, 86];
    }
    // The wall beyond the table's far edge, and the table's planks.
    if ty < 0.16 {
        return [64, 72, 84];
    }
    let plank = (tx / 0.14).fract() < 0.06;
    let base = if plank {
        [128, 112, 94]
    } else {
        [172, 154, 132]
    };
    if shadowed {
        base.map(|value| value / 2 + 20)
    } else {
        base
    }
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
