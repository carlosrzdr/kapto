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
    overflowed: Box<[AtomicU64]>,
    disconnects: Box<[AtomicU64]>,
    publishers_replaced: Box<[AtomicU64]>,
}

impl TransportStats {
    /// Counters for streams `0..streams`.
    #[must_use]
    pub fn new(streams: usize) -> Self {
        Self {
            rejected: (0..streams).map(|_| AtomicU64::new(0)).collect(),
            truncated: (0..streams).map(|_| AtomicU64::new(0)).collect(),
            overflowed: (0..streams).map(|_| AtomicU64::new(0)).collect(),
            disconnects: (0..streams).map(|_| AtomicU64::new(0)).collect(),
            publishers_replaced: (0..streams).map(|_| AtomicU64::new(0)).collect(),
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

    /// Messages for `stream` the operating system dropped before Kapto read
    /// them, its receive buffer full: Kapto's loss, as a message cut to fit
    /// is. Counted for UDP on Linux, which says how many it dropped.
    #[must_use]
    pub fn overflowed(&self, stream: StreamIndex) -> u64 {
        load(&self.overflowed, stream)
    }

    /// Times `stream`'s connection was lost after it had been established.
    /// Messages the robot sent while it was down never arrived: a gap in the
    /// stream that sequence numbers, if the stream has them, also show.
    #[must_use]
    pub fn disconnects(&self, stream: StreamIndex) -> u64 {
        load(&self.disconnects, stream)
    }

    /// Times a new publisher of `stream` took the place of another, every
    /// place to tell publishers apart by their counts being taken (ROS 2):
    /// the one heard from least recently gives it up, most likely gone
    /// after a restart. Its count is then followed for the new one.
    #[must_use]
    pub fn publishers_replaced(&self, stream: StreamIndex) -> u64 {
        load(&self.publishers_replaced, stream)
    }

    /// Sets every count back to zero, so they describe the next episode
    /// only. Call it while no receiver uses the transport.
    pub fn reset(&self) {
        for counter in self
            .rejected
            .iter()
            .chain(&self.truncated)
            .chain(&self.overflowed)
            .chain(&self.disconnects)
            .chain(&self.publishers_replaced)
        {
            counter.store(0, Ordering::Relaxed);
        }
    }

    fn count_disconnect(&self, stream: StreamIndex) {
        add(&self.disconnects, stream);
    }

    #[cfg_attr(
        not(feature = "ros2"),
        allow(dead_code, reason = "only ROS 2 tells publishers apart")
    )]
    fn count_publisher_replaced(&self, stream: StreamIndex) {
        add(&self.publishers_replaced, stream);
    }

    fn count_rejected(&self, stream: StreamIndex) {
        add(&self.rejected, stream);
    }

    fn count_truncated(&self, stream: StreamIndex) {
        add(&self.truncated, stream);
    }

    fn count_overflowed_by(&self, stream: StreamIndex, count: u64) {
        if let Some(counter) = self.overflowed.get(usize::from(stream)) {
            counter.fetch_add(count, Ordering::Relaxed);
        }
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

/// The places of `count` sources in the order to serve them, starting at
/// `first` and going round: a transport starting one further along each
/// read gives each source its turn to be served first, so under load the
/// same ones aren't always left until the batch is full.
pub(crate) fn in_turn(count: usize, first: usize) -> impl Iterator<Item = usize> {
    (0..count).map(move |offset| (first + offset) % count.max(1))
}

#[cfg(test)]
mod in_turn_tests {
    use super::in_turn;

    #[test]
    fn each_source_is_served_first_in_turn_and_every_one_once() {
        assert_eq!(in_turn(3, 0).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(in_turn(3, 1).collect::<Vec<_>>(), [1, 2, 0]);
        assert_eq!(in_turn(3, 2).collect::<Vec<_>>(), [2, 0, 1]);
        assert_eq!(in_turn(0, 0).count(), 0);
    }
}
