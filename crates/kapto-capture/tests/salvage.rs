// SPDX-License-Identifier: Apache-2.0

//! Salvaging recordings cut off part-way: by a crash after any byte, with
//! blocks never written or garbage after their last chunk, or a chunk whose
//! bytes changed; and leaving whole ones and non-recordings alone.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use kapto_capture::reader::read_recording;
use kapto_capture::{Batch, ChannelSpec, LossCounters, Recording, WriterConfig, salvage};
use kapto_core::{ClockMap, ClockSample, ErrorCode, RecvTime};
use kapto_testkit::TempDir;
use mcap::records::{Record, op};
use proptest::prelude::*;

/// When the first message was received, in UTC nanoseconds.
const START_UTC_NS: u64 = 1_800_000_000_000_000_000;
/// Messages per chunk: the writer closes a chunk at each sync.
const PER_CHUNK: u32 = 10;
const CHUNKS: u32 = 40;

fn clock() -> ClockMap {
    ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: i64::try_from(START_UTC_NS).unwrap(),
        uncertainty: Duration::ZERO,
    })
}

/// Message `n`'s payload: its number, then filler.
fn payload(n: u32) -> Vec<u8> {
    let mut payload = n.to_le_bytes().to_vec();
    payload.extend([0xA5; 60]);
    payload
}

/// A recording of [`CHUNKS`] chunks of [`PER_CHUNK`] messages, message `n`
/// received `n` milliseconds in, cut off by a crash after its last chunk
/// was put on disk: it has none of the records a closed file ends with.
fn crashed(path: &Path) {
    let config = WriterConfig {
        sync_interval: Duration::ZERO,
        ..WriterConfig::default()
    };
    let streams = [ChannelSpec::from((0, "arm".parse().unwrap()))];
    let mut recording = Recording::create(path, &streams, &clock(), &config).unwrap();
    let counters = LossCounters::new(1);
    for chunk in 0..CHUNKS {
        for n in chunk * PER_CHUNK..(chunk + 1) * PER_CHUNK {
            let message = payload(n);
            let mut batch = Batch::new(1, message.len());
            batch.push(0, &message);
            batch.set_recv_time(RecvTime::from_nanos(u64::from(n) * 1_000_000));
            recording.write(&batch, &counters).unwrap();
        }
        recording
            .sync_if_due(RecvTime::from_nanos(u64::MAX))
            .unwrap();
    }
    std::mem::forget(recording);
}

/// A recording's bytes, and where each of its chunks ends with the log time
/// of its newest message.
type Reference = (Vec<u8>, Vec<(u64, u64)>);

/// The crashed recording's: made once for every test.
fn reference() -> &'static Reference {
    static REFERENCE: OnceLock<Reference> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        let folder = TempDir::new("salvage-reference");
        let path = folder.0.join("recording.mcap");
        crashed(&path);
        let bytes = std::fs::read(&path).unwrap();
        // Walked by each record's opcode and length.
        let mut chunks = Vec::new();
        let mut at = mcap::MAGIC.len();
        while at + 9 <= bytes.len() {
            let opcode = bytes[at];
            let length = usize::try_from(u64::from_le_bytes(
                bytes[at + 1..at + 9].try_into().unwrap(),
            ))
            .unwrap();
            let end = at + 9 + length;
            if end > bytes.len() {
                break;
            }
            if opcode == op::CHUNK
                && let Record::Chunk { header, .. } =
                    mcap::parse_record(opcode, &bytes[at + 9..end]).unwrap()
            {
                chunks.push((end as u64, header.message_end_time));
            }
            at = end;
        }
        assert_eq!(chunks.len(), CHUNKS as usize);
        (bytes, chunks)
    })
}

/// Writes `bytes` to a recording in `folder` and gives its path.
fn written(folder: &TempDir, bytes: &[u8]) -> PathBuf {
    folder.write("recording.mcap", bytes)
}

/// Every message of the recording at `path`, as its number and log time,
/// checking its payload is the one written.
fn messages(path: &Path) -> Vec<(u32, u64)> {
    let mut read = Vec::new();
    let summary = read_recording(path, |_, message| {
        let n = u32::from_le_bytes(message.data[..4].try_into().unwrap());
        assert_eq!(message.data, payload(n), "message {n} as received");
        read.push((n, message.log_time));
        std::ops::ControlFlow::Continue(())
    })
    .unwrap();
    assert!(!summary.cut_off, "read to its end");
    read
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn cut_off_after_any_byte_a_recording_keeps_every_complete_chunk_and_reads_as_closed(
        fraction in 0.0..=1.0_f64,
    ) {
        let (bytes, chunks) = reference();
        let header = 64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let cut = header + ((bytes.len() - header) as f64 * fraction) as usize;
        let folder = TempDir::new("salvage-cut");
        let path = written(&folder, &bytes[..cut]);

        let salvaged = salvage(&path).unwrap();
        // The newest message of the last chunk wholly before the cut.
        let complete = chunks.iter().rev().find(|(end, _)| *end <= cut as u64);
        let expected = complete.map_or(0, |(_, newest)| {
            u32::try_from((newest - START_UTC_NS) / 1_000_000 + 1).unwrap()
        });
        prop_assert_eq!(salvaged.messages, u64::from(expected));
        prop_assert_eq!(salvaged.kept_bytes + salvaged.cut_bytes, cut as u64);
        prop_assert!(!salvaged.already_whole);
        let read = messages(&path);
        prop_assert_eq!(read.len(), expected as usize);
        prop_assert!(read.iter().enumerate().all(|(index, (n, _))| *n as usize == index));
        prop_assert_eq!(salvaged.last_log_time, read.last().map(|(_, time)| *time));
        // Salvaged, it is whole: salvaging again changes nothing.
        let again = salvage(&path).unwrap();
        prop_assert!(again.already_whole);
        prop_assert_eq!(again.messages, salvaged.messages);
    }
}

