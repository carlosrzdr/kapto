// SPDX-License-Identifier: Apache-2.0

//! An episode's cameras as checks see them: for each, when every frame was
//! received, how large its picture was, and the frames that hold no picture
//! of the camera's kind. Read in the same pass as the roles, by the
//! `streams.toml` the recording carries.

use std::collections::BTreeMap;
use std::path::Path;

use kapto_capture::reader::read_start;
use kapto_config::StreamDeclaration;
use kapto_config::carried::{carried_manifest, carried_streams};
use kapto_core::KaptoError;
use kapto_decode::camera::{CameraKind, Frame, PixelEncoding, frame};

/// One camera's frames through an episode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CameraTimeline {
    /// When each frame holding a picture was received, in UTC nanoseconds,
    /// in the order received.
    pub times_utc_ns: Vec<i64>,
    /// Each one's picture's size in bytes, as compressed for a compressed
    /// one.
    pub sizes: Vec<u64>,
    /// Why the first frame holds no picture of the camera's kind, if it
    /// doesn't.
    pub first_unreadable: Option<String>,
    /// When each later frame holding none was received.
    pub unreadable_utc_ns: Vec<i64>,
    /// Why the first of those holds none.
    pub unreadable_why: Option<String>,
}

impl CameraTimeline {
    /// Every frame, with a picture or not.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.times_utc_ns.len()
            + self.unreadable_utc_ns.len()
            + usize::from(self.first_unreadable.is_some())
    }
}

/// Each camera of a recording, by its stream's name, and what its frames
/// held so far.
#[derive(Debug, Default)]
pub(crate) struct CameraReader {
    kinds: BTreeMap<String, CameraKind>,
    frames: BTreeMap<String, CameraTimeline>,
}

impl CameraReader {
    /// The cameras among `declared`.
    pub(crate) fn of(declared: &[StreamDeclaration]) -> Self {
        let kinds = declared
            .iter()
            .filter_map(|stream| Some((stream.id.to_string(), stream.camera_kind()?)))
            .collect();
        Self {
            kinds,
            frames: BTreeMap::new(),
        }
    }

    /// Whether the recording has no camera.
    pub(crate) fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Takes `data`, a message of the stream `topic` received at
    /// `log_time`, if it is a camera's frame.
    pub(crate) fn take(&mut self, topic: &str, log_time: u64, data: &[u8]) {
        let Some(kind) = self.kinds.get(topic) else {
            return;
        };
        let frames = self.frames.entry(topic.to_owned()).or_default();
        let time = i64::try_from(log_time).unwrap_or(i64::MAX);
        match picture_size(kind, data) {
            Ok(size) => {
                frames.times_utc_ns.push(time);
                frames.sizes.push(size);
            }
            Err(why) if frames.frames() == 0 => frames.first_unreadable = Some(why),
            Err(why) => {
                frames.unreadable_utc_ns.push(time);
                frames.unreadable_why.get_or_insert(why);
            }
        }
    }

    /// Every camera's frames; one that sent none has none.
    pub(crate) fn finish(mut self) -> BTreeMap<String, CameraTimeline> {
        for name in self.kinds.keys() {
            self.frames.entry(name.clone()).or_default();
        }
        self.frames
    }
}

/// The streams the recording at `recording` declares, by the `streams.toml`
/// it carries. A recording whose stream declarations can't be read, as one
/// recorded without a package carrying another file only, has none known:
/// its other checks still run.
pub(crate) fn declared_streams(recording: &Path) -> Result<Vec<StreamDeclaration>, KaptoError> {
    let start = read_start(recording)?;
    Ok(carried_manifest(&start.attachments, recording)
        .and_then(|manifest| carried_streams(&start.attachments, recording, manifest.as_ref()))
        .unwrap_or_default())
}

/// The size of the picture in `data`, a frame of a camera of `kind`, or why
/// it holds none: not what its kind sends, a compressed picture neither
/// JPEG nor PNG, or a raw one shorter than its size needs.
fn picture_size(kind: &CameraKind, data: &[u8]) -> Result<u64, String> {
    match frame(kind, data).map_err(|error| error.to_string())? {
        Frame::Compressed { data, .. } => Ok(data.len() as u64),
        Frame::Raw {
            data,
            width,
            height,
            step,
            encoding,
            ..
        } => {
            let has = data.len() as u64;
            // An encoding not among those named holds pixels of a size
            // unknown here: its rows' length is all there is to go by.
            let row = PixelEncoding::named(encoding).map_or(step, |encoding| {
                u64::from(width) * encoding.bytes_per_pixel()
            });
            let needed = step * u64::from(height.saturating_sub(1)) + row;
            if width == 0 || height == 0 || has < needed {
                Err(format!(
                    "a raw picture of {has} bytes where its {width}x{height} {encoding} needs {needed}"
                ))
            } else {
                Ok(has)
            }
        }
    }
}
