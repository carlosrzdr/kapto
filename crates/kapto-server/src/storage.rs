// SPDX-License-Identifier: Apache-2.0

//! How much room the data directory has, and how much the episodes in it
//! take: what the UI turns into "time left at this rate".
//!
//! Sizes are the files' lengths, summed over each episode folder and over
//! the staging folder, where the episode being recorded grows. Free space is
//! what the file system leaves to Kapto's user (`statvfs`'s `f_bavail`), not
//! counting the share it keeps for root.

use std::path::Path;

use kapto_capture::disk::DiskLimits;
use kapto_capture::finalise::STAGING_DIR;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::BIN_DIR;
use kapto_store::db::finalised_folders;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The data directory's room and use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Storage {
    /// The data directory.
    pub data_dir: String,
    /// The size of the file system it is on, in bytes.
    pub total_bytes: u64,
    /// The bytes Kapto may still write there.
    pub free_bytes: u64,
    /// Finalised episodes in it.
    pub episodes: u64,
    /// What they take, in bytes.
    pub episodes_bytes: u64,
    /// What episodes being recorded, or never finalised, take so far, in
    /// bytes. It grows while an episode is recorded.
    pub staging_bytes: u64,
    /// What the episodes deleted by hand take in the bin until it is
    /// emptied, in bytes; counted apart from `episodes_bytes`.
    #[serde(default)]
    pub bin_bytes: u64,
    /// Below this many free bytes, Kapto warns the disk is running low
    /// (`capture.disk_warn_below`).
    pub warn_below_bytes: u64,
    /// Below this many free bytes, an episode is finalised and none is
    /// started (`capture.disk_stop_below`).
    pub stop_below_bytes: u64,
}

/// Measures `data_dir`, whose disk is watched against `limits`.
///
/// # Errors
///
/// `server.storage_unreadable` if the file system's size can't be read,
/// e.g. because the data directory doesn't exist.
pub fn measure(data_dir: &Path, limits: DiskLimits) -> Result<Storage, KaptoError> {
    let (total_bytes, free_bytes) = room(data_dir).map_err(|error| {
        KaptoError::new(ErrorCode::ServerStorageUnreadable)
            .detail(error.to_string())
            .context("data directory", data_dir.display().to_string())
    })?;
    let bin = data_dir.join(BIN_DIR);
    // Those in the bin are counted apart.
    let folders: Vec<_> = finalised_folders(data_dir)
        .into_iter()
        .filter(|folder| !folder.starts_with(&bin))
        .collect();
    Ok(Storage {
        data_dir: data_dir.display().to_string(),
        total_bytes,
        free_bytes,
        episodes: folders.len() as u64,
        episodes_bytes: folders.iter().map(|folder| size(folder)).sum(),
        staging_bytes: size(&data_dir.join(STAGING_DIR)),
        bin_bytes: size(&bin),
        warn_below_bytes: limits.warn_below,
        stop_below_bytes: limits.stop_below,
    })
}

/// The file system's size and the bytes left to unprivileged users.
fn room(path: &Path) -> std::io::Result<(u64, u64)> {
    let stats = rustix::fs::statvfs(path)?;
    Ok((
        stats.f_blocks.saturating_mul(stats.f_frsize),
        stats.f_bavail.saturating_mul(stats.f_frsize),
    ))
}

/// The lengths of every file under `path`, which may not exist. Links
/// aren't followed, so nothing is counted twice or outside the folder.
fn size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |data| data.len()),
            _ => 0,
        })
        .sum()
}
