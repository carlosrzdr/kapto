// SPDX-License-Identifier: Apache-2.0

//! `cap-hash`: hashes an episode's recording behind the writer, so closing
//! the episode hashes only what it hadn't reached.
//!
//! The writer publishes how far the file is final, one atomic store each
//! time it puts the file on disk ([`Written::publish`]); everything before
//! that offset is never written again. `cap-hash` reads up to it from a
//! handle of its own and keeps the SHA-256 running. It never makes the
//! writer wait: it looks again a little later, and may fall behind.
//!
//! Once the episode is stopped and waits to be closed, it is set aside
//! ([`HashBehind::set_aside`]): `cap-hash` ends at its next read, freeing
//! its stack and buffer, and keeps only what it read: the running digest,
//! how far it read, and the open file, to tell at closing that it is the
//! same file (device, inode and size). So episodes waiting behind the one
//! being closed hold no thread and no buffer, however many there are.
//!
//! At closing, the writer gives the file's final size ([`Written::finish`])
//! and [`HashBehind::finish`] has the rest read: by `cap-hash` if it still
//! runs, otherwise on the calling thread, from where it stopped. The
//! digest is given only if it covers exactly the file as it is: the same
//! file, every byte to its end, and nothing after. Otherwise the caller
//! hashes the whole file, as without `cap-hash`.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kapto_store::manifest::FileEntry;
use sha2::{Digest, Sha256};

/// How often `cap-hash` looks again once it has caught up with the writer.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How much `cap-hash` reads at a time, into one buffer it holds while it
/// runs; reading the rest at closing, once set aside, takes one as large.
pub const CHUNK: usize = 256 * 1024;

/// The writer is still writing.
const WRITING: u8 = 0;
/// The file is complete: the offset published is its size.
const FINISHED: u8 = 1;
/// The writer gave up on the file, or `cap-hash` is no longer wanted.
const ABANDONED: u8 = 2;

#[derive(Debug, Default)]
struct Shared {
    /// How far the file is final.
    settled: AtomicU64,
    /// [`WRITING`], [`FINISHED`] or [`ABANDONED`].
    state: AtomicU8,
    /// How far `cap-hash` has read.
    hashed: AtomicU64,
    /// How far it had read when the file was complete.
    hashed_at_finish: AtomicU64,
    /// The episode waits to be closed: `cap-hash` stops at its next read.
    aside: AtomicBool,
}

/// The writer's side: says how far the file is final. Dropped without
/// [`Written::finish`], it tells `cap-hash` the file won't be finished.
#[derive(Debug)]
pub struct Written {
    shared: Arc<Shared>,
}

impl Written {
    /// Says the file's first `bytes` are final: one atomic store, nothing
    /// else, so the writer can call it on its own path.
    pub fn publish(&self, bytes: u64) {
        self.shared.settled.store(bytes, Ordering::Release);
    }

    /// Says the file is complete at `bytes`.
    pub fn finish(self, bytes: u64) {
        // Read before the final size is published: `cap-hash` can't have
        // read past the size before it.
        let hashed = self.shared.hashed.load(Ordering::Acquire);
        self.shared
            .hashed_at_finish
            .store(hashed, Ordering::Relaxed);
        self.publish(bytes);
        self.shared.state.store(FINISHED, Ordering::Release);
    }
}

