// SPDX-License-Identifier: Apache-2.0

//! How long checking an episode takes once it is recorded: reading its
//! recording by role, camera and stream timing, running the checks and
//! working out its statistics, what the worker's `qa` job does before its
//! detectors (whose learning has a benchmark of its own).
//!
//! Each run is one episode of a seven-joint arm at 1 kHz, across episode
//! lengths (messages) and message sizes (positions only, or positions,
//! velocities and efforts), so `cargo xtask bench model` can split its cost
//! into a part per episode, per message and per KiB, like the recording
//! stages. Run with `cargo bench -p kapto-qa --bench qa_job`.

#![allow(
    clippy::unwrap_used,
    reason = "benchmark setup fails loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use kapto_capture::{Batch, ChannelSchema, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, RecvTime, StreamId};
use kapto_decode::{FieldSpec, STRUCT_ENCODING, ScalarType, StructLayout};
use kapto_qa::roles::read_episode;
use kapto_qa::{Check, EpisodeView, checks, episode_statistics, run_checks};
use kapto_testkit::TempDir;

/// Messages per episode: 15 s, 30 s and a minute at 1 kHz.
const MESSAGES: [u32; 3] = [15_000, 30_000, 60_000];
/// Values per joint in a message: its position, or its position, velocity
/// and effort.
const VALUES: [usize; 2] = [1, 3];
const JOINTS: usize = 7;
const NAMES: [&str; 3] = ["position", "velocity", "effort"];

/// A recording in `folder` of `messages` messages of a seven-joint arm, each
/// joint with `values` values, the mapping naming them attached.
fn recorded(folder: &Path, messages: u32, values: usize) -> PathBuf {
    let mut fields = Vec::new();
    let mut mapping = String::from("[joints]\n");
    for joint in 1..=JOINTS {
        let mut roles = Vec::new();
        for name in &NAMES[..values] {
            let field = format!("{name}_{joint}");
            fields.push(FieldSpec::new(&field, ScalarType::F64, 8 * fields.len()));
            roles.push(format!("{name} = \"arm.{field}\""));
        }
        writeln!(mapping, "joint_{joint} = {{ {} }}", roles.join(", ")).unwrap();
    }
    let layout = StructLayout { fields };
    let arm = ChannelSpec {
        stream: 0,
        id: "arm".parse::<StreamId>().unwrap(),
        schema: Some(ChannelSchema {
            message_encoding: STRUCT_ENCODING.to_owned(),
            name: "arm".to_owned(),
            encoding: STRUCT_ENCODING.to_owned(),
            data: layout.to_schema_json().into_bytes(),
            metadata: BTreeMap::new(),
        }),
    };
    let path = folder.join(format!("{messages}x{values}.mcap"));
    let clock = ClockMap::new(ClockSample::now());
    let mut recording = Recording::create(&path, &[arm], &clock, &WriterConfig::default()).unwrap();
    recording
        .attach("mapping.toml", "application/toml", 0, mapping.as_bytes())
        .unwrap();
    let counters = LossCounters::new(1);
    let size = 8 * JOINTS * values;
    let mut message = vec![0_u8; size];
    for step in 0..messages {
        for (index, value) in message.as_chunks_mut::<8>().0.iter_mut().enumerate() {
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

fn qa_job(c: &mut Criterion) {
    let folder = TempDir::new("bench-qa-job");
    let mut group = c.benchmark_group("qa_job");
    group.sample_size(10);
    for values in VALUES {
        for messages in MESSAGES {
            let recording = recorded(&folder.0, messages, values);
            let episode = EpisodeView::new(
                &folder.0,
                kapto_testkit::episode(
                    "20260305T192345.123Z-1a2b3c4d",
                    (0, i64::from(messages) * 1_000_000),
                    vec![kapto_testkit::stream("arm", u64::from(messages))],
                ),
            );
            let made = checks::from_settings(&kapto_config::QaSettings::default());
            let checks: Vec<&dyn Check> = made.iter().map(AsRef::as_ref).collect();
            let size = 8 * JOINTS * values;
            group.bench_function(
                BenchmarkId::from_parameter(format!("{messages}x{size}B")),
                |b| {
                    b.iter(|| {
                        let span = (0, i64::from(messages) * 1_000_000);
                        let recorded =
                            read_episode(black_box(&recording), span, &mut || Ok(())).unwrap();
                        let episode = episode
                            .clone()
                            .with_roles(recorded.roles)
                            .with_cameras(recorded.cameras)
                            .with_timing(recorded.timing)
                            .with_events(recorded.events);
                        let run = run_checks(&checks, &episode, None);
                        black_box((run, episode_statistics::of_episode(&episode)))
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, qa_job);
criterion_main!(benches);
