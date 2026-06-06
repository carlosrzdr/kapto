// SPDX-License-Identifier: Apache-2.0

//! What Kapto's tests share, so each test file holds only what it tests:
//! scratch folders, waiting for something to happen, and example episodes.
//! Only ever a dev-dependency.

#![allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use kapto_core::{ClockMap, ClockSample, EpisodeId, RecvTime};
use kapto_store::{BufferSummary, DurabilitySummary, Episode, LossDetectionKind, StreamSummary};

/// An empty folder for the test `test`, made again if a run before left
/// it, and left after the test for a look at what it wrote. Its path is
/// short enough for Unix sockets inside it.
#[must_use]
pub fn scratch(test: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kt-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// A [`scratch`] folder that is removed when dropped.
#[derive(Debug)]
pub struct TempDir(pub PathBuf);

impl TempDir {
    /// An empty folder for the test `test`.
    #[must_use]
    pub fn new(test: &str) -> Self {
        Self(scratch(test))
    }

    /// Writes `bytes` to `path` inside it, making the folders on the way,
    /// and gives the file's full path.
    pub fn write(&self, path: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Waits up to 20 seconds for `done`, failing the test with `what` if it
/// never is.
pub fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The summary of episode `id`, recorded from `started_utc_ns` to
/// `ended_utc_ns` with `streams`, which recording made without trouble:
/// two batches, one at a time, synced twice. The rest (package, profile,
/// task) is set with its own methods or fields.
#[must_use]
pub fn episode(
    id: &str,
    (started_utc_ns, ended_utc_ns): (i64, i64),
    streams: Vec<StreamSummary>,
) -> Episode {
    let id: EpisodeId = id.parse().unwrap();
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(1),
        utc_ns: started_utc_ns,
        uncertainty: Duration::ZERO,
    });
    Episode::new(
        &id,
        (started_utc_ns, ended_utc_ns),
        "recording.mcap",
        &clock,
        (
            BufferSummary {
                batches: 2,
                peak_batches: 1,
                memory_bytes: 1024,
            },
            DurabilitySummary {
                sync_interval_ns: 500_000_000,
                syncs: 2,
                max_sync_ns: 1,
                p50_unsynced_ns: 1,
                p99_unsynced_ns: 1,
                max_unsynced_ns: 1,
            },
        ),
        streams,
    )
}

/// A stream `id` with sequence numbers that received `received` messages
/// and wrote them all; set the loss fields to make it lose some.
#[must_use]
pub fn stream(id: &str, received: u64) -> StreamSummary {
    StreamSummary {
        id: id.to_owned(),
        loss_detection: LossDetectionKind::Sequence,
        received,
        decoded: received,
        written: received,
        dropped_by_us: 0,
        lost_upstream: 0,
        disconnects: None,
        carried: None,
    }
}