impl Drop for Written {
    fn drop(&mut self) {
        // A no-op after `finish`, which stored FINISHED first.
        let _ = self.shared.state.compare_exchange(
            WRITING,
            ABANDONED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// What `cap-hash` has read so far: the digest running, how far it is, and
/// the file it read, to check it is the one being listed.
#[derive(Debug)]
struct Progress {
    digest: Sha256,
    hashed: u64,
    file: File,
}

/// Where `cap-hash` stopped reading.
#[derive(Debug)]
enum Stopped {
    /// At the file's end, every byte read.
    Whole(Progress),
    /// Set aside before the file was finished.
    Aside(Progress),
}

/// The `cap-hash` thread hashing one recording.
#[derive(Debug)]
pub struct HashBehind {
    shared: Arc<Shared>,
    path: PathBuf,
    thread: Option<JoinHandle<Result<Stopped, String>>>,
}

/// The digest of the whole recording, hashed behind the writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hashed {
    /// The file's size.
    pub bytes: u64,
    /// Its SHA-256, as 64 lowercase hex digits.
    pub sha256: String,
    /// What `cap-hash` had left to read once the file was complete: the
    /// rest closing waited for.
    pub read_at_close: u64,
}

impl Hashed {
    /// The recording's manifest entry, at `path` in the episode folder.
    #[must_use]
    pub fn entry(&self, path: &str) -> FileEntry {
        FileEntry {
            path: path.to_owned(),
            bytes: self.bytes,
            sha256: self.sha256.clone(),
        }
    }
}

impl HashBehind {
    /// Starts `cap-hash` on the file at `path`, which the writer has just
    /// created, running `first` on the thread before anything else (e.g.
    /// to lower its priority). Returns the writer's side with it.
    ///
    /// # Errors
    ///
    /// The file couldn't be opened, or the thread started.
    pub fn start(
        path: &Path,
        first: impl FnOnce() + Send + 'static,
    ) -> io::Result<(Written, Self)> {
        let file = File::open(path)?;
        let shared = Arc::new(Shared::default());
        let thread = crate::threads::named(crate::threads::Thread::Hash).spawn({
            let shared = Arc::clone(&shared);
            move || {
                first();
                let progress = Progress {
                    digest: Sha256::new(),
                    hashed: 0,
                    file,
                };
                read_behind(progress, &shared, &mut vec![0; CHUNK], true)
            }
        })?;
        Ok((
            Written {
                shared: Arc::clone(&shared),
            },
            Self {
                shared,
                path: path.to_owned(),
                thread: Some(thread),
            },
        ))
    }

    /// How far `cap-hash` has read.
    #[must_use]
    pub fn hashed(&self) -> u64 {
        self.shared.hashed.load(Ordering::Acquire)
    }

    /// Sets the episode aside to wait for closing: `cap-hash` ends at its
    /// next read, freeing its stack and buffer, and keeps what it read for
    /// [`HashBehind::finish`]. Never waits for it.
    pub fn set_aside(&self) {
        self.shared.aside.store(true, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }

    /// Has the rest of the file read, once the writer has finished it, and
    /// returns its digest: `cap-hash` reads it if it still runs, otherwise
    /// it is read here, from where `cap-hash` stopped. The digest covers
    /// the file now at `cap-hash`'s path, every byte of it, or there is
    /// none.
    ///
    /// # Errors
    ///
    /// Why there is no digest: the writer didn't finish the file, reading
    /// it failed, or the file is not the one read or has another size. The
    /// whole file is to be hashed again.
    pub fn finish(mut self) -> Result<Hashed, String> {
        let thread = self.thread.take().ok_or("cap-hash wasn't running")?;
        thread.thread().unpark();
        let stopped = thread
            .join()
            .map_err(|_| "cap-hash stopped unexpectedly".to_owned())??;
        let read = match stopped {
            Stopped::Whole(read) => read,
            Stopped::Aside(progress) => {
                match read_behind(progress, &self.shared, &mut vec![0; CHUNK], false)? {
                    Stopped::Whole(read) => read,
                    Stopped::Aside(_) => return Err("cap-hash stopped unexpectedly".to_owned()),
                }
            }
        };
        let unchanged = same_file(&read.file, &self.path)
            .map_err(|error| format!("couldn't look at the recording again: {error}"))?;
        if !unchanged {
            return Err("the recording is not the file cap-hash read".to_owned());
        }
        Ok(Hashed {
            bytes: read.hashed,
            sha256: kapto_core::hex(&read.digest.finalize()),
            read_at_close: read
                .hashed
                .saturating_sub(self.shared.hashed_at_finish.load(Ordering::Relaxed)),
        })
    }
}

impl Drop for HashBehind {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.shared.state.store(ABANDONED, Ordering::Release);
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// `cap-hash`'s loop: reads up to the published offset, then looks again
/// later, until the file is finished and read to its end, or abandoned,
/// or, if `may_set_aside`, the episode is set aside.
fn read_behind(
    mut progress: Progress,
    shared: &Shared,
    buffer: &mut [u8],
    may_set_aside: bool,
) -> Result<Stopped, String> {
    loop {
        let state = shared.state.load(Ordering::Acquire);
        if state == ABANDONED {
            return Err("the recording wasn't finished".to_owned());
        }
        let settled = shared.settled.load(Ordering::Acquire);
        let hashed = progress.hashed;
        if hashed < settled {
            if may_set_aside && shared.aside.load(Ordering::Acquire) {
                return Ok(Stopped::Aside(progress));
            }
            let wanted = usize::try_from(settled - hashed)
                .map_or(buffer.len(), |left| left.min(buffer.len()));
            let read = progress
                .file
                .read(&mut buffer[..wanted])
                .map_err(|error| format!("couldn't read the recording: {error}"))?;
            if read == 0 {
                return Err(format!(
                    "the recording ended at {hashed} bytes, before the {settled} written"
                ));
            }
            progress.digest.update(&buffer[..read]);
            progress.hashed += read as u64;
            shared.hashed.store(progress.hashed, Ordering::Release);
        } else if state == FINISHED {
            // Nothing past the end the writer gave.
            let past = progress
                .file
                .read(&mut buffer[..1])
                .map_err(|error| format!("couldn't read the recording: {error}"))?;
            if past != 0 {
                return Err(format!(
                    "the recording is longer than the {settled} written"
                ));
            }
            return Ok(Stopped::Whole(progress));
        } else if may_set_aside && shared.aside.load(Ordering::Acquire) {
            return Ok(Stopped::Aside(progress));
        } else {
            thread::park_timeout(LOOK_AGAIN);
        }
    }
}

/// Whether `file` is the file at `path`, with the size it has there.
#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let (read, there) = (file.metadata()?, std::fs::metadata(path)?);
    Ok(read.dev() == there.dev() && read.ino() == there.ino() && read.len() == there.len())
}

/// Without a way to tell two handles name the same file, never assumed.
#[cfg(not(unix))]
fn same_file(_file: &File, _path: &Path) -> io::Result<bool> {
    Ok(false)
}
