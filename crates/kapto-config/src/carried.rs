// SPDX-License-Identifier: Apache-2.0

//! The configuration a recording carries: its package's files, attached
//! to it when it was recorded, read back as they were then, whatever the
//! package says now.

use std::collections::BTreeMap;
use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};

use crate::package::PACKAGE_FILE;
use crate::streams::parse_streams_with;
use crate::{ConfigFile, PackageManifest, StreamDeclaration, parse_package_manifest};

/// A recording's attached files, by name, as it carries them.
pub type Attachments = BTreeMap<String, Vec<u8>>;

/// The `package.toml` the recording at `recording` carries in
/// `attachments`, if it was recorded with a package.
///
/// # Errors
///
/// Its first problem, with how many more, if it can't be parsed.
pub fn carried_manifest(
    attachments: &Attachments,
    recording: &Path,
) -> Result<Option<PackageManifest>, KaptoError> {
    attachments
        .get(PACKAGE_FILE)
        .map(|data| parse_package_manifest(&carried_file(recording, PACKAGE_FILE, data)))
        .transpose()
        .map_err(first)
}

/// The stream declarations the recording carries: its package's, by
/// `manifest`, or, for one recorded without a package, the only file it
/// carries. None if it carries none. The `.msg` files of its ROS 2
/// messages are the ones the recording carries beside it, not those of
/// the package as it is now.
///
/// # Errors
///
/// Its first problem, with how many more, if it can't be parsed.
pub fn carried_streams(
    attachments: &Attachments,
    recording: &Path,
    manifest: Option<&PackageManifest>,
) -> Result<Vec<StreamDeclaration>, KaptoError> {
    let file = match manifest {
        Some(manifest) => {
            let name = slash_path(&manifest.streams);
            attachments.get_key_value(name.as_str())
        }
        None if attachments.len() == 1 => attachments.iter().next(),
        None => None,
    };
    match file {
        Some((name, data)) => {
            let folder = name.rsplit_once('/').map_or("", |(folder, _)| folder);
            let mut msg_files = |package: &str, type_name: &str| {
                let path = format!("{folder}/msg/{package}/msg/{type_name}.msg");
                let path = path.trim_start_matches('/');
                let data = attachments.get(path)?;
                Some(String::from_utf8_lossy(data).into_owned())
            };
            parse_streams_with(&carried_file(recording, name, data), &mut msg_files).map_err(first)
        }
        None => Ok(Vec::new()),
    }
}

/// The carried file `name` of the recording at `recording`, holding
/// `data`: named after where it is, so its problems say so.
#[must_use]
pub fn carried_file(recording: &Path, name: &str, data: &[u8]) -> ConfigFile {
    ConfigFile {
        path: recording.join(name),
        contents: String::from_utf8_lossy(data).into_owned(),
    }
}

/// A path in a package as the recording names its files: separated by `/`.
#[must_use]
pub fn slash_path(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The first of `errors`, saying how many more there are.
#[must_use]
pub fn first(errors: Vec<KaptoError>) -> KaptoError {
    let more = errors.len().saturating_sub(1);
    let first = errors
        .into_iter()
        .next()
        .unwrap_or_else(|| KaptoError::new(ErrorCode::ConfigInvalidValue));
    if more == 0 {
        first
    } else {
        first.context("more problems", more.to_string())
    }
}
