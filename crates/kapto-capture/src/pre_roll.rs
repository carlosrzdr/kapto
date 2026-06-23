// SPDX-License-Identifier: Apache-2.0

//! The newest messages of each stream with a pre-roll, so an episode starts
//! with the seconds before it was started.
//!
//! The writer thread holds them as it takes each batch, between episodes
//! and during them. Each stream's memory is set aside once: a byte ring its
//! messages are copied into end to end, as long as they really are, and an
//! index ring of where each one is. Holding one is a copy, the oldest
//! giving way to the newest; so the ring holds the stream's newest
//! messages, as many seconds of them as fit.
//!
//! Once a stream has a pre-roll, every other stream gets a ring of no
//! seconds: it holds only while a start waits, from when it was asked, so
//! every stream starts with the Start however long recording takes to
//! begin.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kapto_core::RecvTime;

use crate::batch::{Batch, Gap, StreamIndex};

/// One stream with a pre-roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreRollStream {
    /// The stream.
    pub stream: StreamIndex,
    /// How long before an episode's start its messages are written. With
    /// none, its messages are held only while a start waits.
    pub seconds: Duration,
    /// Bytes set aside for its messages.
    pub bytes: usize,
    /// How many messages it may hold at most: room in the index.
    pub messages: usize,
}

/// Each pre-rolled stream's newest messages.
#[derive(Debug, Default)]
pub struct PreRoll {
    /// By stream index, its place in `rings`, if it has a pre-roll.
    by_stream: Box<[Option<usize>]>,
    rings: Vec<Ring>,
    waiting: Waiting,
}

/// When the start being waited for was asked, if one is; shared between
/// the thread that notes it and the writer thread.
#[derive(Clone, Debug)]
pub struct Waiting(Arc<AtomicU64>);

impl Default for Waiting {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(u64::MAX)))
    }
}

impl Waiting {
    /// Notes that a start asked at `asked` waits, or with `None` that none
    /// does any more.
    pub fn set(&self, asked: Option<RecvTime>) {
        self.0.store(
            asked.map_or(u64::MAX, RecvTime::as_nanos),
            Ordering::Relaxed,
        );
    }

    /// When the start waiting was asked, in nanoseconds; `u64::MAX` if none
    /// waits.
    fn since(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct Ring {
    stream: StreamIndex,
    seconds: Duration,
    data: Box<[u8]>,
    index: Box<[Held]>,
    /// The index's oldest entry.
    oldest: usize,
    /// How many entries are held.
    count: usize,
    /// Where in `data` the next message goes.
    write: usize,
}

#[derive(Clone, Copy, Debug)]
struct Held {
    offset: usize,
    len: usize,
    received: RecvTime,
    source_time: Option<u64>,
    /// The messages of the stream missing before its batch, and since when;
    /// 0 for none.
    lost: i64,
    since: RecvTime,
}

/// A message held, as it was received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeldMessage<'a> {
    /// Its stream.
    pub stream: StreamIndex,
    /// Its payload, as received.
    pub payload: &'a [u8],
    /// When Kapto received it.
    pub received: RecvTime,
    /// The time its source gave it, in nanoseconds since the Unix epoch.
    pub source_time: Option<u64>,
    /// The gap loss detection found before its batch, given with the
    /// newest of the stream's messages in it.
    pub gap: Option<Gap>,
}

impl PreRoll {
    /// Sets aside the memory for `streams`.
    #[must_use]
    pub fn new(streams: &[PreRollStream]) -> Self {
        let count = streams
            .iter()
            .map(|stream| usize::from(stream.stream) + 1)
            .max()
            .unwrap_or(0);
        let mut by_stream = vec![None; count].into_boxed_slice();
        let rings = streams
            .iter()
            .enumerate()
            .map(|(place, stream)| {
                by_stream[usize::from(stream.stream)] = Some(place);
                Ring {
                    stream: stream.stream,
                    seconds: stream.seconds,
                    data: vec![0; stream.bytes].into_boxed_slice(),
                    index: vec![
                        Held {
                            offset: 0,
                            len: 0,
                            received: RecvTime::from_nanos(0),
                            source_time: None,
                            lost: 0,
                            since: RecvTime::from_nanos(0),
                        };
                        stream.messages.max(1)
                    ]
                    .into_boxed_slice(),
                    oldest: 0,
                    count: 0,
                    write: 0,
                }
            })
            .collect();
        Self {
            by_stream,
            rings,
            waiting: Waiting::default(),
        }
    }

