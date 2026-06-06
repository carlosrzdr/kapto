// SPDX-License-Identifier: Apache-2.0

//! How long received messages wait before they are on disk.
//!
//! A message is on disk once the sync after it was written completes. The
//! writer counts written messages into fixed buckets by receive time and, at
//! each sync, folds them into a histogram of waits. Nothing is allocated per
//! message or per sync, and each bucket is timed from its oldest message, so
//! the percentiles are never optimistic. The longest wait is tracked exactly.

use std::time::Duration;

use kapto_core::RecvTime;
use kapto_store::DurabilitySummary;

/// The resolution of the waits.
const BUCKET: Duration = Duration::from_millis(10);

/// Waits longer than this are counted as this long in the histogram (the
/// longest wait is still exact).
const LONGEST_BUCKETED: Duration = Duration::from_secs(60);

/// How long a recording's messages waited to be on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DurabilityStats {
    /// Syncs completed.
    pub syncs: u64,
    /// The longest a single sync took.
    pub max_sync: Duration,
    /// Half of the messages were on disk within this.
    pub p50_unsynced: Duration,
    /// 99 % of the messages were on disk within this.
    pub p99_unsynced: Duration,
    /// The longest any message waited.
    pub max_unsynced: Duration,
}

impl DurabilityStats {
    /// The recording's durability entry in `episode.json`, for syncs every
    /// `sync_interval`.
    #[must_use]
    pub fn summary(&self, sync_interval: Duration) -> DurabilitySummary {
        DurabilitySummary {
            sync_interval_ns: nanos(sync_interval),
            syncs: self.syncs,
            max_sync_ns: nanos(self.max_sync),
            p50_unsynced_ns: nanos(self.p50_unsynced),
            p99_unsynced_ns: nanos(self.p99_unsynced),
            max_unsynced_ns: nanos(self.max_unsynced),
        }
    }
}

/// Follows writes and syncs.
#[derive(Debug)]
pub(crate) struct DurabilityTracker {
    window_start: RecvTime,
    /// Messages written since the last sync, by receive time from
    /// `window_start`.
    pending: Box<[u64]>,
    /// Messages written since the last sync but received before
    /// `window_start` (written late, e.g. after a writer stall).
    late: u64,
    oldest_pending: Option<RecvTime>,
    /// Messages by how long they waited, in buckets.
    waits: Box<[u64]>,
    stats: DurabilityStats,
}

impl DurabilityTracker {
    /// A tracker for syncs about every `sync_interval`, starting at `start`.
    pub(crate) fn new(sync_interval: Duration, start: RecvTime) -> Self {
        // Room for a window twice as long as planned, for late syncs.
        let pending = buckets(sync_interval * 2) + 1;
        Self {
            window_start: start,
            pending: vec![0; pending].into_boxed_slice(),
            late: 0,
            oldest_pending: None,
            waits: vec![0; buckets(LONGEST_BUCKETED) + 1].into_boxed_slice(),
            stats: DurabilityStats::default(),
        }
    }

    /// Records `messages` received at `received` and now written.
    pub(crate) fn written(&mut self, received: RecvTime, messages: u64) {
        if messages == 0 {
            return;
        }
        if received < self.window_start {
            self.late += messages;
        } else {
            let index = buckets(received.saturating_since(self.window_start));
            self.pending[index.min(self.pending.len() - 1)] += messages;
        }
        self.oldest_pending = Some(
            self.oldest_pending
                .map_or(received, |oldest| oldest.min(received)),
        );
    }

