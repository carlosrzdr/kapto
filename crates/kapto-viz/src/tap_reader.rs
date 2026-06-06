// SPDX-License-Identifier: Apache-2.0

//! Reading the recording's live tap: the socket the recording sends each
//! stream's newest values to.

use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kapto_capture::tap::MAX_PATH_BYTES;
use kapto_capture::tap::wire::{self, TapSample, WireError};

/// What the reader has seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TapReaderStats {
    /// Samples read.
    pub samples: u64,
    /// Datagrams of a tap version this reader doesn't know, skipped.
    pub other_version: u64,
    /// Datagrams that aren't tap samples at all, skipped.
    pub unreadable: u64,
}

/// The listening end of the tap. The socket file is removed when the
/// reader is dropped.
#[derive(Debug)]
pub struct TapReader {
    socket: UnixDatagram,
    path: PathBuf,
    buffer: Vec<u8>,
    stats: TapReaderStats,
}

impl TapReader {
    /// Listens on `path`. A socket file left there by an earlier run is
    /// replaced; any other file is not.
    ///
    /// # Errors
    ///
    /// `path` is too long for a Unix socket, is taken by a file that isn't
    /// a socket, or can't be bound.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let length = path.as_os_str().len();
        if length > MAX_PATH_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the path is {length} bytes long, but a Unix socket path can have at most {MAX_PATH_BYTES}; set viewer.tap_socket to a shorter path"
                ),
            ));
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a file that isn't a socket is in the way",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(folder) = path
            .parent()
            .filter(|folder| !folder.as_os_str().is_empty())
        {
            std::fs::create_dir_all(folder)?;
        }
        let socket = UnixDatagram::bind(path)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            path: path.to_path_buf(),
            buffer: vec![0; wire::MAX_DATAGRAM],
            stats: TapReaderStats::default(),
        })
    }

    /// Waits up to `timeout` for a sample to arrive, then hands it and every
    /// other waiting to `each`, as [`TapReader::drain`] does. Returns at
    /// once if one is already waiting.
    ///
    /// # Errors
    ///
    /// As [`TapReader::drain`].
    pub fn drain_waiting(
        &mut self,
        timeout: Duration,
        mut each: impl FnMut(&TapSample),
    ) -> io::Result<usize> {
        // Blocking with a timeout only for the first datagram, so the
        // bridge wakes as one arrives rather than on a timer of its own.
        self.socket.set_nonblocking(false)?;
        self.socket
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let first = self.socket.recv(&mut self.buffer);
        self.socket.set_nonblocking(true)?;
        let length = match first {
            Ok(length) => length,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(0);
            }
            Err(error) => return Err(error),
        };
        let first = self.take(length, &mut each);
        Ok(first + self.drain(each)?)
    }

    /// Hands every sample waiting on the socket to `each`, oldest first,
    /// and returns how many there were. Never waits for more.
    ///
    /// # Errors
    ///
    /// Reading from the socket failed for a reason other than it being
    /// empty.
    pub fn drain(&mut self, mut each: impl FnMut(&TapSample)) -> io::Result<usize> {
        let mut count = 0;
        loop {
            let length = match self.socket.recv(&mut self.buffer) {
                Ok(length) => length,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(count),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            count += self.take(length, &mut each);
        }
    }

    /// Decodes the datagram of `length` bytes in the buffer and hands it to
    /// `each`, or counts why not. Returns how many samples it handed: 1 or 0.
    fn take(&mut self, length: usize, each: &mut impl FnMut(&TapSample)) -> usize {
        match wire::decode(&self.buffer[..length]) {
            Ok(sample) => {
                self.stats.samples += 1;
                each(&sample);
                1
            }
            Err(WireError::Version(_)) => {
                self.stats.other_version += 1;
                0
            }
            Err(WireError::NotTap | WireError::Length) => {
                self.stats.unreadable += 1;
                0
            }
        }
    }

    /// What the reader has seen so far.
    #[must_use]
    pub fn stats(&self) -> TapReaderStats {
        self.stats
    }

    /// The socket's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TapReader {
    fn drop(&mut self) {
        // Nothing to do about a failure: the next run replaces the file.
        let _ = std::fs::remove_file(&self.path);
    }
}
