// SPDX-License-Identifier: Apache-2.0

//! Inputs from robots: the `Transport` trait and its UDP, TCP and ROS 2 implementations.
//!
//! A transport receives messages from the robot straight into the free slots
//! of a capture [`Batch`], tagging each with its stream. It never decodes or
//! changes a payload, and every message it receives but doesn't keep is
//! counted in [`TransportStats`].

pub mod backoff;
pub mod receiver;
pub mod ros2;
pub mod tcp;
pub mod udp;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kapto_capture::{Batch, StreamIndex};
use kapto_core::KaptoError;

pub use receiver::{Receiver, ReceiverConfig};
pub use tcp::{Framing, LengthOrder, TcpConfig, TcpEndpoint, TcpTransport};
pub use udp::{UdpOptions, UdpSocketConfig, UdpTransport};

/// One way of receiving messages from a robot, e.g. a set of UDP sockets.
///
/// A receiver thread owns one transport and calls [`Transport::receive`] in a
/// loop, sending each batch on to the writer.
pub trait Transport: Send {
    /// Waits up to `timeout` for messages, then receives as many as are
    /// waiting and fit into `batch`'s free slots, without waiting again.
    /// Returns how many were added; zero after the timeout, or at once if
    /// `batch` is full.
    ///
    /// If `batch` was empty, its receive time is set to when its first
    /// messages were read, so one batch shares one receive time.
    ///
    /// # Errors
    ///
    /// A failure that stops this transport from receiving at all, with its
    /// cause code. A single bad message is counted, never an error.
    fn receive(&mut self, batch: &mut Batch, timeout: Duration) -> Result<usize, KaptoError>;

    /// Counters of messages received but not kept, readable from any thread.
    fn stats(&self) -> &Arc<TransportStats>;
}

/// A boxed transport is a transport, so receivers of different kinds can be
/// handled alike.
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn receive(&mut self, batch: &mut Batch, timeout: Duration) -> Result<usize, KaptoError> {
        (**self).receive(batch, timeout)
    }

    fn stats(&self) -> &Arc<TransportStats> {
        (**self).stats()
    }
}

/// Messages a transport received but didn't put in a batch, per stream.
#[derive(Debug)]
pub struct TransportStats {
    rejected: Box<[AtomicU64]>,
    truncated: Box<[AtomicU64]>,
    disconnects: Box<[AtomicU64]>,
}

impl TransportStats {
    /// Counters for streams `0..streams`.
    #[must_use]
    pub fn new(streams: usize) -> Self {
        Self {
            rejected: (0..streams).map(|_| AtomicU64::new(0)).collect(),
            truncated: (0..streams).map(|_| AtomicU64::new(0)).collect(),
            disconnects: (0..streams).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Messages for `stream` discarded because they came from a source that
    /// isn't allowed to send to it.
    #[must_use]
    pub fn rejected(&self, stream: StreamIndex) -> u64 {
        load(&self.rejected, stream)
    }

    /// Messages for `stream` discarded because they were longer than a
    /// batch slot (for TCP: longer than the stream's largest message).
    #[must_use]
    pub fn truncated(&self, stream: StreamIndex) -> u64 {
        load(&self.truncated, stream)
    }

    /// Times `stream`'s connection was lost after it had been established.
    /// Messages the robot sent while it was down never arrived: a gap in the
    /// stream that sequence numbers, if the stream has them, also show.
    #[must_use]
    pub fn disconnects(&self, stream: StreamIndex) -> u64 {
        load(&self.disconnects, stream)
    }

    /// Sets every count back to zero, so they describe the next episode
    /// only. Call it while no receiver uses the transport.
    pub fn reset(&self) {
        for counter in self
            .rejected
            .iter()
            .chain(&self.truncated)
            .chain(&self.disconnects)
        {
            counter.store(0, Ordering::Relaxed);
        }
    }

    fn count_disconnect(&self, stream: StreamIndex) {
        add(&self.disconnects, stream);
    }

    fn count_rejected(&self, stream: StreamIndex) {
        add(&self.rejected, stream);
    }

    fn count_truncated(&self, stream: StreamIndex) {
        add(&self.truncated, stream);
    }

    fn count_truncated_by(&self, stream: StreamIndex, count: u64) {
        if let Some(counter) = self.truncated.get(usize::from(stream)) {
            counter.fetch_add(count, Ordering::Relaxed);
        }
    }
}

fn load(counters: &[AtomicU64], stream: StreamIndex) -> u64 {
    counters
        .get(usize::from(stream))
        .map_or(0, |count| count.load(Ordering::Relaxed))
}

fn add(counters: &[AtomicU64], stream: StreamIndex) {
    if let Some(count) = counters.get(usize::from(stream)) {
        count.fetch_add(1, Ordering::Relaxed);
    }
}
