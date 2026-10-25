// SPDX-License-Identifier: Apache-2.0

//! Recovering episodes whose recording stopped without finishing, by a
//! crash, a kill or a power cut, so none is left half-written and none is
//! mistaken for complete.
//!
//! An episode still in staging is salvaged: its recording is cut at its
//! last intact record and closed ([`salvage`](crate::salvage())), its `episode.json`
//! is written from what the recording holds, marked `recovered`, and it is
//! finalised as any other. One moved out of staging but stopped before its
//! manifest or its `_FINALISED` marker was complete already, and is only
//! finalised. One that can't be recovered stays where it is, never taken
//! for an episode, and says why. One another process holds is still being
//! written or finalised, and is left to it.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use kapto_core::{ClockMap, ClockSample, EpisodeId, ErrorCode, KaptoError, RecvTime};
use kapto_store::manifest::FINALISED_FILE;
use kapto_store::records::{CAPTURE_RECORD, EPISODE_RECORD, EpisodeStart, PREFLIGHT_RECORD};
use kapto_store::{
    BufferSummary, ClockSummary, DurabilitySummary, Episode, LossDetectionKind, Preflight,
    Recovered, StreamSummary,
};

use crate::finalise::{RECORDING_FILE, STAGING_DIR, Staged, complete};
use crate::reader::{read_recording, read_start};
use crate::salvage::{Salvaged, salvage};

/// Why a salvaged episode had to be recovered, as `episode.json` says.
pub const REASON: &str = "recording stopped without finishing it: a crash, a kill or a power cut";

/// An episode recovered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredEpisode {
    /// Its ID.
    pub episode: EpisodeId,
    /// Its folder, finalised.
    pub folder: PathBuf,
    /// What its recording lost, for one salvaged; `None` for one only
    /// finalised, which had lost nothing.
    pub salvaged: Option<Salvaged>,
}

/// Recovers every episode in `data_dir` that recording left unfinished, as
/// of `now_utc_ns`: each one recovered, or why it couldn't be, with its
/// folder named in the error. An episode another process is recording or
/// finalising holds its folder, and is left to it, so this is safe while
/// others record into `data_dir`.
#[must_use]
pub fn recover_interrupted(
    data_dir: &Path,
    now_utc_ns: i64,
) -> Vec<Result<RecoveredEpisode, KaptoError>> {
    let mut recovered = Vec::new();
    for episode in episodes_in(&data_dir.join(STAGING_DIR)) {
        recovered.extend(recover_staged(data_dir, &episode, now_utc_ns).transpose());
    }
    for episode in episodes_in(data_dir) {
        let folder = data_dir.join(episode.to_string());
        if !folder.join(FINALISED_FILE).exists() {
            match complete(data_dir, &episode) {
                Ok(Some(_)) => recovered.push(Ok(RecoveredEpisode {
                    episode,
                    folder,
                    salvaged: None,
                })),
                Ok(None) => {}
                Err(error) => recovered.push(Err(error)),
            }
        }
    }
    recovered
}

/// The episodes whose folders are in `folder`, oldest first.
fn episodes_in(folder: &Path) -> Vec<EpisodeId> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut episodes: Vec<EpisodeId> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| EpisodeId::from_str(entry.file_name().to_str()?).ok())
        .collect();
    episodes.sort_unstable_by_key(ToString::to_string);
    episodes
}

/// Salvages, describes and finalises `episode`, still in staging; `None`
/// if another process holds it.
fn recover_staged(
    data_dir: &Path,
    episode: &EpisodeId,
    now_utc_ns: i64,
) -> Result<Option<RecoveredEpisode>, KaptoError> {
    let Some(staged) = Staged::claim(data_dir, episode)? else {
        return Ok(None);
    };
    let recording = staged.recording_path();
    let folder = staged.dir();
    let in_folder = |error: KaptoError| error.context("folder", folder.display().to_string());
    if !recording.exists() {
        return Err(in_folder(
            KaptoError::new(ErrorCode::RecordingUnreadable)
                .detail("recording stopped before its recording was created"),
        ));
    }
    let salvaged = salvage(&recording).map_err(in_folder)?;
    let described = describe(episode, &recording, &salvaged, now_utc_ns).map_err(in_folder)?;
    described.write(&folder).map_err(in_folder)?;
    let finalised = staged.finalise().map_err(in_folder)?;
    Ok(Some(RecoveredEpisode {
        episode: *episode,
        folder: finalised.dir().to_path_buf(),
        salvaged: Some(salvaged),
    }))
}

