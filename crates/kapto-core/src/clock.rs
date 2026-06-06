// SPDX-License-Identifier: Apache-2.0

//! Reading the host's clocks, and turning receive times into UTC.
//!
//! Kapto timestamps every message with the **monotonic** clock: it never jumps
//! when someone or something sets the system time, so intervals between
//! messages are always right. People and tools want **UTC**, though, so each
//! episode keeps a [`ClockMap`] relating the two.
//!
//! On Linux the monotonic clock and the wall clock receive the same gradual
//! rate corrections from time synchronisation (NTP, PTP). Their difference
//! only changes when the wall clock is *stepped*: set abruptly to a new value.
//! A `ClockMap` therefore converts with one fixed offset, taken from its first
//! sample, which keeps converted times in order no matter what happens to the
//! wall clock. Later samples are kept so steps are detected and reported, and
//! recordings can be re-timed afterwards if needed.

use std::time::Duration;

use rustix::time::{ClockId, Timespec, clock_gettime};

use crate::time::RecvTime;

/// The monotonic clock now.
///
/// It counts from an arbitrary point (usually boot) and doesn't advance while
/// the machine is suspended. All processes on the machine read the same clock,
/// so receive times from different Kapto processes can be compared.
#[must_use]
pub fn monotonic_now() -> RecvTime {
    RecvTime::from_nanos(u64::try_from(nanos(clock_gettime(ClockId::Monotonic))).unwrap_or(0))
}

/// The wall clock now, in nanoseconds since the Unix epoch (UTC).
#[must_use]
pub fn utc_now_ns() -> i64 {
    i64::try_from(nanos(clock_gettime(ClockId::Realtime))).unwrap_or(i64::MAX)
}

fn nanos(time: Timespec) -> i128 {
    i128::from(time.tv_sec) * 1_000_000_000 + i128::from(time.tv_nsec)
}

/// A simultaneous reading of both clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSample {
    /// The monotonic clock.
    pub mono: RecvTime,
    /// The wall clock, in nanoseconds since the Unix epoch.
    pub utc_ns: i64,
    /// How far apart the two readings may be. The wall clock is read between
    /// two monotonic readings; `mono` is their midpoint and this is the gap
    /// between them, usually well under a microsecond.
    pub uncertainty: Duration,
}

impl ClockSample {
    /// Reads both clocks now.
    #[must_use]
    pub fn now() -> Self {
        let before = monotonic_now();
        let utc_ns = utc_now_ns();
        let after = monotonic_now();
        let gap = after.as_nanos().saturating_sub(before.as_nanos());
        Self {
            mono: RecvTime::from_nanos(before.as_nanos() + gap / 2),
            utc_ns,
            uncertainty: Duration::from_nanos(gap),
        }
    }

    /// Wall clock minus monotonic clock, in nanoseconds.
    #[must_use]
    pub fn offset_ns(&self) -> i64 {
        self.utc_ns - mono_i64(self.mono)
    }
}

/// The monotonic clock counts from boot, so it fits in an `i64` for about
/// 292 years of uptime.
fn mono_i64(mono: RecvTime) -> i64 {
    i64::try_from(mono.as_nanos()).unwrap_or(i64::MAX)
}

/// Offset changes smaller than this are measurement noise, not steps.
pub const STEP_THRESHOLD: Duration = Duration::from_millis(1);

/// A detected change of the wall clock relative to the monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockStep {
    /// The first sample after the step.
    pub at: RecvTime,
    /// How far the wall clock moved, in nanoseconds; negative if it went back.
    pub change_ns: i64,
}

/// How the monotonic clock maps to UTC over one episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockMap {
    samples: Vec<ClockSample>,
}

impl ClockMap {
    /// A map anchored at `first`: every conversion uses its offset.
    #[must_use]
    pub fn new(first: ClockSample) -> Self {
        Self {
            samples: vec![first],
        }
    }

    /// Adds a later sample. Conversion doesn't change; steps become visible
    /// through [`ClockMap::steps`].
    pub fn record(&mut self, sample: ClockSample) {
        self.samples.push(sample);
    }

    /// Every sample, in the order recorded.
    #[must_use]
    pub fn samples(&self) -> &[ClockSample] {
        &self.samples
    }

    /// The UTC time, in nanoseconds since the Unix epoch, of a monotonic
    /// time: the anchor's offset applied. Works for times before the anchor
    /// too (e.g. data kept from just before an episode started). Later
    /// monotonic times always map to later UTC times.
    #[must_use]
    pub fn to_utc_ns(&self, mono: RecvTime) -> i64 {
        mono_i64(mono).saturating_add(self.anchor().offset_ns())
    }

