// SPDX-License-Identifier: Apache-2.0

//! How long to wait before trying a failed request again: twice as long as
//! the time before, from `upload.backoff_initial` up to `upload.backoff_max`,
//! each wait a random time up to that ("full jitter"). Random waits keep
//! robots that lost their link together from all trying again at the same
//! moment when it comes back.

use std::time::Duration;

/// The waits before each new try of one request.
#[derive(Clone, Debug)]
pub struct Backoff {
    initial: Duration,
    max: Duration,
    /// The random sequence's state: the waits only need to differ between
    /// robots and tries, not to be unpredictable.
    state: u64,
}

impl Backoff {
    /// Waits from `initial` doubling up to `max`, drawn from `seed`.
    #[must_use]
    pub const fn new(initial: Duration, max: Duration, seed: u64) -> Self {
        Self {
            initial,
            max,
            state: seed,
        }
    }

    /// Waits seeded from the clock, so robots started together still draw
    /// different ones.
    #[must_use]
    pub fn from_clock(initial: Duration, max: Duration) -> Self {
        Self::new(
            initial,
            max,
            kapto_core::clock::utc_now_ns().cast_unsigned(),
        )
    }

    /// The longest wait before try `retry` (0 for the first retry): the
    /// initial wait doubled `retry` times, at most the maximum.
    #[must_use]
    pub fn cap(&self, retry: u32) -> Duration {
        let doubled = self.initial.saturating_mul(1_u32 << retry.min(31));
        doubled.min(self.max)
    }

    /// The wait before try `retry`: a random time up to [`Backoff::cap`].
    pub fn wait(&mut self, retry: u32) -> Duration {
        let cap = self.cap(retry);
        let nanos = u64::try_from(cap.as_nanos()).unwrap_or(u64::MAX);
        Duration::from_nanos(self.next() % nanos.saturating_add(1))
    }

    /// `SplitMix64`: a fast generator with a full period, plenty for spreading
    /// waits.
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_wait_doubles_from_the_first_up_to_the_maximum() {
        let backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(5), 1);
        let caps: Vec<u128> = (0..6).map(|retry| backoff.cap(retry).as_millis()).collect();
        assert_eq!(caps, [500, 1000, 2000, 4000, 5000, 5000]);
        assert_eq!(backoff.cap(1000), Duration::from_secs(5), "never overflows");
    }

    #[test]
    fn each_wait_is_a_random_time_up_to_its_longest() {
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(1), 7);
        let waits: Vec<Duration> = (0..1000).map(|_| backoff.wait(3)).collect();
        assert!(waits.iter().all(|wait| *wait <= Duration::from_millis(800)));
        let mean = waits.iter().sum::<Duration>() / 1000;
        assert!(
            (300..500).contains(&mean.as_millis()),
            "spread over the whole range: {mean:?}"
        );
    }

    #[test]
    fn robots_seeded_differently_wait_differently() {
        let draw = |seed| {
            let mut backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60), seed);
            (0..5).map(|retry| backoff.wait(retry)).collect::<Vec<_>>()
        };
        assert_eq!(draw(1), draw(1));
        assert_ne!(draw(1), draw(2));
    }
}
