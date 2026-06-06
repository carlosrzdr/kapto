// SPDX-License-Identifier: Apache-2.0

//! Upstream loss from the sequence numbers a source writes into its messages.

/// Where a stream's messages carry their sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceField {
    /// Byte offset of the number in the payload.
    pub offset: usize,
    /// Its size; the number wraps to zero after its largest value.
    pub width: SequenceWidth,
    /// Its byte order.
    pub byte_order: ByteOrder,
}

/// The size of a sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceWidth {
    /// 1 byte.
    U8,
    /// 2 bytes.
    U16,
    /// 4 bytes.
    U32,
    /// 8 bytes.
    U64,
}

impl SequenceWidth {
    fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
            Self::U64 => 8,
        }
    }

    fn mask(self) -> u64 {
        match self {
            Self::U64 => u64::MAX,
            width => (1 << (8 * width.bytes())) - 1,
        }
    }
}

/// The order of a number's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    /// Least significant byte first, as on x86 and most ARM systems.
    Little,
    /// Most significant byte first, as in network protocols.
    Big,
}

impl SequenceField {
    /// Reads the sequence number, or `None` if the payload is too short to
    /// hold it.
    #[must_use]
    pub fn read(&self, payload: &[u8]) -> Option<u64> {
        let len = self.width.bytes();
        let bytes = payload.get(self.offset..self.offset.checked_add(len)?)?;
        let mut number = [0; 8];
        Some(match self.byte_order {
            ByteOrder::Little => {
                number[..len].copy_from_slice(bytes);
                u64::from_le_bytes(number)
            }
            ByteOrder::Big => {
                number[8 - len..].copy_from_slice(bytes);
                u64::from_be_bytes(number)
            }
        })
    }
}

/// How many recent sequence numbers are remembered, so a message that
/// arrives late can fill the gap it left instead of looking like a duplicate.
const WINDOW: u64 = 64 * WINDOW_WORDS as u64;
const WINDOW_WORDS: usize = 16;

/// Numbers this far behind the newest one, arriving in an unbroken run this
/// long, mean the source restarted its count rather than sent stale messages.
const RESTART_RUN: u32 = 8;

/// Follows one stream's sequence numbers and works out how many messages
/// never arrived.
///
/// A jump forward counts the numbers skipped as lost. A number from the
/// skipped range arriving later (reordered) takes one back off. A repeated
/// number changes nothing. Numbers compare across wrap-around, so a jump
/// forward is anything up to half the number range.
#[derive(Clone, Debug)]
pub struct SequenceTracker {
    mask: u64,
    window: u64,
    newest: Option<u64>,
    /// One bit per number in the window behind `newest`: set once received,
    /// or when it can't be counted (before the first message).
    seen: [u64; WINDOW_WORDS],
    stale_run: Option<(u64, u32)>,
}

impl SequenceTracker {
    /// A tracker for numbers of the given width, before any message.
    #[must_use]
    pub fn new(width: SequenceWidth) -> Self {
        let mask = width.mask();
        Self {
            mask,
            // Smaller than half the range, so "behind" is never "ahead".
            window: WINDOW.min(mask / 2 + 1),
            newest: None,
            seen: [u64::MAX; WINDOW_WORDS],
            stale_run: None,
        }
    }

    /// Takes the next number received and returns the change in the number
    /// of messages lost: positive for a gap, -1 for a late arrival that
    /// fills one, zero otherwise.
    pub fn observe(&mut self, number: u64) -> i64 {
        let number = number & self.mask;
        let Some(newest) = self.newest else {
            self.restart(number);
            return 0;
        };
        let ahead = number.wrapping_sub(newest) & self.mask;
        if ahead == 0 {
            return 0;
        }
        if ahead <= self.mask / 2 {
            for skipped in 1..ahead.min(self.window) {
                self.set_seen(newest.wrapping_add(skipped), false);
            }
            if ahead >= self.window {
                self.seen = [0; WINDOW_WORDS];
            }
            self.set_seen(number, true);
            self.newest = Some(number);
            self.stale_run = None;
            return i64::try_from(ahead - 1).unwrap_or(i64::MAX);
        }
        let behind = newest.wrapping_sub(number) & self.mask;
        if behind < self.window {
            let late = !self.is_seen(number);
            self.set_seen(number, true);
            return if late { -1 } else { 0 };
        }
        // Far behind: stale, unless it starts a new count.
        let run = match self.stale_run {
            Some((previous, run)) if number == (previous + 1) & self.mask => run + 1,
            _ => 1,
        };
        if run >= RESTART_RUN {
            self.restart(number);
        } else {
            self.stale_run = Some((number, run));
        }
        0
    }

    fn restart(&mut self, number: u64) {
        self.newest = Some(number);
        self.seen = [u64::MAX; WINDOW_WORDS];
        self.stale_run = None;
    }

    fn bit(&self, number: u64) -> (usize, u64) {
        let slot = number % self.window;
        (usize::try_from(slot / 64).unwrap_or(0), 1 << (slot % 64))
    }

