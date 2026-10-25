// SPDX-License-Identifier: Apache-2.0

//! Finalisation: how an episode folder goes from being written to complete,
//! so that a partial recording never looks finished.
//!
//! ```text
//! Staged ──rename──▶ Renamed ──write_manifest──▶ Manifested ──mark_finalised──▶ Finalised
//! <out>/.recording/<id>/    <out>/<id>/              + manifest.json                  + _FINALISED
//! ```
//!
//! Each step consumes the previous state, so steps can't run out of order.
//! A crash between any two of them leaves a folder that is recognisably
//! incomplete: it is still under `.recording/`, or it has no manifest, or no
//! `_FINALISED` marker, which is written last.
//!
//! The process writing an episode holds its folder locked until it is
//! finalised or discarded, and the lock goes with the folder when it is
//! moved. The operating system releases it when the process ends, however
//! it ends, so an unfinished folder nobody holds was left by a recording
//! that stopped, and one somebody holds is still being written.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use kapto_store::manifest::{FINALISED_FILE, MANIFEST_FILE, Manifest, sync_directory};

/// The folder, under the output folder, where episodes are written.
pub const STAGING_DIR: &str = ".recording";

/// The recording's file name in an episode folder.
pub const RECORDING_FILE: &str = "recording.mcap";

/// An episode being written, in `<out>/.recording/<id>/`.
#[derive(Debug)]
pub struct Staged {
    out: PathBuf,
    episode: EpisodeId,
    held: File,
}

/// An episode moved to `<out>/<id>/`, not yet listed in a manifest.
#[derive(Debug)]
pub struct Renamed {
    dir: PathBuf,
    episode: EpisodeId,
    held: File,
}

/// An episode with its manifest, not yet marked finalised.
#[derive(Debug)]
pub struct Manifested {
    dir: PathBuf,
    held: File,
}

/// A complete episode.
#[derive(Debug)]
pub struct Finalised {
    dir: PathBuf,
}

/// How far an episode folder got, as found on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpisodeState {
    /// No folder for this episode.
    Absent,
    /// Still being written, or interrupted while it was.
    Staged,
    /// Moved out of staging; no manifest yet.
    Renamed,
    /// Has a manifest; no `_FINALISED` marker yet.
    Manifested,
    /// Has its `_FINALISED` marker. Whether its files still match the
    /// manifest is checked by `kapto_store::verify_episode`.
    Finalised,
}

impl Staged {
    /// Creates `<out>/.recording/<id>/`.
    ///
    /// # Errors
    ///
    /// `episode.already_exists` if a staged or finished folder with this ID
    /// exists; `capture.write_failed` if the folder can't be created.
    pub fn create(out: &Path, episode: &EpisodeId) -> Result<Self, KaptoError> {
        let dir = staged_dir(out, episode);
        if final_dir(out, episode).exists() {
            return Err(already_exists(&final_dir(out, episode)));
        }
        let created = fs::create_dir_all(out.join(STAGING_DIR)).and_then(|()| fs::create_dir(&dir));
        match created {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(already_exists(&dir));
            }
            Err(error) => return Err(write_failed(&dir, "create the staging folder", error)),
        }
        match hold(&dir) {
            Ok(Some(held)) => Ok(Self {
                out: out.to_owned(),
                episode: *episode,
                held,
            }),
            // Another process took the folder it had just seen created.
            Ok(None) => Err(already_exists(&dir)),
            Err(error) => Err(write_failed(&dir, "lock the staging folder", error)),
        }
    }

    /// Episode `episode` found in `<out>/.recording/<id>/`, e.g. one whose
    /// recording stopped without finishing, held to finalise now. `None` if
    /// another process holds it, still writing it, or it is gone.
    ///
    /// # Errors
    ///
    /// `capture.write_failed` if the folder can't be locked.
    pub fn claim(out: &Path, episode: &EpisodeId) -> Result<Option<Self>, KaptoError> {
        let dir = staged_dir(out, episode);
        let held = hold(&dir).map_err(|error| write_failed(&dir, "lock the folder", error))?;
        Ok(held.map(|held| Self {
            out: out.to_owned(),
            episode: *episode,
            held,
        }))
    }

    /// The folder being written.
    #[must_use]
    pub fn dir(&self) -> PathBuf {
        staged_dir(&self.out, &self.episode)
    }

    /// Where the writer puts the recording.
    #[must_use]
    pub fn recording_path(&self) -> PathBuf {
        self.dir().join(RECORDING_FILE)
    }

    /// Moves the folder to `<out>/<id>/`, once every file in it is closed.
    /// A rename within one disk is atomic: the folder is either staged or
    /// moved, never half of each.
    ///
    /// # Errors
    ///
    /// `episode.already_exists` if `<out>/<id>/` exists (the staged folder
    /// is left as it is); `capture.write_failed` if the rename fails.
    pub fn rename(self) -> Result<Renamed, KaptoError> {
        kapto_core::failpoint!("closing.rename");
        let to = final_dir(&self.out, &self.episode);
        if to.exists() {
            return Err(already_exists(&to));
        }
        fs::rename(self.dir(), &to)
            .and_then(|()| sync_directory(&self.out.join(STAGING_DIR)))
            .and_then(|()| sync_directory(&self.out))
            .map_err(|error| write_failed(&to, "move the episode out of staging", error))?;
        Ok(Renamed {
            dir: to,
            episode: self.episode,
            held: self.held,
        })
    }

    /// Runs every remaining step: rename, manifest, marker.
    ///
    /// # Errors
    ///
    /// The first step's failure; see each step.
    pub fn finalise(self) -> Result<Finalised, KaptoError> {
        let _span = kapto_core::span!("finalise");
        self.rename()?.write_manifest()?.mark_finalised()
    }

    /// Deletes the folder and everything in it, for an episode that was
    /// aborted. It was never moved out of staging, so nothing ever saw it as
    /// an episode.
    ///
    /// # Errors
    ///
    /// `capture.write_failed` if the folder can't be deleted; whatever is
    /// left stays in staging, where it is never taken for an episode.
    pub fn discard(self) -> Result<(), KaptoError> {
        let dir = self.dir();
        fs::remove_dir_all(&dir)
            .and_then(|()| sync_directory(&self.out.join(STAGING_DIR)))
            .map_err(|error| write_failed(&dir, "delete the aborted episode", error))
    }
}

