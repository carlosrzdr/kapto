// SPDX-License-Identifier: Apache-2.0

//! Cutting messages out of a TCP byte stream.
//!
//! TCP delivers bytes, not messages: one read can hold half a message or
//! several. A [`Framing`] says how the robot marks where each message ends,
//! and a [`Deframer`] keeps the bytes between reads and hands out complete
//! frames.
//!
//! A frame longer than the largest message the stream may send is skipped,
//! not stored: the deframer counts it and keeps its place in the stream, so
//! the frames after it still come out whole.

use std::ops::Range;

/// The byte order of a length field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LengthOrder {
    /// Least significant byte first.
    #[default]
    Little,
    /// Most significant byte first, as most network protocols send it.
    Big,
}

/// How the robot marks message boundaries in its byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Framing {
    /// Each message starts with a header holding its length.
    LengthPrefix {
        /// Bytes in the header, including the length field.
        header_bytes: usize,
        /// Where in the header the length field is.
        length_offset: usize,
        /// Size of the length field: 1, 2, 4 or 8 bytes.
        length_bytes: usize,
        /// Byte order of the length field.
        order: LengthOrder,
        /// Whether the length counts the header too, rather than only what
        /// follows it.
        includes_header: bool,
    },
    /// Each message ends with these bytes, e.g. a newline. The delimiter is
    /// not part of the message.
    Delimiter(Vec<u8>),
    /// Every message has exactly this many bytes.
    Fixed(usize),
}

impl Framing {
    /// What's wrong with this framing, if anything.
    ///
    /// # Errors
    ///
    /// A sentence naming the problem: a length field outside the header or of
    /// an unsupported size, an empty delimiter, or a fixed size of 0.
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::LengthPrefix {
                header_bytes,
                length_offset,
                length_bytes,
                ..
            } => {
                if ![1, 2, 4, 8].contains(length_bytes) {
                    return Err(format!(
                        "the length field has {length_bytes} bytes; it must have 1, 2, 4 or 8"
                    ));
                }
                if length_offset + length_bytes > *header_bytes {
                    return Err(format!(
                        "the length field (bytes {length_offset} to {}) doesn't fit in the {header_bytes}-byte header",
                        length_offset + length_bytes - 1
                    ));
                }
                Ok(())
            }
            Self::Delimiter(delimiter) if delimiter.is_empty() => {
                Err("the delimiter is empty".to_owned())
            }
            Self::Fixed(0) => Err("messages of 0 bytes can't be told apart".to_owned()),
            Self::Delimiter(_) | Self::Fixed(_) => Ok(()),
        }
    }

    /// The next frame at the start of `bytes`, if it is complete.
    fn next(&self, bytes: &[u8], max_frame: usize) -> Next {
        match self {
            Self::LengthPrefix {
                header_bytes,
                length_offset,
                length_bytes,
                order,
                includes_header,
            } => {
                let Some(field) = bytes.get(*length_offset..length_offset + length_bytes) else {
                    return Next::NeedMore;
                };
                let length = field_value(field, *order);
                let total = if *includes_header {
                    // A length shorter than the header is nonsense; treat the
                    // header alone as the frame so the stream moves on.
                    length.max(*header_bytes as u64)
                } else {
                    length.saturating_add(*header_bytes as u64)
                };
                if total > max_frame as u64 {
                    return Next::TooLong { skip: total };
                }
                // `total` fits in `usize`: it is at most `max_frame`.
                let total = usize::try_from(total).unwrap_or(usize::MAX);
                if bytes.len() < total {
                    return Next::NeedMore;
                }
                Next::Frame {
                    frame: 0..total,
                    consumed: total,
                }
            }
            Self::Delimiter(delimiter) => match find(bytes, delimiter) {
                Some(end) if end > max_frame => Next::TooLong {
                    skip: (end + delimiter.len()) as u64,
                },
                Some(end) => Next::Frame {
                    frame: 0..end,
                    consumed: end + delimiter.len(),
                },
                // No delimiter in more bytes than a frame may have: the
                // frame is too long, and everything up to its delimiter,
                // whenever that arrives, is dropped.
                None if bytes.len() > max_frame + delimiter.len() => Next::TooLongUntilDelimiter,
                None => Next::NeedMore,
            },
            Self::Fixed(size) if *size > max_frame => Next::TooLong { skip: *size as u64 },
            Self::Fixed(size) if bytes.len() >= *size => Next::Frame {
                frame: 0..*size,
                consumed: *size,
            },
            Self::Fixed(_) => Next::NeedMore,
        }
    }
}

/// An unsigned length field.
fn field_value(field: &[u8], order: LengthOrder) -> u64 {
    let mut value = 0_u64;
    let mut add = |byte: &u8| value = (value << 8) | u64::from(*byte);
    match order {
        LengthOrder::Big => field.iter().for_each(&mut add),
        LengthOrder::Little => field.iter().rev().for_each(&mut add),
    }
    value
}

