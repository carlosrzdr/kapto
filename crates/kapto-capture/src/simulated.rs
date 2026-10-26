// SPDX-License-Identifier: Apache-2.0

//! A simulation's newest time, as the live view decodes it from the field
//! playing `time.simulated`, for the loss accounting to read without
//! decoding anything itself: one atomic number, written by the tap thread,
//! read once per batch, never locked.

use std::sync::atomic::{AtomicU64, Ordering};

/// The simulation's newest time, in nanoseconds since it started, or
/// nothing yet.
#[derive(Debug)]
pub struct SimulatedNow(AtomicU64);

/// What no time read yet is stored as: a simulation never runs this long.
const UNKNOWN: u64 = u64::MAX;

impl SimulatedNow {
    /// Nothing read yet.
    #[must_use]
    pub fn new() -> Self {
        Self(AtomicU64::new(UNKNOWN))
    }

    /// Keeps `simulated_ns`, the clock's newest decoded value; a negative
    /// time, which no simulation counts, is kept as nothing.
    pub fn set(&self, simulated_ns: i64) {
        let value = u64::try_from(simulated_ns).unwrap_or(UNKNOWN);
        self.0.store(value, Ordering::Relaxed);
    }

    /// The newest time kept, if one is.
    #[must_use]
    pub fn get(&self) -> Option<u64> {
        let value = self.0.load(Ordering::Relaxed);
        (value != UNKNOWN).then_some(value)
    }
}

impl Default for SimulatedNow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_read_until_the_clock_says_a_time() {
        let now = SimulatedNow::new();
        assert_eq!(now.get(), None);
        now.set(1_500_000_000);
        assert_eq!(now.get(), Some(1_500_000_000));
        now.set(0);
        assert_eq!(now.get(), Some(0), "a reset to zero is a time");
    }

    #[test]
    fn a_negative_time_is_kept_as_none() {
        let now = SimulatedNow::new();
        now.set(2);
        now.set(-1);
        assert_eq!(now.get(), None);
    }
}
