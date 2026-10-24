// SPDX-License-Identifier: Apache-2.0

//! One finalised episode in full: what its `episode.json` says and the
//! files its manifest lists, for the web UI's episode details.
//!
//! The API's own types rather than `episode.json`'s: the API version and
//! the file format change on their own schedules.

use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use kapto_store::manifest::{FINALISED_FILE, Manifest};
use kapto_store::{BIN_DIR, Episode, LossDetectionKind, Reader, episode_folder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A finalised episode in full.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeDetail {
    /// Its ID, which is also its folder's name.
    pub id: String,
    /// Its folder, or where it was if it is no longer stored here.
    pub folder: String,
    /// Whether its folder is still here; `false` once deleted here, verified
    /// elsewhere: what it says then is from the summary kept of it, and its
    /// files are those uploaded (`/api/episodes/{id}/uploads`).
    pub stored_here: bool,
    /// When it was deleted by hand into the bin, in UTC nanoseconds since
    /// the Unix epoch; `null` unless it is in the bin.
    #[serde(default)]
    pub binned_utc_ns: Option<i64>,
    /// When recording started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// When it ended, in UTC nanoseconds since the Unix epoch.
    pub ended_utc_ns: i64,
    /// When Start was asked, in UTC nanoseconds since the Unix epoch: later
    /// than `started_utc_ns` by its pre-roll. `null` for an episode recorded
    /// before Kapto kept it.
    pub commanded_start_utc_ns: Option<i64>,
    /// When Stop was asked; `null` if it ended otherwise (aborted, the disk
    /// full, the process stopped). Earlier than `ended_utc_ns` by its
    /// post-roll.
    pub commanded_stop_utc_ns: Option<i64>,
    /// The Kapto version that recorded it.
    pub kapto_version: String,
    /// The robot package it was recorded with; `null` if it is
    /// `unchecked`.
    pub package: Option<EpisodePackage>,
    /// The session profile it was recorded with, as it was then; `null` if
    /// none was chosen.
    pub profile: Option<EpisodeProfile>,
    /// The task it was recorded for, as it was then; `null` if none was
    /// chosen.
    pub task: Option<EpisodeTask>,
    /// Each stream's counts.
    pub streams: Vec<EpisodeStream>,
    /// The files in its folder, as its manifest lists them.
    pub files: Vec<EpisodeFile>,
    /// The ring buffers between receiving and writing.
    pub buffers: EpisodeBuffers,
    /// How quickly what was received reached the disk.
    pub durability: EpisodeDurability,
    /// The wall clock while it was recorded.
    pub clock: EpisodeClock,
    /// How it was recovered, if recording stopped without finishing it;
    /// `null` if recording finished it. A recovered episode's losses, buffers,
    /// time to disk and clock aren't known: only finishing would have
    /// counted them.
    pub recovered: Option<EpisodeRecovered>,
}

/// How an episode was recovered after recording stopped without finishing
/// it, by a crash, a kill or a power cut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeRecovered {
    /// When it was recovered, in UTC nanoseconds since the Unix epoch.
    pub at_utc_ns: i64,
    /// Bytes cut off the end of its recording: written only in part, or not
    /// on disk before recording stopped.
    pub bytes_cut: u64,
    /// The newest message kept, in UTC nanoseconds; `null` if none was.
    pub last_message_utc_ns: Option<i64>,
    /// Why it had to be recovered, in words.
    pub reason: String,
}

impl From<kapto_store::Recovered> for EpisodeRecovered {
    fn from(recovered: kapto_store::Recovered) -> Self {
        Self {
            at_utc_ns: recovered.at_utc_ns,
            bytes_cut: recovered.bytes_cut,
            last_message_utc_ns: recovered.last_message_utc_ns,
            reason: recovered.reason,
        }
    }
}

/// The robot package an episode was recorded with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodePackage {
    /// Its ID.
    pub id: String,
    /// Its version.
    pub version: String,
    /// Its content hash when recording started: episodes with the same hash
    /// were recorded with exactly the same files.
    pub hash: String,
}

impl From<kapto_store::PackageIdentity> for EpisodePackage {
    fn from(package: kapto_store::PackageIdentity) -> Self {
        Self {
            id: package.id,
            version: package.version,
            hash: package.hash,
        }
    }
}