#[test]
fn blocks_never_written_or_garbage_after_the_last_chunk_are_cut() {
    let (bytes, chunks) = reference();
    for tail in [vec![0_u8; 4096], vec![0x06, 0xFF, 0x13, 0x77, 0x01, 0x02]] {
        let folder = TempDir::new("salvage-tail");
        let mut torn = bytes.clone();
        torn.extend(&tail);
        let path = written(&folder, &torn);
        let salvaged = salvage(&path).unwrap();
        assert_eq!(salvaged.messages, u64::from(CHUNKS * PER_CHUNK));
        assert!(salvaged.kept_bytes >= chunks.last().unwrap().0);
        assert_eq!(salvaged.kept_bytes + salvaged.cut_bytes, torn.len() as u64);
        assert!(salvaged.cut_bytes >= tail.len() as u64);
        assert_eq!(messages(&path).len(), (CHUNKS * PER_CHUNK) as usize);
    }
}

#[test]
fn a_chunk_whose_bytes_changed_is_cut_with_everything_after_it() {
    let (bytes, chunks) = reference();
    let folder = TempDir::new("salvage-changed");
    let mut changed = bytes.clone();
    // A byte in the middle of the last chunk, which its checksum covers.
    let (before, _) = chunks[chunks.len() - 2];
    let (last, _) = chunks[chunks.len() - 1];
    let middle = usize::try_from(u64::midpoint(before, last)).unwrap();
    changed[middle] ^= 0xFF;
    let path = written(&folder, &changed);
    let salvaged = salvage(&path).unwrap();
    assert_eq!(salvaged.messages, u64::from((CHUNKS - 1) * PER_CHUNK));
    // Cut after the chunk before it and its message indexes, before it ends.
    assert!(salvaged.kept_bytes >= before && salvaged.kept_bytes < last);
    assert_eq!(messages(&path).len(), ((CHUNKS - 1) * PER_CHUNK) as usize);
}

#[test]
fn a_chunk_whose_length_runs_past_the_end_is_cut_without_setting_room_aside_for_it() {
    let (bytes, chunks) = reference();
    // The last chunk starts where the message indexes after the one before
    // it end: found by walking from there.
    let (before, _) = chunks[chunks.len() - 2];
    let mut at = usize::try_from(before).unwrap();
    while bytes[at] != op::CHUNK {
        let length = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().unwrap());
        at += 9 + usize::try_from(length).unwrap();
    }
    // A byte past the end, and a petabyte no reader could set aside.
    for length in [bytes.len() as u64, 1 << 50] {
        let folder = TempDir::new("salvage-damaged-length");
        let mut damaged = bytes.clone();
        damaged[at + 1..at + 9].copy_from_slice(&length.to_le_bytes());
        let path = written(&folder, &damaged);
        let salvaged = salvage(&path).unwrap();
        assert_eq!(salvaged.messages, u64::from((CHUNKS - 1) * PER_CHUNK));
        assert!(salvaged.kept_bytes >= before && salvaged.kept_bytes <= at as u64);
        assert_eq!(messages(&path).len(), ((CHUNKS - 1) * PER_CHUNK) as usize);
    }
}

#[test]
fn a_closed_recording_is_left_as_it_is() {
    let folder = TempDir::new("salvage-whole");
    let path = folder.0.join("recording.mcap");
    let streams = [ChannelSpec::from((0, "arm".parse().unwrap()))];
    let mut recording =
        Recording::create(&path, &streams, &clock(), &WriterConfig::default()).unwrap();
    let mut batch = Batch::new(1, 64);
    batch.push(0, &payload(0));
    batch.set_recv_time(RecvTime::from_nanos(0));
    recording.write(&batch, &LossCounters::new(1)).unwrap();
    recording.finish(&clock()).unwrap();
    let before = std::fs::read(&path).unwrap();

    let salvaged = salvage(&path).unwrap();
    assert!(salvaged.already_whole);
    assert_eq!((salvaged.messages, salvaged.cut_bytes), (1, 0));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_file_that_isnt_a_recording_or_was_cut_before_its_first_record_is_unreadable() {
    let (bytes, _) = reference();
    for (name, contents) in [
        ("empty", Vec::new()),
        ("magic only", bytes[..8].to_vec()),
        ("its header cut off", bytes[..12].to_vec()),
        ("text", b"not a recording at all".to_vec()),
    ] {
        let folder = TempDir::new("salvage-unreadable");
        let path = written(&folder, &contents);
        let error = salvage(&path).unwrap_err();
        assert_eq!(error.code(), ErrorCode::RecordingUnreadable, "{name}");
        // Left as it was.
        let mut kept = Vec::new();
        let mut file = std::fs::File::open(&path).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_to_end(&mut kept).unwrap();
        assert_eq!(kept, contents, "{name}");
    }
}