/// Where `needle` first occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// What the bytes at the front hold.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Next {
    /// A complete frame: its bytes, and how many bytes to drop after taking it.
    Frame {
        frame: Range<usize>,
        consumed: usize,
    },
    /// A frame longer than allowed: drop this many bytes, some possibly not
    /// received yet.
    TooLong { skip: u64 },
    /// A delimited frame longer than allowed whose end hasn't arrived: drop
    /// everything up to and including the next delimiter.
    TooLongUntilDelimiter,
    /// Not enough bytes to tell yet.
    NeedMore,
}

/// Keeps a connection's unread bytes and hands out complete frames.
///
/// Its buffer is allocated once: room for the largest frame plus one read.
/// Taking frames never allocates.
#[derive(Debug)]
pub struct Deframer {
    framing: Framing,
    max_frame: usize,
    buffer: Box<[u8]>,
    /// Bytes `start..end` of the buffer are received and not yet framed.
    start: usize,
    end: usize,
    /// Bytes of an over-long frame still to be dropped as they arrive.
    skipping: u64,
    /// An over-long delimited frame is being dropped up to its delimiter.
    discarding: bool,
    too_long: u64,
}

impl Deframer {
    /// A deframer for frames of at most `max_frame` bytes, reading up to
    /// `read_bytes` at a time.
    #[must_use]
    pub fn new(framing: Framing, max_frame: usize, read_bytes: usize) -> Self {
        let delimiter = match &framing {
            Framing::Delimiter(delimiter) => delimiter.len(),
            _ => 0,
        };
        Self {
            framing,
            max_frame,
            buffer: vec![0; max_frame + delimiter + read_bytes.max(1)].into_boxed_slice(),
            start: 0,
            end: 0,
            skipping: 0,
            discarding: false,
            too_long: 0,
        }
    }

    /// The free space to read into. Call [`Deframer::filled`] with how many
    /// bytes were read.
    pub fn space(&mut self) -> &mut [u8] {
        if self.start > 0 {
            // Move the unframed bytes to the front, making room at the end.
            self.buffer.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        &mut self.buffer[self.end..]
    }

    /// Records that `read` bytes were written into [`Deframer::space`].
    pub fn filled(&mut self, read: usize) {
        self.end = (self.end + read).min(self.buffer.len());
    }

    /// The next complete frame, if there is one. Frames too long are skipped
    /// and counted on the way.
    pub fn next_frame(&mut self) -> Option<&[u8]> {
        loop {
            if self.skipping > 0 {
                let available = (self.end - self.start) as u64;
                let dropped = self.skipping.min(available);
                // At most the bytes available, so it fits in `usize`.
                self.start += usize::try_from(dropped).unwrap_or(0);
                self.skipping -= dropped;
                if self.skipping > 0 {
                    return None;
                }
            }
            if self.discarding && !self.discard_to_delimiter() {
                return None;
            }
            match self
                .framing
                .next(&self.buffer[self.start..self.end], self.max_frame)
            {
                Next::Frame { frame, consumed } => {
                    let range = self.start + frame.start..self.start + frame.end;
                    self.start += consumed;
                    return Some(&self.buffer[range]);
                }
                Next::TooLong { skip } => {
                    self.too_long += 1;
                    self.skipping = skip;
                }
                Next::TooLongUntilDelimiter => {
                    self.too_long += 1;
                    self.discarding = true;
                }
                Next::NeedMore => return None,
            }
        }
    }

    /// Drops bytes up to and including the next delimiter. Returns whether it
    /// was found; if not, keeps only what could be the start of one.
    fn discard_to_delimiter(&mut self) -> bool {
        let Framing::Delimiter(delimiter) = &self.framing else {
            self.discarding = false;
            return true;
        };
        let available = &self.buffer[self.start..self.end];
        if let Some(at) = find(available, delimiter) {
            self.start += at + delimiter.len();
            self.discarding = false;
            return true;
        }
        let keep = (delimiter.len() - 1).min(available.len());
        self.start = self.end - keep;
        false
    }

    /// Frames skipped because they were longer than the largest allowed.
    #[must_use]
    pub fn too_long(&self) -> u64 {
        self.too_long
    }

    /// Bytes received and not yet part of a returned frame.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.end - self.start
    }

