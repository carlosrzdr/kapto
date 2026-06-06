// SPDX-License-Identifier: Apache-2.0

//! The ring between one receiver thread and the writer thread.
//!
//! A fixed set of [`Batch`]es circulates between two lock-free
//! single-producer, single-consumer queues: the receiver ([`Filler`]) fills a
//! batch and sends it forward; the writer ([`Drainer`]) takes it, writes it,
//! and the batch goes back automatically when the writer is done with it. No
//! step allocates once the ring exists.
//!
//! If the writer falls so far behind that every batch is waiting for it, the
//! receiver keeps reading but discards the batch it was filling, and counts
//! each discarded message against its stream. Capture never waits for the
//! writer, and no loss goes uncounted.

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::batch::{Batch, StreamIndex};

/// The shape of a ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingConfig {
    /// Batches in circulation.
    pub batches: usize,
    /// Messages per batch.
    pub batch_capacity: usize,
    /// Largest message, in bytes.
    pub max_message_len: usize,
    /// Number of streams, for counting drops per stream.
    pub streams: usize,
}

impl RingConfig {
    /// A ring that can absorb a writer stall of `max_stall` while messages
    /// arrive at `messages_per_second`, plus the two batches that are always
    /// in use (one being filled, one being written).
    ///
    /// It counts the batches the receiver hands over in that time, not only
    /// the messages: the receiver sends a batch once it is full or once its
    /// first message has waited `max_batch_latency`, so a stream slower than
    /// that sends nearly every message in a batch of its own. Sized for
    /// messages alone, a 200 Hz stream's ring would hold 45 ms of a 2 s
    /// stall. A batch ends when the first of those happens, so batches are
    /// handed over at most once per `max_batch_latency`, or once per
    /// `batch_capacity` messages if those come quicker, and never more
    /// often than messages arrive.
    #[must_use]
    pub fn for_rate(
        messages_per_second: u64,
        max_stall: Duration,
        max_batch_latency: Duration,
        batch_capacity: usize,
        max_message_len: usize,
        streams: usize,
    ) -> Self {
        let capacity = batch_capacity.max(1) as u128;
        let rate = u128::from(messages_per_second);
        let lingering = 1_000_000_000 / max_batch_latency.as_nanos().max(1);
        let handed_per_second = rate.min(lingering.max(rate.div_ceil(capacity)));
        let in_stall = (handed_per_second * max_stall.as_nanos()).div_ceil(1_000_000_000);
        let batches = usize::try_from(in_stall).unwrap_or(usize::MAX);
        Self {
            batches: batches.saturating_add(2),
            batch_capacity: batch_capacity.max(1),
            max_message_len,
            streams,
        }
    }

    /// Memory the ring's message slots take, in bytes.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.batches * self.batch_capacity * self.max_message_len
    }
}

/// Counters a ring keeps, readable from any thread.
#[derive(Debug)]
pub struct RingStats {
    batches: usize,
    dropped_batches: AtomicU64,
    dropped_messages: Box<[AtomicU64]>,
    max_queued: AtomicUsize,
}

impl RingStats {
    /// Batches in circulation.
    #[must_use]
    pub fn batches(&self) -> usize {
        self.batches
    }

    /// Batches discarded because the writer had none free.
    #[must_use]
    pub fn dropped_batches(&self) -> u64 {
        self.dropped_batches.load(Ordering::Relaxed)
    }

    /// Messages of `stream` discarded because the writer was behind.
    #[must_use]
    pub fn dropped_messages(&self, stream: StreamIndex) -> u64 {
        self.dropped_messages
            .get(usize::from(stream))
            .map_or(0, |count| count.load(Ordering::Relaxed))
    }

    /// The most batches ever waiting for the writer at once. Compare with
    /// [`RingStats::batches`] to see how close the writer came to falling
    /// behind.
    #[must_use]
    pub fn max_queued(&self) -> usize {
        self.max_queued.load(Ordering::Relaxed)
    }
}