/// `episode.json` for `episode`, from its salvaged recording: what it is
/// from the records written before its first message, and its streams'
/// counts from the messages kept, in the order they first appear. What
/// only finishing would have said (the clock's samples, the buffers' peak,
/// the losses) isn't known, and is left at nothing.
fn describe(
    episode: &EpisodeId,
    recording: &Path,
    salvaged: &Salvaged,
    now_utc_ns: i64,
) -> Result<Episode, KaptoError> {
    let start = read_start(recording)?;
    let begun = start
        .records
        .get(EPISODE_RECORD)
        .and_then(EpisodeStart::from_record);
    let mut streams: Vec<StreamSummary> = Vec::new();
    let mut first_message = None;
    read_recording(recording, |channel, message| {
        first_message.get_or_insert(message.log_time);
        match streams.iter_mut().find(|stream| stream.id == channel.topic) {
            Some(stream) => {
                stream.received += 1;
                stream.decoded += 1;
                stream.written += 1;
            }
            None => streams.push(StreamSummary {
                id: channel.topic.clone(),
                loss_detection: LossDetectionKind::Undetectable,
                received: 1,
                decoded: 1,
                written: 1,
                dropped_by_us: 0,
                lost_upstream: 0,
                disconnects: None,
                rejected: None,
                carried: None,
                pre_roll: None,
                post_roll: None,
            }),
        }
        ControlFlow::Continue(())
    })?;
    let utc = |time: u64| i64::try_from(time).unwrap_or(i64::MAX);
    let started = begun.as_ref().map_or_else(
        || first_message.map_or(now_utc_ns, utc),
        |begun| begun.started_utc_ns,
    );
    let last_message = salvaged.last_log_time.map(utc);
    let preflight = start
        .records
        .get(PREFLIGHT_RECORD)
        .and_then(Preflight::from_record);
    let sync_interval_ns = start
        .records
        .get(CAPTURE_RECORD)
        .and_then(|settings| settings.get("writer.sync_interval_ns"))
        .and_then(|nanos| nanos.parse().ok())
        .unwrap_or(0);
    // Only to make the episode; its summary is replaced below.
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: started,
        uncertainty: Duration::ZERO,
    });
    let mut described = Episode::new(
        episode,
        (started, last_message.unwrap_or(started).max(started)),
        RECORDING_FILE,
        &clock,
        (
            BufferSummary {
                batches: 0,
                peak_batches: 0,
                memory_bytes: 0,
            },
            DurabilitySummary {
                sync_interval_ns,
                syncs: 0,
                max_sync_ns: 0,
                p50_unsynced_ns: 0,
                p99_unsynced_ns: 0,
                max_unsynced_ns: 0,
            },
        ),
        streams,
    )
    .with_recovered(Recovered {
        at_utc_ns: now_utc_ns,
        bytes_cut: salvaged.cut_bytes,
        last_message_utc_ns: last_message,
        reason: REASON.to_owned(),
    })
    .with_preflight(preflight);
    described.clock = ClockSummary {
        samples: 0,
        max_uncertainty_ns: 0,
        steps: Vec::new(),
    };
    if let Some(begun) = begun {
        described = described
            .with_package(begun.package)
            .with_session(begun.profile, begun.task);
        if !begun.kapto_version.is_empty() {
            described.kapto_version = begun.kapto_version;
        }
    }
    Ok(described)
}
