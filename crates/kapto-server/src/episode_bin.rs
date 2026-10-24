// SPDX-License-Identifier: Apache-2.0

//! Episodes deleted by hand: each episode's folder moved into the data
//! directory's bin ([`BIN_DIR`]), in one rename, and recorded in
//! `kapto.db`; put back the same way until the retention executor empties
//! the bin. Nothing in a folder changes on its way in or out.

use std::path::Path;

use kapto_capture::disk::free_bytes;
use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use kapto_store::manifest::FINALISED_FILE;
use kapto_store::{BIN_DIR, Binned, Deletion, DeletionReason, JobId, JobState, Writer};

use crate::jobs::{Bin, DELETE};
use crate::replays::REPLAY_CACHE;
use crate::retention;

/// Where an episode deleted at serve's start was found: in the bin, but
/// not recorded, after a crash between its move and its record.
pub const FOUND_AT_START: &str = "found in the bin at start";

/// Moves the finalised episode `id` into the bin, as deleted `from` (`web
/// UI`, `API`, `command line`) at `now_utc_ns`, and records it. Its replay
/// is dropped from the cache.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID; `api.episode_in_bin`
/// if it is in the bin already; `api.episode_not_found` if no finalised
/// episode here has it (one being recorded, or being deleted, hasn't one
/// yet); `store.bin_failed` if its folder can't be moved;
/// `store.query_failed` if it can't be recorded, its folder then put back.
pub fn put_in_bin(
    data_dir: &Path,
    store: &Writer,
    id: &str,
    (from, now_utc_ns): (&str, i64),
) -> Result<Binned, KaptoError> {
    check_id(id)?;
    if store.reader()?.binned(id)?.is_some() {
        return Err(KaptoError::new(ErrorCode::ApiEpisodeInBin).context("episode", id));
    }
    let folder = data_dir.join(id);
    if !folder.join(FINALISED_FILE).exists() {
        return Err(
            KaptoError::new(ErrorCode::ApiEpisodeNotFound).detail(format!(
                "no finalised episode `{id}` in {}",
                data_dir.display()
            )),
        );
    }
    let bin = data_dir.join(BIN_DIR);
    let binned_folder = bin.join(id);
    std::fs::create_dir_all(&bin)
        .and_then(|()| std::fs::rename(&folder, &binned_folder))
        .map_err(|error| move_failed(&folder, "move it into the bin", &error))?;
    let binned = Binned {
        episode: id.to_owned(),
        binned_utc_ns: now_utc_ns,
        from: from.to_owned(),
    };
    if let Err(error) = store.record_binned(binned.clone()) {
        // Not recorded: as if never deleted.
        let _ = std::fs::rename(&binned_folder, &folder);
        return Err(error);
    }
    // Made again from the bin if it is asked for again.
    let _ = std::fs::remove_file(data_dir.join(REPLAY_CACHE).join(format!("{id}.view")));
    Ok(binned)
}

/// Puts the episode `id` back from the bin, as it was.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID;
/// `api.episode_not_in_bin` if it isn't in the bin; `store.bin_failed` if
/// its folder can't be moved back, or a folder of its name is in the way.
pub fn put_back(data_dir: &Path, store: &Writer, id: &str) -> Result<(), KaptoError> {
    check_id(id)?;
    let binned_folder = data_dir.join(BIN_DIR).join(id);
    if store.reader()?.binned(id)?.is_none() || !binned_folder.join(FINALISED_FILE).exists() {
        return Err(KaptoError::new(ErrorCode::ApiEpisodeNotInBin).context("episode", id));
    }
    let folder = data_dir.join(id);
    if folder.exists() {
        return Err(KaptoError::new(ErrorCode::StoreBinFailed)
            .detail(format!("{} is in the way", folder.display())));
    }
    std::fs::rename(&binned_folder, &folder)
        .map_err(|error| move_failed(&binned_folder, "put it back", &error))?;
    store.forget_binned(id)?;
    Ok(())
}

