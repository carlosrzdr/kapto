// SPDX-License-Identifier: Apache-2.0

//! The settings files a recording carries, attached at its start: work on
//! an episode reads its settings from there, as they were when it was
//! recorded, never from the robot package as it is now.

use std::path::Path;

use kapto_capture::reader::read_start;
use kapto_config::ConfigFile;
use kapto_config::carried::{carried_file, first};
use kapto_core::KaptoError;

/// The settings file `name` the recording at `recording` carries, read
/// with `parse`; `None` if it carries none.
///
/// # Errors
///
/// `recording.unreadable` if the recording can't be read; the first
/// problem `parse` reports, with how many more there are.
pub fn carried<T>(
    recording: &Path,
    name: &str,
    parse: impl FnOnce(&ConfigFile) -> Result<T, Vec<KaptoError>>,
) -> Result<Option<T>, KaptoError> {
    let start = read_start(recording)?;
    let Some(data) = start.attachments.get(name) else {
        return Ok(None);
    };
    parse(&carried_file(recording, name, data))
        .map(Some)
        .map_err(first)
}