    /// Where a start waiting is noted: the streams with no seconds are held
    /// from when it was asked.
    #[must_use]
    pub fn waiting(&self) -> Waiting {
        self.waiting.clone()
    }

    /// Whether no stream has a pre-roll.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    /// The bytes set aside for every stream's messages and their index.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.rings
            .iter()
            .map(|ring| ring.data.len() + size_of_val(&*ring.index))
            .sum()
    }

    /// Holds each message of `batch` whose stream has a pre-roll, and those
    /// of the streams with no seconds if a start waits since before it,
    /// each stream's gap before the batch with its newest message.
    pub fn hold(&mut self, batch: &Batch) {
        if self.rings.is_empty() {
            return;
        }
        let waited_for = batch.recv_time().as_nanos() >= self.waiting.since();
        for (index, (stream, payload)) in batch.messages().enumerate() {
            if let Some(Some(place)) = self.by_stream.get(usize::from(stream)) {
                let ring = &mut self.rings[*place];
                if waited_for || !ring.seconds.is_zero() {
                    ring.hold(payload, batch.recv_time(), batch.source_time(index));
                }
            }
        }
        for gap in batch.gaps() {
            if let Some(Some(place)) = self.by_stream.get(usize::from(gap.stream)) {
                self.rings[*place].attach(gap, batch.recv_time());
            }
        }
    }

    /// The earliest message an episode asked for at `asked` and begun at
    /// `begun` starts with; see [`PreRoll::messages`].
    #[must_use]
    pub fn earliest(&self, asked: RecvTime, begun: RecvTime) -> Option<RecvTime> {
        self.messages(asked, begun)
            .map(|message| message.received)
            .min()
    }

    /// Every message an episode asked for at `asked` and begun at `begun`
    /// starts with: each stream's held from its seconds before `asked` up to
    /// `begun`, stream by stream, oldest first. Those from after `asked`
    /// arrived while recording began, and belong to the episode too.
    pub fn messages(
        &self,
        asked: RecvTime,
        begun: RecvTime,
    ) -> impl Iterator<Item = HeldMessage<'_>> {
        self.rings.iter().flat_map(move |ring| {
            let seconds = u64::try_from(ring.seconds.as_nanos()).unwrap_or(u64::MAX);
            let from = asked.as_nanos().saturating_sub(seconds);
            (0..ring.count)
                .map(|offset| ring.index[(ring.oldest + offset) % ring.index.len()])
                .filter(move |held| (from..begun.as_nanos()).contains(&held.received.as_nanos()))
                .map(|held| HeldMessage {
                    stream: ring.stream,
                    payload: &ring.data[held.offset..held.offset + held.len],
                    received: held.received,
                    source_time: held.source_time,
                    gap: (held.lost != 0).then_some(Gap {
                        stream: ring.stream,
                        lost: held.lost,
                        since: held.since,
                    }),
                })
        })
    }
}

impl Ring {
    /// Copies `payload` in after the newest message, letting the oldest go
    /// until it fits. A message larger than the whole ring isn't held.
    fn hold(&mut self, payload: &[u8], received: RecvTime, source_time: Option<u64>) {
        let len = payload.len();
        if len > self.data.len() {
            return;
        }
        if self.count == self.index.len() {
            self.let_go();
        }
        // A message goes in one piece: if it doesn't fit before the end, it
        // starts again at the beginning.
        if self.write + len > self.data.len() {
            self.write = 0;
        }
        while self.count > 0 {
            let oldest = self.index[self.oldest];
            let overlaps =
                oldest.offset < self.write + len && self.write < oldest.offset + oldest.len;
            if !overlaps {
                break;
            }
            self.let_go();
        }
        self.data[self.write..self.write + len].copy_from_slice(payload);
        let newest = (self.oldest + self.count) % self.index.len();
        self.index[newest] = Held {
            offset: self.write,
            len,
            received,
            source_time,
            lost: 0,
            since: received,
        };
        self.count += 1;
        self.write += len;
    }

