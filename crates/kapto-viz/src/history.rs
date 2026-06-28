// SPDX-License-Identifier: Apache-2.0

//! The last minutes of the live view, kept by the bridge so a page can look
//! back at something odd: every frame it streamed, as a record, for
//! `viewer.history_age` and at most `viewer.history` MiB, the oldest going
//! first. A page asks for a stretch at the number of points it can show,
//! and gets it merged down to that many: each value's minimum and maximum
//! over each part of the stretch, so a spike still shows, and the robot's
//! newest pose in each part.
//!
//! Records are kept in blocks of about a second. A finished block is never
//! changed, so a page's question takes the blocks it needs while the bridge
//! goes on adding to the newest, and only copies that one: answering never
//! holds the bridge up for more than a moment.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use kapto_robot::Transform;
use tungstenite::Bytes;

/// What a reply starts with.
pub const HISTORY_MAGIC: &[u8; 4] = b"KVHI";
/// The most points a page may ask for.
pub const MAX_POINTS: u32 = 10_000;
/// How long a block of records spans, in nanoseconds.
const BLOCK_NS: i64 = 1_000_000_000;

/// A link's pose: x, y and z, then a unit quaternion x, y, z, w.
pub type Pose = [f32; 7];

/// `transform` as a record gives a link's pose.
#[must_use]
pub fn pose(transform: &Transform) -> Pose {
    let [x, y, z] = transform.translation();
    let [qx, qy, qz, qw] = transform.quaternion();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the page draws in f32; sub-micrometre differences don't show"
    )]
    [x, y, z, qx, qy, qz, qw].map(|value| value as f32)
}

/// One stream's sample: the extremes of the messages it summarises.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleRecord {
    /// The stream's position in the declarations.
    pub stream: u16,
    /// How many messages it summarises.
    pub messages: u32,
    /// When the oldest of them was received, UTC nanoseconds.
    pub first_utc_ns: i64,
    /// When the newest was received.
    pub newest_utc_ns: i64,
    /// Each value's minimum and maximum over those messages.
    pub extremes: Vec<(f64, f64)>,
}

impl SampleRecord {
    /// Takes in `other`, of the same stream: its messages counted, the span
    /// and each value's extremes widened to cover both.
    pub fn absorb(&mut self, other: &Self) {
        self.messages = self.messages.saturating_add(other.messages);
        self.first_utc_ns = self.first_utc_ns.min(other.first_utc_ns);
        self.newest_utc_ns = self.newest_utc_ns.max(other.newest_utc_ns);
        widen(&mut self.extremes, &other.extremes);
    }
}

/// Widens each of `kept`'s minimum and maximum to cover `other`'s.
fn widen(kept: &mut [(f64, f64)], other: &[(f64, f64)]) {
    for (kept, (minimum, maximum)) in kept.iter_mut().zip(other) {
        *kept = (kept.0.min(*minimum), kept.1.max(*maximum));
    }
}

/// What one frame showed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// When Kapto received the newest message it shows, UTC nanoseconds.
    pub received_utc_ns: i64,
    /// Every link's pose, if the robot moved.
    pub poses: Option<Vec<Pose>>,
    /// Each stream's samples.
    pub samples: Vec<SampleRecord>,
}

impl Record {
    /// Roughly what it takes in memory, for the size bound.
    fn bytes(&self) -> usize {
        64 + self.poses.as_ref().map_or(0, |poses| poses.len() * 28)
            + self
                .samples
                .iter()
                .map(|sample| 48 + sample.extremes.len() * 16)
                .sum::<usize>()
    }
}

/// The records kept.
#[derive(Debug)]
pub struct History {
    /// Finished blocks, oldest first, each with its size.
    blocks: VecDeque<(Arc<[Record]>, usize)>,
    /// The block being filled.
    current: Vec<Record>,
    current_bytes: usize,
    bytes: usize,
    age_ns: i64,
    max_bytes: usize,
}

/// The records a question is answered from, taken without holding up the
/// bridge.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    blocks: Vec<Arc<[Record]>>,
    current: Vec<Record>,
}

impl History {
    /// Keeps records for `age_ns` nanoseconds behind the newest, and at most
    /// about `max_bytes`.
    #[must_use]
    pub fn new(age_ns: i64, max_bytes: usize) -> Self {
        Self {
            blocks: VecDeque::new(),
            current: Vec::new(),
            current_bytes: 0,
            bytes: 0,
            age_ns,
            max_bytes,
        }
    }

