// SPDX-License-Identifier: Apache-2.0

//! A TCP stream cut into frames: any framing that passes its check, any
//! bytes, arriving in any pieces. The frames, and how many were too long,
//! must not depend on how the bytes were split into reads, and each frame
//! must be one its framing allows.

#![no_main]

use kapto_transport::tcp::{Deframer, Framing, LengthOrder};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = run(data);
});

fn run(data: &[u8]) -> Result<()> {
    let mut input = Unstructured::new(data);
    let framing = framing(&mut input)?;
    assert!(framing.check().is_ok(), "{framing:?}");
    let max_frame = input.int_in_range(1..=256_usize)?;
    // A real connection reads 64 KiB at a time, more than any header.
    let header = match &framing {
        Framing::LengthPrefix { header_bytes, .. } => *header_bytes,
        _ => 0,
    };
    let read_bytes = input.int_in_range(1..=64_usize)?.max(header);
    let mut seed = input.arbitrary::<u64>()? | 1;
    let stream = input.take_rest();

    let one_by_one = frames(&framing, max_frame, read_bytes, stream, || 1);
    let pieces = frames(&framing, max_frame, read_bytes, stream, || {
        // xorshift: piece sizes from the input, without using up its bytes.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed % read_bytes as u64).unwrap_or(0) + 1
    });
    assert_eq!(one_by_one, pieces, "{framing:?}, frames of at most {max_frame}");
    for frame in &one_by_one.0 {
        allowed(&framing, max_frame, frame);
    }
    Ok(())
}

fn framing(input: &mut Unstructured<'_>) -> Result<Framing> {
    Ok(match input.int_in_range(0..=2_u8)? {
        0 => {
            let length_bytes = *input.choose(&[1, 2, 4, 8])?;
            let length_offset = input.int_in_range(0..=8)?;
            Framing::LengthPrefix {
                header_bytes: length_offset + length_bytes + input.int_in_range(0..=8)?,
                length_offset,
                length_bytes,
                order: if input.arbitrary()? {
                    LengthOrder::Big
                } else {
                    LengthOrder::Little
                },
                includes_header: input.arbitrary()?,
            }
        }
        1 => {
            let length = input.int_in_range(1..=4)?;
            Framing::Delimiter(input.bytes(length)?.to_vec())
        }
        _ => Framing::Fixed(input.int_in_range(1..=64)?),
    })
}

/// Every frame `stream` holds, read in pieces of `piece()` bytes, and how
/// many frames were too long.
fn frames(
    framing: &Framing,
    max_frame: usize,
    read_bytes: usize,
    mut stream: &[u8],
    mut piece: impl FnMut() -> usize,
) -> (Vec<Vec<u8>>, u64) {
    let mut deframer = Deframer::new(framing.clone(), max_frame, read_bytes);
    let mut frames = Vec::new();
    while !stream.is_empty() {
        let space = deframer.space();
        assert!(!space.is_empty(), "no room left to read into");
        let read = piece().min(space.len()).min(stream.len());
        space[..read].copy_from_slice(&stream[..read]);
        deframer.filled(read);
        stream = &stream[read..];
        while let Some(frame) = deframer.next_frame() {
            assert!(frame.len() <= max_frame);
            frames.push(frame.to_vec());
        }
    }
    (frames, deframer.too_long())
}

/// Checks that `frame` is one `framing` cuts.
fn allowed(framing: &Framing, max_frame: usize, frame: &[u8]) {
    match framing {
        Framing::Fixed(size) => assert_eq!(frame.len(), *size),
        Framing::Delimiter(delimiter) => assert!(
            !frame.windows(delimiter.len()).any(|window| window == delimiter),
            "a frame holds its delimiter"
        ),
        Framing::LengthPrefix {
            header_bytes,
            length_offset,
            length_bytes,
            order,
            includes_header,
        } => {
            let field = &frame[*length_offset..length_offset + length_bytes];
            let mut length = 0_u64;
            let mut add = |byte: &u8| length = (length << 8) | u64::from(*byte);
            match order {
                LengthOrder::Big => field.iter().for_each(&mut add),
                LengthOrder::Little => field.iter().rev().for_each(&mut add),
            }
            let header = *header_bytes as u64;
            let total = if *includes_header {
                length.max(header)
            } else {
                length + header
            };
            assert_eq!(total, frame.len() as u64);
            assert!(frame.len() <= max_frame);
        }
    }
}
