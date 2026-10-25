// SPDX-License-Identifier: Apache-2.0

//! How long reading an episode's events back from its recording takes:
//! what the worker's `events` job does, so episodes can be found by what
//! happened in them. Only the chunks the recording's indexes say hold
//! events are read, though every chunk's header is looked at once.
//!
//! Each run is one episode of one stream at 1 kHz with an event every
//! second, across episode lengths (messages) and message sizes, so
//! `cargo xtask bench model` can split its cost into a part per episode,
//! per message and per KiB. Run with
//! `cargo bench -p kapto-capture --bench events_job`.

#![allow(
    clippy::unwrap_used,
    reason = "benchmark setup fails loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kapto_capture::reader::read_events;
use kapto_capture::{Batch, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, RecvTime, StreamId};
use kapto_store::{Event, Phase};
use kapto_testkit::TempDir;

/// Messages per episode: 15 s, 30 s and a minute at 1 kHz.
const MESSAGES: [u32; 3] = [15_000, 30_000, 60_000];
/// Bytes per message: a small arm's positions, and a larger message.
const SIZES: [usize; 2] = [68, 512];

/// A recording in `folder` of `messages` messages of `size` bytes, a
/// moment marked every second.
fn recorded(folder: &Path, messages: u32, size: usize) -> PathBuf {
    let path = folder.join(format!("{messages}x{size}.mcap"));
    let clock = ClockMap::new(ClockSample::now());
    let arm = ChannelSpec::from((0, "arm".parse::<StreamId>().unwrap()));
    let mut recording = Recording::create(&path, &[arm], &clock, &WriterConfig::default()).unwrap();
    let counters = LossCounters::new(1);
    let message = vec![7_u8; size];
    for step in 0..messages {
        let mut batch = Batch::new(1, size);
        batch.push(0, &message);
        let at = u64::from(step) * 1_000_000 + 1;
        batch.set_recv_time(RecvTime::from_nanos(at));
        recording.write(&batch, &counters).unwrap();
        if step % 1000 == 0 {
            recording
                .write_event(&Event {
                    kind: "note".to_owned(),
                    at_utc_ns: i64::try_from(at).unwrap(),
                    until_utc_ns: None,
                    phase: Phase::Moment,
                    id: None,
                    source: "page".to_owned(),
                    text: None,
                    attributes: BTreeMap::new(),
                })
                .unwrap();
        }
    }
    recording.finish(&clock).unwrap();
    path
}

fn events_job(c: &mut Criterion) {
    let folder = TempDir::new("bench-events-job");
    let mut group = c.benchmark_group("events_job");
    group.sample_size(10);
    for size in SIZES {
        for messages in MESSAGES {
            let recording = recorded(&folder.0, messages, size);
            group.bench_function(
                BenchmarkId::from_parameter(format!("{messages}x{size}B")),
                |b| b.iter(|| black_box(read_events(black_box(&recording)).unwrap())),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, events_job);
criterion_main!(benches);
