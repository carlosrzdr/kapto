// SPDX-License-Identifier: Apache-2.0

//! Loss accounting: what happened to every message of every stream.
//!
//! Five counters per stream ([`StreamTotals`]) explain every message.
//! `received` is what reached Kapto, and `decoded` is the part whose live
//! fields could be read. Of what was received, `written` reached the
//! recording and `dropped_by_us` was discarded inside Kapto. `lost_upstream`
//! never reached Kapto at all. It is worked out from sequence-number gaps
//! when the source numbers its messages, or estimated from silences in a
//! stream with a declared rate, once the messages a silence only delayed
//! have had time to arrive.
//!
//! The receiver thread counts each batch with [`Accounting::count`], adding
//! to the shared [`LossCounters`] once per run of messages from one stream,
//! not once per message.

mod rate;
mod sequence;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kapto_core::{RecvTime, StreamId};
use kapto_store::{LossDetectionKind, StreamSummary};

use crate::batch::{Batch, StreamIndex};

pub use rate::{RateGap, RateGapDetector, Sent};
pub use sequence::{ByteOrder, SequenceField, SequenceTracker, SequenceWidth};

/// How a stream's upstream loss can be detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LossDetection {
    /// From the sequence number in each message.
    Sequence(SequenceField),
    /// From silences longer than the declared rate allows.
    RateGap(RateGap),
    /// Not at all: the stream has neither sequence numbers nor a rate.
    /// `lost_upstream` stays zero and says nothing.
    Undetectable,
}

/// One stream's counters at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamTotals {
    /// Messages that reached Kapto.
    pub received: u64,
    /// Received messages whose live fields (such as the sequence number)
    /// could be read. A message too short for them is still recorded.
    pub decoded: u64,
    /// Received messages stored in the recording.
    pub written: u64,
    /// Received messages Kapto discarded, e.g. because the writer fell behind.
    pub dropped_by_us: u64,
    /// Messages the source sent that never reached Kapto.
    pub lost_upstream: u64,
}

impl StreamTotals {
    /// This stream's entry in `episode.json`.
    #[must_use]
    pub fn summary(self, id: &StreamId, detection: &LossDetection) -> StreamSummary {
        StreamSummary {
            id: id.to_string(),
            loss_detection: match detection {
                LossDetection::Sequence(_) => LossDetectionKind::Sequence,
                LossDetection::RateGap(_) => LossDetectionKind::RateGap,
                LossDetection::Undetectable => LossDetectionKind::Undetectable,
            },
            received: self.received,
            decoded: self.decoded,
            written: self.written,
            dropped_by_us: self.dropped_by_us,
            lost_upstream: self.lost_upstream,
            disconnects: None,
            carried: None,
        }
    }
}

#[derive(Debug, Default)]
struct StreamCounters {
    received: AtomicU64,
    decoded: AtomicU64,
    written: AtomicU64,
    dropped_by_us: AtomicU64,
    lost_upstream: AtomicU64,
}

/// Every stream's counters, shared by the threads that update them and
/// readable from any thread.
#[derive(Debug)]
pub struct LossCounters {
    streams: Box<[StreamCounters]>,
}

impl LossCounters {
    /// Counters for streams `0..streams`, all zero.
    #[must_use]
    pub fn new(streams: usize) -> Self {
        Self {
            streams: (0..streams).map(|_| StreamCounters::default()).collect(),
        }
    }

    /// The counters of `stream` now, or all zero for an unknown stream.
    #[must_use]
    pub fn totals(&self, stream: StreamIndex) -> StreamTotals {
        self.streams
            .get(usize::from(stream))
            .map_or_else(StreamTotals::default, |counters| StreamTotals {
                received: counters.received.load(Ordering::Relaxed),
                decoded: counters.decoded.load(Ordering::Relaxed),
                written: counters.written.load(Ordering::Relaxed),
                dropped_by_us: counters.dropped_by_us.load(Ordering::Relaxed),
                lost_upstream: counters.lost_upstream.load(Ordering::Relaxed),
            })
    }