    /// Forgets everything unread, e.g. after the connection drops: a new
    /// connection starts with a new frame.
    pub fn reset(&mut self) {
        self.start = 0;
        self.end = 0;
        self.skipping = 0;
        self.discarding = false;
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn length_prefix(includes_header: bool) -> Framing {
        Framing::LengthPrefix {
            header_bytes: 4,
            length_offset: 2,
            length_bytes: 2,
            order: LengthOrder::Big,
            includes_header,
        }
    }

    /// Feeds `bytes` in chunks of the given sizes and collects every frame.
    fn frames(deframer: &mut Deframer, bytes: &[u8], chunks: &[usize]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut rest = bytes;
        let mut chunks = chunks.iter().cycle();
        while !rest.is_empty() {
            let size = (*chunks.next().unwrap()).clamp(1, rest.len());
            let space = deframer.space();
            let size = size.min(space.len());
            space[..size].copy_from_slice(&rest[..size]);
            deframer.filled(size);
            rest = &rest[size..];
            while let Some(frame) = deframer.next_frame() {
                out.push(frame.to_vec());
            }
        }
        out
    }

    /// A length-prefixed message: 2 type bytes, a 2-byte big-endian length,
    /// then the body.
    fn prefixed(body: &[u8], includes_header: bool) -> Vec<u8> {
        let length = body.len() + if includes_header { 4 } else { 0 };
        let mut message = vec![0xAB, 0xCD];
        message.extend(u16::try_from(length).unwrap().to_be_bytes());
        message.extend(body);
        message
    }

    #[test]
    fn length_prefixed_frames_include_their_header() {
        for includes_header in [false, true] {
            let mut bytes = prefixed(b"joints", includes_header);
            bytes.extend(prefixed(b"", includes_header));
            let mut deframer = Deframer::new(length_prefix(includes_header), 64, 16);
            assert_eq!(
                frames(&mut deframer, &bytes, &[100]),
                [
                    prefixed(b"joints", includes_header),
                    prefixed(b"", includes_header)
                ]
            );
        }
    }

    #[test]
    fn little_endian_and_one_byte_lengths_are_read() {
        let framing = Framing::LengthPrefix {
            header_bytes: 1,
            length_offset: 0,
            length_bytes: 1,
            order: LengthOrder::Little,
            includes_header: false,
        };
        let mut deframer = Deframer::new(framing, 64, 16);
        assert_eq!(
            frames(&mut deframer, b"\x02ab\x01c", &[1]),
            [b"\x02ab".to_vec(), b"\x01c".to_vec()]
        );
        assert_eq!(field_value(&[0x01, 0x02], LengthOrder::Little), 0x0201);
        assert_eq!(field_value(&[0x01, 0x02], LengthOrder::Big), 0x0102);
    }

    #[test]
    fn delimited_frames_leave_the_delimiter_out() {
        let mut deframer = Deframer::new(Framing::Delimiter(b"\r\n".to_vec()), 64, 16);
        assert_eq!(
            frames(&mut deframer, b"{\"a\":1}\r\n\r\n{\"b\":2}\r\n", &[3]),
            [b"{\"a\":1}".to_vec(), Vec::new(), b"{\"b\":2}".to_vec()]
        );
    }

    #[test]
    fn fixed_size_frames_are_cut_every_n_bytes() {
        let mut deframer = Deframer::new(Framing::Fixed(3), 64, 16);
        assert_eq!(
            frames(&mut deframer, b"abcdefg", &[2]),
            [b"abc".to_vec(), b"def".to_vec()]
        );
        assert_eq!(deframer.pending(), 1);
    }

    #[test]
    fn a_length_prefixed_frame_too_long_is_skipped_and_the_next_still_comes_out() {
        let mut bytes = prefixed(&[7; 50], false);
        bytes.extend(prefixed(b"ok", false));
        let mut deframer = Deframer::new(length_prefix(false), 20, 8);
        assert_eq!(
            frames(&mut deframer, &bytes, &[5]),
            [prefixed(b"ok", false)]
        );
        assert_eq!(deframer.too_long(), 1);
    }

    #[test]
    fn a_delimited_frame_too_long_is_skipped_up_to_its_delimiter() {
        let mut bytes = vec![b'x'; 100];
        bytes.extend(b"\nok\n");
        let mut deframer = Deframer::new(Framing::Delimiter(b"\n".to_vec()), 10, 8);
        assert_eq!(frames(&mut deframer, &bytes, &[7]), [b"ok".to_vec()]);
        assert_eq!(deframer.too_long(), 1);
    }

    #[test]
    fn a_delimited_frame_too_long_arriving_slowly_is_dropped_until_its_delimiter() {
        let mut bytes = vec![b'x'; 300];
        bytes.extend(b"END");
        bytes.extend(b"okEND");
        let mut deframer = Deframer::new(Framing::Delimiter(b"END".to_vec()), 10, 4);
        assert_eq!(frames(&mut deframer, &bytes, &[1, 2, 3]), [b"ok".to_vec()]);
        assert_eq!(deframer.too_long(), 1);
    }

    #[test]
    fn a_split_delimiter_is_still_found() {
        let mut deframer = Deframer::new(Framing::Delimiter(b"END".to_vec()), 4, 8);
        assert_eq!(
            frames(&mut deframer, b"abcdEND", &[5, 1, 1]),
            [b"abcd".to_vec()]
        );
        assert_eq!(deframer.too_long(), 0);
    }

    #[test]
    fn resetting_drops_a_half_received_frame() {
        let mut deframer = Deframer::new(Framing::Fixed(4), 64, 16);
        assert_eq!(frames(&mut deframer, b"ab", &[2]), Vec::<Vec<u8>>::new());
        deframer.reset();
        assert_eq!(frames(&mut deframer, b"wxyz", &[4]), [b"wxyz".to_vec()]);
    }

    #[test]
    fn framings_are_checked() {
        let bad_size = Framing::LengthPrefix {
            header_bytes: 4,
            length_offset: 0,
            length_bytes: 3,
            order: LengthOrder::Big,
            includes_header: false,
        };
        assert!(
            bad_size
                .check()
                .unwrap_err()
                .contains("must have 1, 2, 4 or 8")
        );
        let outside = Framing::LengthPrefix {
            header_bytes: 4,
            length_offset: 3,
            length_bytes: 2,
            order: LengthOrder::Big,
            includes_header: false,
        };
        assert!(
            outside
                .check()
                .unwrap_err()
                .contains("doesn't fit in the 4-byte header")
        );
        assert!(Framing::Delimiter(Vec::new()).check().is_err());
        assert!(Framing::Fixed(0).check().is_err());
        assert!(length_prefix(true).check().is_ok());
    }

    fn framing() -> impl Strategy<Value = Framing> {
        prop_oneof![
            any::<bool>().prop_map(length_prefix),
            Just(Framing::Delimiter(b"\n".to_vec())),
            Just(Framing::Delimiter(b"<|>".to_vec())),
            (1_usize..16).prop_map(Framing::Fixed),
        ]
    }

    /// Bodies that can be framed with `framing`: no delimiter inside, the
    /// right size for fixed frames.
    fn encode(framing: &Framing, bodies: &[Vec<u8>]) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut stream = Vec::new();
        let mut expected = Vec::new();
        for body in bodies {
            match framing {
                Framing::LengthPrefix {
                    includes_header, ..
                } => {
                    let message = prefixed(body, *includes_header);
                    stream.extend(&message);
                    expected.push(message);
                }
                Framing::Delimiter(delimiter) => {
                    let mut body = body.clone();
                    body.retain(|byte| !delimiter.contains(byte));
                    stream.extend(&body);
                    stream.extend(delimiter);
                    expected.push(body);
                }
                Framing::Fixed(size) => {
                    let mut body = body.clone();
                    body.resize(*size, 0x55);
                    stream.extend(&body);
                    expected.push(body);
                }
            }
        }
        (stream, expected)
    }

