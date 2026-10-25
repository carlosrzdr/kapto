// SPDX-License-Identifier: Apache-2.0

//! Writing an episode's JSON files so each appears complete or not at all.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

/// Writes `value` as pretty JSON to `dir/name`: to a temporary file first,
/// put on disk, then renamed over the final name, and the folder synced. A
/// crash leaves either no file or a complete one, never a truncated one.
pub(crate) fn write_json(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let mut json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    json.push(b'\n');
    write_whole(&dir.join(name), &json)
}

/// Writes `bytes` to `file` the same way: beside it first, put on disk,
/// renamed over it and its folder synced, so a crash leaves the old file or
/// the new one whole. What is left beside it after a failure is removed.
///
/// # Errors
///
/// The operating system's error writing, syncing or renaming.
pub fn write_whole(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = file.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = Path::new(&temporary);
    let written = File::create(temporary)
        .and_then(|mut out| out.write_all(bytes).and_then(|()| out.sync_all()))
        .and_then(|()| fs::rename(temporary, file));
    if written.is_err() {
        let _ = fs::remove_file(temporary);
    }
    written?;
    file.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map_or(Ok(()), sync_directory)
}

/// Puts a folder's entries (e.g. a rename into it) on disk.
///
/// # Errors
///
/// The operating system's error if the folder can't be opened or synced.
pub fn sync_directory(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_written_whole_replaces_the_old_and_a_failed_write_leaves_nothing_beside_it() {
        let dir = std::env::temp_dir().join(format!("kapto-write-whole-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("layout.toml");
        write_whole(&file, b"old").unwrap();
        write_whole(&file, b"new").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"new");

        // A folder where the file should be: renaming over it fails.
        let taken = dir.join("taken");
        fs::create_dir_all(taken.join("inside")).unwrap();
        assert!(write_whole(&taken, b"text").is_err());
        let mut left: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["layout.toml", "taken"], "nothing left beside it");
        fs::remove_dir_all(&dir).unwrap();
    }
}
