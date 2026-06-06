// SPDX-License-Identifier: Apache-2.0

//! The `replay` job: an episode's replay made in the data directory's
//! replay cache, the replay view the web UI's player reads.

use std::path::Path;

use kapto_capture::disk::free_bytes;
use kapto_config::DeploymentConfig;
use kapto_core::{ErrorCode, KaptoError};
use kapto_server::jobs::REPLAY;
use kapto_server::replays::view_file;
use kapto_store::Episode;
use kapto_viz::{DrawnRobots, ReplaySettings, write_replay};

use super::{Done, Job};

/// Makes an episode's replay in the data directory's replay cache: its
/// replay view to watch it again, from what its recording carries, in one
/// pass over it. None is made while the disk is low: a replay never takes
/// the room a recording needs. The robot drawn last is kept for the next
/// replay, for as long as the worker runs.
pub(super) struct Replay {
    settings: ReplaySettings,
    /// Below this many free bytes, no replay is made
    /// (`capture.disk_warn_below`).
    free_below: u64,
}

impl Replay {
    pub(super) fn new(deployment: &DeploymentConfig) -> Self {
        Self {
            settings: ReplaySettings {
                display_rate: deployment.viewer_display_rate.value,
                packages: deployment.packages_dir.value.clone(),
                robots: DrawnRobots::default(),
            },
            free_below: deployment.disk_warn_below_mib.value.saturating_mul(1 << 20),
        }
    }
}

impl Job for Replay {
    fn kind(&self) -> &'static str {
        REPLAY
    }

    fn run(
        &self,
        folder: &Path,
        between: &mut dyn FnMut() -> Result<(), KaptoError>,
    ) -> Result<Done, KaptoError> {
        let data_dir = folder.parent().unwrap_or(folder);
        let id = folder.file_name().and_then(|name| name.to_str());
        let view = id.and_then(|id| view_file(data_dir, id)).ok_or_else(|| {
            KaptoError::new(ErrorCode::EpisodeUnreadable)
                .detail("its folder isn't named after an episode ID")
                .context("folder", folder.display().to_string())
        })?;
        let recording = folder.join(Episode::read(folder)?.recording);
        room(free_bytes(data_dir).ok(), self.free_below)?;
        let cache = view.parent().unwrap_or(data_dir);
        std::fs::create_dir_all(cache).map_err(|error| {
            KaptoError::new(ErrorCode::ViewerReplayWriteFailed)
                .detail(error.to_string())
                .context("folder", cache.display().to_string())
        })?;
        let replayed = write_replay(&recording, &view, &self.settings, between)?;
        Ok(Done {
            warnings: replayed.problems,
            ..Done::default()
        })
    }
}

/// Whether `free` bytes leave room for a replay: not below `free_below`,
/// where recording warns the disk is low. Free space that can't be read
/// is never a reason not to make one.
fn room(free: Option<u64>, free_below: u64) -> Result<(), KaptoError> {
    match free {
        Some(free) if free < free_below => Err(KaptoError::new(ErrorCode::ViewerReplayDiskLow)
            .detail(format!(
                "{} MiB free, below capture.disk_warn_below ({} MiB)",
                free >> 20,
                free_below >> 20
            ))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_replay_is_made_below_the_free_space_recording_warns_at() {
        let error = room(Some(99 << 20), 100 << 20).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ViewerReplayDiskLow);
        assert!(
            error.message().contains("99 MiB free"),
            "{}",
            error.message()
        );
        assert!(room(Some(100 << 20), 100 << 20).is_ok());
        assert!(room(None, 100 << 20).is_ok());
    }
}