/// Makes `kapto.db` say what the bin holds, at serve's start: each episode
/// in it not recorded is, as deleted at `now_utc_ns`, and each recorded
/// whose folder is gone is forgotten.
///
/// # Errors
///
/// `store.query_failed` if the database can't be read or written.
pub fn reconcile(data_dir: &Path, store: &Writer, now_utc_ns: i64) -> Result<(), KaptoError> {
    let in_bin: Vec<String> = std::fs::read_dir(data_dir.join(BIN_DIR))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join(FINALISED_FILE).exists())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| check_id(name).is_ok())
        .collect();
    let recorded = store.reader()?.bin()?;
    for episode in &in_bin {
        if !recorded.iter().any(|binned| &binned.episode == episode) {
            store.record_binned(Binned {
                episode: episode.clone(),
                binned_utc_ns: now_utc_ns,
                from: FOUND_AT_START.to_owned(),
            })?;
        }
    }
    for binned in recorded {
        if !in_bin.contains(&binned.episode) {
            store.forget_binned(&binned.episode)?;
        }
    }
    Ok(())
}

/// Queues the retention executor's deletion of each episode in the bin for
/// longer than `bin` keeps one, at `now_utc_ns`, and, while the disk
/// holding the data directory is low, of the one longest in it, unless one
/// is being deleted already, so no more go than the room needs.
///
/// # Errors
///
/// `store.query_failed` if reading or queueing failed.
pub fn queue_emptying(store: &Writer, bin: &Bin, now_utc_ns: i64) -> Result<(), KaptoError> {
    let reader = store.reader()?;
    let in_bin = reader.bin()?;
    let keep_ns = i64::try_from(bin.keep.as_nanos()).unwrap_or(i64::MAX);
    let mut deleting = false;
    for binned in &in_bin {
        if reader
            .job(DELETE, &binned.episode)?
            .is_some_and(|job| matches!(job.state, JobState::Queued | JobState::Running))
        {
            deleting = true;
        } else if now_utc_ns.saturating_sub(binned.binned_utc_ns) >= keep_ns {
            store.enqueue_again(DELETE, &binned.episode)?;
            deleting = true;
        }
    }
    let low = free_bytes(&bin.data_dir).is_ok_and(|free| free < bin.free_below_bytes);
    if low
        && !deleting
        && let Some(oldest) = in_bin.first()
    {
        store.enqueue_again(DELETE, &oldest.episode)?;
    }
    Ok(())
}

/// Queues the deletion of every episode in the bin, as the bin is emptied
/// by hand; how many were queued.
///
/// # Errors
///
/// `store.query_failed` if reading or queueing failed.
pub fn empty(store: &Writer) -> Result<usize, KaptoError> {
    let in_bin = store.reader()?.bin()?;
    for binned in &in_bin {
        store.enqueue_again(DELETE, &binned.episode)?;
    }
    Ok(in_bin.len())
}

/// Whether `episode`, in the bin, may be deleted now by job `job`: nothing
/// else of it is being done, such as an upload stopping.
///
/// # Errors
///
/// `store.query_failed` if reading failed.
pub fn may_empty(store: &Writer, episode: &str, job: JobId) -> Result<bool, KaptoError> {
    Ok(!store
        .reader()?
        .jobs_of(episode)?
        .iter()
        .any(|other| other.id != job && other.state == JobState::Running))
}

/// Records `episode`, emptied from the bin, as deleted here by hand, with
/// `summary`, its `episode.json`, and the copies of it uploaded, if any;
/// then it is no longer in the bin, and the jobs it had waiting end.
///
/// # Errors
///
/// `store.query_failed` if reading or writing failed.
pub fn record_emptied(store: &Writer, episode: &str, summary: String) -> Result<(), KaptoError> {
    store.record_deletion(Deletion {
        episode: episode.to_owned(),
        deleted_utc_ns: 0,
        reason: DeletionReason::ByHand,
        deleted_by: "worker".to_owned(),
        copies: retention::copies(store, episode)?,
        summary,
    })?;
    store.forget_binned(episode)?;
    store.end_queued_jobs(episode)?;
    Ok(())
}

/// `id`, if it is an episode ID, so it can never name a folder elsewhere.
fn check_id(id: &str) -> Result<(), KaptoError> {
    id.parse::<EpisodeId>().map(|_| ()).map_err(|_| {
        KaptoError::new(ErrorCode::ApiInvalidRequest).detail(format!("`{id}` isn't an episode ID"))
    })
}

fn move_failed(folder: &Path, what: &str, error: &std::io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::StoreBinFailed)
        .detail(format!("can't {what}: {error}"))
        .context("folder", folder.display().to_string())
}
