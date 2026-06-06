// SPDX-License-Identifier: Apache-2.0

//! How long to wait before trying to reconnect.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::time::Duration;

/// Exponential backoff with jitter: each failed attempt doubles the wait, up
/// to a maximum, and each wait is drawn from the upper half of its range so
/// many collectors restarting together don't retry in lockstep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    failures: u32,
}

impl Backoff {
    /// Waits starting at `initial`, never longer than `max`.
    #[must_use]
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max: max.max(initial),
            failures: 0,
        }
    }

    /// The wait after one more failure: between half and all of
    /// `initial × 2^failures`, capped at `max`.
    pub fn next_wait(&mut self) -> Duration {
        let full = self.ceiling();
        self.failures = self.failures.saturating_add(1);
        let half = full / 2;
        half + random_fraction(full.saturating_sub(half))
    }

    /// The longest the next wait can be.
    #[must_use]
    pub fn ceiling(&self) -> Duration {
        let factor = 2_u32.saturating_pow(self.failures.min(31));
        self.initial.saturating_mul(factor).min(self.max)
    }

    /// Starts again from the initial wait, after a success.
    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

/// A random duration from zero up to `range`.
fn random_fraction(range: Duration) -> Duration {
    // `RandomState` is seeded from the operating system's randomness.
    let random = RandomState::new().hash_one(());
    let nanos = u64::try_from(range.as_nanos()).unwrap_or(u64::MAX);
    if nanos == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(random % (nanos + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_double_up_to_the_maximum_and_stay_in_the_upper_half() {
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(1));
        for ceiling_ms in [100, 200, 400, 800, 1_000, 1_000] {
            let ceiling = Duration::from_millis(ceiling_ms);
            assert_eq!(backoff.ceiling(), ceiling);
            let wait = backoff.next_wait();
            assert!(
                wait >= ceiling / 2 && wait <= ceiling,
                "{wait:?} for {ceiling:?}"
            );
        }
    }

    #[test]
    fn a_success_starts_again_from_the_initial_wait() {
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(5));
        backoff.next_wait();
        backoff.next_wait();
        backoff.reset();
        assert_eq!(backoff.ceiling(), Duration::from_millis(100));
    }

    #[test]
    fn waits_vary_so_collectors_dont_retry_in_lockstep() {
        let mut waits = std::collections::BTreeSet::new();
        for _ in 0..20 {
            let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(1));
            waits.insert(backoff.next_wait());
        }
        assert!(waits.len() > 1);
    }

    #[test]
    fn many_failures_never_overflow() {
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(5));
        for _ in 0..1_000 {
            assert!(backoff.next_wait() <= Duration::from_secs(5));
        }
    }
}
