// SPDX-License-Identifier: Apache-2.0

//! Batches: fixed blocks of memory that carry received messages.

use kapto_core::RecvTime;

/// Which stream a message belongs to: an index into the session's stream
/// list, small and copyable for the hot path.
pub type StreamIndex = u16;

/// Where a message stands in what its publisher sent: which of its
/// stream's publishers sent it, and that publisher's count of it, from 1.
/// A ROS 2 middleware may give it with every message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Publication {
    /// Which of the stream's publishers, in the order they were first heard.
    pub publisher: u8,
    /// The publisher's count of this message, from 1; 0 for none.
    pub number: u64,
}

/// A fixed block of memory with room for `capacity` messages of up to
/// `max_message_len` bytes each, plus each message's length, stream and
/// source time, and one receive time for the whole batch.
///
/// A batch is allocated once and reused: filling and clearing it never
/// allocates. Slot `i` always starts at byte `i × max_message_len`, so a
/// receiver can hand every free slot to the operating system at once.
#[derive(Debug)]
pub struct Batch {
    data: Box<[u8]>,
    lens: Box<[u32]>,
    streams: Box<[StreamIndex]>,
    /// The time each message's source gave it, in nanoseconds; 0 for none.
    /// Only read, and kept up to date, once a message in the batch has one,
    /// so transports without source times never touch it.
    source_times: Box<[u64]>,
    any_source_time: bool,
    /// Each message's place in what its publisher sent; a number of 0 for
    /// none. Kept as the source times are, once a message has one.
    publications: Box<[Publication]>,
    any_publication: bool,
    max_message_len: usize,
    len: usize,
    recv_time: RecvTime,
}

impl Batch {
    /// Allocates a batch. This is the only allocation a batch ever makes.
    #[must_use]
    pub fn new(capacity: usize, max_message_len: usize) -> Self {
        Self {
            data: vec![0; capacity * max_message_len].into_boxed_slice(),
            lens: vec![0; capacity].into_boxed_slice(),
            streams: vec![0; capacity].into_boxed_slice(),
            source_times: vec![0; capacity].into_boxed_slice(),
            any_source_time: false,
            publications: vec![Publication::default(); capacity].into_boxed_slice(),
            any_publication: false,
            max_message_len,
            len: 0,
            recv_time: RecvTime::from_nanos(0),
        }
    }