impl RingStats {
    /// The ring's entry in `episode.json`, for a ring of `config`.
    #[must_use]
    pub fn summary(&self, config: &RingConfig) -> kapto_store::BufferSummary {
        kapto_store::BufferSummary {
            batches: self.batches as u64,
            peak_batches: self.max_queued() as u64,
            memory_bytes: config.memory_bytes() as u64,
        }
    }
}

/// Creates a ring: the receiver's end, the writer's end, and its counters.
#[must_use]
pub fn ring(config: &RingConfig) -> (Filler, Drainer, Arc<RingStats>) {
    let batches = config.batches.max(2);
    let (full_tx, full_rx) = RingBuffer::new(batches);
    let (mut free_tx, free_rx) = RingBuffer::new(batches);
    for _ in 1..batches {
        // Can't fail: the queue holds every batch there is.
        let _ = free_tx.push(Batch::new(config.batch_capacity, config.max_message_len));
    }
    let stats = Arc::new(RingStats {
        batches,
        dropped_batches: AtomicU64::new(0),
        dropped_messages: (0..config.streams).map(|_| AtomicU64::new(0)).collect(),
        max_queued: AtomicUsize::new(0),
    });
    let filler = Filler {
        current: Batch::new(config.batch_capacity, config.max_message_len),
        full: full_tx,
        free: free_rx,
        stats: Arc::clone(&stats),
        queue_capacity: batches,
    };
    let drainer = Drainer {
        full: full_rx,
        free: free_tx,
    };
    (filler, drainer, stats)
}

/// What happened to a batch handed to [`Filler::send`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// It was empty, so nothing was sent.
    Empty,
    /// It went to the writer.
    Handed,
    /// Every batch was waiting for the writer, so its messages were
    /// discarded and counted.
    Dropped {
        /// How many messages were discarded.
        messages: usize,
    },
}

/// The receiver's end: fills batches and sends them to the writer.
#[derive(Debug)]
pub struct Filler {
    current: Batch,
    full: Producer<Batch>,
    free: Consumer<Batch>,
    stats: Arc<RingStats>,
    queue_capacity: usize,
}

impl Filler {
    /// The batch being filled.
    pub fn batch(&mut self) -> &mut Batch {
        &mut self.current
    }

    /// Sends the batch being filled to the writer and starts an empty one.
    /// If the writer holds every other batch, the messages are discarded and
    /// counted instead, and the batch is reused. Never blocks.
    pub fn send(&mut self) -> Sent {
        if self.current.is_empty() {
            return Sent::Empty;
        }
        let queued = self.queue_capacity - self.full.slots() + 1;
        self.stats.max_queued.fetch_max(queued, Ordering::Relaxed);

        let Ok(empty) = self.free.pop() else {
            return Sent::Dropped {
                messages: self.discard_current(),
            };
        };
        let filled = std::mem::replace(&mut self.current, empty);
        match self.full.push(filled) {
            Ok(()) => Sent::Handed,
            // Can't happen: the queue holds every batch there is. If it did,
            // the batch's messages are counted as lost and the batch freed.
            Err(rtrb::PushError::Full(mut lost)) => Sent::Dropped {
                messages: count_dropped(&self.stats, &mut lost),
            },
        }
    }

    fn discard_current(&mut self) -> usize {
        count_dropped(&self.stats, &mut self.current)
    }
}

/// Counts a batch's messages as dropped, per stream, and clears it.
fn count_dropped(stats: &RingStats, batch: &mut Batch) -> usize {
    for (stream, _) in batch.messages() {
        if let Some(count) = stats.dropped_messages.get(usize::from(stream)) {
            count.fetch_add(1, Ordering::Relaxed);
        }
    }
    stats.dropped_batches.fetch_add(1, Ordering::Relaxed);
    let dropped = batch.len();
    batch.clear();
    dropped
}

