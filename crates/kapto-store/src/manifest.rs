// SPDX-License-Identifier: Apache-2.0

//! The manifest: every file of a finalised episode, with its size and
//! SHA-256, in `manifest.json`.
//!
//! An episode folder is complete only when it holds both `manifest.json` and
//! `_FINALISED`, and its files match the manifest. Anything reading episodes
//! (uploads, exporters, other tools) checks exactly that, and nothing else.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::sidecar::sync_directory;
use crate::sidecar::write_json;

/// The manifest's file name in an episode folder.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The marker written last, once the manifest is in place.
pub const FINALISED_FILE: &str = "_FINALISED";

/// The value of the manifest's `format` field.
pub const FORMAT: &str = "kapto.manifest";

/// The manifest version this code writes.
pub const VERSION: u32 = 1;

/// Every file of a finalised episode, with its size and SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Always `"kapto.manifest"`.
    pub format: String,
    /// The format version; versions only ever add fields.
    pub version: u32,
    /// The episode's ID, the same as its folder name.
    pub episode: String,
    /// Every file in the episode folder and its subfolders, except
    /// `manifest.json` and `_FINALISED`, sorted by path.
    pub files: Vec<FileEntry>,
}

/// One file of an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// The path from the episode folder, with `/` between folders.
    pub path: String,
    /// The file's size in bytes.
    pub bytes: u64,
    /// The SHA-256 of the file's contents, as 64 lowercase hex digits.
    pub sha256: String,
}

/// One way an episode folder differs from its manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Discrepancy {
    /// A listed file is missing.
    Missing(String),
    /// A file is present but not listed.
    Unlisted(String),
    /// A listed file has a different size or contents.
    Changed(String),
}

impl Manifest {
    /// Lists every file in `dir` with its size and SHA-256.
    ///
    /// # Errors
    ///
    /// `manifest.write_failed` if a file can't be read, or the folder holds
    /// something other than regular files and folders.
    pub fn of_directory(dir: &Path, episode: &EpisodeId) -> Result<Self, KaptoError> {
        let mut files = Vec::new();
        list_files(dir, "", &mut files).map_err(|error| write_failed(dir, error))?;
        files.sort();
        let files = files
            .into_iter()
            .map(|path| {
                let (bytes, sha256) = hash_file(&dir.join(&path), &mut || Ok(())).map_err(
                    |stopped| match stopped {
                        Stopped::Read(error) => write_failed(dir, error),
                        Stopped::Between(error) => error,
                    },
                )?;
                Ok(FileEntry {
                    path,
                    bytes,
                    sha256,
                })
            })
            .collect::<Result<_, KaptoError>>()?;
        Ok(Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            episode: episode.to_string(),
            files,
        })
    }

    /// Writes `manifest.json` in `dir` so that it appears complete or not at
    /// all.
    ///
    /// # Errors
    ///
    /// `manifest.write_failed`.
    pub fn write(&self, dir: &Path) -> Result<(), KaptoError> {
        write_json(dir, MANIFEST_FILE, self).map_err(|error| write_failed(dir, error))
    }

    /// Reads `manifest.json` from `dir`.
    ///
    /// # Errors
    ///
    /// `manifest.unreadable` if it is missing, isn't valid JSON, or isn't a
    /// manifest of a version this code knows.
    pub fn read(dir: &Path) -> Result<Self, KaptoError> {
        let path = dir.join(MANIFEST_FILE);
        let unreadable = |detail: String| {
            KaptoError::new(ErrorCode::ManifestUnreadable)
                .detail(detail)
                .context("file", path.display().to_string())
        };
        let text = fs::read(&path).map_err(|error| unreadable(error.to_string()))?;
        let manifest: Self =
            serde_json::from_slice(&text).map_err(|error| unreadable(error.to_string()))?;
        if manifest.format != FORMAT || manifest.version == 0 || manifest.version > VERSION {
            return Err(unreadable(format!(
                "format `{}` version {}; this Kapto reads `{FORMAT}` up to version {VERSION}",
                manifest.format, manifest.version
            )));
        }
        Ok(manifest)
    }

    /// How the files in `dir` differ from this manifest, if they do.
    ///
    /// # Errors
    ///
    /// `manifest.write_failed` if the folder can't be listed or a file read.
    pub fn compare(&self, dir: &Path) -> Result<Vec<Discrepancy>, KaptoError> {
        self.compare_with(dir, &mut || Ok(()))
    }

    /// The same, calling `between` after each chunk of a file read, so a
    /// long check can wait there or be stopped.
    ///
    /// # Errors
    ///
    /// `manifest.write_failed` if the folder can't be listed or a file read;
    /// whatever `between` returns.
    pub fn compare_with(
        &self,
        dir: &Path,
        between: &mut dyn FnMut() -> Result<(), KaptoError>,
    ) -> Result<Vec<Discrepancy>, KaptoError> {
        let mut present = Vec::new();
        list_files(dir, "", &mut present).map_err(|error| unreadable_file(dir, &error))?;
        let mut discrepancies = Vec::new();
        for entry in &self.files {
            if !present.contains(&entry.path) {
                discrepancies.push(Discrepancy::Missing(entry.path.clone()));
                continue;
            }
            let (bytes, sha256) =
                hash_file(&dir.join(&entry.path), between).map_err(|stopped| match stopped {
                    Stopped::Read(error) => unreadable_file(dir, &error),
                    Stopped::Between(error) => error,
                })?;
            if bytes != entry.bytes || sha256 != entry.sha256 {
                discrepancies.push(Discrepancy::Changed(entry.path.clone()));
            }
        }
        for path in present {
            if !self.files.iter().any(|entry| entry.path == path) {
                discrepancies.push(Discrepancy::Unlisted(path));
            }
        }
        Ok(discrepancies)
    }
}