    /// How many messages fit.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.lens.len()
    }

    /// The largest message a slot holds, in bytes.
    #[must_use]
    pub fn max_message_len(&self) -> usize {
        self.max_message_len
    }

    /// How many messages it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether it holds no messages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether every slot is used.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.len == self.capacity()
    }

    /// When the messages in this batch were received.
    #[must_use]
    pub fn recv_time(&self) -> RecvTime {
        self.recv_time
    }

    /// Sets when the messages in this batch were received.
    pub fn set_recv_time(&mut self, recv_time: RecvTime) {
        self.recv_time = recv_time;
    }

    /// The next free slot, to receive a message into, or `None` if the batch
    /// is full. Call [`Batch::commit`] once the message is in it.
    pub fn next_slot(&mut self) -> Option<&mut [u8]> {
        if self.is_full() {
            return None;
        }
        let start = self.len * self.max_message_len;
        Some(&mut self.data[start..start + self.max_message_len])
    }

    /// Records the message just written into the next free slot, without a
    /// source time. Lengths beyond the slot are cut to the slot size; the
    /// caller is responsible for counting a message that didn't fit. Does
    /// nothing if the batch is full.
    pub fn commit(&mut self, stream: StreamIndex, len: usize) {
        if self.is_full() {
            return;
        }
        let len = len.min(self.max_message_len);
        self.lens[self.len] = u32::try_from(len).unwrap_or(u32::MAX);
        self.streams[self.len] = stream;
        if self.any_source_time {
            self.source_times[self.len] = 0;
        }
        if self.any_publication {
            self.publications[self.len] = Publication::default();
        }
        self.len += 1;
    }

    /// Every free slot as one block of memory, for receiving several messages
    /// at once: free slot `k` is the [`Batch::max_message_len`] bytes starting
    /// at `k × max_message_len`. Call [`Batch::commit_from`] for each message
    /// received.
    pub fn free_slots(&mut self) -> &mut [u8] {
        &mut self.data[self.len * self.max_message_len..]
    }

    /// Records a message received into free slot `free_slot` of
    /// [`Batch::free_slots`], counted from the next free slot. If earlier
    /// free slots hold messages that were not kept (`free_slot > 0`), the
    /// message is moved down into the next free slot, so slots stay in
    /// arrival order. Lengths beyond the slot are cut to the slot size. Does
    /// nothing if there is no such free slot.
    pub fn commit_from(&mut self, free_slot: usize, stream: StreamIndex, len: usize) {
        if self.len + free_slot >= self.capacity() {
            return;
        }
        if free_slot > 0 {
            let slot_len = self.max_message_len;
            let from = (self.len + free_slot) * slot_len;
            self.data
                .copy_within(from..from + len.min(slot_len), self.len * slot_len);
        }
        self.commit(stream, len);
    }

    /// Copies a message into the next free slot. Returns `false`, storing
    /// nothing, if the batch is full or the message is longer than a slot.
    pub fn push(&mut self, stream: StreamIndex, message: &[u8]) -> bool {
        if message.len() > self.max_message_len {
            return false;
        }
        let Some(slot) = self.next_slot() else {
            return false;
        };
        slot[..message.len()].copy_from_slice(message);
        self.commit(stream, message.len());
        true
    }

    /// Copies a message into the next free slot as [`Batch::push`] does,
    /// with the time its source gave it, in nanoseconds since the Unix
    /// epoch, as the source has it (0, the epoch itself, is taken as none).
    pub fn push_with_source_time(
        &mut self,
        stream: StreamIndex,
        message: &[u8],
        source_time_ns: u64,
    ) -> bool {
        if !self.push(stream, message) {
            return false;
        }
        if !self.any_source_time {
            // The first in this batch: the messages before it have none,
            // whatever a reused slot held.
            self.source_times[..self.len].fill(0);
            self.any_source_time = true;
        }
        self.source_times[self.len - 1] = source_time_ns;
        true
    }

    /// Copies a message into the next free slot as
    /// [`Batch::push_with_source_time`] does, with its place in what its
    /// publisher sent.
    pub fn push_from_publisher(
        &mut self,
        stream: StreamIndex,
        message: &[u8],
        source_time_ns: u64,
        publication: Publication,
    ) -> bool {
        if !self.push_with_source_time(stream, message, source_time_ns) {
            return false;
        }
        if !self.any_publication {
            // The first in this batch, as for source times.
            self.publications[..self.len].fill(Publication::default());
            self.any_publication = true;
        }
        self.publications[self.len - 1] = publication;
        true
    }

    /// Message `index`'s place in what its publisher sent, if it was given
    /// one.
    #[must_use]
    pub fn publication(&self, index: usize) -> Option<Publication> {
        if !self.any_publication {
            return None;
        }
        self.publications
            .get(..self.len)
            .and_then(|publications| publications.get(index))
            .copied()
            .filter(|publication| publication.number != 0)
    }

    /// The time message `index` was given by its source, in nanoseconds
    /// since the Unix epoch, if it was given one.
    #[must_use]
    pub fn source_time(&self, index: usize) -> Option<u64> {
        if !self.any_source_time {
            return None;
        }
        self.source_times
            .get(..self.len)
            .and_then(|times| times.get(index))
            .copied()
            .filter(|&time| time != 0)
    }

    /// The messages in arrival order, each with its stream.
    pub fn messages(&self) -> impl ExactSizeIterator<Item = (StreamIndex, &[u8])> {
        (0..self.len).map(|index| {
            let start = index * self.max_message_len;
            let len = self.lens[index] as usize;
            (self.streams[index], &self.data[start..start + len])
        })
    }

    /// Whether any message in it was given a source time.
    #[must_use]
    pub fn has_source_times(&self) -> bool {
        self.any_source_time
    }

    /// Forgets every message, keeping the memory.
    pub fn clear(&mut self) {
        self.len = 0;
        self.any_source_time = false;
        self.any_publication = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_come_back_in_order_with_their_streams() {
        let mut batch = Batch::new(4, 16);
        assert!(batch.push(2, b"first"));
        assert!(batch.push(0, b""));
        assert!(batch.push(1, b"third message"));
        let messages: Vec<_> = batch.messages().collect();
        assert_eq!(
            messages,
            [
                (2, &b"first"[..]),
                (0, &b""[..]),
                (1, &b"third message"[..])
            ]
        );
    }

    #[test]
    fn a_full_batch_refuses_more_messages() {
        let mut batch = Batch::new(2, 8);
        assert!(batch.push(0, b"a"));
        assert!(batch.push(0, b"b"));
        assert!(batch.is_full());
        assert!(!batch.push(0, b"c"));
        assert!(batch.next_slot().is_none());
        assert_eq!(batch.len(), 2);
    }

    #[test]
    fn a_message_longer_than_a_slot_is_refused_by_push() {
        let mut batch = Batch::new(2, 4);
        assert!(!batch.push(0, b"too long"));
        assert!(batch.is_empty());
    }

    #[test]
    fn committing_a_longer_length_is_cut_to_the_slot() {
        let mut batch = Batch::new(1, 4);
        batch.next_slot().unwrap().copy_from_slice(b"abcd");
        batch.commit(3, 100);
        assert_eq!(batch.messages().next(), Some((3, &b"abcd"[..])));
    }

    #[test]
    fn receiving_into_a_slot_then_committing_stores_the_message() {
        let mut batch = Batch::new(2, 8);
        let slot = batch.next_slot().unwrap();
        assert_eq!(slot.len(), 8);
        slot[..3].copy_from_slice(b"xyz");
        batch.commit(5, 3);
        assert_eq!(batch.messages().collect::<Vec<_>>(), [(5, &b"xyz"[..])]);
    }

    #[test]
    fn a_message_received_past_a_discarded_one_moves_down_to_keep_slots_in_order() {
        let mut batch = Batch::new(4, 4);
        batch.push(0, b"kept");
        let free = batch.free_slots();
        assert_eq!(free.len(), 3 * 4);
        free[..4].copy_from_slice(b"drop");
        free[4..7].copy_from_slice(b"new");
        batch.commit_from(1, 2, 3);
        assert_eq!(
            batch.messages().collect::<Vec<_>>(),
            [(0, &b"kept"[..]), (2, &b"new"[..])]
        );
        batch.commit_from(2, 1, 1);
        assert_eq!(batch.len(), 2, "free slot 2 is past the end");
    }

    #[test]
    fn a_source_time_belongs_to_its_message_and_a_reused_slot_forgets_it() {
        let mut batch = Batch::new(2, 8);
        assert!(batch.push_with_source_time(0, b"a", 1_700_000_000_000_000_001));
        assert!(batch.push(1, b"b"));
        assert_eq!(
            (
                batch.source_time(0),
                batch.source_time(1),
                batch.source_time(2)
            ),
            (Some(1_700_000_000_000_000_001), None, None)
        );
        assert!(!batch.push_with_source_time(0, b"c", 5), "full");
        batch.clear();
        assert_eq!(batch.source_time(0), None, "no message there any more");
        assert!(batch.push(0, b"d"));
        assert_eq!(batch.source_time(0), None, "not the earlier message's");
        assert!(batch.push_with_source_time(1, b"e", 7));
        assert_eq!(
            (batch.source_time(0), batch.source_time(1)),
            (None, Some(7)),
            "a message before the batch's first source time has none"
        );
    }

    #[test]
    fn a_publishers_count_belongs_to_its_message_and_a_reused_slot_forgets_it() {
        let counted = |number| Publication {
            publisher: 2,
            number,
        };
        let mut batch = Batch::new(3, 8);
        assert!(batch.push_with_source_time(0, b"a", 5));
        assert!(batch.push_from_publisher(0, b"b", 6, counted(41)));
        assert!(batch.push(1, b"c"));
        assert_eq!(
            (
                batch.publication(0),
                batch.publication(1),
                batch.publication(2)
            ),
            (None, Some(counted(41)), None)
        );
        assert_eq!(batch.source_time(1), Some(6), "with its source time");
        batch.clear();
        assert!(batch.push_with_source_time(0, b"d", 7));
        assert_eq!(batch.publication(0), None, "not the earlier message's");
    }

    #[test]
    fn clearing_keeps_capacity_and_forgets_messages() {
        let mut batch = Batch::new(3, 8);
        batch.push(0, b"a");
        batch.set_recv_time(RecvTime::from_nanos(42));
        batch.clear();
        assert!(batch.is_empty());
        assert_eq!(batch.capacity(), 3);
        assert_eq!(batch.messages().count(), 0);
        assert_eq!(batch.recv_time(), RecvTime::from_nanos(42));
    }
}