    /// Gives `gap` to the newest message held, if it is of the batch
    /// received at `received`: one the ring didn't hold has no gap to keep.
    fn attach(&mut self, gap: &Gap, received: RecvTime) {
        if self.count == 0 {
            return;
        }
        let newest = &mut self.index[(self.oldest + self.count - 1) % self.index.len()];
        if newest.received == received {
            newest.lost = gap.lost;
            newest.since = gap.since;
        }
    }

    fn let_go(&mut self) {
        self.oldest = (self.oldest + 1) % self.index.len();
        self.count -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(at: u64, messages: &[(StreamIndex, &[u8])]) -> Batch {
        let mut batch = Batch::new(8, 256);
        batch.set_recv_time(RecvTime::from_nanos(at));
        for (stream, payload) in messages {
            assert!(batch.push(*stream, payload));
        }
        batch
    }

    fn held(pre_roll: &PreRoll, start: u64) -> Vec<(StreamIndex, Vec<u8>, u64)> {
        pre_roll
            .messages(RecvTime::from_nanos(start), RecvTime::from_nanos(start))
            .map(|message| {
                (
                    message.stream,
                    message.payload.to_vec(),
                    message.received.as_nanos(),
                )
            })
            .collect()
    }

    fn stream(stream: StreamIndex, seconds: u64, bytes: usize, messages: usize) -> PreRollStream {
        PreRollStream {
            stream,
            seconds: Duration::from_secs(seconds),
            bytes,
            messages,
        }
    }

    #[test]
    fn an_episode_starts_with_each_streams_messages_within_its_seconds_before_it() {
        let mut pre_roll = PreRoll::new(&[stream(2, 2, 1024, 16), stream(0, 1, 1024, 16)]);
        for (second, payload) in [(1, "a"), (2, "b"), (3, "c"), (4, "d")] {
            let at = second * 1_000_000_000;
            pre_roll.hold(&batch(
                at,
                &[
                    (2, payload.as_bytes()),
                    (1, b"not held"),
                    (0, payload.as_bytes()),
                ],
            ));
        }
        // Started at 4.5 s: stream 2 from 2.5 s, stream 0 from 3.5 s; nothing
        // at or after the start.
        assert_eq!(
            held(&pre_roll, 4_500_000_000),
            [
                (2, b"c".to_vec(), 3_000_000_000),
                (2, b"d".to_vec(), 4_000_000_000),
                (0, b"d".to_vec(), 4_000_000_000)
            ]
        );
        let at = RecvTime::from_nanos(4_500_000_000);
        assert_eq!(
            pre_roll.earliest(at, at),
            Some(RecvTime::from_nanos(3_000_000_000))
        );
        // Asked at 3.5 s but begun at 4.5 s: stream 2 from 1.5 s and stream 0
        // from 2.5 s, both up to 4.5 s.
        let asked = RecvTime::from_nanos(3_500_000_000);
        let seconds: Vec<(StreamIndex, u64)> = pre_roll
            .messages(asked, at)
            .map(|message| (message.stream, message.received.as_nanos() / 1_000_000_000))
            .collect();
        assert_eq!(seconds, [(2, 2), (2, 3), (2, 4), (0, 3), (0, 4)]);
        assert!(
            held(&pre_roll, 4_000_000_000)
                .iter()
                .all(|(_, _, at)| *at < 4_000_000_000)
        );
    }

    #[test]
    fn the_newest_messages_stay_as_long_as_they_are_the_oldest_giving_way() {
        // 100 bytes: messages of 30 bytes, three at a time, wrapping around.
        let mut pre_roll = PreRoll::new(&[stream(0, 60, 100, 16)]);
        for at in 1..=10_u8 {
            pre_roll.hold(&batch(u64::from(at), &[(0, &[at; 30])]));
        }
        let kept = held(&pre_roll, 11);
        assert_eq!(
            kept.iter().map(|(_, _, at)| *at).collect::<Vec<_>>(),
            [8, 9, 10]
        );
        for (_, payload, at) in &kept {
            assert_eq!(payload, &vec![u8::try_from(*at).unwrap(); 30]);
        }
        // Varying lengths: a large one takes the room of several small ones.
        pre_roll.hold(&batch(12, &[(0, &[12; 70])]));
        assert_eq!(
            held(&pre_roll, 13)
                .iter()
                .map(|(_, p, at)| (*at, p.len()))
                .collect::<Vec<_>>(),
            [(10, 30), (12, 70)]
        );
        // One larger than the ring isn't held, and takes nothing away.
        pre_roll.hold(&batch(14, &[(0, &[14; 101])]));
        assert_eq!(held(&pre_roll, 15).len(), 2);
    }

    #[test]
    fn a_full_index_lets_the_oldest_go_however_much_room_is_left() {
        let mut pre_roll = PreRoll::new(&[stream(0, 60, 1024, 3)]);
        for at in 1..=5_u8 {
            pre_roll.hold(&batch(u64::from(at), &[(0, &[at])]));
        }
        assert_eq!(
            held(&pre_roll, 6)
                .iter()
                .map(|(_, _, at)| *at)
                .collect::<Vec<_>>(),
            [3, 4, 5]
        );
    }

    #[test]
    fn a_stream_with_no_seconds_is_held_only_from_when_a_start_waiting_was_asked() {
        let mut pre_roll = PreRoll::new(&[stream(0, 1, 1024, 16), stream(1, 0, 1024, 16)]);
        let waiting = pre_roll.waiting();
        pre_roll.hold(&batch(1, &[(0, b"a"), (1, b"a")]));
        waiting.set(Some(RecvTime::from_nanos(3)));
        // Received before the start was asked, taken after: not held.
        pre_roll.hold(&batch(2, &[(0, b"b"), (1, b"b")]));
        pre_roll.hold(&batch(4, &[(0, b"c"), (1, b"c")]));
        waiting.set(None);
        pre_roll.hold(&batch(5, &[(0, b"d"), (1, b"d")]));
        let held: Vec<(StreamIndex, Vec<u8>)> = pre_roll
            .messages(RecvTime::from_nanos(3), RecvTime::from_nanos(6))
            .map(|message| (message.stream, message.payload.to_vec()))
            .collect();
        assert_eq!(
            held,
            [
                (0, b"a".to_vec()),
                (0, b"b".to_vec()),
                (0, b"c".to_vec()),
                (0, b"d".to_vec()),
                (1, b"c".to_vec())
            ]
        );
    }

    #[test]
    fn a_gap_is_held_with_the_newest_message_of_its_stream_in_its_batch() {
        let mut pre_roll = PreRoll::new(&[stream(0, 10, 1024, 16)]);
        let gap = Gap {
            stream: 0,
            lost: 3,
            since: RecvTime::from_nanos(1),
        };
        let mut gapped = batch(2, &[(0, b"a"), (1, b"other"), (0, b"b")]);
        gapped.add_gap(gap);
        gapped.add_gap(Gap { stream: 1, ..gap });
        pre_roll.hold(&batch(1, &[(0, b"before")]));
        pre_roll.hold(&gapped);
        pre_roll.hold(&batch(3, &[(0, b"after")]));
        let gaps: Vec<(Vec<u8>, Option<Gap>)> = pre_roll
            .messages(RecvTime::from_nanos(4), RecvTime::from_nanos(4))
            .map(|message| (message.payload.to_vec(), message.gap))
            .collect();
        assert_eq!(
            gaps,
            [
                (b"before".to_vec(), None),
                (b"a".to_vec(), None),
                (b"b".to_vec(), Some(gap)),
                (b"after".to_vec(), None)
            ]
        );
    }

    #[test]
    fn without_pre_rolled_streams_nothing_is_held_or_set_aside() {
        let mut none = PreRoll::new(&[]);
        assert!(none.is_empty());
        none.hold(&batch(1, &[(0, b"moving")]));
        assert_eq!(
            none.messages(RecvTime::from_nanos(2), RecvTime::from_nanos(2))
                .count(),
            0
        );
        assert_eq!(none.memory_bytes(), 0);
        assert!(PreRoll::new(&[stream(0, 1, 4096, 8)]).memory_bytes() >= 4096);
    }
}
