// SPDX-License-Identifier: Apache-2.0

//! Which episodes may be deleted from this machine, as `upload.delete_local`
//! says: only whole episodes, and only those verified on at least
//! `upload.min_verified_destinations` destinations (one at the least),
//! whatever else is asked. Deleting them is the retention executor's, in
//! the worker, the only code that removes raw data.

use std::path::Path;

use kapto_capture::disk::free_bytes;
use kapto_config::DeleteLocal;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{Deletion, DeletionReason, JobId, JobState, RemoteCopy, UploadState, Writer};

use crate::jobs::DELETE;

/// How episodes are deleted here, from `[upload]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retention {
    /// When (`upload.delete_local`).
    pub delete_local: DeleteLocal,
    /// On how many destinations an episode must be verified first
    /// (`upload.min_verified_destinations`), one at the least.
    pub min_verified_destinations: u32,
    /// Below how much free space the disk is low (`capture.disk_warn_below`).
    pub free_below_bytes: u64,
}

impl Default for Retention {
    /// Nothing is ever deleted.
    fn default() -> Self {
        Self {
            delete_local: DeleteLocal::Never,
            min_verified_destinations: 1,
            free_below_bytes: 0,
        }
    }
}

/// Queues the deletion of what may go now, as `retention` says: with
/// `after_upload` each episode verified on enough destinations; with
/// `when_disk_low`, while the disk holding `data_dir` is low, only the
/// oldest, unless one is being deleted already, so no more go than the room
/// needs. A free space that can't be read is never low.
///
/// # Errors
///
/// `store.query_failed` if reading or queueing failed.
pub fn queue_deletions(
    store: &Writer,
    retention: &Retention,
    data_dir: &Path,
) -> Result<(), KaptoError> {
    if retention.delete_local == DeleteLocal::Never {
        return Ok(());
    }
    let disk_low = retention.delete_local == DeleteLocal::WhenDiskLow
        && free_bytes(data_dir).is_ok_and(|free| free < retention.free_below_bytes);
    let reader = store.reader()?;
    let candidates: Vec<Candidate> = reader
        .kept_here()?
        .into_iter()
        .map(|(episode, started_utc_ns, verified_on)| Candidate {
            episode,
            started_utc_ns,
            verified_on,
        })
        .collect();
    let chosen = deletable(
        retention.delete_local,
        retention.min_verified_destinations,
        disk_low,
        &candidates,
    );
    for episode in chosen {
        let deleting = reader
            .job(DELETE, episode)?
            .is_some_and(|job| matches!(job.state, JobState::Queued | JobState::Running));
        if retention.delete_local == DeleteLocal::WhenDiskLow {
            if !deleting {
                store.enqueue_again(DELETE, episode)?;
            }
            return Ok(());
        }
        store.enqueue_again(DELETE, episode)?;
    }
    Ok(())
}

/// Whether `episode`, whose deletion is job `job`, may be deleted now: still
/// verified on enough destinations, not deleted already, and no other job
/// of it queued or being done, e.g. an upload to another destination or its
/// replay being made.
///
/// # Errors
///
/// `store.query_failed` if reading failed.
pub fn may_delete(
    store: &Writer,
    retention: &Retention,
    episode: &str,
    job: JobId,
) -> Result<bool, KaptoError> {
    if retention.delete_local == DeleteLocal::Never {
        return Ok(false);
    }
    let reader = store.reader()?;
    let busy = reader.jobs_of(episode)?.iter().any(|other| {
        other.id != job && matches!(other.state, JobState::Queued | JobState::Running)
    });
    Ok(!busy
        && reader.deletion(episode)?.is_none()
        && copies(store, episode)?.len() >= enough(retention))
}

/// Records `episode` as deleted here by the worker, with `summary`, its
/// `episode.json`: the last check before its folder goes.
///
/// # Errors
///
/// `store.deletion_refused` if `retention` lets nothing go or the episode
/// isn't verified on enough destinations; `store.query_failed` if reading
/// or writing failed.
pub fn record(
    store: &Writer,
    retention: &Retention,
    episode: &str,
    summary: String,
) -> Result<(), KaptoError> {
    let refused = |detail: String| {
        KaptoError::new(ErrorCode::StoreDeletionRefused)
            .detail(detail)
            .context("episode", episode.to_owned())
    };
    let reason = match retention.delete_local {
        DeleteLocal::Never => {
            return Err(refused("`upload.delete_local` is `never`".to_owned()));
        }
        DeleteLocal::AfterUpload => DeletionReason::AfterUpload,
        DeleteLocal::WhenDiskLow => DeletionReason::DiskLow,
    };
    let copies = copies(store, episode)?;
    if copies.len() < enough(retention) {
        return Err(refused(format!(
            "verified on {} destinations, fewer than `upload.min_verified_destinations` asks",
            copies.len()
        )));
    }
    store.record_deletion(Deletion {
        episode: episode.to_owned(),
        deleted_utc_ns: 0,
        reason,
        deleted_by: "worker".to_owned(),
        copies,
        summary,
    })
}

