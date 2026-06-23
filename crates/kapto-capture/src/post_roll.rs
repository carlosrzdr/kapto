// SPDX-License-Identifier: Apache-2.0

//! Each stream's post-roll: how long after an episode is stopped its
//! messages go on being recorded.
//!
//! When Stop is asked, the time is noted here. The receiver threads read it
//! once per batch and stamp it into the batch, so loss accounting and the
//! writer judge each message by the same stop: a message received after
//! its stream's post-roll is neither counted nor written.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kapto_core::RecvTime;

use crate::batch::StreamIndex;

/// When the episode being recorded was stopped, shared between the thread
/// that notes it and those that judge each message by it, and each
/// stream's post-roll.
#[derive(Clone, Debug)]
pub struct PostRoll(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    /// When Stop was asked, in nanoseconds; `u64::MAX` until it is.
    stopped: AtomicU64,
    /// Each stream's post-roll, in nanoseconds, by stream index.
    nanos: Box<[u64]>,
}

/// What [`PostRoll::stopped`] reads before Stop is asked.
pub const NOT_STOPPED: u64 = u64::MAX;

impl Default for PostRoll {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl PostRoll {
    /// Each stream's post-roll, by stream index; none is stopped yet.
    #[must_use]
    pub fn new(post_rolls: &[Duration]) -> Self {
        let nanos = post_rolls
            .iter()
            .map(|post_roll| u64::try_from(post_roll.as_nanos()).unwrap_or(u64::MAX))
            .collect();
        Self(Arc::new(Shared {
            stopped: AtomicU64::new(NOT_STOPPED),
            nanos,
        }))
    }

    /// Notes that Stop was asked at `at`, or with `None` that the episode
    /// starting isn't stopped.
    pub fn stop(&self, at: Option<RecvTime>) {
        self.0.stopped.store(
            at.map_or(NOT_STOPPED, RecvTime::as_nanos),
            Ordering::Relaxed,
        );
    }

    /// When Stop was asked, in nanoseconds, or [`NOT_STOPPED`].
    #[must_use]
    pub fn stopped(&self) -> u64 {
        self.0.stopped.load(Ordering::Relaxed)
    }

    /// Whether a message of `stream` received at `at` is past its stream's
    /// post-roll after a stop at `stopped`, as [`PostRoll::stopped`] read it.
    #[must_use]
    pub fn ended(&self, stopped: u64, stream: StreamIndex, at: RecvTime) -> bool {
        stopped != NOT_STOPPED
            && at.as_nanos()
                >= stopped
                    .saturating_add(self.0.nanos.get(usize::from(stream)).copied().unwrap_or(0))
    }

    /// The longest post-roll: how long after Stop the episode ends.
    #[must_use]
    pub fn longest(&self) -> Duration {
        Duration::from_nanos(self.0.nanos.iter().copied().max().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_a_stop_each_stream_ends_once_its_own_post_roll_has_passed() {
        let post_roll = PostRoll::new(&[Duration::ZERO, Duration::from_nanos(50)]);
        let at = RecvTime::from_nanos;
        assert_eq!(post_roll.stopped(), NOT_STOPPED);
        assert!(!post_roll.ended(post_roll.stopped(), 0, at(1_000)));
        post_roll.stop(Some(at(100)));
        let stopped = post_roll.stopped();
        assert!(!post_roll.ended(stopped, 0, at(99)));
        assert!(post_roll.ended(stopped, 0, at(100)));
        assert!(!post_roll.ended(stopped, 1, at(149)));
        assert!(post_roll.ended(stopped, 1, at(150)));
        // A stream it doesn't know has none.
        assert!(post_roll.ended(stopped, 7, at(100)));
        assert_eq!(post_roll.longest(), Duration::from_nanos(50));
        post_roll.stop(None);
        assert!(!post_roll.ended(post_roll.stopped(), 0, at(1_000)));
    }
}
