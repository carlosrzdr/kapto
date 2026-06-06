// SPDX-License-Identifier: Apache-2.0

//! The newest messages of each stream whose messages are kept for late
//! subscribers, such as a ROS 2 robot's fixed transforms on `/tf_static`,
//! so that every episode starts with them, however long after they were
//! sent it begins.
//!
//! The writer thread keeps them as it takes each batch, while an episode is
//! recorded and while capture idles between episodes. Their memory is set
//! aside once, for as many messages per stream as its subscription keeps;
//! keeping one is a copy into it, the oldest giving way to the newest.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kapto_core::RecvTime;

use crate::batch::{Batch, StreamIndex};

/// One stream whose messages are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeptStream {
    /// The stream.
    pub stream: StreamIndex,
    /// How many of its newest messages are kept.
    pub depth: usize,
    /// The largest message it sends, in bytes.
    pub max_message_len: usize,
}

/// The newest messages of each kept stream.
#[derive(Debug, Default)]
pub struct Kept {
    /// By stream index, its place in `streams`, if it is kept.
    by_stream: Box<[Option<usize>]>,
    streams: Vec<Slots>,
    held: KeptHeld,
}

/// Which streams have a message kept, read from other threads than the
/// writer's: pre-flight, which passes a kept stream once it has one.
#[derive(Clone, Debug, Default)]
pub struct KeptHeld(Arc<[AtomicBool]>);

impl KeptHeld {
    /// Whether a message of `stream` is kept.
    #[must_use]
    pub fn has(&self, stream: StreamIndex) -> bool {
        self.0
            .get(usize::from(stream))
            .is_some_and(|held| held.load(Ordering::Relaxed))
    }
}

#[derive(Debug)]
struct Slots {
    stream: StreamIndex,
    messages: Box<[Message]>,
    /// Where the next message goes.
    next: usize,
    /// How many are kept so far, up to `messages.len()`.
    held: usize,
}

#[derive(Debug)]
struct Message {
    data: Box<[u8]>,
    len: usize,
    received: RecvTime,
    source_time: Option<u64>,
}

/// A message kept, as it was received.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeptMessage<'a> {
    /// Its stream.
    pub stream: StreamIndex,
    /// Its payload, as received.
    pub payload: &'a [u8],
    /// When Kapto received it.
    pub received: RecvTime,
    /// The time its source gave it, in nanoseconds since the Unix epoch.
    pub source_time: Option<u64>,
}

impl Kept {
    /// Sets aside the memory for `streams`, each of `depth` messages.
    #[must_use]
    pub fn new(streams: &[KeptStream]) -> Self {
        let count = streams
            .iter()
            .map(|kept| usize::from(kept.stream) + 1)
            .max()
            .unwrap_or(0);
        let mut by_stream = vec![None; count].into_boxed_slice();
        let streams: Vec<Slots> = streams
            .iter()
            .enumerate()
            .map(|(place, kept)| {
                by_stream[usize::from(kept.stream)] = Some(place);
                Slots {
                    stream: kept.stream,
                    messages: (0..kept.depth.max(1))
                        .map(|_| Message {
                            data: vec![0; kept.max_message_len].into_boxed_slice(),
                            len: 0,
                            received: RecvTime::from_nanos(0),
                            source_time: None,
                        })
                        .collect(),
                    next: 0,
                    held: 0,
                }
            })
            .collect();
        let held = KeptHeld((0..count).map(|_| AtomicBool::new(false)).collect());
        Self {
            by_stream,
            streams,
            held,
        }
    }

    /// Which streams have a message kept, as it changes.
    #[must_use]
    pub fn held(&self) -> KeptHeld {
        self.held.clone()
    }

    /// Whether no stream's messages are kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Keeps each message of `batch` whose stream is kept.
    pub fn keep(&mut self, batch: &Batch) {
        if self.streams.is_empty() {
            return;
        }
        for (index, (stream, payload)) in batch.messages().enumerate() {
            let Some(Some(place)) = self.by_stream.get(usize::from(stream)) else {
                continue;
            };
            let slots = &mut self.streams[*place];
            let message = &mut slots.messages[slots.next];
            let len = payload.len().min(message.data.len());
            message.data[..len].copy_from_slice(&payload[..len]);
            message.len = len;
            message.received = batch.recv_time();
            message.source_time = batch.source_time(index);
            slots.next = (slots.next + 1) % slots.messages.len();
            if slots.held == 0
                && let Some(held) = self.held.0.get(usize::from(stream))
            {
                held.store(true, Ordering::Relaxed);
            }
            slots.held = (slots.held + 1).min(slots.messages.len());
        }
    }

    /// Every message kept, stream by stream in the order given, oldest
    /// first.
    pub fn messages(&self) -> impl Iterator<Item = KeptMessage<'_>> {
        self.streams.iter().flat_map(|slots| {
            let depth = slots.messages.len();
            let oldest = (slots.next + depth - slots.held) % depth;
            (0..slots.held).map(move |offset| {
                let message = &slots.messages[(oldest + offset) % depth];
                KeptMessage {
                    stream: slots.stream,
                    payload: &message.data[..message.len],
                    received: message.received,
                    source_time: message.source_time,
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(at: u64, messages: &[(StreamIndex, &[u8])]) -> Batch {
        let mut batch = Batch::new(8, 16);
        batch.set_recv_time(RecvTime::from_nanos(at));
        for (stream, payload) in messages {
            assert!(batch.push(*stream, payload));
        }
        batch
    }

    fn kept(kept: &Kept) -> Vec<(StreamIndex, Vec<u8>, u64)> {
        kept.messages()
            .map(|message| {
                (
                    message.stream,
                    message.payload.to_vec(),
                    message.received.as_nanos(),
                )
            })
            .collect()
    }

    #[test]
    fn only_kept_streams_newest_messages_are_kept_oldest_first() {
        let mut store = Kept::new(&[
            KeptStream {
                stream: 2,
                depth: 2,
                max_message_len: 16,
            },
            KeptStream {
                stream: 0,
                depth: 1,
                max_message_len: 16,
            },
        ]);
        let held = store.held();
        assert!(!held.has(2));
        store.keep(&batch(10, &[(1, b"moving"), (2, b"camera"), (0, b"map")]));
        assert!(held.has(2) && held.has(0) && !held.has(1));
        assert_eq!(
            kept(&store),
            [(2, b"camera".to_vec(), 10), (0, b"map".to_vec(), 10)]
        );
        store.keep(&batch(20, &[(2, b"gripper"), (2, b"lidar"), (0, b"map2")]));
        assert_eq!(
            kept(&store),
            [
                (2, b"gripper".to_vec(), 20),
                (2, b"lidar".to_vec(), 20),
                (0, b"map2".to_vec(), 20)
            ]
        );
    }

    #[test]
    fn nothing_is_kept_without_kept_streams_or_before_a_message_arrives() {
        let mut none = Kept::new(&[]);
        assert!(none.is_empty());
        none.keep(&batch(10, &[(0, b"moving")]));
        assert_eq!(none.messages().count(), 0);
        let waiting = Kept::new(&[KeptStream {
            stream: 0,
            depth: 3,
            max_message_len: 16,
        }]);
        assert_eq!(waiting.messages().count(), 0);
    }
}
