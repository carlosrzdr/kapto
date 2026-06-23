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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kapto_core::{RecvTime, StreamId};
use kapto_store::{LossDetectionKind, StreamSummary};

use crate::batch::{Batch, Gap, Publication, StreamIndex};

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
    /// Whether its publishers' own counts counted that loss.
    pub counted_by_publishers: bool,
}

impl StreamTotals {
    /// This stream's entry in `episode.json`.
    #[must_use]
    pub fn summary(self, id: &StreamId, detection: &LossDetection) -> StreamSummary {
        StreamSummary {
            id: id.to_string(),
            loss_detection: match detection {
                LossDetection::Sequence(_) => LossDetectionKind::Sequence,
                _ if self.counted_by_publishers => LossDetectionKind::PublisherCount,
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
    counted_by_publishers: AtomicBool,
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
                counted_by_publishers: counters.counted_by_publishers.load(Ordering::Relaxed),
            })
    }

    /// Counts `messages` of `stream` as received: messages received before
    /// the episode began, held for it, as its pre-roll.
    pub fn add_received(&self, stream: StreamIndex, messages: u64) {
        if let Some(counters) = self.streams.get(usize::from(stream)) {
            counters.received.fetch_add(messages, Ordering::Relaxed);
        }
    }

    /// Counts `messages` of `stream` as stored in the recording.
    pub fn add_written(&self, stream: StreamIndex, messages: u64) {
        if let Some(counters) = self.streams.get(usize::from(stream)) {
            counters.written.fetch_add(messages, Ordering::Relaxed);
        }
    }