/// The session profile an episode was recorded with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeProfile {
    /// Its ID.
    pub id: String,
    /// Its content hash: the same for every episode recorded with exactly
    /// this profile, whatever its file is called.
    pub hash: Option<String>,
    /// Who operated the robot.
    pub operator: Option<String>,
    /// Where it was recorded.
    pub environment: Option<String>,
    /// How the robot was driven: `teleoperated`, `autonomous`, `mixed` or
    /// `scripted`.
    pub mode: Option<String>,
    /// Words to find the episode by.
    pub tags: Vec<String>,
    /// Anything else worth knowing.
    pub notes: Option<String>,
}

/// The task an episode was recorded for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeTask {
    /// Its ID.
    pub id: String,
    /// Its content hash: the same for every episode recorded for exactly
    /// this task, whatever its file is called.
    pub hash: Option<String>,
    /// What people call it.
    pub name: Option<String>,
    /// A sentence or two about it.
    pub description: Option<String>,
    /// The instruction given to the operator or the policy.
    pub prompt: Option<String>,
    /// The objects it involves.
    pub objects: Vec<String>,
    /// Its steps in order.
    pub subtasks: Vec<String>,
}

/// One stream of an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeStream {
    /// The stream's name.
    pub stream: String,
    /// How messages lost before reaching Kapto were noticed: `sequence`
    /// (exact, from sequence numbers), `rate_gap` (an estimate, from
    /// silences) or `undetectable` (they can't be).
    pub loss_detection: String,
    /// Messages that reached Kapto.
    pub received: u64,
    /// Received messages whose live fields could be read.
    pub decoded: u64,
    /// Received messages stored in the recording.
    pub written: u64,
    /// Received messages Kapto discarded.
    pub dropped_by_us: u64,
    /// Messages the source sent that never reached Kapto.
    pub lost_upstream: u64,
    /// For a stream over a connection: how often the connection was lost.
    pub disconnects: Option<u64>,
    /// For a UDP stream only some sources may send to: datagrams from any
    /// other, discarded, and neither received nor lost.
    pub rejected: Option<u64>,
    /// For a stream kept for late subscribers: how many messages kept from
    /// before the episode it starts with, not counted in `received`.
    pub carried: Option<u64>,
}

/// One file of an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeFile {
    /// Its path inside the episode's folder.
    pub path: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// The ring buffers of an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeBuffers {
    /// Batches the buffers held.
    pub batches: u64,
    /// The most batches waiting at once.
    pub peak_batches: u64,
    /// The memory they took, in bytes.
    pub memory_bytes: u64,
}

/// How quickly an episode's messages reached the disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeDurability {
    /// How often the recording was synced, in nanoseconds.
    pub sync_interval_ns: u64,
    /// How many times.
    pub syncs: u64,
    /// 99 % of the messages were on disk within this many nanoseconds of
    /// arriving.
    pub p99_unsynced_ns: u64,
    /// The longest any message waited, in nanoseconds.
    pub max_unsynced_ns: u64,
}

/// The wall clock while an episode was recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeClock {
    /// How often it was compared with the receive clock.
    pub samples: u64,
    /// The largest uncertainty of a comparison, in nanoseconds.
    pub max_uncertainty_ns: u64,
    /// How many times it jumped, e.g. when the time was set.
    pub steps: u64,
}

/// The folder of the finalised episode `id` in `data_dir`, or in its bin
/// if it was deleted by hand and the bin not emptied yet.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID, so it can never name
/// a folder elsewhere; `api.episode_not_found` if no finalised episode has
/// it.
pub(crate) fn finalised_folder(data_dir: &Path, id: &str) -> Result<PathBuf, KaptoError> {
    let folder = episode_folder(data_dir, id).ok_or_else(|| {
        KaptoError::new(ErrorCode::ApiInvalidRequest).detail(format!("`{id}` isn't an episode ID"))
    })?;
    let binned = data_dir.join(BIN_DIR).join(id);
    if folder.join(FINALISED_FILE).exists() {
        Ok(folder)
    } else if binned.join(FINALISED_FILE).exists() {
        Ok(binned)
    } else {
        Err(
            KaptoError::new(ErrorCode::ApiEpisodeNotFound).detail(format!(
                "no finalised episode `{id}` in {}",
                data_dir.display()
            )),
        )
    }
}

