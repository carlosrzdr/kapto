// SPDX-License-Identifier: Apache-2.0

//! The receiver loop: batches are filled before they are sent, every message
//! is counted, and stopping sends what is held.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::VecDeque;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use kapto_capture::loss::{ByteOrder, SequenceField, SequenceWidth};
use kapto_capture::{Accounting, Batch, Drainer, LossCounters, LossDetection, RingConfig, ring};
use kapto_core::clock::monotonic_now;
use kapto_core::{ErrorCode, KaptoError, RecvTime};
use kapto_transport::{
    Receiver, ReceiverConfig, Transport, TransportStats, UdpOptions, UdpSocketConfig, UdpTransport,
};

const SEQUENCE: LossDetection = LossDetection::Sequence(SequenceField {
    offset: 0,
    width: SequenceWidth::U32,
    byte_order: ByteOrder::Little,
});

/// A transport that hands out queued messages, `per_read` at a time, then
/// optionally fails.
struct Scripted {
    queue: VecDeque<u32>,
    per_read: usize,
    fail_when_empty: bool,
    stats: Arc<TransportStats>,
}

impl Scripted {
    fn new(messages: u32, per_read: usize) -> Self {
        Self {
            queue: (0..messages).collect(),
            per_read,
            fail_when_empty: false,
            stats: Arc::new(TransportStats::new(1)),
        }
    }
}

impl Transport for Scripted {
    fn receive(&mut self, batch: &mut Batch, timeout: Duration) -> Result<usize, KaptoError> {
        if self.queue.is_empty() {
            if self.fail_when_empty {
                return Err(KaptoError::new(ErrorCode::TransportUdpReceiveFailed));
            }
            thread::sleep(timeout.min(Duration::from_millis(1)));
            return Ok(0);
        }
        let was_empty = batch.is_empty();
        let mut added = 0;
        while added < self.per_read && !batch.is_full() {
            let Some(number) = self.queue.pop_front() else {
                break;
            };
            batch.push(0, &number.to_le_bytes());
            added += 1;
        }
        if was_empty && added > 0 {
            batch.set_recv_time(monotonic_now());
        }
        Ok(added)
    }

    fn stats(&self) -> &Arc<TransportStats> {
        &self.stats
    }
}

fn ring_config(batches: usize, capacity: usize) -> RingConfig {
    RingConfig {
        batches,
        batch_capacity: capacity,
        max_message_len: 16,
        streams: 1,
    }
}

fn receiver<T: Transport>(
    transport: T,
    config: &RingConfig,
) -> (Receiver<T>, Drainer, Arc<LossCounters>) {
    let (filler, drainer, stats) = ring(config);
    let counters = Arc::new(LossCounters::new(config.streams));
    let accounting = Accounting::new(Arc::clone(&counters), &[(0, SEQUENCE)]);
    let receiver = Receiver::new(
        transport,
        (filler, stats),
        accounting,
        Arc::clone(&counters),
        ReceiverConfig::default(),
    );
    (receiver, drainer, counters)
}

/// Every batch's size, in order, until the receiver's end is gone.
fn drain(drainer: &mut Drainer) -> Vec<usize> {
    let mut sizes = Vec::new();
    while !drainer.is_finished() {
        match drainer.take() {
            Some(batch) => sizes.push(batch.len()),
            None => thread::yield_now(),
        }
    }
    sizes
}

#[test]
fn batches_are_filled_before_they_are_sent_and_every_message_is_counted() {
    let (mut receiver, mut drainer, counters) =
        receiver(Scripted::new(100, 3), &ring_config(64, 8));
    let stop = AtomicBool::new(false);
    let sizes = thread::scope(|scope| {
        let taken = scope.spawn(|| drain(&mut drainer));
        let running = scope.spawn(|| receiver.run(&stop).map(|()| receiver));
        while counters.totals(0).received < 100 {
            thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
        drop(running.join().unwrap().unwrap().finish());
        taken.join().unwrap()
    });
    // 3 messages per read into batches of 8: sent when full, not per read.
    assert_eq!(sizes.iter().sum::<usize>(), 100);
    assert_eq!(
        sizes.iter().filter(|&&size| size == 8).count(),
        12,
        "{sizes:?}"
    );
    let totals = counters.totals(0);
    assert_eq!(
        (totals.received, totals.decoded, totals.lost_upstream),
        (100, 100, 0)
    );
}

#[test]
fn a_batch_that_is_not_full_is_still_sent_without_waiting_for_stop() {
    let (mut receiver, mut drainer, _) = receiver(Scripted::new(3, 3), &ring_config(4, 64));
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let running = scope.spawn(|| receiver.run(&stop));
        let first = loop {
            if let Some(batch) = drainer.take() {
                break batch.len();
            }
            thread::yield_now();
        };
        assert_eq!(first, 3);
        stop.store(true, Ordering::Relaxed);
        running.join().unwrap().unwrap();
    });
}