    /// Wall-clock steps between consecutive samples larger than
    /// [`STEP_THRESHOLD`].
    #[must_use]
    pub fn steps(&self) -> Vec<ClockStep> {
        let threshold = i64::try_from(STEP_THRESHOLD.as_nanos()).unwrap_or(i64::MAX);
        self.samples
            .windows(2)
            .filter_map(|pair| {
                let change_ns = pair[1].offset_ns() - pair[0].offset_ns();
                (change_ns.abs() > threshold).then_some(ClockStep {
                    at: pair[1].mono,
                    change_ns,
                })
            })
            .collect()
    }

    fn anchor(&self) -> ClockSample {
        self.samples[0]
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const SECOND: i64 = 1_000_000_000;

    fn sample(mono_s: u64, utc_ns: i64) -> ClockSample {
        ClockSample {
            mono: RecvTime::from_nanos(mono_s * 1_000_000_000),
            utc_ns,
            uncertainty: Duration::ZERO,
        }
    }

    #[test]
    fn the_monotonic_clock_never_goes_back() {
        let mut previous = monotonic_now();
        for _ in 0..100_000 {
            let now = monotonic_now();
            assert!(now >= previous);
            previous = now;
        }
    }

    #[test]
    fn a_sample_reads_both_clocks_close_together() {
        let sample = ClockSample::now();
        // Generous: two clock reads take nanoseconds unless the thread is
        // preempted in between.
        assert!(sample.uncertainty < Duration::from_millis(10));
        // The wall clock reads as some time after 2020.
        assert!(sample.utc_ns > 1_577_836_800 * SECOND);
    }

    #[test]
    fn converting_a_live_reading_matches_the_wall_clock() {
        let map = ClockMap::new(ClockSample::now());
        std::thread::sleep(Duration::from_millis(20));
        let mono = monotonic_now();
        let utc = utc_now_ns();
        // Both clocks advance at the same rate, so the conversion stays
        // within a millisecond of a direct reading.
        assert!((map.to_utc_ns(mono) - utc).abs() < 1_000_000);
    }

    #[test]
    fn conversion_uses_the_first_samples_offset() {
        let map = ClockMap::new(sample(100, 1_000 * SECOND));
        assert_eq!(
            map.to_utc_ns(RecvTime::from_nanos(150 * 1_000_000_000)),
            1_050 * SECOND
        );
        // Before the anchor: data kept from just before the episode started.
        assert_eq!(
            map.to_utc_ns(RecvTime::from_nanos(95 * 1_000_000_000)),
            995 * SECOND
        );
    }

    #[test]
    fn a_wall_clock_step_is_reported_but_does_not_change_conversion() {
        let mut map = ClockMap::new(sample(100, 1_000 * SECOND));
        map.record(sample(101, 1_001 * SECOND));
        // Someone sets the clock back by 2 s.
        map.record(sample(102, 1_000 * SECOND));
        map.record(sample(103, 1_001 * SECOND));
        assert_eq!(
            map.steps(),
            [ClockStep {
                at: RecvTime::from_nanos(102 * 1_000_000_000),
                change_ns: -2 * SECOND,
            }]
        );
        assert_eq!(
            map.to_utc_ns(RecvTime::from_nanos(103 * 1_000_000_000)),
            1_003 * SECOND
        );
        assert_eq!(map.samples().len(), 4);
    }

    #[test]
    fn offset_noise_below_the_threshold_is_not_a_step() {
        let mut map = ClockMap::new(sample(100, 1_000 * SECOND));
        map.record(sample(101, 1_001 * SECOND + 300_000));
        map.record(sample(102, 1_002 * SECOND - 400_000));
        assert_eq!(map.steps(), []);
    }

    proptest! {
        #[test]
        fn later_monotonic_times_always_map_to_later_utc(
            anchor_mono in 0_u64..1_000_000 * 1_000_000_000,
            anchor_utc in 0_i64..4_000_000_000 * SECOND,
            a in 0_u64..2_000_000 * 1_000_000_000,
            b in 0_u64..2_000_000 * 1_000_000_000,
            later_offsets in prop::collection::vec(-10 * SECOND..10 * SECOND, 0..5),
        ) {
            let mut map = ClockMap::new(ClockSample {
                mono: RecvTime::from_nanos(anchor_mono),
                utc_ns: anchor_utc,
                uncertainty: Duration::ZERO,
            });
            for (index, offset) in (1_u64..).zip(later_offsets) {
                let mono = anchor_mono + index * 1_000_000_000;
                map.record(ClockSample {
                    mono: RecvTime::from_nanos(mono),
                    utc_ns: anchor_utc + offset,
                    uncertainty: Duration::ZERO,
                });
            }
            let (earlier, later) = (a.min(b), a.max(b));
            let converted_earlier = map.to_utc_ns(RecvTime::from_nanos(earlier));
            let converted_later = map.to_utc_ns(RecvTime::from_nanos(later));
            prop_assert!(converted_earlier <= converted_later);
            prop_assert_eq!(
                converted_later - converted_earlier,
                i64::try_from(later - earlier).unwrap()
            );
        }
    }
}
