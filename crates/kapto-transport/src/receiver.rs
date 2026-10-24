// SPDX-License-Identifier: Apache-2.0

//! The receiver: the loop a receiver thread runs, the same for every
//! transport.
//!
//! It fills one batch until the batch is full or its first message has
//! waited `max_batch_latency`, then counts the batch and sends it to the
//! writer. Filling batches, rather than sending after every read, keeps the
//! ring's batches full, so a ring sized for a writer stall of so many
//! seconds really absorbs that many seconds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kapto_capture::{Accounting, Filler, LossCounters, RingStats, StreamIndex};
use kapto_core::KaptoError;
use kapto_core::clock::monotonic_now;

use crate::Transport;

/// How a receiver batches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverConfig {
    /// The longest a message waits in a batch that isn't full before the
    /// batch is sent. Receive times are as precise as this.
    pub max_batch_latency: Duration,
    /// How long a read waits when nothing arrives, which is also how quickly
    /// the receiver notices it has been asked to stop.
    pub idle_wait: Duration,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            max_batch_latency: Duration::from_millis(2),
            idle_wait: Duration::from_millis(50),
        }
    }
}

/// One transport feeding one ring.
#[derive(Debug)]
pub struct Receiver<T> {
    transport: T,
    filler: Filler,
    ring: Arc<RingStats>,
    accounting: Accounting,
    counters: Arc<LossCounters>,
    config: ReceiverConfig,
}

impl<T: Transport> Receiver<T> {
    /// A receiver reading `transport` into the ring behind `filler`, counting
    /// into `counters` through `accounting`.
    #[must_use]
    pub fn new(
        transport: T,
        (filler, ring): (Filler, Arc<RingStats>),
        accounting: Accounting,
        counters: Arc<LossCounters>,
        config: ReceiverConfig,
    ) -> Self {
        Self {
            transport,
            filler,
            ring,
            accounting,
            counters,
            config,
        }
    }

    /// Receives, counts and sends batches until `stop` is set, then sends
    /// what it holds. Never waits for the writer.
    ///
    /// # Errors
    ///
    /// The transport's failure, after sending what was already received.
    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), KaptoError> {
        let mut deadline = None;
        loop {
            let stopping = stop.load(Ordering::Relaxed);
            let wait = match deadline {
                Some(deadline) => deadline_wait(deadline),
                None if stopping => Duration::ZERO,
                None => self.config.idle_wait,
            };
            let received = self.transport.receive(self.filler.batch(), wait);
            let batch = self.filler.batch();
            if deadline.is_none() && !batch.is_empty() {
                deadline = Some(
                    batch
                        .recv_time()
                        .as_nanos()
                        .saturating_add(nanos(self.config.max_batch_latency)),
                );
            }
            let due = deadline.is_some_and(|deadline| monotonic_now().as_nanos() >= deadline);
            if batch.is_full() || due || stopping || received.is_err() {
                self.send();
                deadline = None;
            }
            received?;
            if stopping {
                return Ok(());
            }
        }
    }

    /// Ends the receiver: the writer sees the ring finish once it has taken
    /// every batch. Messages discarded by the ring (writer too far behind)
    /// or by the transport (too long for a slot) are added to the counters,
    /// and so is the loss of any silence still settling.
    /// Returns the transport, still open, for the next receiver.
    pub fn finish(mut self) -> T {
        self.accounting.settle();
        self.hand_over().0
    }

    /// Ends the receiver as [`Receiver::finish`] does, but without settling
    /// any silence: returns its loss detection with the transport, to go on
    /// in the next receiver, for when the episode starting holds what
    /// follows.
    pub fn hand_over(self) -> (T, Accounting) {
        for index in 0..self.counters.streams() {
            let Ok(stream) = StreamIndex::try_from(index) else {
                break;
            };
            self.counters
                .add_dropped_by_us(stream, self.ring.dropped_messages(stream));
            self.counters
                .add_discarded_on_arrival(stream, self.transport.stats().truncated(stream));
        }
        (self.transport, self.accounting)
    }

    fn send(&mut self) {
        if !self.filler.batch().is_empty() {
            let _span = kapto_core::span!("hand-off", messages = self.filler.batch().len());
            self.accounting.count(self.filler.batch());
            self.filler.send();
        }
    }
}

fn deadline_wait(deadline_ns: u64) -> Duration {
    Duration::from_nanos(deadline_ns.saturating_sub(monotonic_now().as_nanos()))
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}