    /// Records a sync that started at `started` and completed at `done`.
    pub(crate) fn synced(&mut self, started: RecvTime, done: RecvTime) {
        for (index, count) in self.pending.iter_mut().enumerate() {
            if *count > 0 {
                let received_from = self.window_start.as_nanos() + nanos(BUCKET) * index as u64;
                let wait = done.saturating_since(RecvTime::from_nanos(received_from));
                let slot = buckets(wait).min(self.waits.len() - 1);
                self.waits[slot] += std::mem::take(count);
            }
        }
        if let Some(oldest) = self.oldest_pending.take() {
            let longest = done.saturating_since(oldest);
            self.stats.max_unsynced = self.stats.max_unsynced.max(longest);
            // Late messages are timed from the oldest message waiting: their
            // waits may be overstated, never understated.
            let slot = buckets(longest).min(self.waits.len() - 1);
            self.waits[slot] += std::mem::take(&mut self.late);
        }
        self.stats.max_sync = self.stats.max_sync.max(done.saturating_since(started));
        self.stats.syncs += 1;
        self.window_start = done;
    }

    /// The waits so far.
    pub(crate) fn stats(&self) -> DurabilityStats {
        DurabilityStats {
            p50_unsynced: self.percentile(50),
            p99_unsynced: self.percentile(99),
            ..self.stats
        }
    }

    /// The upper edge of the bucket holding the `percent`-th percentile.
    fn percentile(&self, percent: u64) -> Duration {
        let total: u64 = self.waits.iter().sum();
        let wanted = (total * percent).div_ceil(100);
        let mut seen = 0;
        for (index, count) in self.waits.iter().enumerate() {
            seen += count;
            if seen >= wanted && seen > 0 {
                return BUCKET * u32::try_from(index + 1).unwrap_or(u32::MAX);
            }
        }
        Duration::ZERO
    }
}

fn buckets(duration: Duration) -> usize {
    usize::try_from(duration.as_nanos() / BUCKET.as_nanos()).unwrap_or(usize::MAX)
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn at(ms: u64) -> RecvTime {
        RecvTime::from_nanos(ms * MS)
    }

    #[test]
    fn messages_received_evenly_over_a_second_wait_up_to_the_sync() {
        let mut tracker = DurabilityTracker::new(Duration::from_secs(1), at(0));
        for ms in 0..1_000 {
            tracker.written(at(ms), 1);
        }
        tracker.synced(at(1_000), at(1_004));
        let stats = tracker.stats();
        assert_eq!(stats.syncs, 1);
        assert_eq!(stats.max_sync, Duration::from_millis(4));
        assert_eq!(stats.max_unsynced, Duration::from_millis(1_004));
        // Half waited about 0.5 s, 99 % about 1 s; each rounded up to 10 ms.
        assert_eq!(stats.p50_unsynced, Duration::from_millis(510));
        assert_eq!(stats.p99_unsynced, Duration::from_millis(1_000));
    }

    #[test]
    fn each_sync_starts_a_new_window() {
        let mut tracker = DurabilityTracker::new(Duration::from_secs(1), at(0));
        tracker.written(at(900), 10);
        tracker.synced(at(1_000), at(1_001));
        tracker.written(at(1_950), 10);
        tracker.synced(at(2_000), at(2_002));
        let stats = tracker.stats();
        assert_eq!(stats.syncs, 2);
        assert_eq!(stats.max_unsynced, Duration::from_millis(101));
        assert_eq!(stats.p99_unsynced, Duration::from_millis(110));
    }

    #[test]
    fn a_message_received_before_the_window_started_still_counts_its_full_wait() {
        let mut tracker = DurabilityTracker::new(Duration::from_secs(1), at(1_000));
        // Received before the window began, written late by a stalled writer.
        tracker.written(at(200), 1);
        tracker.synced(at(2_000), at(2_000));
        let stats = tracker.stats();
        assert_eq!(stats.max_unsynced, Duration::from_millis(1_800));
        assert_eq!(stats.p99_unsynced, Duration::from_millis(1_810));
    }

    #[test]
    fn no_messages_means_no_waits() {
        let mut tracker = DurabilityTracker::new(Duration::from_secs(1), at(0));
        tracker.synced(at(1_000), at(1_001));
        let stats = tracker.stats();
        assert_eq!(
            (stats.syncs, stats.p99_unsynced, stats.max_unsynced),
            (1, Duration::ZERO, Duration::ZERO)
        );
    }
}