    /// Counts `messages` of `stream` as stored in the recording.
    pub fn add_written(&self, stream: StreamIndex, messages: u64) {
        if let Some(counters) = self.streams.get(usize::from(stream)) {
            counters.written.fetch_add(messages, Ordering::Relaxed);
        }
    }

    /// How many streams there are counters for.
    #[must_use]
    pub fn streams(&self) -> usize {
        self.streams.len()
    }

    /// Counts `messages` of `stream` that reached Kapto but were discarded
    /// before entering a batch (e.g. longer than a slot), as both received
    /// and dropped, so every received message is still written or dropped.
    pub fn add_discarded_on_arrival(&self, stream: StreamIndex, messages: u64) {
        if let Some(counters) = self.streams.get(usize::from(stream)) {
            counters.received.fetch_add(messages, Ordering::Relaxed);
            counters
                .dropped_by_us
                .fetch_add(messages, Ordering::Relaxed);
        }
    }

    /// Counts `messages` of `stream` as discarded inside Kapto.
    pub fn add_dropped_by_us(&self, stream: StreamIndex, messages: u64) {
        if let Some(counters) = self.streams.get(usize::from(stream)) {
            counters
                .dropped_by_us
                .fetch_add(messages, Ordering::Relaxed);
        }
    }
}

/// The receiver thread's side of loss accounting: looks at each message's
/// sequence number or arrival time and keeps the shared counters current.
#[derive(Debug)]
pub struct Accounting {
    counters: Arc<LossCounters>,
    detectors: Box<[Detector]>,
}

#[derive(Clone, Debug)]
enum Detector {
    Sequence(SequenceField, SequenceTracker),
    /// With its messages in the batch being counted, so the detector sees
    /// each batch once, however many runs the stream has in it.
    RateGap(RateGapDetector, InBatch),
    Undetectable,
}

/// One rate-gap stream's messages in the batch being counted.
#[derive(Clone, Copy, Debug, Default)]
struct InBatch {
    count: u64,
    /// When they were sent, until one of them doesn't say.
    sent: Option<Sent>,
    unstamped: bool,
}

impl InBatch {
    fn add(&mut self, sent: Option<u64>) {
        self.count += 1;
        match (sent, &mut self.sent) {
            (None, _) => self.unstamped = true,
            (Some(time), Some(sent)) => {
                sent.first_ns = sent.first_ns.min(time);
                sent.last_ns = sent.last_ns.max(time);
            }
            (Some(time), None) => {
                self.sent = Some(Sent {
                    first_ns: time,
                    last_ns: time,
                });
            }
        }
    }

    /// When they were sent, if every one of them says.
    fn sent(&self) -> Option<Sent> {
        if self.unstamped { None } else { self.sent }
    }
}

/// What one run of messages from one stream adds to its counters.
#[derive(Clone, Copy, Debug)]
struct Run {
    stream: StreamIndex,
    received: u64,
    decoded: u64,
    lost: i64,
}

impl Accounting {
    /// Accounting for the streams this receiver reads, each with its way of
    /// detecting loss. Messages of other streams are counted as received
    /// and decoded, with no loss detection.
    #[must_use]
    pub fn new(counters: Arc<LossCounters>, streams: &[(StreamIndex, LossDetection)]) -> Self {
        let mut detectors = vec![Detector::Undetectable; counters.streams.len()];
        for &(stream, detection) in streams {
            if let Some(detector) = detectors.get_mut(usize::from(stream)) {
                *detector = match detection {
                    LossDetection::Sequence(field) => {
                        Detector::Sequence(field, SequenceTracker::new(field.width))
                    }
                    LossDetection::RateGap(gap) => {
                        Detector::RateGap(RateGapDetector::new(gap), InBatch::default())
                    }
                    LossDetection::Undetectable => Detector::Undetectable,
                };
            }
        }
        Self {
            counters,
            detectors: detectors.into_boxed_slice(),
        }
    }