/// Checks that `dir` holds a finalised episode whose files match its
/// manifest: the single completeness rule for anything reading episodes.
///
/// # Errors
///
/// `manifest.unreadable` if it isn't finalised or has no readable manifest;
/// `manifest.mismatch` naming every file that differs.
pub fn verify_episode(dir: &Path) -> Result<Manifest, KaptoError> {
    verify_episode_with(dir, &mut || Ok(()))
}

/// The same, calling `between` after each chunk of a file read, so a long
/// check can wait there or be stopped.
///
/// # Errors
///
/// As [`verify_episode`]; whatever `between` returns.
pub fn verify_episode_with(
    dir: &Path,
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<Manifest, KaptoError> {
    if !dir.join(FINALISED_FILE).is_file() {
        return Err(KaptoError::new(ErrorCode::ManifestUnreadable)
            .detail(format!(
                "no {FINALISED_FILE} marker: the episode was never finalised"
            ))
            .context("folder", dir.display().to_string()));
    }
    let manifest = Manifest::read(dir)?;
    let discrepancies = manifest.compare_with(dir, between)?;
    if discrepancies.is_empty() {
        return Ok(manifest);
    }
    let detail = discrepancies
        .iter()
        .map(|discrepancy| match discrepancy {
            Discrepancy::Missing(path) => format!("{path} is missing"),
            Discrepancy::Unlisted(path) => format!("{path} is not listed"),
            Discrepancy::Changed(path) => format!("{path} has changed"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(KaptoError::new(ErrorCode::ManifestMismatch)
        .detail(detail)
        .context("folder", dir.display().to_string()))
}

/// Adds the path of every regular file under `dir` to `files`, skipping the
/// manifest, its temporary file and the marker. Links and other special
/// files are refused: their contents could change without the episode
/// changing.
fn list_files(dir: &Path, prefix: &str, files: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|name| {
            io::Error::other(format!("{} is not a UTF-8 name", name.to_string_lossy()))
        })?;
        let path = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_dir() {
            list_files(&entry.path(), &format!("{path}/"), files)?;
        } else if !kind.is_file() {
            return Err(io::Error::other(format!("{path} is not a regular file")));
        } else if !(prefix.is_empty() && is_bookkeeping(&name)) {
            files.push(path);
        }
    }
    Ok(())
}

/// The files finalisation itself writes, which the manifest doesn't list.
fn is_bookkeeping(name: &str) -> bool {
    name == MANIFEST_FILE || name == FINALISED_FILE || name == format!("{MANIFEST_FILE}.tmp")
}

/// Why hashing a file stopped short.
enum Stopped {
    /// The file couldn't be read.
    Read(io::Error),
    /// What is called between chunks said to stop.
    Between(KaptoError),
}

/// The size and SHA-256 of the file at `path`, calling `between` after each
/// chunk read.
fn hash_file(
    path: &Path,
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<(u64, String), Stopped> {
    let _span = kapto_core::span!(
        "checksum",
        bytes = fs::metadata(path).map_or(0, |metadata| metadata.len())
    );
    let mut file = File::open(path).map_err(Stopped::Read)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer).map_err(Stopped::Read)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
        between().map_err(Stopped::Between)?;
    }
    Ok((bytes, kapto_core::hex(&hasher.finalize())))
}

fn write_failed(dir: &Path, error: io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::ManifestWriteFailed)
        .detail(error.to_string())
        .context("folder", dir.display().to_string())
        .source(error)
}