/// The writer's end: takes filled batches in the order they were sent.
#[derive(Debug)]
pub struct Drainer {
    full: Consumer<Batch>,
    free: Producer<Batch>,
}

impl Drainer {
    /// The next filled batch, if one is waiting. It goes back to the
    /// receiver, emptied, when the returned value is dropped.
    pub fn take(&mut self) -> Option<Received<'_>> {
        let batch = self.full.pop().ok()?;
        Some(Received {
            batch,
            free: &mut self.free,
        })
    }

    /// Whether the receiver's end is gone and every batch it sent has been
    /// taken: nothing more will arrive.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.full.is_abandoned() && self.full.is_empty()
    }
}

/// A filled batch held by the writer. Dropping it returns the batch to the
/// receiver.
#[derive(Debug)]
pub struct Received<'a> {
    batch: Batch,
    free: &'a mut Producer<Batch>,
}

impl Deref for Received<'_> {
    type Target = Batch;

    fn deref(&self) -> &Batch {
        &self.batch
    }
}

impl Drop for Received<'_> {
    fn drop(&mut self) {
        // A zero-sized batch allocates nothing, so this swap is free.
        let mut batch = std::mem::replace(&mut self.batch, Batch::new(0, 0));
        batch.clear();
        // Can't fail: the queue holds every batch there is.
        let _ = self.free.push(batch);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn small_ring(batches: usize, capacity: usize) -> (Filler, Drainer, Arc<RingStats>) {
        ring(&RingConfig {
            batches,
            batch_capacity: capacity,
            max_message_len: 16,
            streams: 3,
        })
    }

    #[test]
    fn sizing_covers_the_stall_in_batches_handed_over_plus_the_two_in_use() {
        let sized = |rate| {
            RingConfig::for_rate(
                rate,
                Duration::from_secs(2),
                Duration::from_millis(2),
                64,
                128,
                16,
            )
        };
        // 16 streams at 2 kHz: batches fill in 2 ms, 500 a second.
        let config = sized(32_000);
        assert_eq!(config.batches, 500 * 2 + 2);
        assert_eq!(config.memory_bytes(), 1_002 * 64 * 128);
        // Faster still: they fill before they wait out.
        assert_eq!(sized(64_000).batches, 1_000 * 2 + 2);
        // 200 Hz, slower than a batch waits: every message alone.
        assert_eq!(sized(200).batches, 200 * 2 + 2);
        assert_eq!(sized(0).batches, 2);
    }

    #[test]
    fn a_slow_stream_sending_every_message_alone_survives_the_whole_stall() {
        // 2 s of a 200 Hz stream, each message a batch of its own, while the
        // writer takes nothing: what a busy disk does to a slow robot.
        let config = RingConfig::for_rate(
            200,
            Duration::from_secs(2),
            Duration::from_millis(2),
            64,
            16,
            3,
        );
        let (mut filler, mut drainer, stats) = ring(&config);
        for message in 0..400_u32 {
            filler.batch().push(0, &message.to_le_bytes());
            assert_eq!(filler.send(), Sent::Handed, "message {message}");
        }
        assert_eq!(stats.dropped_messages(0), 0);
        let mut written = 0;
        while let Some(batch) = drainer.take() {
            written += batch.len();
        }
        assert_eq!(written, 400);
    }

    #[test]
    fn sizing_rounds_up_partial_batches() {
        let config = RingConfig::for_rate(
            10,
            Duration::from_millis(1_500),
            Duration::from_millis(2),
            4,
            8,
            1,
        );
        // 10 batches a second, a message each, for 1.5 s, plus 2.
        assert_eq!(config.batches, 17);
    }

    #[test]
    fn batches_arrive_in_order_and_come_back_for_reuse() {
        let (mut filler, mut drainer, stats) = small_ring(3, 2);
        for round in 0_u8..10 {
            filler.batch().push(0, &[round]);
            assert_eq!(filler.send(), Sent::Handed);
            let received = drainer.take().unwrap();
            assert_eq!(received.messages().next(), Some((0, &[round][..])));
        }
        assert!(drainer.take().is_none());
        assert_eq!(stats.dropped_batches(), 0);
    }

    #[test]
    fn sending_an_empty_batch_does_nothing() {
        let (mut filler, mut drainer, _) = small_ring(3, 2);
        assert_eq!(filler.send(), Sent::Empty);
        assert!(drainer.take().is_none());
    }

    #[test]
    fn a_stalled_writer_makes_the_receiver_drop_and_count_per_stream() {
        let (mut filler, mut drainer, stats) = small_ring(3, 4);
        // Two batches fill the queue; the writer takes none.
        for _ in 0..2 {
            filler.batch().push(0, b"x");
            assert_eq!(filler.send(), Sent::Handed);
        }
        filler.batch().push(1, b"a");
        filler.batch().push(2, b"b");
        filler.batch().push(2, b"c");
        assert_eq!(filler.send(), Sent::Dropped { messages: 3 });
        assert_eq!(stats.dropped_messages(0), 0);
        assert_eq!(stats.dropped_messages(1), 1);
        assert_eq!(stats.dropped_messages(2), 2);
        assert_eq!(stats.dropped_batches(), 1);
        assert!(filler.batch().is_empty());

        // Once the writer catches up, batches flow again.
        drop(drainer.take());
        filler.batch().push(0, b"y");
        assert_eq!(filler.send(), Sent::Handed);
    }

    #[test]
    fn the_highest_queue_level_is_remembered() {
        let (mut filler, mut drainer, stats) = small_ring(4, 1);
        for _ in 0..3 {
            filler.batch().push(0, b"x");
            filler.send();
        }
        while drainer.take().is_some() {}
        assert_eq!(stats.max_queued(), 3);
        assert_eq!(stats.batches(), 4);
    }

    #[test]
    fn the_drainer_knows_when_the_receiver_is_gone() {
        let (mut filler, mut drainer, _) = small_ring(3, 1);
        filler.batch().push(0, b"x");
        filler.send();
        drop(filler);
        assert!(!drainer.is_finished());
        drop(drainer.take());
        assert!(drainer.is_finished());
    }

    /// One step of a single-threaded run.
    #[derive(Clone, Debug)]
    enum Step {
        Push(StreamIndex),
        Send,
        Drain,
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0_u16..3).prop_map(Step::Push),
            Just(Step::Send),
            Just(Step::Drain),
        ]
    }

    proptest! {
        #[test]
        fn every_message_is_delivered_in_order_or_counted_as_dropped(
            steps in prop::collection::vec(step(), 0..400),
            batches in 2_usize..6,
            capacity in 1_usize..5,
        ) {
            let (mut filler, mut drainer, stats) = small_ring(batches, capacity);
            let mut next_number = 0_u32;
            let mut pushed = 0_u64;
            let mut delivered = Vec::new();
            for step in steps {
                match step {
                    Step::Push(stream) => {
                        if filler.batch().push(stream, &next_number.to_le_bytes()) {
                            next_number += 1;
                            pushed += 1;
                        }
                    }
                    Step::Send => {
                        filler.send();
                    }
                    Step::Drain => {
                        if let Some(batch) = drainer.take() {
                            for (_, bytes) in batch.messages() {
                                delivered.push(u32::from_le_bytes(bytes.try_into().unwrap()));
                            }
                        }
                    }
                }
            }
            filler.send();
            while let Some(batch) = drainer.take() {
                for (_, bytes) in batch.messages() {
                    delivered.push(u32::from_le_bytes(bytes.try_into().unwrap()));
                }
            }
            let still_unsent = filler.batch().len() as u64;
            let dropped: u64 = (0..3).map(|stream| stats.dropped_messages(stream)).sum();
            prop_assert!(delivered.windows(2).all(|pair| pair[0] < pair[1]));
            prop_assert_eq!(delivered.len() as u64 + dropped + still_unsent, pushed);
        }
    }
}
