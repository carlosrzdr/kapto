// SPDX-License-Identifier: Apache-2.0

//! The live tap: messages handed from the writer thread to the tap thread,
//! which decodes them and sends them to the viewer at the display's rate.
//!
//! Everything here is lossy by design, so the viewer can never slow down
//! recording. The writer thread offers each stream's newest message once per
//! batch into a preallocated slot; with no slot free, the message is skipped
//! and counted. The tap thread decodes every message offered and folds it
//! into its stream's [`Decimator`] bucket, then, at the display's rate,
//! sends one summary per stream (the newest values with each value's
//! minimum and maximum since the last send) as a datagram that is dropped
//! and counted when the viewer's socket is full or nothing listens on it.
//! It also keeps each stream's newest decoded values ([`LatestValues`]) for
//! the capture process's status, and sends each camera's newest frame to the
//! bridge over a socket of its own ([`frames`]).

mod decimate;
pub mod frames;
mod latest;
mod socket;
pub mod wire;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kapto_core::RecvTime;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::batch::{Batch, StreamIndex};

pub use decimate::Decimator;
pub use latest::{Latest, LatestValues};
pub use socket::{MAX_PATH_BYTES, TapSocket};

/// How the tap's slots are sized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapConfig {
    /// Slots the writer can fill before the tap thread takes them. A few per
    /// stream absorb the tap thread's waits between sends.
    pub slots: usize,
    /// The longest message a slot holds; longer ones aren't tapped.
    pub max_message_len: usize,
    /// Streams `0..streams` can be tapped.
    pub streams: usize,
}

/// One tapped message.
#[derive(Debug)]
struct Slot {
    stream: StreamIndex,
    recv_time: RecvTime,
    len: usize,
    bytes: Box<[u8]>,
}

/// Creates a tap: the writer thread's end, the tap thread's end, and its
/// counters.
#[must_use]
pub fn tap(config: &TapConfig) -> (TapFeed, TapDrain, Arc<TapStats>) {
    let slots = config.slots.max(1);
    let (full_tx, full_rx) = RingBuffer::new(slots);
    let (mut free_tx, free_rx) = RingBuffer::new(slots);
    for _ in 0..slots {
        // Can't fail: the queue holds every slot there is.
        let _ = free_tx.push(Slot {
            stream: 0,
            recv_time: RecvTime::from_nanos(0),
            len: 0,
            bytes: vec![0; config.max_message_len].into_boxed_slice(),
        });
    }
    let stats = Arc::new(TapStats::default());
    let feed = TapFeed {
        full: full_tx,
        free: free_rx,
        newest: vec![usize::MAX; config.streams].into_boxed_slice(),
        stats: Arc::clone(&stats),
    };
    let drain = TapDrain {
        full: full_rx,
        free: free_tx,
    };
    (feed, drain, stats)
}

/// The writer thread's end.
#[derive(Debug)]
pub struct TapFeed {
    full: Producer<Slot>,
    free: Consumer<Slot>,
    /// Per stream, the position of its newest message in the current batch.
    newest: Box<[usize]>,
    stats: Arc<TapStats>,
}

impl TapFeed {
    /// Offers the newest message of each stream in `batch`. Never allocates
    /// or waits: with no free slot, the message is skipped and counted.
    pub fn offer(&mut self, batch: &Batch) {
        self.newest.fill(usize::MAX);
        for (position, (stream, _)) in batch.messages().enumerate() {
            if let Some(newest) = self.newest.get_mut(usize::from(stream)) {
                *newest = position;
            }
        }
        for (position, (stream, message)) in batch.messages().enumerate() {
            if self.newest.get(usize::from(stream)) != Some(&position) {
                continue;
            }
            self.stats.offered.fetch_add(1, Ordering::Relaxed);
            let Ok(mut slot) = self.free.pop() else {
                self.stats.skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            if message.len() > slot.bytes.len() {
                self.stats.skipped.fetch_add(1, Ordering::Relaxed);
                let _ = self.full.push(Slot { len: 0, ..slot });
                continue;
            }
            slot.bytes[..message.len()].copy_from_slice(message);
            slot.stream = stream;
            slot.recv_time = batch.recv_time();
            slot.len = message.len();
            // Can't fail: the queue holds every slot there is.
            let _ = self.full.push(slot);
        }
    }
}

/// The tap thread's end.
#[derive(Debug)]
pub struct TapDrain {
    full: Consumer<Slot>,
    free: Producer<Slot>,
}

impl TapDrain {
    /// Calls `each` with every message offered since the last call, oldest
    /// first (its stream, receive time and bytes), and frees the slots.
    pub fn each(&mut self, mut each: impl FnMut(StreamIndex, RecvTime, &[u8])) {
        while let Ok(slot) = self.full.pop() {
            if slot.len > 0 {
                each(slot.stream, slot.recv_time, &slot.bytes[..slot.len]);
            }
            // Can't fail: the queue holds every slot there is.
            let _ = self.free.push(slot);
        }
    }