    fn is_seen(&self, number: u64) -> bool {
        let (word, bit) = self.bit(number);
        self.seen[word] & bit != 0
    }

    fn set_seen(&mut self, number: u64, seen: bool) {
        let (word, bit) = self.bit(number);
        if seen {
            self.seen[word] |= bit;
        } else {
            self.seen[word] &= !bit;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn lost(width: SequenceWidth, numbers: &[u64]) -> i64 {
        let mut tracker = SequenceTracker::new(width);
        numbers.iter().map(|&number| tracker.observe(number)).sum()
    }

    #[test]
    fn reads_numbers_of_every_width_in_both_byte_orders() {
        let payload = [0xaa, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let field = |width, byte_order| SequenceField {
            offset: 1,
            width,
            byte_order,
        };
        assert_eq!(
            field(SequenceWidth::U8, ByteOrder::Big).read(&payload),
            Some(1)
        );
        assert_eq!(
            field(SequenceWidth::U16, ByteOrder::Little).read(&payload),
            Some(0x0201)
        );
        assert_eq!(
            field(SequenceWidth::U16, ByteOrder::Big).read(&payload),
            Some(0x0102)
        );
        assert_eq!(
            field(SequenceWidth::U32, ByteOrder::Little).read(&payload),
            Some(0x0403_0201)
        );
        assert_eq!(
            field(SequenceWidth::U64, ByteOrder::Big).read(&payload),
            Some(0x0102_0304_0506_0708)
        );
    }

    #[test]
    fn a_payload_too_short_for_the_number_reads_nothing() {
        let field = SequenceField {
            offset: usize::MAX,
            width: SequenceWidth::U32,
            byte_order: ByteOrder::Little,
        };
        assert_eq!(field.read(&[0; 16]), None);
        let field = SequenceField {
            offset: 14,
            ..field
        };
        assert_eq!(field.read(&[0; 16]), None);
    }

    #[test]
    fn a_gap_across_wraparound_counts_the_numbers_skipped() {
        assert_eq!(lost(SequenceWidth::U8, &[254, 255, 2]), 2);
        assert_eq!(lost(SequenceWidth::U16, &[65_534, 0, 1]), 1);
    }

    #[test]
    fn a_late_message_fills_its_gap_and_a_repeat_changes_nothing() {
        assert_eq!(lost(SequenceWidth::U32, &[1, 2, 5, 3, 3, 5, 4]), 0);
        assert_eq!(lost(SequenceWidth::U32, &[1, 4, 2, 2]), 1);
    }

    #[test]
    fn numbers_older_than_the_first_message_are_never_counted() {
        assert_eq!(lost(SequenceWidth::U32, &[10, 11, 8, 9, 12]), 0);
    }

    #[test]
    fn a_source_that_restarts_its_count_is_followed_without_counting_loss() {
        let mut numbers: Vec<u64> = (5_000..5_010).collect();
        numbers.extend(0..20);
        assert_eq!(lost(SequenceWidth::U32, &numbers), 0);
        numbers.extend([25]);
        assert_eq!(lost(SequenceWidth::U32, &numbers), 5);
    }

    #[test]
    fn a_few_stale_numbers_far_behind_are_ignored() {
        assert_eq!(lost(SequenceWidth::U32, &[5_000, 3, 4, 5_001, 5_003]), 1);
    }

    fn width() -> impl Strategy<Value = SequenceWidth> {
        prop_oneof![
            Just(SequenceWidth::U16),
            Just(SequenceWidth::U32),
            Just(SequenceWidth::U64),
        ]
    }

    proptest! {
        /// A source sends 0..n from a random start (so wrap-around happens);
        /// the network drops, repeats and locally reorders messages. Lost is
        /// exactly the numbers up to the highest delivered that never came.
        #[test]
        fn gaps_duplicates_reorders_and_wraps_give_the_exact_loss(
            width in width(),
            start in any::<u64>(),
            sent in 1_usize..2_000,
            drops in prop::collection::vec(any::<bool>(), 2_000),
            duplicates in prop::collection::vec((1_usize..2_000, 0_usize..30), 0..40),
            swaps in prop::collection::vec((1_usize..2_000, 1_usize..30), 0..80),
        ) {
            // The first message always arrives first; anything else may not.
            let mut delivered: Vec<usize> = (0..sent).filter(|&n| n == 0 || !drops[n]).collect();
            for (at, distance) in duplicates {
                if at < delivered.len() {
                    let copy = delivered[at];
                    delivered.insert((at + distance).min(delivered.len()), copy);
                }
            }
            for (at, distance) in swaps {
                if at + distance < delivered.len() {
                    delivered.swap(at, at + distance);
                }
            }
            let highest = *delivered.iter().max().unwrap();
            let mut distinct = delivered.clone();
            distinct.sort_unstable();
            distinct.dedup();
            let expected = i64::try_from(highest + 1 - distinct.len()).unwrap();

            let mask = width.mask();
            let numbers: Vec<u64> = delivered
                .iter()
                .map(|&n| start.wrapping_add(n as u64) & mask)
                .collect();
            prop_assert_eq!(lost(width, &numbers), expected);
        }
    }
}
