// SPDX-License-Identifier: Apache-2.0

//! Sending tap samples to the viewer's Unix datagram socket, never waiting.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::TapStats;
use super::wire::{self, Summary};

/// The longest path a Unix socket can be bound to.
pub const MAX_PATH_BYTES: usize = 107;

/// Sends samples to the socket a viewer listens on. A sample the viewer
/// can't take right now, or that no viewer is there to take, is dropped
/// and counted: recording never waits for the viewer.
#[derive(Debug)]
pub struct TapSocket {
    socket: UnixDatagram,
    path: PathBuf,
    datagram: Vec<u8>,
    stats: Arc<TapStats>,
}

impl TapSocket {
    /// A sender to the viewer socket at `path`, which needn't exist yet.
    ///
    /// # Errors
    ///
    /// The operating system's error if no socket can be created at all, or
    /// an error saying so if `path` is longer than a Unix socket path may be.
    pub fn new(path: &Path, stats: Arc<TapStats>) -> io::Result<Self> {
        // Socket addresses hold at most 108 bytes, ending with a zero byte.
        let length = path.as_os_str().len();
        if length > MAX_PATH_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the viewer socket path {} is {length} bytes long, but a Unix socket path can have at most {MAX_PATH_BYTES}; set viewer.tap_socket to a shorter path",
                    path.display()
                ),
            ));
        }
        let socket = UnixDatagram::unbound()?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            socket,
            path: path.to_path_buf(),
            datagram: Vec::with_capacity(wire::MAX_DATAGRAM),
            stats,
        })
    }

    /// Sends one stream's summary. Never waits.
    pub fn send(&mut self, summary: &Summary<'_>) {
        wire::encode(summary, &mut self.datagram);
        let counter = match self.socket.send_to(&self.datagram, &self.path) {
            Ok(_) => &self.stats.sent,
            Err(error) => match error.kind() {
                // The viewer's receive buffer is full: it is reading too slowly.
                io::ErrorKind::WouldBlock => &self.stats.viewer_slow,
                // Nothing listens there (any more).
                _ => &self.stats.no_viewer,
            },
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}
