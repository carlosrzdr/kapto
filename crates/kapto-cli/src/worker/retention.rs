// SPDX-License-Identifier: Apache-2.0

//! The retention executor: the only code that removes raw data, here whole
//! episodes serve hands out to delete once they are verified elsewhere, as
//! `upload.delete_local` says.
//!
//! An episode's folder is first renamed aside, out of the episodes, then
//! reported to serve with its `episode.json`, which records the deletion or
//! refuses it, and only then removed. A crash between those steps leaves
//! the folder aside, never half an episode looking whole; the worker
//! finishes what it finds aside whenever it reaches serve, and an episode
//! serve refuses is put back.

use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};
use kapto_server::jobs::{Found, JobClient, JobOrder};
use kapto_store::episode::EPISODE_FILE;
use kapto_store::{JobFailure, episode_folder};

/// What a folder set aside to be removed is named after, before the
/// episode's ID.
pub(super) const ASIDE: &str = ".deleting-";

/// Deletes the episode of `order`, a `delete` job, and reports how it
/// ended; `false` if serve can no longer be reached.
pub(super) fn delete_job(data_dir: &Path, order: &JobOrder, client: &mut JobClient) -> bool {
    let result = delete(data_dir, &order.episode, client);
    match &result {
        Ok(()) => eprintln!("[worker] delete {}: done", order.episode),
        Err(error) => eprintln!("[worker] delete {}: {error}", order.episode),
    }
    let failure = result.as_ref().err().map(JobFailure::from);
    if let Err(error) = client.finished(order.id, failure, Found::default()) {
        eprintln!("[worker] {error}");
        return false;
    }
    true
}

/// Finishes each deletion a crash, or serve not answering, left with its
/// folder set aside in `data_dir`: reported again, then removed, or put
/// back if serve refuses.
pub(super) fn finish_left_aside(data_dir: &Path, client: &mut JobClient) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    let left: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.strip_prefix(ASIDE).map(str::to_owned)
        })
        .collect();
    for episode in left {
        match delete(data_dir, &episode, client) {
            Ok(()) => eprintln!("[worker] delete {episode}: finished after a restart"),
            Err(error) => eprintln!("[worker] delete {episode}: {error}"),
        }
    }
}

/// Deletes `episode` from `data_dir`, carrying on a deletion set aside
/// before; nothing to do if neither its folder nor one set aside is there.
fn delete(data_dir: &Path, episode: &str, client: &mut JobClient) -> Result<(), KaptoError> {
    let folder = episode_folder(data_dir, episode).ok_or_else(|| {
        KaptoError::new(ErrorCode::EpisodeUnreadable)
            .detail(format!("`{episode}` isn't an episode ID"))
    })?;
    let aside = data_dir.join(format!("{ASIDE}{episode}"));
    if folder.exists() {
        std::fs::rename(&folder, &aside).map_err(|error| failed(&folder, &error))?;
    } else if !aside.exists() {
        return Ok(());
    }
    let summary = std::fs::read_to_string(aside.join(EPISODE_FILE));
    let reported = summary
        .map_err(|error| failed(&aside.join(EPISODE_FILE), &error))
        .and_then(|summary| client.deleting(episode, summary));
    if let Err(error) = reported {
        // Refused, so not gone: back among the episodes. Unanswered, serve
        // may have recorded it: left aside, to be reported again, which
        // records it once, the next time the worker reaches serve.
        if error.code() == ErrorCode::StoreDeletionRefused {
            put_back(&aside, &folder);
        }
        return Err(error);
    }
    std::fs::remove_dir_all(&aside).map_err(|error| failed(&aside, &error))
}

fn put_back(aside: &Path, folder: &Path) {
    if let Err(error) = std::fs::rename(aside, folder) {
        eprintln!(
            "[worker] can't put {} back as {}: {error}",
            aside.display(),
            folder.display()
        );
    }
}

fn failed(path: &Path, error: &std::io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::StoreDeleteFailed)
        .detail(error.to_string())
        .context("path", path.display().to_string())
}
