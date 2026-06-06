// SPDX-License-Identifier: Apache-2.0

//! The bandwidth cap (`upload.bandwidth`), for a link an upload shares with
//! the robot's own traffic.
//!
//! `object_store` sends each request's body as fast as the link takes it,
//! so the cap is kept between requests: each request that sends data waits
//! until the ones before it would have taken at the cap, then goes. A part
//! leaves as a burst, and on average an upload sends no more than the cap.
//! With a cap, parts are made about one second at it, so each burst lasts
//! about a second at most.

use std::num::NonZeroU64;
use std::time::Duration;

/// When requests may go, at most `bytes_per_second` on average.
#[derive(Clone, Debug)]
pub struct Pace {
    bytes_per_second: Option<NonZeroU64>,
    /// When everything counted so far would have been sent at the cap, on
    /// the caller's clock.
    free_at: Option<Duration>,
}

impl Pace {
    /// A pace of at most `bytes_per_second`, or none.
    #[must_use]
    pub fn new(bytes_per_second: Option<NonZeroU64>) -> Self {
        Self {
            bytes_per_second,
            free_at: None,
        }
    }

    /// How long to wait, at `now`, before sending `bytes`, which are then
    /// counted as sent. A sender slower than the cap never waits.
    pub fn wait(&mut self, bytes: u64, now: Duration) -> Duration {
        let Some(rate) = self.bytes_per_second else {
            return Duration::ZERO;
        };
        let start = self.free_at.map_or(now, |free| free.max(now));
        let nanos = u128::from(bytes) * 1_000_000_000 / u128::from(rate.get());
        let takes = Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
        self.free_at = Some(start.saturating_add(takes));
        start.saturating_sub(now)
    }
}

/// The size of the parts a large file goes up in: `configured`, or with a
/// cap of `bytes_per_second` about one second at it if that is smaller,
/// and never below `smallest`, the smallest part the store takes.
#[must_use]
pub fn part_bytes(configured: u64, bytes_per_second: Option<NonZeroU64>, smallest: u64) -> u64 {
    let paced = bytes_per_second.map_or(configured, |rate| configured.min(rate.get()));
    paced.max(smallest).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn rate(bytes: u64) -> Option<NonZeroU64> {
        NonZeroU64::new(bytes)
    }

    #[test]
    fn requests_sent_at_once_wait_for_the_ones_before_them_at_the_cap() {
        let mut pace = Pace::new(rate(MIB));
        let at = |seconds: f64| Duration::from_secs_f64(seconds);
        assert_eq!(
            pace.wait(MIB, at(0.0)),
            Duration::ZERO,
            "the first goes at once"
        );
        assert_eq!(pace.wait(MIB, at(0.0)), at(1.0));
        assert_eq!(pace.wait(MIB / 2, at(1.0)), at(1.0));
        assert_eq!(pace.wait(MIB, at(2.0)), at(0.5));
    }

    #[test]
    fn a_sender_slower_than_the_cap_never_waits_and_none_without_a_cap() {
        let mut pace = Pace::new(rate(MIB));
        for second in 0..5 {
            assert_eq!(
                pace.wait(MIB / 2, Duration::from_secs(second)),
                Duration::ZERO
            );
        }
        let mut none = Pace::new(None);
        for _ in 0..5 {
            assert_eq!(none.wait(100 * MIB, Duration::ZERO), Duration::ZERO);
        }
    }

    #[test]
    fn over_any_stretch_no_more_than_the_cap_and_one_request_is_sent() {
        // Requests of varying sizes, each sent the moment it may go, with
        // the clock advanced only by the waits.
        let cap = 3 * MIB;
        let mut pace = Pace::new(rate(cap));
        let mut now = Duration::ZERO;
        let mut sent = 0;
        for size in (1..200).map(|index| (index * 7919) % (2 * MIB) + 1) {
            now += pace.wait(size, now);
            sent += size;
            let allowed = now.as_nanos() * u128::from(cap) / 1_000_000_000 + u128::from(size);
            assert!(u128::from(sent) <= allowed + 1, "{sent} by {now:?}");
        }
    }

    #[test]
    fn parts_are_about_one_second_at_the_cap_within_the_stores_limits() {
        let s3 = 5 * MIB;
        assert_eq!(part_bytes(16 * MIB, None, s3), 16 * MIB);
        assert_eq!(part_bytes(16 * MIB, rate(8 * MIB), s3), 8 * MIB);
        // Never below what the store takes, nor above what is configured.
        assert_eq!(part_bytes(16 * MIB, rate(MIB), s3), s3);
        assert_eq!(part_bytes(16 * MIB, rate(MIB), 0), MIB);
        assert_eq!(part_bytes(16 * MIB, rate(100 * MIB), s3), 16 * MIB);
        assert_eq!(part_bytes(0, None, 0), 1);
    }
}