impl Renamed {
    /// The episode's folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Lists every file with its size and SHA-256 in `manifest.json`.
    ///
    /// # Errors
    ///
    /// `manifest.write_failed`.
    pub fn write_manifest(self) -> Result<Manifested, KaptoError> {
        let _span = kapto_core::span!("write the manifest");
        kapto_core::failpoint!("closing.manifest");
        Manifest::of_directory(&self.dir, &self.episode)?.write(&self.dir)?;
        Ok(Manifested {
            dir: self.dir,
            held: self.held,
        })
    }
}

impl Manifested {
    /// The episode's folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes the `_FINALISED` marker and puts it on disk. Written last, so
    /// its presence means everything before it is complete.
    ///
    /// # Errors
    ///
    /// `capture.write_failed`.
    pub fn mark_finalised(self) -> Result<Finalised, KaptoError> {
        kapto_core::failpoint!("closing.finalised");
        let marker = self.dir.join(FINALISED_FILE);
        File::create(&marker)
            .and_then(|file| file.sync_all())
            .and_then(|()| sync_directory(&self.dir))
            .map_err(|error| write_failed(&marker, "write the finalised marker", error))?;
        // Complete: nothing is left to hold it for.
        drop(self.held);
        Ok(Finalised { dir: self.dir })
    }
}

impl Finalised {
    /// The episode's folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Finishes finalising episode `episode` in `out`, moved out of staging but
/// stopped before its manifest or its `_FINALISED` marker was written: its
/// files were complete when it was moved, so they are listed as they are.
/// `None` if another process holds it, still finalising it, or it is
/// complete by now.
///
/// # Errors
///
/// `manifest.write_failed`; `capture.write_failed`.
pub fn complete(out: &Path, episode: &EpisodeId) -> Result<Option<Finalised>, KaptoError> {
    let dir = final_dir(out, episode);
    let Some(held) = hold(&dir).map_err(|error| write_failed(&dir, "lock the folder", error))?
    else {
        return Ok(None);
    };
    // Finalised by its own process before the lock was taken.
    if dir.join(FINALISED_FILE).exists() {
        return Ok(None);
    }
    if dir.join(MANIFEST_FILE).exists() {
        return Manifested { dir, held }.mark_finalised().map(Some);
    }
    Renamed {
        dir,
        episode: *episode,
        held,
    }
    .write_manifest()?
    .mark_finalised()
    .map(Some)
}

/// Locks the folder `dir` for this process. `None` if another process
/// holds it, or it is gone.
fn hold(dir: &Path) -> io::Result<Option<File>> {
    let folder = match File::open(dir) {
        Ok(folder) => folder,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match folder.try_lock() {
        Ok(()) => Ok(Some(folder)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// How far the episode `episode` in `out` got.
#[must_use]
pub fn inspect(out: &Path, episode: &EpisodeId) -> EpisodeState {
    let dir = final_dir(out, episode);
    if dir.join(FINALISED_FILE).exists() {
        EpisodeState::Finalised
    } else if dir.join(MANIFEST_FILE).exists() {
        EpisodeState::Manifested
    } else if dir.exists() {
        EpisodeState::Renamed
    } else if staged_dir(out, episode).exists() {
        EpisodeState::Staged
    } else {
        EpisodeState::Absent
    }
}

fn final_dir(out: &Path, episode: &EpisodeId) -> PathBuf {
    out.join(episode.to_string())
}

fn staged_dir(out: &Path, episode: &EpisodeId) -> PathBuf {
    out.join(STAGING_DIR).join(episode.to_string())
}

fn already_exists(dir: &Path) -> KaptoError {
    KaptoError::new(ErrorCode::EpisodeAlreadyExists).context("folder", dir.display().to_string())
}

fn write_failed(path: &Path, step: &str, error: io::Error) -> KaptoError {
    let code = if error.kind() == io::ErrorKind::StorageFull {
        ErrorCode::CaptureDiskFull
    } else {
        ErrorCode::CaptureWriteFailed
    };
    KaptoError::new(code)
        .detail(format!("couldn't {step}: {error}"))
        .context("path", path.display().to_string())
        .source(error)
}
