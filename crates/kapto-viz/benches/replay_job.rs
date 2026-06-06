// SPDX-License-Identifier: Apache-2.0

//! How long making an episode's replay takes: what the worker's `replay`
//! job does, its replay view written in one pass over the recording, with
//! the robot drawn from the package its recording carries, posed 60 times
//! a second, and every value kept.
//!
//! Each run is one episode of the sample arm at 1 kHz, across episode
//! lengths (messages) and message sizes (the arm's 76 bytes, or with 16
//! more values), so `cargo xtask bench model` can split its cost into a
//! part per episode, per message and per KiB, like the recording stages.
//! Run with `cargo bench -p kapto-viz --bench replay_job`.

#![allow(
    clippy::unwrap_used,
    reason = "benchmark setup fails loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kapto_capture::{Batch, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, RecvTime, StreamId};
use kapto_store::records::EPISODE_RECORD;
use kapto_testkit::TempDir;
use kapto_viz::{DrawnRobots, ReplaySettings, write_replay};

/// Messages per episode: 1, 4 and 15 s at 1 kHz. Short enough that the
/// part an episode costs whatever its length isn't lost in the noise of
/// the rest; the cost grows linearly, so a minute is four times 15 s.
const MESSAGES: [u32; 3] = [1_000, 4_000, 15_000];
/// Values in a message besides the arm's own: none, or 16 more.
const EXTRA: [usize; 2] = [0, 16];
/// The sample arm's message: sequence number, clock, seven joints and the
/// gripper.
const ARM_BYTES: usize = 76;

fn arm7(path: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/arm7")
            .join(path),
    )
    .unwrap()
}

/// A recording in `folder` of `messages` messages of the sample arm, each
/// with `extra` more values, carrying the arm's package but its meshes, as
/// recordings do by default.
fn recorded(folder: &Path, messages: u32, extra: usize) -> PathBuf {
    let mut streams = String::from_utf8(arm7("streams.toml")).unwrap();
    let size = ARM_BYTES + 8 * extra;
    streams = streams.replace(
        "max_message_bytes = 76",
        &format!("max_message_bytes = {size}"),
    );
    if extra > 0 {
        write!(
            streams,
            "\n[[streams.arm_joints.decoder.fields]]\nname = \"extra\"\ntype = \"f64\"\n\
             offset = {ARM_BYTES}\ncount = {extra}\n"
        )
        .unwrap();
    }
    let path = folder.join(format!("{messages}x{size}.mcap"));
    let clock = ClockMap::new(ClockSample::now());
    let arm = ChannelSpec::from((0, "arm_joints".parse::<StreamId>().unwrap()));
    let mut recording = Recording::create(&path, &[arm], &clock, &WriterConfig::default()).unwrap();
    // A hash no local package has: the robot comes from the recording.
    let record = BTreeMap::from([
        ("package.id".to_owned(), "arm7".to_owned()),
        ("package.hash".to_owned(), "carried".to_owned()),
    ]);
    recording.write_record(EPISODE_RECORD, record).unwrap();
    for (name, data) in [
        ("package.toml", arm7("package.toml")),
        ("urdf/arm7.urdf", arm7("urdf/arm7.urdf")),
        ("mapping.toml", arm7("mapping.toml")),
        ("streams.toml", streams.into_bytes()),
    ] {
        recording.attach(name, "text/plain", 0, &data).unwrap();
    }
    let counters = LossCounters::new(1);
    let mut message = vec![0_u8; size];
    for step in 0..messages {
        message[..4].copy_from_slice(&step.to_le_bytes());
        message[4..12].copy_from_slice(&(u64::from(step) * 1_000_000).to_le_bytes());
        for (index, value) in message[12..].as_chunks_mut::<8>().0.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "small indices")]
            let angle = (f64::from(step) / 1000.0 + index as f64).sin();
            value.copy_from_slice(&angle.to_le_bytes());
        }
        let mut batch = Batch::new(1, size);
        batch.push(0, &message);
        batch.set_recv_time(RecvTime::from_nanos(u64::from(step) * 1_000_000 + 1));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();
    path
}

fn replay_job(c: &mut Criterion) {
    let folder = TempDir::new("bench-replay-job");
    let settings = ReplaySettings {
        display_rate: 60,
        packages: folder.0.join("packages"),
        robots: DrawnRobots::default(),
    };
    let view = folder.0.join("episode.view");
    let mut group = c.benchmark_group("replay_job");
    group.sample_size(10);
    for extra in EXTRA {
        for messages in MESSAGES {
            let recording = recorded(&folder.0, messages, extra);
            let size = ARM_BYTES + 8 * extra;
            group.bench_function(
                BenchmarkId::from_parameter(format!("{messages}x{size}B")),
                |b| {
                    b.iter(|| {
                        // Each drawn afresh, as the first replay of a
                        // package is: the costlier case.
                        let settings = ReplaySettings {
                            robots: DrawnRobots::default(),
                            ..settings.clone()
                        };
                        let replayed =
                            write_replay(black_box(&recording), &view, &settings, &mut || Ok(()))
                                .unwrap();
                        assert!(replayed.problems.is_empty(), "{:?}", replayed.problems);
                        black_box(replayed)
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, replay_job);
criterion_main!(benches);
