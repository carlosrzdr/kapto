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
    let temporary = dir.join(format!("{name}.tmp"));
    let mut json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    json.push(b'\n');
    let mut file = File::create(&temporary)?;
    file.write_all(&json)?;
    file.sync_all()?;
    fs::rename(&temporary, dir.join(name))?;
    sync_directory(dir)
}

/// Puts a folder's entries (e.g. a rename into it) on disk.
///
/// # Errors
///
/// The operating system's error if the folder can't be opened or synced.
pub fn sync_directory(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}
