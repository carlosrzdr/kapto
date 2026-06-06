// SPDX-License-Identifier: Apache-2.0

//! The episode index in `kapto.db`, kept up to date while serve runs. It is
//! rebuilt from the episode folders when serve starts, since episodes may
//! have been recorded, copied in or deleted meanwhile, and each episode is
//! added as soon as capture announces it finalised or recovered, with a job
//! queued to check its files. If announcements were missed, the index is
//! rebuilt again.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use kapto_capture::control::Announcement;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::db::{DATABASE_FILE, finalised_folders};
use kapto_store::{Episode, Writer, episode_folder};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::TryRecvError;

use crate::jobs::{EVENTS, QA, VERIFY};

/// How long to wait before looking for announcements again.
const WAIT: Duration = Duration::from_millis(50);

/// Opens `kapto.db` in `data_dir`, creating both if needed, and rebuilds
/// its episode index. Episode folders that can't be read are reported and
/// left out.
///
/// # Errors
///
/// What opening the database or rebuilding the index reports.
pub fn open(data_dir: &Path) -> Result<Writer, KaptoError> {
    std::fs::create_dir_all(data_dir).map_err(|error| {
        KaptoError::new(ErrorCode::StoreOpenFailed)
            .detail(format!("can't create the data directory: {error}"))
            .context("folder", data_dir.display().to_string())
    })?;
    let store = Writer::open(&data_dir.join(DATABASE_FILE))?;
    rebuild(&store, data_dir)?;
    Ok(store)
}

/// Adds each episode capture announces finalised or recovered to the
/// index, while `running` is set. `announced` is taken before capture can
/// announce anything (`CaptureAnnouncements::subscribe`), so no episode is
/// missed.
/// Blocks: run it on a thread.
pub fn keep_up_to_date(
    store: &Writer,
    data_dir: &Path,
    mut announced: Receiver<Announcement>,
    running: &AtomicBool,
) {
    while running.load(Ordering::Relaxed) {
        let done = match announced.try_recv() {
            Ok(
                Announcement::EpisodeEnded {
                    episode,
                    finalised: true,
                }
                | Announcement::EpisodeRecovered { episode },
            ) => add(store, data_dir, &episode),
            Ok(_) => Ok(()),
            Err(TryRecvError::Lagged(_)) => rebuild(store, data_dir),
            Err(TryRecvError::Empty) => {
                thread::sleep(WAIT);
                Ok(())
            }
            Err(TryRecvError::Closed) => return,
        };
        if let Err(error) = done {
            eprintln!("[serve] the episode index wasn't updated: {error}");
        }
    }
}

/// Adds the finalised episode `episode` in `data_dir` to the index, and
/// queues checking its files against its manifest, then reading its events
/// and checking its quality.
///
/// # Errors
///
/// `episode.unreadable` if `episode` isn't an episode ID or its
/// `episode.json` can't be read; what writing the index reports.
pub fn add(store: &Writer, data_dir: &Path, episode: &str) -> Result<(), KaptoError> {
    // Only an episode ID, so the announcement can never name a folder
    // elsewhere.
    let folder = episode_folder(data_dir, episode).ok_or_else(|| {
        KaptoError::new(ErrorCode::EpisodeUnreadable).detail(format!(
            "capture announced `{episode}`, which isn't an episode ID"
        ))
    })?;
    store.index(Episode::read(&folder)?.into())?;
    // In this order, which is the order they are done in: the files are
    // checked before what they hold is.
    store.enqueue(VERIFY, episode)?;
    store.enqueue(EVENTS, episode)?;
    store.enqueue(QA, episode).map(drop)
}

/// Makes the index hold the episodes in `data_dir`, and queues reading the
/// events of each whose events were never read, as for episodes copied in
/// or a database made again: a job already queued or done isn't queued
/// twice.
fn rebuild(store: &Writer, data_dir: &Path) -> Result<(), KaptoError> {
    let unreadable = store.rebuild(data_dir)?;
    for (folder, error) in &unreadable {
        eprintln!("[serve] {} isn't listed: {error}", folder.display());
    }
    for folder in finalised_folders(data_dir) {
        let listed = !unreadable.iter().any(|(left_out, _)| *left_out == folder);
        if let Some(episode) = folder.file_name().and_then(|name| name.to_str())
            && listed
        {
            store.enqueue(EVENTS, episode)?;
        }
    }
    Ok(())
}