    /// Whether the writer's end is gone and everything it offered was taken.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.full.is_abandoned() && self.full.is_empty()
    }
}

/// What happened to tapped messages, readable from any thread.
#[derive(Debug, Default)]
pub struct TapStats {
    offered: AtomicU64,
    skipped: AtomicU64,
    sent: AtomicU64,
    viewer_slow: AtomicU64,
    no_viewer: AtomicU64,
}

impl TapStats {
    /// Messages the writer offered: each stream's newest per batch.
    #[must_use]
    pub fn offered(&self) -> u64 {
        self.offered.load(Ordering::Relaxed)
    }

    /// Offered messages skipped because the tap thread hadn't freed a slot,
    /// or that were longer than a slot.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::Relaxed)
    }

    /// Samples sent to the viewer.
    #[must_use]
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    /// Samples dropped because the viewer wasn't reading fast enough.
    #[must_use]
    pub fn viewer_slow(&self) -> u64 {
        self.viewer_slow.load(Ordering::Relaxed)
    }

    /// Samples dropped because no viewer was listening.
    #[must_use]
    pub fn no_viewer(&self) -> u64 {
        self.no_viewer.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(messages: &[(StreamIndex, &[u8])]) -> Batch {
        let mut batch = Batch::new(8, 8);
        for (stream, message) in messages {
            assert!(batch.push(*stream, message));
        }
        batch.set_recv_time(RecvTime::from_nanos(42));
        batch
    }

    fn drained(drain: &mut TapDrain) -> Vec<(StreamIndex, u64, Vec<u8>)> {
        let mut seen = Vec::new();
        drain.each(|stream, time, bytes| seen.push((stream, time.as_nanos(), bytes.to_vec())));
        seen
    }

    #[test]
    fn the_newest_message_of_each_stream_per_batch_reaches_the_tap_thread_in_order() {
        let config = TapConfig {
            slots: 8,
            max_message_len: 8,
            streams: 3,
        };
        let (mut feed, mut drain, stats) = tap(&config);
        feed.offer(&batch(&[(0, b"a1"), (2, b"c1"), (0, b"a2")]));
        feed.offer(&batch(&[(2, b"c2")]));
        assert_eq!(
            drained(&mut drain),
            [
                (2, 42, b"c1".to_vec()),
                (0, 42, b"a2".to_vec()),
                (2, 42, b"c2".to_vec())
            ]
        );
        assert_eq!((stats.offered(), stats.skipped()), (3, 0));
        // Nothing new: nothing again.
        assert_eq!(drained(&mut drain), Vec::new());
    }

    #[test]
    fn with_every_slot_taken_messages_are_skipped_and_counted() {
        let config = TapConfig {
            slots: 2,
            max_message_len: 8,
            streams: 4,
        };
        let (mut feed, mut drain, stats) = tap(&config);
        feed.offer(&batch(&[(0, b"a"), (1, b"b"), (2, b"c"), (3, b"d")]));
        assert_eq!((stats.offered(), stats.skipped()), (4, 2));
        assert_eq!(drained(&mut drain).len(), 2);
        // The slots are free again.
        feed.offer(&batch(&[(3, b"e")]));
        assert_eq!(drained(&mut drain), [(3, 42, b"e".to_vec())]);
    }

    #[test]
    fn messages_longer_than_a_slot_are_skipped_and_counted() {
        let config = TapConfig {
            slots: 2,
            max_message_len: 2,
            streams: 1,
        };
        let (mut feed, mut drain, stats) = tap(&config);
        feed.offer(&batch(&[(0, b"long")]));
        assert_eq!(stats.skipped(), 1);
        assert_eq!(drained(&mut drain), Vec::new());
        feed.offer(&batch(&[(0, b"ok")]));
        assert_eq!(drained(&mut drain), [(0, 42, b"ok".to_vec())]);
    }

    #[test]
    fn the_drain_is_finished_once_the_feed_is_gone_and_emptied() {
        let config = TapConfig {
            slots: 2,
            max_message_len: 2,
            streams: 1,
        };
        let (mut feed, mut drain, _) = tap(&config);
        feed.offer(&batch(&[(0, b"x")]));
        drop(feed);
        assert!(!drain.is_finished());
        drained(&mut drain);
        assert!(drain.is_finished());
    }
}