#[test]
fn a_failing_transport_stops_the_receiver_after_sending_what_it_holds() {
    let mut transport = Scripted::new(5, 5);
    transport.fail_when_empty = true;
    let (mut receiver, mut drainer, counters) = receiver(transport, &ring_config(4, 64));
    let error = receiver.run(&AtomicBool::new(false)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::TransportUdpReceiveFailed);
    drop(receiver.finish());
    assert_eq!(drain(&mut drainer), [5]);
    assert_eq!(counters.totals(0).received, 5);
}

#[test]
fn messages_the_ring_had_no_room_for_are_counted_as_dropped_when_finished() {
    // Two batches of 4, never drained: the third full batch has nowhere to go.
    let (mut receiver, _drainer, counters) = receiver(Scripted::new(12, 4), &ring_config(2, 4));
    while counters.totals(0).received < 12 {
        receiver.run(&AtomicBool::new(true)).unwrap();
    }
    drop(receiver.finish());
    let totals = counters.totals(0);
    assert_eq!(totals.received, 12);
    assert_eq!(totals.dropped_by_us, 8);
}

#[test]
fn loss_detection_handed_over_goes_on_in_the_next_receiver_and_finds_a_gap_across_them() {
    let config = ring_config(4, 64);
    let (mut idle, _drainer, before) = receiver(Scripted::new(5, 5), &config);
    while before.totals(0).received < 5 {
        idle.run(&AtomicBool::new(true)).unwrap();
    }
    let (_, mut accounting) = idle.hand_over();

    // 0 to 4 arrived before; 5 to 9 never do.
    let mut transport = Scripted::new(0, 5);
    transport.queue = (10..15).collect();
    let episode = Arc::new(LossCounters::new(1));
    // The episode began before the first receiver's messages, so the whole
    // gap is in it.
    accounting.resume(Arc::clone(&episode), RecvTime::from_nanos(0));
    let (filler, _drainer, stats) = ring(&config);
    let mut next = Receiver::new(
        transport,
        (filler, stats),
        accounting,
        Arc::clone(&episode),
        ReceiverConfig::default(),
    );
    while episode.totals(0).received < 5 {
        next.run(&AtomicBool::new(true)).unwrap();
    }
    drop(next.finish());
    assert_eq!(episode.totals(0).lost_upstream, 5);
    assert_eq!(before.totals(0).lost_upstream, 0);
}

#[test]
fn udp_datagrams_flow_through_the_receiver_in_order_with_nothing_lost() {
    let mut config = UdpSocketConfig::new(0, "127.0.0.1:0".parse().unwrap());
    config.allowed_sources = Vec::new();
    let transport = UdpTransport::open(&[config], UdpOptions::default()).unwrap();
    let to = transport.local_addrs().next().unwrap();
    let ring_config = RingConfig {
        max_message_len: 8,
        ..ring_config(64, 32)
    };
    let (mut receiver, mut drainer, counters) = receiver(transport, &ring_config);
    let stop = AtomicBool::new(false);
    let numbers = thread::scope(|scope| {
        let running = scope.spawn(|| receiver.run(&stop));
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut numbers = Vec::new();
        for number in 0_u32..1_000 {
            sender.send_to(&number.to_le_bytes(), to).unwrap();
            // Keep pace with the receiver so loopback never overflows.
            if number % 50 == 49 {
                while counters.totals(0).received < u64::from(number) + 1 {
                    while let Some(batch) = drainer.take() {
                        numbers.extend(
                            batch
                                .messages()
                                .map(|(_, bytes)| u32::from_le_bytes(bytes.try_into().unwrap())),
                        );
                    }
                    thread::yield_now();
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        running.join().unwrap().unwrap();
        while let Some(batch) = drainer.take() {
            numbers.extend(
                batch
                    .messages()
                    .map(|(_, bytes)| u32::from_le_bytes(bytes.try_into().unwrap())),
            );
        }
        numbers
    });
    assert_eq!(numbers, (0..1_000).collect::<Vec<_>>());
    let totals = counters.totals(0);
    assert_eq!((totals.received, totals.lost_upstream), (1_000, 0));
}