    /// Counts every message of a batch just received. Never allocates.
    pub fn count(&mut self, batch: &Batch) {
        let mut run: Option<Run> = None;
        for (index, (stream, payload)) in batch.messages().enumerate() {
            let mut current = match run {
                Some(current) if current.stream == stream => current,
                _ => {
                    if let Some(finished) = run {
                        self.flush(finished);
                    }
                    Run {
                        stream,
                        received: 0,
                        decoded: 0,
                        lost: 0,
                    }
                }
            };
            current.received += 1;
            match self.detectors.get_mut(usize::from(stream)) {
                Some(Detector::Sequence(field, tracker)) => {
                    if let Some(number) = field.read(payload) {
                        current.decoded += 1;
                        current.lost += tracker.observe(number);
                    }
                }
                Some(Detector::RateGap(_, in_batch)) => {
                    current.decoded += 1;
                    in_batch.add(batch.source_time(index));
                }
                _ => current.decoded += 1,
            }
            run = Some(current);
        }
        if let Some(finished) = run {
            self.flush(finished);
        }
        self.observe_rate_gaps(batch.recv_time());
    }

    /// Tells each rate-gap stream that appeared in the batch how many of its
    /// messages it held, and counts the loss estimated.
    fn observe_rate_gaps(&mut self, time: RecvTime) {
        for (counters, detector) in self.counters.streams.iter().zip(self.detectors.iter_mut()) {
            if let Detector::RateGap(detector, in_batch) = detector
                && in_batch.count > 0
            {
                let lost = detector.observe_sent(time, in_batch.count, in_batch.sent());
                *in_batch = InBatch::default();
                counters.lost_upstream.fetch_add(lost, Ordering::Relaxed);
            }
        }
    }

    /// Counts the loss of every silence still settling, as of each stream's
    /// last message: for when no more messages will come, as at the end of
    /// an episode.
    pub fn settle(&mut self) {
        for (counters, detector) in self.counters.streams.iter().zip(self.detectors.iter_mut()) {
            if let Detector::RateGap(detector, _) = detector {
                let lost = detector.settle();
                counters.lost_upstream.fetch_add(lost, Ordering::Relaxed);
            }
        }
    }

