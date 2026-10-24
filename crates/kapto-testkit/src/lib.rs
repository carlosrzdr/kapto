// SPDX-License-Identifier: Apache-2.0

//! What Kapto's tests share, so each test file holds only what it tests:
//! scratch folders, waiting for something to happen, and example episodes.
//! Only ever a dev-dependency.

#![allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime};

use kapto_core::{ClockMap, ClockSample, EpisodeId, RecvTime};
use kapto_store::{BufferSummary, DurabilitySummary, Episode, LossDetectionKind, StreamSummary};

/// How long a folder [`scratch`] made is kept once its test has ended, for
/// a look at what a failed test wrote.
pub const KEPT_FOR: Duration = Duration::from_hours(1);

/// An empty folder for the test `test`, made again if a run before left
/// it, and left after the test for a look at what it wrote. Its path is
/// short enough for Unix sockets inside it. The first in a process removes
/// those of tests that ended over [`KEPT_FOR`] ago: each test runs in a
/// process of its own, so no later one reuses them.
#[must_use]
pub fn scratch(test: &str) -> PathBuf {
    static SWEPT: Once = Once::new();
    SWEPT.call_once(|| sweep(&std::env::temp_dir(), KEPT_FOR));
    let folder = std::env::temp_dir().join(format!("kt-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// Removes the [`scratch`] folders in `temp` of test processes that have
/// ended, untouched for longer than `kept_for`. Which processes run is read
/// from `/proc`, so elsewhere nothing is removed.
pub fn sweep(temp: &Path, kept_for: Duration) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix("kt-"))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let ended = !Path::new("/proc").join(pid.to_string()).exists();
        let untouched = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > kept_for);
        if ended && untouched {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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
        rejected: None,
        carried: None,
        pre_roll: None,
        post_roll: None,
    }
}