    /// Adds `record`, then lets go of what is too old or too much.
    pub fn push(&mut self, record: Record) {
        let started = self.current.first().map(|first| first.received_utc_ns);
        if started.is_some_and(|started| record.received_utc_ns - started >= BLOCK_NS) {
            let block: Arc<[Record]> = std::mem::take(&mut self.current).into();
            self.blocks
                .push_back((block, std::mem::take(&mut self.current_bytes)));
        }
        let bytes = record.bytes();
        self.current_bytes += bytes;
        self.bytes += bytes;
        let newest = record.received_utc_ns;
        self.current.push(record);
        while let Some((block, size)) = self.blocks.front() {
            let ended = block.last().map_or(i64::MIN, |last| last.received_utc_ns);
            if self.bytes <= self.max_bytes && newest.saturating_sub(ended) <= self.age_ns {
                break;
            }
            self.bytes -= size;
            self.blocks.pop_front();
        }
    }

    /// How much it holds, roughly, in bytes.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The records kept now, cheaply: finished blocks are shared, only the
    /// newest is copied.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            blocks: self
                .blocks
                .iter()
                .map(|(block, _)| Arc::clone(block))
                .collect(),
            current: self.current.clone(),
        }
    }
}

impl Snapshot {
    fn records(&self) -> impl Iterator<Item = &Record> {
        self.blocks
            .iter()
            .flat_map(|block| block.iter())
            .chain(&self.current)
    }

    /// The oldest and newest times kept, if any.
    #[must_use]
    pub fn span(&self) -> Option<(i64, i64)> {
        let mut records = self.records();
        let oldest = records.next()?.received_utc_ns;
        Some((
            oldest,
            self.records()
                .last()
                .map_or(oldest, |last| last.received_utc_ns),
        ))
    }

    /// The stretch from `from` to `to` (UTC nanoseconds, both included)
    /// merged into `points` parts of equal length.
    #[must_use]
    pub fn stretch(&self, from: i64, to: i64, points: u32) -> Stretch {
        stretch(self.records(), self.span(), from, to, points)
    }
}

/// The stretch of `records`, in time order, from `from` to `to` (UTC
/// nanoseconds, both included) merged into `points` parts of equal length:
/// each value's extremes over each part, and the robot's newest pose in it.
/// `span` is what the records cover, whatever was asked for.
#[must_use]
pub fn stretch<'a>(
    records: impl IntoIterator<Item = &'a Record>,
    span: Option<(i64, i64)>,
    from: i64,
    to: i64,
    points: u32,
) -> Stretch {
    let points = points.clamp(1, MAX_POINTS);
    // One more than the even share, so the stretch's end falls in the
    // last part rather than one past it.
    let width = (to.saturating_sub(from) / i64::from(points)).saturating_add(1);
    let part = |time: i64| time.saturating_sub(from) / width;
    let mut poses: Vec<(i64, i64, Vec<Pose>)> = Vec::new();
    let mut streams: BTreeMap<u16, Vec<(i64, Merged)>> = BTreeMap::new();
    for record in records {
        if let Some(pose) = &record.poses
            && (from..=to).contains(&record.received_utc_ns)
        {
            let at = part(record.received_utc_ns);
            match poses.last_mut() {
                Some(last) if last.0 == at => {
                    (last.1, last.2) = (record.received_utc_ns, pose.clone());
                }
                _ => poses.push((at, record.received_utc_ns, pose.clone())),
            }
        }
        for sample in &record.samples {
            if !(from..=to).contains(&sample.newest_utc_ns) {
                continue;
            }
            let at = part(sample.newest_utc_ns);
            let parts = streams.entry(sample.stream).or_default();
            match parts.last_mut() {
                Some((last, merged)) if *last == at => merged.add(sample),
                _ => parts.push((at, Merged::of(sample))),
            }
        }
    }
    Stretch {
        span,
        poses: poses
            .into_iter()
            .map(|(_, time, pose)| (time, pose))
            .collect(),
        streams: streams
            .into_iter()
            .map(|(stream, parts)| (stream, parts.into_iter().map(|(_, part)| part).collect()))
            .collect(),
    }
}

/// One part of a stretch of a stream: its samples merged.
#[derive(Clone, Debug, PartialEq)]
pub struct Merged {
    /// When the oldest message in it was received.
    pub first_utc_ns: i64,
    /// When the newest was.
    pub newest_utc_ns: i64,
    /// How many messages it covers.
    pub messages: u64,
    /// Each value's minimum and maximum over them.
    pub extremes: Vec<(f64, f64)>,
}

impl Merged {
    fn of(sample: &SampleRecord) -> Self {
        Self {
            first_utc_ns: sample.first_utc_ns,
            newest_utc_ns: sample.newest_utc_ns,
            messages: u64::from(sample.messages),
            extremes: sample.extremes.clone(),
        }
    }

    fn add(&mut self, sample: &SampleRecord) {
        self.first_utc_ns = self.first_utc_ns.min(sample.first_utc_ns);
        self.newest_utc_ns = self.newest_utc_ns.max(sample.newest_utc_ns);
        self.messages = self.messages.saturating_add(u64::from(sample.messages));
        widen(&mut self.extremes, &sample.extremes);
    }
}