    fn flush(&mut self, run: Run) {
        let index = usize::from(run.stream);
        let Some(counters) = self.counters.streams.get(index) else {
            return;
        };
        counters.received.fetch_add(run.received, Ordering::Relaxed);
        counters.decoded.fetch_add(run.decoded, Ordering::Relaxed);
        // A late message takes back loss counted earlier, so the change can
        // be negative; adding it as two's complement wraps to the right total.
        counters
            .lost_upstream
            .fetch_add(run.lost.cast_unsigned(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEQUENCE: LossDetection = LossDetection::Sequence(SequenceField {
        offset: 0,
        width: SequenceWidth::U32,
        byte_order: ByteOrder::Little,
    });

    fn batch_of(messages: &[(StreamIndex, u32)], time_ms: u64) -> Batch {
        let mut batch = Batch::new(messages.len(), 8);
        for &(stream, number) in messages {
            batch.push(stream, &number.to_le_bytes());
        }
        batch.set_recv_time(RecvTime::from_nanos(time_ms * 1_000_000));
        batch
    }

    #[test]
    fn each_stream_is_counted_with_its_own_detection() {
        let counters = Arc::new(LossCounters::new(3));
        let mut accounting = Accounting::new(
            Arc::clone(&counters),
            &[
                (0, SEQUENCE),
                (1, LossDetection::RateGap(RateGap::for_rate(1_000))),
            ],
        );
        accounting.count(&batch_of(&[(0, 7), (0, 8), (1, 0), (2, 0), (0, 11)], 100));
        accounting.count(&batch_of(&[(1, 0), (0, 10)], 120));
        assert_eq!(counters.totals(1).lost_upstream, 0, "still settling");
        accounting.settle();

        let numbered = counters.totals(0);
        assert_eq!((numbered.received, numbered.decoded), (4, 4));
        assert_eq!(numbered.lost_upstream, 1, "9 never came; 10 came late");
        assert_eq!(
            counters.totals(1).lost_upstream,
            19,
            "20 ms silent at 1 kHz"
        );
        assert_eq!(counters.totals(2).received, 1);
        assert_eq!(counters.totals(2).lost_upstream, 0);
    }

    #[test]
    fn a_rate_gap_stream_interleaved_with_others_in_a_batch_shows_no_loss() {
        // Two streams at 2 kHz read alternately, 4 of each per 2 ms batch:
        // each batch holds the rate-gap stream in four separate runs.
        let counters = Arc::new(LossCounters::new(2));
        let rate = LossDetection::RateGap(RateGap::for_rate(2_000));
        let mut accounting = Accounting::new(Arc::clone(&counters), &[(0, SEQUENCE), (1, rate)]);
        let mut number = 0_u32;
        for batch_index in 0..1_000_u64 {
            let mut batch = Batch::new(8, 8);
            for _ in 0..4 {
                batch.push(0, &number.to_le_bytes());
                batch.push(1, b"x");
                number += 1;
            }
            batch.set_recv_time(RecvTime::from_nanos(batch_index * 2_000_000));
            accounting.count(&batch);
        }
        assert_eq!(counters.totals(1).received, 4_000);
        assert_eq!(counters.totals(1).lost_upstream, 0);
    }

    /// Joint states at 2 kHz in batches of 4, sent on time, delivered 6 ms
    /// later from 100 ms on; `stamp` says whether a message carries its
    /// sending time. What the stream counts lost.
    fn delivered_late(stamp: impl Fn(u64) -> bool) -> u64 {
        let counters = Arc::new(LossCounters::new(1));
        let rate = LossDetection::RateGap(RateGap::for_rate(2_000));
        let mut accounting = Accounting::new(Arc::clone(&counters), &[(0, rate)]);
        for batch_index in 0..2_000_u64 {
            let mut batch = Batch::new(4, 8);
            for n in 0..4 {
                let message = batch_index * 4 + n;
                if stamp(message) {
                    batch.push_with_source_time(0, b"x", 1 + message * 500_000);
                } else {
                    batch.push(0, b"x");
                }
            }
            let delay = if batch_index < 50 { 0 } else { 6_000_000 };
            batch.set_recv_time(RecvTime::from_nanos(batch_index * 2_000_000 + delay));
            accounting.count(&batch);
        }
        accounting.settle();
        counters.totals(0).lost_upstream
    }

    #[test]
    fn a_rate_gap_stream_whose_messages_say_when_they_were_sent_is_judged_by_it() {
        assert_eq!(delivered_late(|_| true), 0);
    }

    #[test]
    fn a_batch_with_a_message_not_saying_when_it_was_sent_is_judged_by_receive_time() {
        // One message in four stamped: the delay reads as missing messages,
        // as it does with none stamped.
        let none = delivered_late(|_| false);
        assert!(none > 0);
        assert_eq!(delivered_late(|message| message % 4 == 0), none);
    }

    #[test]
    fn a_message_too_short_for_its_sequence_number_is_received_but_not_decoded() {
        let counters = Arc::new(LossCounters::new(1));
        let mut accounting = Accounting::new(Arc::clone(&counters), &[(0, SEQUENCE)]);
        let mut batch = Batch::new(2, 8);
        batch.push(0, &[1, 0]);
        batch.push(0, &5_u32.to_le_bytes());
        accounting.count(&batch);
        let totals = counters.totals(0);
        assert_eq!(
            (totals.received, totals.decoded, totals.lost_upstream),
            (2, 1, 0)
        );
    }

    #[test]
    fn written_and_dropped_are_added_by_the_threads_that_know_them() {
        let counters = LossCounters::new(2);
        counters.add_written(1, 40);
        counters.add_dropped_by_us(1, 2);
        counters.add_written(9, 1);
        let totals = counters.totals(1);
        assert_eq!((totals.written, totals.dropped_by_us), (40, 2));
        assert_eq!(counters.totals(9), StreamTotals::default());
    }
}
