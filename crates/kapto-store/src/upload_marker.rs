// SPDX-License-Identifier: Apache-2.0

//! The completeness marker of an uploaded episode: `_SUCCESS`, written last
//! in the episode's folder at its destination, listing every object of the
//! episode there with its size and SHA-256.
//!
//! Storage can't make several objects appear at once, so an episode's files
//! become visible one by one as they are uploaded. The marker is what makes
//! the episode appear whole: it is written only once every file is up and
//! verified, and only if no marker is there yet, so a finished episode is
//! never overwritten. Anyone reading the bucket treats a folder without it
//! as not there yet.

use kapto_core::{ErrorCode, KaptoError, hex};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::manifest::{FileEntry, MANIFEST_FILE, Manifest};

/// The marker's object name in an uploaded episode's folder.
pub const SUCCESS_FILE: &str = "_SUCCESS";

/// The value of the marker's `format` field.
pub const FORMAT: &str = "kapto.upload";

/// The marker version this code writes.
pub const VERSION: u32 = 1;

/// An uploaded episode: every object of it at its destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadMarker {
    /// Always `"kapto.upload"`.
    pub format: String,
    /// The format version; versions only ever add fields.
    pub version: u32,
    /// The episode's ID.
    pub episode: String,
    /// The ID of the robot package it was recorded with, if any.
    pub robot: Option<String>,
    /// The Kapto version that uploaded it.
    pub kapto_version: String,
    /// When the upload was complete, in UTC nanoseconds since the Unix
    /// epoch.
    pub uploaded_utc_ns: i64,
    /// Every object of the episode beside the marker, sorted by path: the
    /// files the manifest lists, and `manifest.json` itself.
    pub files: Vec<FileEntry>,
}

impl UploadMarker {
    /// The marker of the episode `manifest` describes, `manifest.json`
    /// being `manifest_bytes`.
    #[must_use]
    pub fn new(
        manifest: &Manifest,
        manifest_bytes: &[u8],
        robot: Option<String>,
        kapto_version: &str,
        uploaded_utc_ns: i64,
    ) -> Self {
        use sha2::{Digest, Sha256};

        let mut files = manifest.files.clone();
        files.push(FileEntry {
            path: MANIFEST_FILE.to_owned(),
            bytes: manifest_bytes.len() as u64,
            sha256: hex(&Sha256::digest(manifest_bytes)),
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            episode: manifest.episode.clone(),
            robot,
            kapto_version: kapto_version.to_owned(),
            uploaded_utc_ns,
            files,
        }
    }

    /// The marker as written: JSON, indented, ending with a newline.
    #[must_use]
    pub fn to_json(&self) -> String {
        // Plain strings and numbers always serialise.
        let mut json = serde_json::to_string_pretty(self).unwrap_or_default();
        json.push('\n');
        json
    }

    /// Reads a marker found at a destination.
    ///
    /// # Errors
    ///
    /// `upload.marker_unreadable` if it isn't a marker of a version this
    /// Kapto knows.
    pub fn parse(bytes: &[u8]) -> Result<Self, KaptoError> {
        let unreadable =
            |detail: String| KaptoError::new(ErrorCode::UploadMarkerUnreadable).detail(detail);
        let head: Head = serde_json::from_slice(bytes)
            .map_err(|error| unreadable(format!("not a marker: {error}")))?;
        if head.format != FORMAT || head.version > VERSION {
            return Err(unreadable(format!(
                "format `{}` version {}; this Kapto reads `{FORMAT}` up to version {VERSION}",
                head.format, head.version
            )));
        }
        serde_json::from_slice(bytes).map_err(|error| unreadable(format!("not a marker: {error}")))
    }

    /// Whether `other` lists the same objects, each the same size and
    /// SHA-256: the same episode uploaded, whenever and by whichever Kapto.
    #[must_use]
    pub fn same_files(&self, other: &Self) -> bool {
        self.episode == other.episode && self.files == other.files
    }
}

/// A marker's format and version, read first: a newer marker may hold
/// fields whose meaning this version would miss.
#[derive(Deserialize)]
struct Head {
    format: String,
    version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            format: crate::manifest::FORMAT.to_owned(),
            version: crate::manifest::VERSION,
            episode: "20260323T073012.345Z-5f729d2c".to_owned(),
            files: vec![
                FileEntry {
                    path: "recording.mcap".to_owned(),
                    bytes: 10,
                    sha256: "a".repeat(64),
                },
                FileEntry {
                    path: "episode.json".to_owned(),
                    bytes: 5,
                    sha256: "b".repeat(64),
                },
            ],
        }
    }

    #[test]
    fn the_marker_lists_every_file_and_the_manifest_sorted_by_path() {
        let marker = UploadMarker::new(&manifest(), b"", Some("arm7".into()), "0.1.0", 7);
        let paths: Vec<&str> = marker.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["episode.json", "manifest.json", "recording.mcap"]);
        assert_eq!(
            marker.files[1].sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "the manifest's own checksum"
        );
        assert_eq!(
            UploadMarker::parse(marker.to_json().as_bytes()).unwrap(),
            marker
        );
    }

    #[test]
    fn two_uploads_of_one_episode_have_the_same_files_and_another_episode_not() {
        let first = UploadMarker::new(&manifest(), b"{}", None, "0.1.0", 7);
        let again = UploadMarker::new(&manifest(), b"{}", None, "0.2.0", 9);
        assert!(first.same_files(&again));
        let mut changed = manifest();
        changed.files[0].bytes = 11;
        assert!(!first.same_files(&UploadMarker::new(&changed, b"{}", None, "0.1.0", 7)));
    }

    #[test]
    fn a_marker_of_another_format_or_a_newer_version_is_unreadable() {
        let marker = UploadMarker::new(&manifest(), b"{}", None, "0.1.0", 7).to_json();
        for text in [
            marker.replace("kapto.upload", "kapto.manifest"),
            marker.replace("\"version\": 1", "\"version\": 2"),
            "not json".to_owned(),
        ] {
            let error = UploadMarker::parse(text.as_bytes()).unwrap_err();
            assert_eq!(error.code(), ErrorCode::UploadMarkerUnreadable, "{text}");
        }
    }
}
