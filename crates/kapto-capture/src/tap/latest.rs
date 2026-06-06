// SPDX-License-Identifier: Apache-2.0

//! Each stream's newest decoded values, kept by the tap thread as it sends
//! them to the viewer, for whoever asks what the robot is doing right now
//! (the capture process's `status`).
//!
//! Written at most once per stream each time the tap thread sends, about
//! sixty times a second, never per message; the lock is held only to copy a
//! few numbers.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The newest values of every stream.
#[derive(Debug)]
pub struct LatestValues {
    streams: Mutex<Vec<Slot>>,
}

#[derive(Debug, Default)]
struct Slot {
    values: Vec<f64>,
    at: Option<Instant>,
}

/// One stream's newest values, and how long ago they were decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Latest {
    /// The values, field after field, array elements in order.
    pub values: Vec<f64>,
    /// Time since they were decoded.
    pub age: Duration,
}

impl LatestValues {
    /// Nothing known yet for any of `streams` streams.
    #[must_use]
    pub fn new(streams: usize) -> Self {
        Self {
            streams: Mutex::new((0..streams).map(|_| Slot::default()).collect()),
        }
    }

    /// Notes `values` as stream `stream`'s newest. The stream's buffer is
    /// reused, so after the first time this doesn't allocate.
    pub fn set(&self, stream: usize, values: &[f64]) {
        let mut streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = streams.get_mut(stream) {
            slot.values.clear();
            slot.values.extend_from_slice(values);
            slot.at = Some(Instant::now());
        }
    }

    /// Each stream's newest values, in stream order; `None` for a stream
    /// nothing was decoded from yet.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Option<Latest>> {
        let streams = self.streams.lock().unwrap_or_else(PoisonError::into_inner);
        streams
            .iter()
            .map(|slot| {
                slot.at.map(|at| Latest {
                    values: slot.values.clone(),
                    age: at.elapsed(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_stream_keeps_only_its_newest_values() {
        let latest = LatestValues::new(2);
        assert_eq!(latest.snapshot(), [None, None]);
        latest.set(1, &[1.0, 2.0, 3.0]);
        latest.set(1, &[4.0, 5.0]);
        let snapshot = latest.snapshot();
        assert_eq!(snapshot[0], None);
        let newest = snapshot[1].as_ref().map(|latest| latest.values.clone());
        assert_eq!(newest, Some(vec![4.0, 5.0]));
        // A stream that doesn't exist is ignored.
        latest.set(7, &[1.0]);
        assert_eq!(latest.snapshot().len(), 2);
    }
}
