// SPDX-License-Identifier: Apache-2.0

//! A camera's frames read from a recording by its indexes: every frame's
//! time and every frame byte for byte as a read from start to end finds
//! them, whatever chunk holds them, and a recording without indexes
//! refused with its cause.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::time::Duration;

use kapto_capture::reader::read_recording;
use kapto_capture::seek::ChannelIndex;
use kapto_capture::{
    Batch, ChannelSpec, Compression, LossCounters, Recording, StreamIndex, WriterConfig,
};
use kapto_core::{ClockMap, ClockSample, ErrorCode, RecvTime};
use proptest::prelude::*;

struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("kapto-seek-{}-{name}.mcap", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Frame `n`: `size` bytes that tell it apart from every other.
fn frame(n: usize, size: usize) -> Vec<u8> {
    (0..size)
        .map(|at| u8::try_from((n * 31 + at * 7) % 251).unwrap())
        .collect()
}

/// Records `sizes.len()` frames on a camera channel and a joint state
/// before each on another, in chunks of `chunk_bytes`, compressed with
/// `compression`, finalised.
fn record(file: &TempFile, sizes: &[usize], chunk_bytes: u64, compression: Compression) {
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 1_000_000_000_000,
        uncertainty: Duration::ZERO,
    });
    let streams: Vec<ChannelSpec> = [(0, "joints"), (1, "camera")]
        .into_iter()
        .map(|(index, name): (StreamIndex, &str)| ChannelSpec::from((index, name.parse().unwrap())))
        .collect();
    let config = WriterConfig {
        compression,
        chunk_bytes,
        ..WriterConfig::default()
    };
    let mut recording = Recording::create(&file.0, &streams, &clock, &config).unwrap();
    let counters = LossCounters::new(2);
    for (n, size) in sizes.iter().enumerate() {
        let mut batch = Batch::new(2, (*size).max(8));
        assert!(batch.push(0, &u64::try_from(n).unwrap().to_le_bytes()));
        assert!(batch.push(1, &frame(n, *size)));
        batch.set_recv_time(RecvTime::from_nanos(u64::try_from(n).unwrap() * 33_000_000));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();
}

/// The camera's frames as a read from start to end finds them.
fn read_through(file: &TempFile) -> Vec<(u64, Vec<u8>)> {
    let mut frames = Vec::new();
    read_recording(&file.0, |channel, message| {
        if channel.topic == "camera" {
            frames.push((message.log_time, message.data.to_vec()));
        }
        ControlFlow::Continue(())
    })
    .unwrap();
    frames
}

/// The same, by the indexes.
fn read_by_index(file: &TempFile) -> Vec<(u64, Vec<u8>)> {
    let index = ChannelIndex::open(&file.0, "camera").unwrap();
    let times: Vec<u64> = index.times().collect();
    let frames: Vec<(u64, Vec<u8>)> = (0..index.len())
        .map(|n| index.read(n).unwrap().unwrap())
        .collect();
    assert_eq!(
        times,
        frames.iter().map(|(time, _)| *time).collect::<Vec<_>>()
    );
    assert_eq!(index.read(index.len()).unwrap(), None);
    frames
}

#[test]
fn every_frame_is_found_by_the_indexes_as_a_read_through_finds_it_in_any_chunk() {
    for compression in [Compression::Lz4, Compression::Zstd, Compression::None] {
        let file = TempFile::new(&format!("{compression:?}"));
        // Frames of 3 to 9 KiB in chunks of 16 KiB: a few to a chunk, some
        // starting one.
        let sizes: Vec<usize> = (0..60).map(|n| 3_000 + (n * 997) % 6_000).collect();
        record(&file, &sizes, 16 * 1024, compression);
        let indexed = read_by_index(&file);
        assert_eq!(indexed.len(), 60, "{compression:?}");
        assert!(indexed == read_through(&file), "{compression:?}");
    }
}

#[test]
fn the_frame_shown_at_a_time_is_the_newest_received_by_then() {
    let file = TempFile::new("at");
    record(&file, &[100; 5], 1024, Compression::Lz4);
    let index = ChannelIndex::open(&file.0, "camera").unwrap();
    let first = index.times().next().unwrap();
    assert_eq!(index.at(first - 1), None);
    assert_eq!(index.at(first), Some(0));
    assert_eq!(index.at(first + 33_000_000 - 1), Some(0));
    assert_eq!(index.at(first + 2 * 33_000_000 + 5), Some(2));
    assert_eq!(index.at(u64::MAX), Some(4));
}

#[test]
fn a_recording_cut_off_or_without_the_channel_is_refused_with_its_cause() {
    let file = TempFile::new("cut");
    record(&file, &[2_000; 20], 4 * 1024, Compression::Lz4);
    let missing = ChannelIndex::open(&file.0, "wrist").unwrap_err();
    assert_eq!(missing.code(), ErrorCode::RecordingUnreadable);
    assert!(
        missing.message().contains("no channel named `wrist`"),
        "{}",
        missing.message()
    );

    // Cut off before its summary, as a crash leaves it.
    let bytes = std::fs::read(&file.0).unwrap();
    std::fs::write(&file.0, &bytes[..bytes.len() / 2]).unwrap();
    let cut = ChannelIndex::open(&file.0, "camera").unwrap_err();
    assert_eq!(cut.code(), ErrorCode::RecordingUnreadable);
    assert!(cut.message().contains("cut off"), "{}", cut.message());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn frames_of_any_sizes_in_chunks_of_any_size_are_found_whole(
        sizes in proptest::collection::vec(1_usize..20_000, 1..40),
        chunk_kib in 1_u64..64,
    ) {
        let file = TempFile::new("prop");
        record(&file, &sizes, chunk_kib * 1024, Compression::Lz4);
        prop_assert!(read_by_index(&file) == read_through(&file));
    }
}
