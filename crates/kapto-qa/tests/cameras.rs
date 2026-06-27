// SPDX-License-Identifier: Apache-2.0

//! An episode's cameras' frames, its streams' timing and its events, read from real
//! recordings by the `streams.toml` they carry.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use kapto_capture::{Batch, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, RecvTime};
use kapto_qa::cameras::CameraFrames;
use kapto_qa::roles::read_episode;
use kapto_store::{Event, Phase};

/// A recording for one test, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kapto-cameras-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.join("recording.mcap"))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A TCP camera of JPEG pictures, each after its 4-byte length, and a
/// stream that isn't a camera.
const STREAMS: &str = "[streams.wrist]\nrate_hz = 30\nmax_message_bytes = 65536\n\
                       tcp = { listen = \"127.0.0.1:7010\", framing = { kind = \"length_prefix\", length_bytes = 4 } }\n\
                       camera = { format = \"jpeg\" }\n\
                       [streams.arm]\nrate_hz = 100\nudp = { bind = \"127.0.0.1:9000\" }\n";

/// The episodes' span: their messages from UTC 1 000 005 s on.
const SPAN: (i64, i64) = (1_000_005_000_000_000, 1_000_006_000_000_000);

/// A frame as the camera sends it: its length, then a JPEG of `size`
/// bytes.
fn jpeg(size: usize) -> Vec<u8> {
    let mut frame = u32::try_from(size).unwrap().to_be_bytes().to_vec();
    frame.extend([0xff, 0xd8, 0xff]);
    frame.resize(4 + size, 7);
    frame
}

/// Records `frames` of the camera a millisecond apart from UTC
/// 1 000 005 s, carrying `streams` as its only file.
fn record(file: &TempFile, streams: Option<&str>, frames: &[Vec<u8>]) {
    let channels = [
        ChannelSpec::from((0, "wrist".parse().unwrap())),
        ChannelSpec::from((1, "arm".parse().unwrap())),
    ];
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 1_000_005_000_000_000,
        uncertainty: Duration::ZERO,
    });
    let mut recording =
        Recording::create(&file.0, &channels, &clock, &WriterConfig::default()).unwrap();
    if let Some(streams) = streams {
        recording
            .attach("streams.toml", "application/toml", 0, streams.as_bytes())
            .unwrap();
    }
    let counters = LossCounters::new(2);
    for (at, frame) in (0_u64..).zip(frames) {
        for (stream, payload) in [(0, frame.as_slice()), (1, b"arm".as_slice())] {
            let mut batch = Batch::new(1, payload.len());
            batch.push(stream, payload);
            batch.set_recv_time(RecvTime::from_nanos(at * 1_000_000));
            recording.write(&batch, &counters).unwrap();
        }
    }
    recording
        .write_event(&Event {
            kind: "subtask.reach".to_owned(),
            at_utc_ns: 1_000_005_000_000_000,
            until_utc_ns: Some(1_000_005_002_000_000),
            phase: Phase::Stretch,
            id: None,
            source: "page".to_owned(),
            text: None,
            attributes: BTreeMap::new(),
        })
        .unwrap();
    recording.finish(&clock).unwrap();
}

#[test]
fn each_cameras_frames_are_read_with_their_pictures_sizes_and_those_without_one() {
    let file = TempFile::new("frames");
    record(
        &file,
        Some(STREAMS),
        &[jpeg(900), jpeg(1000), vec![0, 0, 0], jpeg(10)],
    );
    let recorded = read_episode(&file.0, SPAN, &mut || Ok(())).unwrap();
    assert!(recorded.roles.is_empty());
    // Every stream's timing, read in the same pass: the camera's broken
    // frame arrived as much as the others.
    let timing: Vec<(&str, u64)> = recorded
        .timing
        .iter()
        .map(|(name, timing)| (name.as_str(), timing.messages))
        .collect();
    assert_eq!(timing, [("arm", 4), ("wrist", 4)]);
    assert_eq!(recorded.timing["wrist"].rate_hz, Some(30));
    // And the events, in the same pass.
    let kinds: Vec<&str> = recorded
        .events
        .iter()
        .map(|event| event.kind.as_str())
        .collect();
    assert_eq!(kinds, ["subtask.reach"]);
    let cameras = recorded.cameras;
    let at = |ms: i64| 1_000_005_000_000_000 + ms * 1_000_000;
    assert_eq!(
        cameras.into_iter().collect::<Vec<_>>(),
        [(
            "wrist".to_owned(),
            CameraFrames {
                times_utc_ns: vec![at(0), at(1), at(3)],
                sizes: vec![900, 1000, 10],
                first_unreadable: None,
                unreadable_utc_ns: vec![at(2)],
                unreadable_why: Some(
                    "a frame that can't be read: shorter than its framing's header".to_owned()
                ),
            }
        )]
    );
}

#[test]
fn a_camera_whose_first_frame_is_not_its_kind_says_why_and_one_unrecorded_has_no_frames() {
    let file = TempFile::new("first");
    let mut raw = vec![0, 0, 0, 3];
    raw.extend([1, 2, 3]);
    record(&file, Some(STREAMS), &[raw, jpeg(900)]);
    let cameras = read_episode(&file.0, SPAN, &mut || Ok(())).unwrap().cameras;
    assert_eq!(
        cameras["wrist"].first_unreadable.as_deref(),
        Some("pictures in `jpeg`, neither JPEG nor PNG")
    );
    assert_eq!(cameras["wrist"].frames(), 2);

    let silent = TempFile::new("silent");
    record(&silent, Some(STREAMS), &[]);
    let cameras = read_episode(&silent.0, SPAN, &mut || Ok(()))
        .unwrap()
        .cameras;
    assert_eq!(cameras["wrist"], CameraFrames::default());
    // Without its streams.toml, a recording's cameras aren't known.
    let bare = TempFile::new("bare");
    record(&bare, None, &[jpeg(900)]);
    assert!(
        read_episode(&bare.0, SPAN, &mut || Ok(()))
            .unwrap()
            .cameras
            .is_empty()
    );
    // Nor with another file only, which isn't a streams.toml.
    let other = TempFile::new("other");
    record(
        &other,
        Some("[joints]\nshoulder = \"arm.position\"\n"),
        &[jpeg(900)],
    );
    assert!(
        read_episode(&other.0, SPAN, &mut || Ok(()))
            .unwrap()
            .cameras
            .is_empty()
    );
}
