// SPDX-License-Identifier: Apache-2.0

//! How much room the disk holding the recordings has left, so an episode is
//! finalised cleanly before the disk fills rather than cut off by it.
//!
//! The room is read at most once a second ([`DiskWatch::EVERY`]), by the
//! thread that runs the capture process's control loop: never per message,
//! never by a receiver or the writer. Free space is what the file system
//! leaves to Kapto's user (`statvfs`'s `f_bavail`), not counting the share
//! it keeps for root.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Below how many free bytes the disk is low, and below how many it is too
/// full to record on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskLimits {
    /// Below this, Kapto warns.
    pub warn_below: u64,
    /// Below this, an episode is finalised and none is started.
    pub stop_below: u64,
}

/// How much room is left, against the limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Room {
    /// More than the warning limit, or not known: a disk whose room can't
    /// be read is never a reason to stop recording.
    Enough,
    /// Below the warning limit, with this many bytes free.
    Low(u64),
    /// Below the stopping limit, with this many bytes free.
    TooLow(u64),
}

impl DiskLimits {
    /// The room `free` bytes leave.
    #[must_use]
    pub fn room(&self, free: u64) -> Room {
        if free < self.stop_below {
            Room::TooLow(free)
        } else if free < self.warn_below {
            Room::Low(free)
        } else {
            Room::Enough
        }
    }
}

/// The bytes Kapto's user may still write on the file system holding
/// `path`.
///
/// # Errors
///
/// The operating system's, e.g. when `path` doesn't exist.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let stats = rustix::fs::statvfs(path)?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

/// Reads the room left at most once every [`DiskWatch::EVERY`], keeping
/// the last answer in between.
pub struct DiskWatch {
    limits: DiskLimits,
    probe: Box<dyn FnMut() -> io::Result<u64> + Send>,
    last: Option<(Instant, Room)>,
}

// The probe is a closure, which can't be printed.
impl std::fmt::Debug for DiskWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskWatch")
            .field("limits", &self.limits)
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

impl DiskWatch {
    /// How often the room is read.
    pub const EVERY: Duration = Duration::from_secs(1);

    /// Watches the disk holding `folder`.
    #[must_use]
    pub fn new(folder: PathBuf, limits: DiskLimits) -> Self {
        Self::with_probe(limits, move || free_bytes(&folder))
    }

    /// Watches whatever `probe` says is free, e.g. a pretend disk in a test.
    #[must_use]
    pub fn with_probe(
        limits: DiskLimits,
        probe: impl FnMut() -> io::Result<u64> + Send + 'static,
    ) -> Self {
        Self {
            limits,
            probe: Box::new(probe),
            last: None,
        }
    }

    /// The limits it checks against.
    #[must_use]
    pub fn limits(&self) -> DiskLimits {
        self.limits
    }

    /// The room left as of `now`, read again if the last reading is older
    /// than [`DiskWatch::EVERY`].
    pub fn room(&mut self, now: Instant) -> Room {
        match self.last {
            Some((at, room)) if now.saturating_duration_since(at) < Self::EVERY => room,
            _ => {
                let room = (self.probe)().map_or(Room::Enough, |free| self.limits.room(free));
                self.last = Some((now, room));
                room
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const LIMITS: DiskLimits = DiskLimits {
        warn_below: 10_000,
        stop_below: 2_000,
    };

    #[test]
    fn the_room_is_enough_then_low_then_too_low_as_the_disk_fills() {
        assert_eq!(LIMITS.room(10_000), Room::Enough);
        assert_eq!(LIMITS.room(9_999), Room::Low(9_999));
        assert_eq!(LIMITS.room(2_000), Room::Low(2_000));
        assert_eq!(LIMITS.room(1_999), Room::TooLow(1_999));
    }

    #[test]
    fn the_disk_is_read_at_most_once_a_second_and_unknown_room_never_stops_recording() {
        let free = Arc::new(AtomicU64::new(50_000));
        let reads = Arc::new(AtomicU64::new(0));
        let mut watch = DiskWatch::with_probe(LIMITS, {
            let (free, reads) = (Arc::clone(&free), Arc::clone(&reads));
            move || {
                reads.fetch_add(1, Ordering::Relaxed);
                Ok(free.load(Ordering::Relaxed))
            }
        });
        let start = Instant::now();
        assert_eq!(watch.room(start), Room::Enough);
        free.store(1_000, Ordering::Relaxed);
        assert_eq!(watch.room(start + Duration::from_millis(500)), Room::Enough);
        assert_eq!(reads.load(Ordering::Relaxed), 1);
        assert_eq!(watch.room(start + DiskWatch::EVERY), Room::TooLow(1_000));

        let mut unknown = DiskWatch::with_probe(LIMITS, || Err(io::ErrorKind::NotFound.into()));
        assert_eq!(unknown.room(start), Room::Enough);
    }

    #[test]
    fn a_real_disk_has_some_room() {
        assert!(free_bytes(&std::env::temp_dir()).unwrap() > 0);
        assert!(free_bytes(Path::new("/no/such/folder")).is_err());
    }
}