/// Whether the episode `id` is stored here, its finalised folder in
/// `data_dir`: `false` if it was deleted here, as `reader` has it, once
/// verified elsewhere. What was worked out from it is kept in `kapto.db`
/// all the same.
///
/// # Errors
///
/// As [`finalised_folder`], `api.episode_not_found` if it wasn't deleted
/// here either; `store.query_failed` if its deletion can't be read.
pub(crate) fn stored_here(data_dir: &Path, reader: &Reader, id: &str) -> Result<bool, KaptoError> {
    match finalised_folder(data_dir, id) {
        Ok(_) => Ok(true),
        Err(error) if error.code() == ErrorCode::ApiEpisodeNotFound => {
            if reader.deletion(id)?.is_some() {
                Ok(false)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

/// The finalised episode `id` in `data_dir`.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID, so it can never name
/// a folder elsewhere; `api.episode_not_found` if no finalised episode has
/// it; whatever reading its `episode.json` or manifest reports.
pub fn read(data_dir: &Path, id: &str) -> Result<EpisodeDetail, KaptoError> {
    let folder = finalised_folder(data_dir, id)?;
    let episode = Episode::read(&folder)?;
    let manifest = Manifest::read(&folder)?;
    let files = manifest
        .files
        .into_iter()
        .map(|file| EpisodeFile {
            path: file.path,
            bytes: file.bytes,
        })
        .collect();
    Ok(detail(episode, &folder, files, true))
}

/// An episode deleted from `data_dir`, from `summary`, the `episode.json`
/// kept of it, with `files`, those uploaded.
///
/// # Errors
///
/// `episode.unreadable` if the summary can't be read.
pub fn from_summary(
    data_dir: &Path,
    summary: &str,
    files: Vec<EpisodeFile>,
) -> Result<EpisodeDetail, KaptoError> {
    let episode = Episode::parse(summary.as_bytes()).map_err(|why| {
        KaptoError::new(ErrorCode::EpisodeUnreadable)
            .detail(format!("the summary kept of it, deleted here: {why}"))
    })?;
    let folder = data_dir.join(&episode.id);
    Ok(detail(episode, &folder, files, false))
}

fn detail(
    episode: Episode,
    folder: &Path,
    files: Vec<EpisodeFile>,
    stored_here: bool,
) -> EpisodeDetail {
    EpisodeDetail {
        id: episode.id,
        folder: folder.display().to_string(),
        stored_here,
        binned_utc_ns: None,
        started_utc_ns: episode.started_utc_ns,
        ended_utc_ns: episode.ended_utc_ns,
        commanded_start_utc_ns: episode.commanded_start_utc_ns,
        commanded_stop_utc_ns: episode.commanded_stop_utc_ns,
        kapto_version: episode.kapto_version,
        package: episode.package.map(EpisodePackage::from),
        profile: episode.profile.map(|profile| EpisodeProfile {
            id: profile.id,
            hash: profile.hash,
            operator: profile.operator,
            environment: profile.environment,
            mode: profile.mode.map(|mode| mode.as_str().to_owned()),
            tags: profile.tags,
            notes: profile.notes,
        }),
        task: episode.task.map(|task| EpisodeTask {
            id: task.id,
            hash: task.hash,
            name: task.name,
            description: task.description,
            prompt: task.prompt,
            objects: task.objects,
            subtasks: task.subtasks,
        }),
        streams: episode
            .streams
            .into_iter()
            .map(|stream| EpisodeStream {
                stream: stream.id,
                loss_detection: match stream.loss_detection {
                    LossDetectionKind::Sequence => "sequence",
                    LossDetectionKind::RateGap => "rate_gap",
                    LossDetectionKind::Undetectable => "undetectable",
                    LossDetectionKind::PublisherCount => "publisher_count",
                }
                .to_owned(),
                received: stream.received,
                decoded: stream.decoded,
                written: stream.written,
                dropped_by_us: stream.dropped_by_us,
                lost_upstream: stream.lost_upstream,
                disconnects: stream.disconnects,
                rejected: stream.rejected,
                carried: stream.carried,
            })
            .collect(),
        files,
        buffers: EpisodeBuffers {
            batches: episode.buffers.batches,
            peak_batches: episode.buffers.peak_batches,
            memory_bytes: episode.buffers.memory_bytes,
        },
        durability: EpisodeDurability {
            sync_interval_ns: episode.durability.sync_interval_ns,
            syncs: episode.durability.syncs,
            p99_unsynced_ns: episode.durability.p99_unsynced_ns,
            max_unsynced_ns: episode.durability.max_unsynced_ns,
        },
        clock: EpisodeClock {
            samples: episode.clock.samples,
            max_uncertainty_ns: episode.clock.max_uncertainty_ns,
            steps: episode.clock.steps.len() as u64,
        },
        recovered: episode.recovered.map(EpisodeRecovered::from),
    }
}
