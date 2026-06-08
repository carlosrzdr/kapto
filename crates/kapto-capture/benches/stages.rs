// SPDX-License-Identifier: Apache-2.0

//! Benchmarks of each stage of the recording path that runs on whole batches:
//! handing a batch from receiver to writer, loss accounting, writing MCAP,
//! offering it to the live view, and reading the clock.
//!
//! Each stage runs across a grid of messages per batch and message sizes, so
//! its cost can be split into a part per batch, per message and per byte.
//! Run with `cargo bench -p kapto-capture --bench stages`; results land in
//! `target/criterion/`.

#![allow(
    clippy::unwrap_used,
    reason = "benchmark setup fails loudly when the setup itself breaks"
)]

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kapto_capture::loss::{ByteOrder, RateGap, SequenceField, SequenceWidth};
use kapto_capture::tap::frames::{FrameConfig, frames};
use kapto_capture::tap::{TapConfig, tap};
use kapto_capture::{
    Accounting, Batch, ChannelSpec, Compression, LossCounters, LossDetection, Recording,
    RingConfig, StreamIndex, WriterConfig, ring,
};
use kapto_core::clock::monotonic_now;
use kapto_core::{ClockMap, ClockSample, StreamId};

/// Messages per batch: one shows the cost per batch, the largest the cost per
/// message.
const MESSAGES: [usize; 4] = [1, 8, 64, 256];
/// Message sizes in bytes: the load generator's 68 up to a large status message.
const SIZES: [usize; 3] = [68, 256, 1024];
/// Streams the messages are spread over, as at the reference load.
const STREAMS: u16 = 16;

/// A batch of `messages` messages of `size` bytes, round-robin over the
/// streams, each starting with its stream's next sequence number (u32, little
/// endian) taken from `next_sequence`.
fn batch(messages: usize, size: usize, next_sequence: &mut [u32]) -> Batch {
    let mut batch = Batch::new(messages, size);
    let mut payload = vec![0_u8; size];
    for index in 0..messages {
        let stream = StreamIndex::try_from(index % usize::from(STREAMS)).unwrap();
        let sequence = &mut next_sequence[usize::from(stream)];
        payload[..4].copy_from_slice(&sequence.to_le_bytes());
        *sequence = sequence.wrapping_add(1);
        batch.push(stream, &payload);
    }
    batch.set_recv_time(monotonic_now());
    batch
}

fn grid() -> impl Iterator<Item = (usize, usize)> {
    MESSAGES
        .into_iter()
        .flat_map(|messages| SIZES.into_iter().map(move |size| (messages, size)))
}

fn id(messages: usize, size: usize) -> BenchmarkId {
    BenchmarkId::from_parameter(format!("{messages}x{size}B"))
}

/// Sending a filled batch to the writer and getting it back. Only the
/// hand-off is timed, not filling the batch.
fn hand_off(c: &mut Criterion) {
    let mut group = c.benchmark_group("hand_off");
    for (messages, size) in grid() {
        group.throughput(Throughput::Elements(messages as u64));
        group.bench_function(id(messages, size), |b| {
            let (mut filler, mut drainer, _) = ring(&RingConfig {
                batches: 4,
                batch_capacity: messages,
                max_message_len: size,
                streams: usize::from(STREAMS),
            });
            let payload = vec![7_u8; size];
            b.iter_custom(|iterations| {
                let mut timed = Duration::ZERO;
                for _ in 0..iterations {
                    for _ in 0..messages {
                        filler.batch().push(0, &payload);
                    }
                    let start = Instant::now();
                    black_box(filler.send());
                    drop(black_box(drainer.take()));
                    timed += start.elapsed();
                }
                timed
            });
        });
    }
    group.finish();
}

/// Counting a batch with sequence-number detection (which reads each
/// message's number) and with rate-gap detection.
fn accounting(c: &mut Criterion) {
    let sequence = LossDetection::Sequence(SequenceField {
        offset: 0,
        width: SequenceWidth::U32,
        byte_order: ByteOrder::Little,
    });
    let rate_gap = LossDetection::RateGap(RateGap::for_rate(2_000));
    for (name, detection) in [
        ("accounting_sequence", sequence),
        ("accounting_rate_gap", rate_gap),
    ] {
        let mut group = c.benchmark_group(name);
        for (messages, size) in grid() {
            group.throughput(Throughput::Elements(messages as u64));
            group.bench_function(id(messages, size), |b| {
                let streams: Vec<_> = (0..STREAMS).map(|stream| (stream, detection)).collect();
                let counters = Arc::new(LossCounters::new(usize::from(STREAMS)));
                let mut accounting = Accounting::new(counters, &streams);
                let mut next_sequence = vec![0_u32; usize::from(STREAMS)];
                // A fresh batch each time, so sequence numbers keep rising as
                // they do in a real recording; building it isn't timed.
                b.iter_batched_ref(
                    || batch(messages, size, &mut next_sequence),
                    |batch| accounting.count(batch),
                    BatchSize::SmallInput,
                );
            });
        }
        group.finish();
    }
}