/// A stretch of the history, merged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stretch {
    /// The oldest and newest times kept, whatever was asked for.
    pub span: Option<(i64, i64)>,
    /// The robot's newest pose in each part where it moved, with its time.
    pub poses: Vec<(i64, Vec<Pose>)>,
    /// Each stream with samples in the stretch, by its position, and its
    /// parts in time order.
    pub streams: Vec<(u16, Vec<Merged>)>,
}

impl Stretch {
    /// The reply to question `id`, in the binary form the viewer stream's
    /// specification gives.
    #[must_use]
    pub fn reply(&self, id: u32) -> Bytes {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes();
        let (oldest, newest) = self.span.unwrap_or((0, 0));
        let links = self.poses.first().map_or(0, |(_, pose)| pose.len());
        let mut bytes = Vec::new();
        bytes.extend(HISTORY_MAGIC);
        bytes.extend(crate::stream::STREAM_VERSION.to_le_bytes());
        bytes.extend(0_u16.to_le_bytes());
        bytes.extend(id.to_le_bytes());
        bytes.extend(oldest.to_le_bytes());
        bytes.extend(newest.to_le_bytes());
        bytes.extend(count(links));
        bytes.extend(count(self.poses.len()));
        for (time, pose) in &self.poses {
            bytes.extend(time.to_le_bytes());
            for value in pose.iter().flatten() {
                bytes.extend(value.to_le_bytes());
            }
        }
        bytes.extend(count(self.streams.len()));
        for (stream, parts) in &self.streams {
            let values = parts.first().map_or(0, |part| part.extremes.len());
            bytes.extend(stream.to_le_bytes());
            bytes.extend(0_u16.to_le_bytes());
            bytes.extend(count(values));
            bytes.extend(count(parts.len()));
            for part in parts {
                bytes.extend(part.first_utc_ns.to_le_bytes());
                bytes.extend(part.newest_utc_ns.to_le_bytes());
                bytes.extend(
                    u32::try_from(part.messages)
                        .unwrap_or(u32::MAX)
                        .to_le_bytes(),
                );
                bytes.extend(0_u32.to_le_bytes());
                for index in 0..values {
                    let (minimum, maximum) = part
                        .extremes
                        .get(index)
                        .copied()
                        .unwrap_or((f64::NAN, f64::NAN));
                    bytes.extend(minimum.to_le_bytes());
                    bytes.extend(maximum.to_le_bytes());
                }
            }
        }
        Bytes::from(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_absorbs_another_widening_its_span_and_extremes_and_never_overflowing() {
        let mut kept = SampleRecord {
            stream: 0,
            messages: u32::MAX - 1,
            first_utc_ns: 20,
            newest_utc_ns: 30,
            extremes: vec![(1.0, 2.0), (-1.0, 1.0)],
        };
        kept.absorb(&SampleRecord {
            stream: 0,
            messages: 5,
            first_utc_ns: 10,
            newest_utc_ns: 25,
            extremes: vec![(0.5, 1.5), (-3.0, 0.0)],
        });
        assert_eq!(kept.messages, u32::MAX);
        assert_eq!((kept.first_utc_ns, kept.newest_utc_ns), (10, 30));
        assert_eq!(kept.extremes, [(0.5, 2.0), (-3.0, 1.0)]);
    }

    fn record(second: i64, value: f64) -> Record {
        Record {
            received_utc_ns: second * BLOCK_NS,
            poses: Some(vec![[0.0; 7]]),
            samples: vec![SampleRecord {
                stream: 0,
                messages: 1,
                first_utc_ns: second * BLOCK_NS,
                newest_utc_ns: second * BLOCK_NS,
                extremes: vec![(value, value)],
            }],
        }
    }

    #[test]
    fn records_older_than_the_age_go_a_block_at_a_time() {
        let mut history = History::new(10 * BLOCK_NS, usize::MAX);
        for second in 0..30 {
            history.push(record(second, 0.0));
        }
        let (oldest, newest) = history.snapshot().span().unwrap();
        assert_eq!(newest, 29 * BLOCK_NS);
        assert_eq!(oldest, 19 * BLOCK_NS, "ten seconds behind the newest");
    }

    #[test]
    fn records_past_the_size_go_oldest_first() {
        let size = record(0, 0.0).bytes();
        let mut history = History::new(i64::MAX, size * 5);
        for second in 0..30 {
            history.push(record(second, 0.0));
        }
        assert!(history.bytes() <= size * 5, "{}", history.bytes());
        assert_eq!(history.snapshot().span().unwrap().0, 25 * BLOCK_NS);
    }
}