fn unreadable_file(dir: &Path, error: &io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::ManifestMismatch)
        .detail(format!("can't read the episode's files: {error}"))
        .context("folder", dir.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder unique to this test, removed when dropped.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kapto-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn episode() -> EpisodeId {
        "20260305T192345.123Z-1a2b3c4d".parse().unwrap()
    }

    /// An episode folder with two files, one in a subfolder.
    fn sample(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        fs::write(dir.0.join("recording.mcap"), b"abc").unwrap();
        fs::create_dir(dir.0.join("config")).unwrap();
        fs::write(dir.0.join("config/effective.toml"), b"").unwrap();
        dir
    }

    fn finalise(dir: &Path) -> Manifest {
        let manifest = Manifest::of_directory(dir, &episode()).unwrap();
        manifest.write(dir).unwrap();
        fs::write(dir.join(FINALISED_FILE), b"").unwrap();
        manifest
    }

    #[test]
    fn every_file_is_listed_sorted_with_its_size_and_sha256() {
        let dir = sample("listed");
        let manifest = finalise(&dir.0);
        // Reference values from `sha256sum`.
        assert_eq!(
            manifest.files,
            [
                FileEntry {
                    path: "config/effective.toml".to_owned(),
                    bytes: 0,
                    sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                        .to_owned(),
                },
                FileEntry {
                    path: "recording.mcap".to_owned(),
                    bytes: 3,
                    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                        .to_owned(),
                },
            ]
        );
        assert_eq!(Manifest::read(&dir.0).unwrap(), manifest);
        assert_eq!(verify_episode(&dir.0).unwrap(), manifest);
    }

    #[test]
    fn the_manifest_file_is_readable_json_ending_in_a_newline() {
        let dir = sample("json");
        finalise(&dir.0);
        let text = fs::read_to_string(dir.0.join(MANIFEST_FILE)).unwrap();
        assert!(text.starts_with("{\n  \"format\": \"kapto.manifest\",\n  \"version\": 1,"));
        assert!(text.ends_with("}\n"));
        assert!(!dir.0.join("manifest.json.tmp").exists());
    }

    #[test]
    fn a_changed_added_or_removed_file_is_a_mismatch_naming_each() {
        let dir = sample("mismatch");
        finalise(&dir.0);
        fs::write(dir.0.join("recording.mcap"), b"abd").unwrap();
        fs::remove_file(dir.0.join("config/effective.toml")).unwrap();
        fs::write(dir.0.join("extra.txt"), b"x").unwrap();
        let error = verify_episode(&dir.0).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ManifestMismatch);
        assert_eq!(
            error.message(),
            "The episode's files don't match its manifest: \
             config/effective.toml is missing, recording.mcap has changed, extra.txt is not listed"
        );
    }

    #[test]
    fn an_episode_without_its_marker_or_manifest_is_never_complete() {
        let dir = sample("incomplete");
        let error = verify_episode(&dir.0).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ManifestUnreadable);

        fs::write(dir.0.join(FINALISED_FILE), b"").unwrap();
        let error = verify_episode(&dir.0).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ManifestUnreadable);
    }

    #[test]
    fn a_manifest_of_another_format_or_a_newer_version_is_unreadable() {
        let dir = sample("version");
        for text in [
            "not json",
            r#"{"format":"other","version":1,"episode":"x","files":[]}"#,
            r#"{"format":"kapto.manifest","version":2,"episode":"x","files":[]}"#,
            r#"{"format":"kapto.manifest","version":1,"episode":"x","files":[],"extra":1}"#,
        ] {
            fs::write(dir.0.join(MANIFEST_FILE), text).unwrap();
            let error = Manifest::read(&dir.0).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ManifestUnreadable, "{text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_episode_folder_is_refused() {
        let dir = sample("link");
        std::os::unix::fs::symlink("/etc/hostname", dir.0.join("link")).unwrap();
        let error = Manifest::of_directory(&dir.0, &episode()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ManifestWriteFailed);
        assert!(error.message().contains("link is not a regular file"));
    }

    #[test]
    fn a_long_check_calls_between_after_each_chunk_and_stops_when_it_says_so() {
        let dir = sample("between");
        // Three chunks of 256 KiB, the last one short.
        fs::write(dir.0.join("recording.mcap"), vec![7_u8; 600 * 1024]).unwrap();
        finalise(&dir.0);
        let mut calls = 0;
        verify_episode_with(&dir.0, &mut || {
            calls += 1;
            Ok(())
        })
        .unwrap();
        // The empty config file has none.
        assert_eq!(calls, 3);

        let stopped = verify_episode_with(&dir.0, &mut || {
            Err(KaptoError::new(ErrorCode::WorkerJobInterrupted))
        })
        .unwrap_err();
        assert_eq!(stopped.code(), ErrorCode::WorkerJobInterrupted);
    }
}
