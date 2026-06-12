// SPDX-License-Identifier: Apache-2.0

//! A finalised episode's cameras, read from its recording by the
//! recording's indexes (`kapto_capture::seek`): which streams are cameras,
//! every frame's receive time, and any one frame's picture, a raw one
//! encoded as JPEG as the live view's bridge would.
//!
//! The frame times of the cameras asked about last are kept, so stepping
//! through an episode reads only the chunk that holds each frame.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::SystemTime;

use kapto_capture::finalise::RECORDING_FILE;
use kapto_capture::seek::ChannelIndex;
use kapto_core::{ErrorCode, KaptoError};
use kapto_viz::picture::{CameraKind, PictureFormat, Pictures};
use kapto_viz::recorded_cameras;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::episode::finalised_folder;

/// How many cameras' frame times are kept at once.
const CACHED: usize = 8;

/// A camera's frames in an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CameraFrames {
    /// The camera's stream.
    pub camera: String,
    /// When each frame was received, UTC nanoseconds, in order: frame `n`
    /// is the `n`-th.
    pub times_utc_ns: Vec<i64>,
}

/// One camera's frames as found by the recording's indexes, with how they
/// are laid out.
struct Indexed {
    recording: PathBuf,
    /// The recording's modification time when it was indexed: a recording
    /// changed since (by a retention plan) is indexed again.
    modified: Option<SystemTime>,
    camera: String,
    kind: CameraKind,
    index: ChannelIndex,
}

static CACHE: LazyLock<Mutex<Vec<Arc<Indexed>>>> = LazyLock::new(Mutex::default);

/// Every frame time of camera `camera` in episode `id`.
///
/// # Errors
///
/// As the episode's other endpoints for `id`; `viewer.camera_not_found` if
/// the episode's recording has no such camera; `recording.unreadable` if
/// it can't be read by its indexes.
pub fn frames(data_dir: &Path, id: &str, camera: &str) -> Result<CameraFrames, KaptoError> {
    let indexed = indexed(data_dir, id, camera)?;
    Ok(CameraFrames {
        camera: camera.to_owned(),
        times_utc_ns: indexed
            .index
            .times()
            .map(|time| i64::try_from(time).unwrap_or(i64::MAX))
            .collect(),
    })
}

/// Frame `n` of camera `camera` in episode `id`: its picture, as the page
/// draws it, and its media type.
///
/// # Errors
///
/// As [`frames`]; `viewer.camera_not_found` if the camera has no frame
/// `n`; `viewer.picture_unshown` with the reason if it can't be shown.
pub fn picture(
    data_dir: &Path,
    id: &str,
    camera: &str,
    n: usize,
) -> Result<(&'static str, Vec<u8>), KaptoError> {
    let indexed = indexed(data_dir, id, camera)?;
    let (_, frame) = indexed.index.read(n)?.ok_or_else(|| {
        KaptoError::new(ErrorCode::ViewerCameraNotFound)
            .detail(format!(
                "camera `{camera}` has {} frames, numbered from 0; there is no frame {n}",
                indexed.index.len()
            ))
            .context("episode", id.to_owned())
    })?;
    let mut pictures = Pictures::default();
    let shown = pictures.picture(&indexed.kind, &frame).map_err(|why| {
        KaptoError::new(ErrorCode::ViewerPictureUnshown)
            .detail(why.to_string())
            .context("episode", id.to_owned())
            .context("camera", camera.to_owned())
    })?;
    let media_type = match shown.format {
        PictureFormat::Jpeg => "image/jpeg",
        PictureFormat::Png => "image/png",
    };
    Ok((media_type, shown.bytes.to_vec()))
}

/// The names of the cameras episode `id` was recorded with, in declaration
/// order.
///
/// # Errors
///
/// As [`frames`], but not `viewer.camera_not_found`.
pub fn cameras(data_dir: &Path, id: &str) -> Result<Vec<String>, KaptoError> {
    let recording = finalised_folder(data_dir, id)?.join(RECORDING_FILE);
    Ok(recorded_cameras(&recording)?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// Camera `camera` of episode `id`, indexed: from the cache if the
/// recording hasn't changed since, read and kept otherwise.
fn indexed(data_dir: &Path, id: &str, camera: &str) -> Result<Arc<Indexed>, KaptoError> {
    let recording = finalised_folder(data_dir, id)?.join(RECORDING_FILE);
    let modified = std::fs::metadata(&recording)
        .and_then(|metadata| metadata.modified())
        .ok();
    let cache = || CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let found = cache()
        .iter()
        .find(|indexed| {
            indexed.recording == recording
                && indexed.camera == camera
                && indexed.modified == modified
        })
        .cloned();
    if let Some(found) = found {
        return Ok(found);
    }
    let kind = recorded_cameras(&recording)?
        .into_iter()
        .find(|(name, _)| name == camera)
        .map(|(_, kind)| kind)
        .ok_or_else(|| {
            KaptoError::new(ErrorCode::ViewerCameraNotFound)
                .detail(format!(
                    "the episode wasn't recorded with a camera `{camera}`"
                ))
                .context("episode", id.to_owned())
        })?;
    // Read without the cache's lock: other requests go on meanwhile.
    let index = ChannelIndex::open(&recording, camera)?;
    let indexed = Arc::new(Indexed {
        recording,
        modified,
        camera: camera.to_owned(),
        kind,
        index,
    });
    let mut cache = cache();
    cache.retain(|kept| !(kept.recording == indexed.recording && kept.camera == indexed.camera));
    cache.push(Arc::clone(&indexed));
    if cache.len() > CACHED {
        cache.remove(0);
    }
    Ok(indexed)
}
