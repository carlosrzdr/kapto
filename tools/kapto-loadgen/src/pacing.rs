// SPDX-License-Identifier: Apache-2.0

//! Sending on time: absolute deadlines and how late each tick started.

use std::time::{Duration, Instant};

/// Tick `k` is due at exactly `start + k / rate`. Deadlines are computed from
/// the tick number rather than by adding a period repeatedly, so rounding
/// never accumulates and the long-run rate is exact.
#[derive(Clone, Copy, Debug)]
pub struct Schedule {
    start: Instant,
    rate_hz: u32,
}

impl Schedule {
    /// A schedule of `rate_hz` ticks per second, the first due at `start`.
    #[must_use]
    pub const fn new(start: Instant, rate_hz: u32) -> Self {
        Self { start, rate_hz }
    }

    /// Time from the start to tick `tick`, rounded down to a nanosecond.
    #[must_use]
    pub fn offset(&self, tick: u64) -> Duration {
        let nanos = u128::from(tick) * 1_000_000_000 / u128::from(self.rate_hz.max(1));
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// When tick `tick` is due.
    #[must_use]
    pub fn deadline(&self, tick: u64) -> Instant {
        self.start + self.offset(tick)
    }
}

/// Waits until `deadline`: sleeps until `spin` before it, then spins.
///
/// Sleeping alone can overshoot by tens to hundreds of microseconds. Spinning
/// for the last stretch makes ticks start within a few microseconds of their
/// deadline, but a spinning thread keeps a CPU core busy for that time.
pub fn wait_until(deadline: Instant, spin: Duration) {
    let sleep = deadline
        .checked_duration_since(Instant::now())
        .and_then(|wait| wait.checked_sub(spin));
    if let Some(sleep) = sleep {
        std::thread::sleep(sleep);
    }
    while Instant::now() < deadline {
        std::hint::spin_loop();
    }
}

/// Median, 99th percentile and maximum of a set of durations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Percentiles {
    /// Half the values are at or below this.
    pub p50: Duration,
    /// 99% of the values are at or below this.
    pub p99: Duration,
    /// The largest value.
    pub max: Duration,
}

/// Buckets of one microsecond up to this many microseconds; longer values are
/// counted together and reported through the maximum.
const BUCKETS: usize = 10_000;

/// Counts durations in 1 µs buckets, so percentiles over any number of values
/// need a fixed 80 KB instead of keeping every value.
#[derive(Clone, Debug)]
pub struct Histogram {
    buckets: Vec<u64>,
    over: u64,
    count: u64,
    max: Duration,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: vec![0; BUCKETS],
            over: 0,
            count: 0,
            max: Duration::ZERO,
        }
    }
}

impl Histogram {
    /// Counts one value.
    pub fn record(&mut self, value: Duration) {
        match usize::try_from(value.as_micros()) {
            Ok(micros) if micros < BUCKETS => self.buckets[micros] += 1,
            _ => self.over += 1,
        }
        self.count += 1;
        self.max = self.max.max(value);
    }

    /// Number of values counted.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// Nearest-rank percentiles, rounded down to the microsecond. Values past
    /// the last bucket report the maximum. No values gives zeros.
    #[must_use]
    pub fn percentiles(&self) -> Percentiles {
        Percentiles {
            p50: self.rank(50),
            p99: self.rank(99),
            max: self.max,
        }
    }

    /// The smallest value with at least `percent`% of values at or below it.
    fn rank(&self, percent: u64) -> Duration {
        if self.count == 0 {
            return Duration::ZERO;
        }
        let wanted = (self.count * percent).div_ceil(100).max(1);
        let mut seen = 0;
        for (micros, count) in (0_u64..).zip(&self.buckets) {
            seen += count;
            if seen >= wanted {
                return Duration::from_micros(micros);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_are_exact_multiples_of_the_period() {
        let schedule = Schedule::new(Instant::now(), 2_000);
        assert_eq!(schedule.offset(0), Duration::ZERO);
        assert_eq!(schedule.offset(1), Duration::from_micros(500));
        assert_eq!(schedule.offset(2_000), Duration::from_secs(1));
        assert_eq!(schedule.offset(7_200_000), Duration::from_secs(3_600));
    }

    #[test]
    fn rates_that_do_not_divide_a_second_do_not_drift() {
        // 1/3 s isn't a whole number of nanoseconds, but every third tick
        // lands exactly on a whole second.
        let schedule = Schedule::new(Instant::now(), 3);
        assert_eq!(schedule.offset(1), Duration::from_nanos(333_333_333));
        assert_eq!(schedule.offset(3), Duration::from_secs(1));
        assert_eq!(schedule.offset(3 * 86_400), Duration::from_hours(24));
    }

    #[test]
    fn waiting_never_returns_before_the_deadline() {
        for micros in [0, 50, 300, 2_000] {
            let deadline = Instant::now() + Duration::from_micros(micros);
            for spin in [Duration::ZERO, Duration::from_micros(200)] {
                wait_until(deadline, spin);
                assert!(Instant::now() >= deadline);
            }
        }
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        let mut histogram = Histogram::default();
        for micros in 1..=100 {
            histogram.record(Duration::from_micros(micros));
        }
        let percentiles = histogram.percentiles();
        assert_eq!(percentiles.p50, Duration::from_micros(50));
        assert_eq!(percentiles.p99, Duration::from_micros(99));
        assert_eq!(percentiles.max, Duration::from_micros(100));
        assert_eq!(histogram.count(), 100);
    }

    #[test]
    fn percentiles_round_down_to_the_microsecond() {
        let mut histogram = Histogram::default();
        histogram.record(Duration::from_nanos(7_900));
        assert_eq!(histogram.percentiles().p50, Duration::from_micros(7));
        assert_eq!(histogram.percentiles().max, Duration::from_nanos(7_900));
    }

    #[test]
    fn values_past_the_last_bucket_report_the_maximum() {
        let mut histogram = Histogram::default();
        histogram.record(Duration::from_millis(50));
        assert_eq!(histogram.percentiles().p99, Duration::from_millis(50));
    }

    #[test]
    fn percentiles_of_nothing_are_zero() {
        assert_eq!(Histogram::default().percentiles(), Percentiles::default());
    }
}