/// Where benchmark recordings are written: under `target/`, on disk, not in
/// a memory-backed temporary folder.
fn recording_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bench-recordings");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}-{}.mcap", std::process::id()))
}

/// Recordings are restarted (untimed) after this many bytes, so a benchmark
/// never fills the disk.
const RESTART_BYTES: usize = 256 * 1024 * 1024;

/// Writing a batch into an MCAP recording, with each compression. Chunks are
/// compressed as they fill, so compression is included at its amortised cost.
fn mcap_write(c: &mut Criterion) {
    let streams: Vec<ChannelSpec> = (0..STREAMS)
        .map(|stream| {
            ChannelSpec::from((
                stream,
                format!("joints_{stream}").parse::<StreamId>().unwrap(),
            ))
        })
        .collect();
    for (name, compression) in [
        ("mcap_write_uncompressed", Compression::None),
        ("mcap_write_lz4", Compression::Lz4),
        ("mcap_write_zstd", Compression::Zstd),
    ] {
        let mut group = c.benchmark_group(name);
        for (messages, size) in grid() {
            group.throughput(Throughput::Elements(messages as u64));
            group.bench_function(id(messages, size), |b| {
                let config = WriterConfig {
                    compression,
                    ..WriterConfig::default()
                };
                let clock = ClockMap::new(ClockSample::now());
                let counters = LossCounters::new(usize::from(STREAMS));
                let path = recording_path(name);
                let open = || Recording::create(&path, &streams, &clock, &config).unwrap();
                let mut recording = Some(open());
                let mut written = 0;
                let mut next_sequence = vec![0_u32; usize::from(STREAMS)];
                let batch = batch(messages, size, &mut next_sequence);
                b.iter_custom(|iterations| {
                    let mut timed = Duration::ZERO;
                    for _ in 0..iterations {
                        if written > RESTART_BYTES {
                            if let Some(full) = recording.take() {
                                full.finish(&clock).unwrap();
                            }
                            recording = Some(open());
                            written = 0;
                        }
                        let current = recording.as_mut().unwrap();
                        let start = Instant::now();
                        current.write(&batch, &counters).unwrap();
                        timed += start.elapsed();
                        written += messages * size;
                    }
                    timed
                });
                drop(recording);
                let _ = std::fs::remove_file(&path);
            });
        }
        group.finish();
    }
}

/// Offering a batch's newest message of each stream to the live view, the
/// writer's part of the tap: alone, and with two of the streams cameras,
/// whose newest frame is offered too when one is due (at most thirty times
/// a second: the batches between only look for them). The tap thread's
/// taking the offers isn't timed.
fn tap_offer(c: &mut Criterion) {
    for (name, cameras) in [("tap_offer", 0), ("tap_offer_cameras", 2)] {
        let mut group = c.benchmark_group(name);
        for (messages, size) in grid() {
            group.throughput(Throughput::Elements(messages as u64));
            group.bench_function(id(messages, size), |b| {
                let (mut feed, mut drain, _) = tap(&TapConfig {
                    slots: 16 * usize::from(STREAMS),
                    max_message_len: size,
                    streams: usize::from(STREAMS),
                });
                let config = FrameConfig {
                    cameras: (0..cameras).map(|stream| (stream, size)).collect(),
                    min_interval: Duration::from_secs(1) / 30,
                };
                // Nothing listens there: the frames are only offered.
                let socket = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bench-frames.sock");
                let (frame_feed, sender, _) = frames(&config, &socket);
                if cameras > 0 {
                    feed.set_frames(frame_feed);
                }
                let mut next_sequence = vec![0_u32; usize::from(STREAMS)];
                let batch = batch(messages, size, &mut next_sequence);
                b.iter_custom(|iterations| {
                    let mut timed = Duration::ZERO;
                    for _ in 0..iterations {
                        let start = Instant::now();
                        feed.offer(black_box(&batch));
                        timed += start.elapsed();
                        drain.each(|_, _, bytes| {
                            black_box(bytes);
                        });
                    }
                    timed
                });
                drop(sender);
            });
        }
        group.finish();
    }
}

/// Reading the monotonic clock (once per batch) and converting a receive
/// time to UTC (once per batch when writing).
fn timestamps(c: &mut Criterion) {
    let mut group = c.benchmark_group("timestamp");
    group.bench_function("monotonic_now", |b| b.iter(monotonic_now));
    let clock = ClockMap::new(ClockSample::now());
    group.bench_function("to_utc", |b| {
        b.iter(|| clock.to_utc_ns(black_box(monotonic_now())));
    });
    group.finish();
}

criterion_group!(
    stages, hand_off, accounting, mcap_write, tap_offer, timestamps
);
criterion_main!(stages);