    proptest! {
        #[test]
        fn any_split_of_the_stream_gives_the_same_frames(
            framing in framing(),
            bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..40), 0..30),
            chunks in prop::collection::vec(1_usize..64, 1..10),
            read_bytes in 1_usize..64,
        ) {
            let (stream, expected) = encode(&framing, &bodies);
            let mut deframer = Deframer::new(framing, 64, read_bytes);
            prop_assert_eq!(frames(&mut deframer, &stream, &chunks), expected);
            prop_assert_eq!(deframer.too_long(), 0);
        }

        #[test]
        fn frames_too_long_are_counted_and_the_rest_come_out_whole(
            framing in prop_oneof![
                any::<bool>().prop_map(length_prefix),
                Just(Framing::Delimiter(b"\n".to_vec())),
                Just(Framing::Delimiter(b"<|>".to_vec())),
            ],
            bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..90), 0..30),
            chunks in prop::collection::vec(1_usize..64, 1..10),
        ) {
            const MAX_FRAME: usize = 40;
            let (stream, encoded) = encode(&framing, &bodies);
            let (kept, long): (Vec<_>, Vec<_>) =
                encoded.into_iter().partition(|frame| frame.len() <= MAX_FRAME);
            let mut deframer = Deframer::new(framing, MAX_FRAME, 16);
            prop_assert_eq!(frames(&mut deframer, &stream, &chunks), kept);
            prop_assert_eq!(deframer.too_long(), long.len() as u64);
        }

        #[test]
        fn arbitrary_bytes_never_panic(
            framing in framing(),
            bytes in prop::collection::vec(any::<u8>(), 0..500),
            chunks in prop::collection::vec(1_usize..64, 1..10),
        ) {
            let mut deframer = Deframer::new(framing, 16, 8);
            let _ = frames(&mut deframer, &bytes, &chunks);
        }
    }
}
