// SPDX-License-Identifier: Apache-2.0

//! One `kapto serve` per data directory. Two would each rebuild the episode
//! index and requeue the other's jobs, and one built from other sources
//! makes `kapto.db` again, deleting it under the other. So serve holds a
//! lock on [`LOCK_FILE`] in the data directory for as long as it runs; the
//! operating system lets it go when the process ends, however it ends, so a
//! crash never leaves the directory locked.
//!
//! The file holds the process ID of the serve holding it, so the one
//! refused can say which process is in the way.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};

/// The lock's file, in the data directory.
pub const LOCK_FILE: &str = "kapto.lock";

/// The data directory, held for this process until dropped.
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
}

impl DataDirLock {
    /// Takes the lock on `data_dir`, creating the directory if needed.
    ///
    /// # Errors
    ///
    /// `server.data_dir_in_use` if another process holds it, naming that
    /// process if it wrote its ID; `store.open_failed` if the lock's file
    /// can't be made or locked.
    pub fn take(data_dir: &Path) -> Result<Self, KaptoError> {
        let path = data_dir.join(LOCK_FILE);
        let failed = |error: io::Error| {
            KaptoError::new(ErrorCode::StoreOpenFailed)
                .detail(format!("can't lock the data directory: {error}"))
                .context("file", path.display().to_string())
        };
        std::fs::create_dir_all(data_dir).map_err(failed)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(failed)?;
        match file.try_lock() {
            Ok(()) => {
                // Only a name for the message of a serve refused: a failure
                // to write it costs no more than that.
                let _ = file
                    .set_len(0)
                    .and_then(|()| file.rewind())
                    .and_then(|()| writeln!(file, "{}", std::process::id()));
                Ok(Self { _file: file })
            }
            Err(TryLockError::WouldBlock) => {
                let mut holder = String::new();
                let _ = file.read_to_string(&mut holder);
                let detail = holder.trim().parse::<u32>().map_or_else(
                    |_| "another process holds it".to_owned(),
                    |pid| format!("process {pid} holds it"),
                );
                Err(KaptoError::new(ErrorCode::ServerDataDirInUse)
                    .detail(detail)
                    .context("folder", data_dir.display().to_string()))
            }
            Err(TryLockError::Error(error)) => Err(failed(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use kapto_testkit::scratch;

    use super::*;

    #[test]
    fn a_data_directory_is_held_by_one_process_at_a_time_named_to_the_next() {
        let data_dir = scratch("lock").join("data");
        let held = DataDirLock::take(&data_dir).ok();
        assert!(held.is_some());
        // Another open of the file, as another process's would be.
        let refused = DataDirLock::take(&data_dir).err();
        assert_eq!(
            refused.as_ref().map(KaptoError::code),
            Some(ErrorCode::ServerDataDirInUse)
        );
        let report = refused.map(|error| error.report()).unwrap_or_default();
        assert!(
            report.contains(&format!("process {} holds it", std::process::id())),
            "{report}"
        );

        drop(held);
        assert!(DataDirLock::take(&data_dir).is_ok(), "free once let go");
    }
}
