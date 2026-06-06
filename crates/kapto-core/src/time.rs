// SPDX-License-Identifier: Apache-2.0

//! The two timestamps every recorded message carries.
//!
//! [`RecvTime`] comes from the host's clock, [`SourceTime`] from the robot's.
//! Those clocks can be offset from each other and drift apart, so the types
//! are deliberately incompatible: they can't be compared, subtracted or
//! converted into each other. Relating them is an explicit, recorded step.

use std::time::Duration;

/// When Kapto received a message: the host's monotonic clock, in nanoseconds.
///
/// The monotonic clock never jumps when the wall clock is adjusted, so
/// differences between two receive times are reliable. It has no meaning
/// across reboots or machines; each episode stores how it maps to UTC.
///
/// A receive time can't be compared with a [`SourceTime`]:
///
/// ```compile_fail
/// use kapto_core::{RecvTime, SourceTime};
///
/// let _ = RecvTime::from_nanos(1) == SourceTime::Integer(1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecvTime {
    mono_ns: u64,
}

impl RecvTime {
    /// A receive time from a monotonic clock reading in nanoseconds.
    #[must_use]
    pub const fn from_nanos(mono_ns: u64) -> Self {
        Self { mono_ns }
    }

    /// The monotonic clock reading in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.mono_ns
    }

    /// Time elapsed since `earlier`, or zero if `earlier` is later than `self`.
    #[must_use]
    pub const fn saturating_since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.mono_ns.saturating_sub(earlier.mono_ns))
    }
}

/// The timestamp a robot wrote into its message, exactly as received.
///
/// Its unit and epoch are described by the stream's declaration in the robot
/// package, not here: this type only preserves the raw value so it is never
/// lost or rounded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SourceTime {
    /// A single integer, e.g. nanoseconds or ticks since some epoch.
    Integer(i64),
    /// A floating-point number, e.g. seconds since some epoch.
    Float(f64),
    /// Whole seconds and nanoseconds, as in ROS message headers.
    SecondsNanos {
        /// Whole seconds.
        seconds: i64,
        /// Nanoseconds within the second.
        nanos: u32,
    },
}

/// A UTC time in nanoseconds since the Unix epoch as ISO 8601, e.g.
/// `2026-03-09T14:33:31.637123456Z`. Times before 1970 are shown as their
/// number of nanoseconds, since recordings never have them.
#[must_use]
pub fn utc_iso8601(utc_ns: i64) -> String {
    const NS_PER_SECOND: u64 = 1_000_000_000;
    let Ok(ns) = u64::try_from(utc_ns) else {
        return format!("{utc_ns} ns");
    };
    let seconds = ns / NS_PER_SECOND;
    let (year, month, day) = crate::episode::civil_from_days(seconds / 86_400);
    let second_of_day = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:09}Z",
        second_of_day / 3_600,
        second_of_day / 60 % 60,
        second_of_day % 60,
        ns % NS_PER_SECOND
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn later_receive_times_measure_the_elapsed_nanoseconds(
            earlier in 0..u64::MAX / 2,
            elapsed in 0..u64::MAX / 2,
        ) {
            let later = RecvTime::from_nanos(earlier + elapsed);
            let earlier = RecvTime::from_nanos(earlier);
            prop_assert_eq!(later.saturating_since(earlier), Duration::from_nanos(elapsed));
            prop_assert!(earlier <= later);
        }
    }

    #[test]
    fn utc_times_are_shown_as_iso_8601_to_the_nanosecond() {
        // 2026-03-09T14:33:31.637123456Z
        assert_eq!(
            utc_iso8601(1_773_066_811_637_123_456),
            "2026-03-09T14:33:31.637123456Z"
        );
        assert_eq!(utc_iso8601(0), "1970-01-01T00:00:00.000000000Z");
        assert_eq!(utc_iso8601(-5), "-5 ns");
    }

    #[test]
    fn elapsed_time_is_zero_when_the_other_time_is_later() {
        let earlier = RecvTime::from_nanos(10);
        let later = RecvTime::from_nanos(20);
        assert_eq!(earlier.saturating_since(later), Duration::ZERO);
    }

    #[test]
    fn source_times_keep_the_raw_value() {
        let header = SourceTime::SecondsNanos {
            seconds: 1_772_700_703,
            nanos: 787_000_001,
        };
        assert_eq!(
            header,
            SourceTime::SecondsNanos {
                seconds: 1_772_700_703,
                nanos: 787_000_001,
            }
        );
        assert_ne!(SourceTime::Integer(1), SourceTime::Float(1.0));
    }
}