    /// Counts the share of `gap`, before a message received at `at`, from
    /// `from` on as lost on the way: loss found before the episode began,
    /// in what it holds of the time before.
    pub fn add_gap(&self, gap: &Gap, at: RecvTime, from: RecvTime) {
        if let Some(counters) = self.streams.get(usize::from(gap.stream)) {
            // Negative for messages that arrived late after all; adding it
            // as two's complement wraps to the right total.
            counters
                .lost_upstream
                .fetch_add(gap.share_from(at, from).cast_unsigned(), Ordering::Relaxed);
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
    /// Each stream's messages counted by their publishers' own counts,
    /// where the transport gives them.
    published: Box<[PublisherCounts]>,
    /// Each stream's loss in the batch being counted, and when its last
    /// batch was received.
    gaps: Box<[StreamGap]>,
    /// Loss is counted from here on: of a gap that began before, only its
    /// share after.
    from: RecvTime,
}

/// One stream's loss in the batch being counted, which becomes a
/// [`Gap`] in the batch, and when its batch before was received.
#[derive(Clone, Copy, Debug, Default)]
struct StreamGap {
    in_batch: bool,
    /// Missing before its first message in the batch.
    lost: i64,
    /// Missing between its messages in the batch: lost when it was received.
    within: i64,
    /// When the silence its rate counted lost began, if earlier than its
    /// batch before.
    silent_since: Option<RecvTime>,
    last: Option<RecvTime>,
}

/// How many publishers of one stream are counted apart, as the transport
/// tells them apart.
const PUBLISHERS: usize = 8;

/// One stream's messages followed by each publisher's count of them, from
/// 1 up: a number skipped is a message lost on the way, however short the
/// gap, which a rate can't tell from jitter.
#[derive(Clone, Debug, Default)]
struct PublisherCounts {
    trackers: [Option<SequenceTracker>; PUBLISHERS],
    /// In the batch being counted: its messages of the stream, how many
    /// came numbered, and the change in loss the numbers show.
    messages: u64,
    numbered: u64,
    lost: i64,
}

impl PublisherCounts {
    fn add(&mut self, publication: Option<Publication>) {
        self.messages += 1;
        let Some(publication) = publication else {
            return;
        };
        let Some(tracker) = self.trackers.get_mut(usize::from(publication.publisher)) else {
            return;
        };
        self.numbered += 1;
        self.lost += tracker
            .get_or_insert_with(|| SequenceTracker::new(SequenceWidth::U64))
            .observe(publication.number);
    }

    /// The loss the batch's numbers show, if every message of the stream in
    /// it was numbered; starts the next batch.
    fn take(&mut self) -> Option<i64> {
        let counted = (self.messages > 0 && self.numbered == self.messages).then_some(self.lost);
        (self.messages, self.numbered, self.lost) = (0, 0, 0);
        counted
    }
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
    /// Missing before the stream's first message in the batch.
    lost_before: i64,
    /// Missing between its messages in the batch.
    lost_within: i64,
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
        let published = vec![PublisherCounts::default(); detectors.len()].into_boxed_slice();
        let gaps = vec![StreamGap::default(); detectors.len()].into_boxed_slice();
        Self {
            counters,
            detectors: detectors.into_boxed_slice(),
            published,
            gaps,
            from: RecvTime::from_nanos(0),
        }
    }

    /// Goes on detecting loss into `counters`, as of the episode starting
    /// at `from`: a gap found next that began before counts only for its
    /// share from then. What each stream received last is kept, so a gap
    /// across the start is found.
    pub fn resume(&mut self, counters: Arc<LossCounters>, from: RecvTime) {
        self.counters = counters;
        self.from = from;
    }

    /// Counts every message of a batch just received, and records in it
    /// the gap loss detection found before each stream's messages in it.
    /// Never allocates.
    pub fn count(&mut self, batch: &mut Batch) {
        let numbered = batch.has_publications();
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
                        lost_before: 0,
                        lost_within: 0,
                    }
                }
            };
            current.received += 1;
            let sequenced = matches!(
                self.detectors.get(usize::from(stream)),
                Some(Detector::Sequence(..))
            );
            if numbered
                && !sequenced
                && let Some(published) = self.published.get_mut(usize::from(stream))
            {
                published.add(batch.publication(index));
            }
            match self.detectors.get_mut(usize::from(stream)) {
                Some(Detector::Sequence(field, tracker)) => {
                    if let Some(number) = field.read(payload) {
                        current.decoded += 1;
                        let lost = tracker.observe(number);
                        // Missing before the stream's first message in the
                        // batch, or between two of them.
                        if lost != 0 {
                            let first = current.received == 1
                                && self
                                    .gaps
                                    .get(usize::from(stream))
                                    .is_some_and(|gap| !gap.in_batch);
                            if first {
                                current.lost_before += lost;
                            } else {
                                current.lost_within += lost;
                            }
                        }
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
        self.finish_batch(batch, numbered);
    }

    /// Counts each stream's loss in the batch: by its publishers' counts
    /// where every one of its messages in the batch had one, and then the
    /// rate-gap detector forgets what they covered, so a gap is counted
    /// once; otherwise by its rate, if it has one.
    fn finish_batch(&mut self, batch: &mut Batch, numbered: bool) {
        let time = batch.recv_time();
        let streams = self
            .counters
            .streams
            .iter()
            .zip(self.detectors.iter_mut())
            .zip(self.published.iter_mut())
            .zip(self.gaps.iter_mut());
        for ((((counters, detector), published), gap), stream) in streams.zip(0..) {
            let by_publishers = if numbered { published.take() } else { None };
            if let Some(lost) = by_publishers {
                counters
                    .counted_by_publishers
                    .store(true, Ordering::Relaxed);
                gap.lost += lost;
            }
            if let Detector::RateGap(detector, in_batch) = detector
                && in_batch.count > 0
            {
                if by_publishers.is_some() {
                    detector.forget();
                } else {
                    let lost = detector.observe_sent(time, in_batch.count, in_batch.sent());
                    if lost > 0 {
                        gap.lost = gap.lost.saturating_add_unsigned(lost);
                        gap.silent_since = detector.counted_since();
                    }
                }
                *in_batch = InBatch::default();
            }
            if !gap.in_batch {
                continue;
            }
            let since = gap.last.unwrap_or(time);
            let gaps = [
                (
                    gap.lost,
                    gap.silent_since.map_or(since, |silent| silent.min(since)),
                ),
                (gap.within, time),
            ];
            for (lost, since) in gaps {
                if lost != 0 {
                    let found = Gap {
                        stream,
                        lost,
                        since,
                    };
                    batch.add_gap(found);
                    // A late message takes back loss counted earlier, so the
                    // change can be negative; adding it as two's complement
                    // wraps to the right total.
                    counters.lost_upstream.fetch_add(
                        found.share_from(time, self.from).cast_unsigned(),
                        Ordering::Relaxed,
                    );
                }
            }
            *gap = StreamGap {
                last: Some(time),
                ..StreamGap::default()
            };
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
        if let Some(gap) = self.gaps.get_mut(index) {
            gap.in_batch = true;
            gap.lost += run.lost_before;
            gap.within += run.lost_within;
        }
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
        accounting.count(&mut batch_of(
            &[(0, 7), (0, 8), (1, 0), (2, 0), (0, 11)],
            100,
        ));
        accounting.count(&mut batch_of(&[(1, 0), (0, 10)], 120));
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
    fn each_batch_says_the_gaps_found_before_and_between_each_streams_messages_in_it() {
        let counters = Arc::new(LossCounters::new(3));
        let mut accounting = Accounting::new(
            Arc::clone(&counters),
            &[
                (0, SEQUENCE),
                (1, LossDetection::RateGap(RateGap::for_rate(1_000))),
            ],
        );
        let mut gaps = |batch: &mut Batch| {
            accounting.count(batch);
            batch.gaps().to_vec()
        };
        let gap = |stream, lost, since_ms: u64| Gap {
            stream,
            lost,
            since: RecvTime::from_nanos(since_ms * 1_000_000),
        };
        assert_eq!(gaps(&mut batch_of(&[(0, 1), (1, 0), (2, 0)], 100)), []);
        // 2 to 4 missing since the batch before; the rate-gap stream silent.
        assert_eq!(
            gaps(&mut batch_of(&[(0, 5), (2, 0)], 110)),
            [gap(0, 3, 100)]
        );
        // 3 arrives late, taking one back.
        assert_eq!(gaps(&mut batch_of(&[(0, 3)], 120)), [gap(0, -1, 110)]);
        // 7 and 8 missing between two of its messages: when it was received.
        assert_eq!(
            gaps(&mut batch_of(&[(0, 6), (0, 9)], 130)),
            [gap(0, 2, 130)]
        );
        // The rate-gap stream's silence is counted once it settles, as
        // missing since the batch before it.
        assert_eq!(gaps(&mut batch_of(&[(1, 0)], 600)), []);
        assert_eq!(gaps(&mut batch_of(&[(1, 0)], 1_700)), [gap(1, 1_598, 100)]);
        assert_eq!(counters.totals(0).lost_upstream, 4);
        assert_eq!(counters.totals(1).lost_upstream, 1_598);
    }

    #[test]
    fn resumed_for_an_episode_a_gap_across_its_start_counts_its_share_after_it() {
        let before = Arc::new(LossCounters::new(1));
        let mut accounting = Accounting::new(Arc::clone(&before), &[(0, SEQUENCE)]);
        accounting.count(&mut batch_of(&[(0, 1)], 100));
        let episode = Arc::new(LossCounters::new(1));
        accounting.resume(Arc::clone(&episode), RecvTime::from_nanos(150 * 1_000_000));
        // 2 to 11 missing between 100 ms and 200 ms: half of it after 150 ms.
        let mut batch = batch_of(&[(0, 12), (0, 14)], 200);
        accounting.count(&mut batch);
        assert_eq!(episode.totals(0).lost_upstream, 5 + 1);
        assert_eq!(before.totals(0).lost_upstream, 0);
        // The batch says the whole gap, for whatever holds it.
        assert_eq!(batch.gaps()[0].lost, 10);
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
            accounting.count(&mut batch);
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
            accounting.count(&mut batch);
        }
        accounting.settle();
        counters.totals(0).lost_upstream
    }

    /// A batch of stream 0's messages received at `time_ms`, each `(publisher,
    /// number)` or, for `None`, with no count.
    fn numbered(messages: &[Option<(u8, u64)>], time_ms: u64) -> Batch {
        let mut batch = Batch::new(messages.len().max(1), 8);
        for (place, message) in (0_u64..).zip(messages) {
            let sent = 1 + time_ms * 1_000_000 + place * 500_000;
            match message {
                Some((publisher, number)) => batch.push_from_publisher(
                    0,
                    b"x",
                    sent,
                    Publication {
                        publisher: *publisher,
                        number: *number,
                    },
                ),
                None => batch.push_with_source_time(0, b"x", sent),
            };
        }
        batch.set_recv_time(RecvTime::from_nanos(time_ms * 1_000_000));
        batch
    }

    /// Accounting for one stream at 2 kHz, with `detection`.
    fn one_stream(detection: LossDetection) -> (Arc<LossCounters>, Accounting) {
        let counters = Arc::new(LossCounters::new(1));
        let accounting = Accounting::new(Arc::clone(&counters), &[(0, detection)]);
        (counters, accounting)
    }

    #[test]
    fn a_number_its_publisher_skipped_is_lost_and_one_arriving_late_takes_it_back() {
        let (counters, mut accounting) =
            one_stream(LossDetection::RateGap(RateGap::for_rate(2_000)));
        accounting.count(&mut numbered(
            &[Some((0, 1)), Some((0, 2)), Some((0, 4))],
            0,
        ));
        assert_eq!(
            counters.totals(0).lost_upstream,
            1,
            "a single message, too short a gap for a rate"
        );
        accounting.count(&mut numbered(&[Some((0, 3)), Some((0, 5))], 2));
        assert_eq!(counters.totals(0).lost_upstream, 0);
    }

    #[test]
    fn a_stream_counted_by_its_publishers_says_so_in_its_summary() {
        let (counters, mut accounting) =
            one_stream(LossDetection::RateGap(RateGap::for_rate(2_000)));
        let rate = LossDetection::RateGap(RateGap::for_rate(2_000));
        let id: StreamId = "arm".parse().unwrap();
        assert_eq!(
            counters.totals(0).summary(&id, &rate).loss_detection,
            LossDetectionKind::RateGap
        );
        accounting.count(&mut numbered(&[Some((0, 1)), Some((0, 2))], 0));
        assert_eq!(
            counters.totals(0).summary(&id, &rate).loss_detection,
            LossDetectionKind::PublisherCount
        );
    }

    #[test]
    fn each_publisher_of_a_stream_is_followed_by_its_own_count() {
        let (counters, mut accounting) = one_stream(LossDetection::Undetectable);
        accounting.count(&mut numbered(
            &[Some((0, 10)), Some((1, 1)), Some((0, 11)), Some((1, 2))],
            0,
        ));
        assert_eq!(
            counters.totals(0).lost_upstream,
            0,
            "interleaved, each without a gap"
        );
        accounting.count(&mut numbered(&[Some((1, 5)), Some((0, 12))], 2));
        assert_eq!(counters.totals(0).lost_upstream, 2);
    }

    #[test]
    fn a_publisher_pausing_is_no_loss_however_long_its_silence() {
        // Counted by its numbers, a stream that stops sending for a second
        // lost nothing, where its rate alone would read a silence as loss.
        let (counters, mut accounting) =
            one_stream(LossDetection::RateGap(RateGap::for_rate(2_000)));
        let mut number = 0;
        let mut batch_of = |time_ms| {
            let messages: Vec<_> = (0..4)
                .map(|_| {
                    number += 1;
                    Some((0, number))
                })
                .collect();
            numbered(&messages, time_ms)
        };
        for time_ms in (0..100).step_by(2) {
            accounting.count(&mut batch_of(time_ms));
        }
        for time_ms in (1_100..3_000).step_by(2) {
            accounting.count(&mut batch_of(time_ms));
        }
        accounting.settle();
        assert_eq!(counters.totals(0).lost_upstream, 0);
    }

    #[test]
    fn a_batch_with_a_message_without_its_count_is_judged_by_rate_and_never_twice() {
        // A number skipped in a batch that also holds a message without a
        // count: the rate counts the batch, as before numbers, and the
        // skipped number isn't counted on top.
        let (counters, mut accounting) =
            one_stream(LossDetection::RateGap(RateGap::for_rate(2_000)));
        accounting.count(&mut numbered(&[Some((0, 1)), Some((0, 2))], 0));
        accounting.count(&mut numbered(&[Some((0, 4)), None], 2));
        assert_eq!(counters.totals(0).lost_upstream, 0);
    }

    #[test]
    fn a_stream_with_sequence_numbers_of_its_own_keeps_counting_by_them() {
        let (counters, mut accounting) = one_stream(SEQUENCE);
        let mut batch = Batch::new(2, 8);
        for (sequence, count) in [(1_u32, 1), (2, 5)] {
            batch.push_from_publisher(
                0,
                &sequence.to_le_bytes(),
                1,
                Publication {
                    publisher: 0,
                    number: count,
                },
            );
        }
        accounting.count(&mut batch);
        assert_eq!(
            counters.totals(0).lost_upstream,
            0,
            "its own numbers have no gap"
        );
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
        accounting.count(&mut batch);
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