/// Where `episode` is uploaded and verified.
pub(crate) fn copies(store: &Writer, episode: &str) -> Result<Vec<RemoteCopy>, KaptoError> {
    Ok(store
        .reader()?
        .uploads(episode)?
        .into_iter()
        .filter(|upload| upload.state == UploadState::Uploaded)
        .map(|upload| RemoteCopy {
            destination: upload.destination,
            key: upload.key,
        })
        .collect())
}

fn enough(retention: &Retention) -> usize {
    usize::try_from(retention.min_verified_destinations.max(1)).unwrap_or(usize::MAX)
}

/// An episode stored here, with how many destinations hold a verified copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Its ID.
    pub episode: String,
    /// When recording started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// On how many destinations it is uploaded and verified.
    pub verified_on: u32,
}

/// What may be deleted now, oldest first: with `after_upload`, every
/// candidate verified on enough destinations; with `when_disk_low`, the
/// same but only while `disk_low`, for the executor to delete from the
/// oldest until the disk has room again; with `never`, none.
#[must_use]
pub fn deletable(
    delete_local: DeleteLocal,
    min_verified_destinations: u32,
    disk_low: bool,
    candidates: &[Candidate],
) -> Vec<&str> {
    let wanted = match delete_local {
        DeleteLocal::Never => false,
        DeleteLocal::AfterUpload => true,
        DeleteLocal::WhenDiskLow => disk_low,
    };
    if !wanted {
        return Vec::new();
    }
    let enough = min_verified_destinations.max(1);
    let mut chosen: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.verified_on >= enough)
        .collect();
    chosen.sort_by(|a, b| (a.started_utc_ns, &a.episode).cmp(&(b.started_utc_ns, &b.episode)));
    chosen
        .into_iter()
        .map(|candidate| candidate.episode.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn candidate(episode: &str, started_utc_ns: i64, verified_on: u32) -> Candidate {
        Candidate {
            episode: episode.to_owned(),
            started_utc_ns,
            verified_on,
        }
    }

    #[test]
    fn each_setting_deletes_what_it_says_oldest_first_and_never_one_not_verified_enough() {
        let candidates = [
            candidate("newer", 30, 2),
            candidate("unverified", 10, 0),
            candidate("older", 20, 1),
        ];
        assert_eq!(
            deletable(DeleteLocal::AfterUpload, 1, false, &candidates),
            ["older", "newer"]
        );
        assert_eq!(
            deletable(DeleteLocal::AfterUpload, 2, false, &candidates),
            ["newer"]
        );
        assert_eq!(
            deletable(DeleteLocal::WhenDiskLow, 1, false, &candidates),
            [""; 0]
        );
        assert_eq!(
            deletable(DeleteLocal::WhenDiskLow, 1, true, &candidates),
            ["older", "newer"]
        );
        // Zero asked for is one: an episode is never deleted with no copy.
        assert_eq!(
            deletable(DeleteLocal::AfterUpload, 0, true, &candidates),
            ["older", "newer"]
        );
    }

    fn setting() -> impl Strategy<Value = DeleteLocal> {
        prop_oneof![
            Just(DeleteLocal::Never),
            Just(DeleteLocal::WhenDiskLow),
            Just(DeleteLocal::AfterUpload),
        ]
    }

    proptest! {
        #[test]
        fn no_setting_disk_level_or_upload_outcome_deletes_an_episode_not_verified_on_enough(
            delete_local in setting(),
            min in 0u32..4,
            disk_low in any::<bool>(),
            uploads in prop::collection::vec((0i64..1_000, 0u32..4), 0..20),
        ) {
            let candidates: Vec<Candidate> = uploads
                .iter()
                .enumerate()
                .map(|(n, (started, verified))| candidate(&format!("e{n}"), *started, *verified))
                .collect();
            let chosen = deletable(delete_local, min, disk_low, &candidates);
            for id in &chosen {
                let found = candidates.iter().find(|c| c.episode == *id).unwrap();
                prop_assert!(found.verified_on >= min.max(1), "{found:?}");
            }
            if delete_local == DeleteLocal::Never
                || (delete_local == DeleteLocal::WhenDiskLow && !disk_low)
            {
                prop_assert!(chosen.is_empty());
            }
            // Oldest first.
            let started: Vec<i64> = chosen
                .iter()
                .map(|id| candidates.iter().find(|c| c.episode == *id).unwrap().started_utc_ns)
                .collect();
            prop_assert!(started.windows(2).all(|pair| pair[0] <= pair[1]));
        }
    }
}
