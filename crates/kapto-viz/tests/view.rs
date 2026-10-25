// SPDX-License-Identifier: Apache-2.0

//! An episode's replay view: every step keeps the extremes and count of the
//! messages it covers, a stretch reads as the whole step does, and a view
//! that can't be read says so.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::Path;

use kapto_core::ErrorCode;
use kapto_robot::Transform;
use kapto_store::events::{Event, Phase};
use kapto_testkit::TempDir;
use kapto_viz::history::Record;
use kapto_viz::page_scene::RobotScene;
use kapto_viz::stream::{OpeningStream, OpeningValue};
use kapto_viz::view::{VIEW_MAGIC, View, ViewContents, ViewWriter};
use proptest::prelude::*;

/// The finest step: 10 ms.
const STEP_NS: i64 = 10_000_000;

/// A message: its stream, when it was received, its one value.
type Message = (u16, i64, f64);

/// Messages from gaps of up to 30 ms between them, each of stream 0 or 1.
fn messages(gaps: &[(i64, bool, f64)]) -> Vec<Message> {
    let mut time = 1_760_000_000_000_000_000_i64;
    gaps.iter()
        .map(|&(gap, second, value)| {
            time += gap;
            (u16::from(second), time, value)
        })
        .collect()
}

fn contents() -> ViewContents {
    let value = |name: &str| OpeningValue {
        name: name.to_owned(),
        plotted: true,
    };
    ViewContents {
        links: vec!["base_link".to_owned()],
        streams: vec![
            OpeningStream {
                name: "arm".to_owned(),
                values: vec![value("joint")],
            },
            OpeningStream {
                name: "gripper".to_owned(),
                values: vec![value("opening")],
            },
        ],
        ..ViewContents::default()
    }
}

/// Writes `messages` into a view at `path`, a pose with each of stream 0's.
fn write(path: &Path, messages: &[Message], contents: ViewContents) {
    let mut writer = ViewWriter::create(path, STEP_NS).unwrap();
    for &(stream, time, value) in messages {
        writer.message(stream, time, &[value]).unwrap();
        if stream == 0 {
            writer.pose(time, &[Transform::IDENTITY]).unwrap();
        }
    }
    writer.finish(contents).unwrap();
}

/// What a record of a step `step_ns` long, at `at`, should hold of each
/// stream: its messages' count and extremes.
fn expected(
    messages: &[Message],
    origin: i64,
    step_ns: i64,
    at: i64,
) -> BTreeMap<u16, (u32, f64, f64)> {
    let mut covered: BTreeMap<u16, (u32, f64, f64)> = BTreeMap::new();
    for &(stream, time, value) in messages {
        if (time - origin) / step_ns == at {
            let kept = covered.entry(stream).or_insert((0, value, value));
            *kept = (kept.0 + 1, kept.1.min(value), kept.2.max(value));
        }
    }
    covered
}

fn held(record: &Record) -> BTreeMap<u16, (u32, f64, f64)> {
    record
        .samples
        .iter()
        .map(|sample| {
            (
                sample.stream,
                (sample.messages, sample.extremes[0].0, sample.extremes[0].1),
            )
        })
        .collect()
}

proptest! {
    #[test]
    fn each_steps_records_hold_the_extremes_and_count_of_the_messages_they_cover(
        gaps in prop::collection::vec((0..30_000_000_i64, any::<bool>(), -1e6..1e6_f64), 1..2000),
    ) {
        let folder = TempDir::new("view-extremes");
        let path = folder.0.join("episode.view");
        let messages = messages(&gaps);
        write(&path, &messages, contents());
        let mut view = View::open(&path).unwrap();
        let about = view.about().clone();
        let origin = messages[0].1;
        prop_assert_eq!(about.messages, messages.len() as u64);
        prop_assert_eq!(about.start_utc_ns, origin);
        prop_assert_eq!(about.end_utc_ns, messages.last().unwrap().1);
        prop_assert_eq!(about.steps.last().unwrap().records, 1, "the last step spans the episode");
        for (level, step) in about.steps.iter().enumerate() {
            let records = view.records(level, i64::MIN, i64::MAX).unwrap();
            prop_assert_eq!(records.len() as u64, step.records);
            let mut covered = 0;
            for record in &records {
                let at = (record.received_utc_ns - origin) / step.step_ns;
                prop_assert_eq!(held(record), expected(&messages, origin, step.step_ns, at));
                covered += record.samples.iter().map(|sample| u64::from(sample.messages)).sum::<u64>();
                // The robot's newest pose in it, if stream 0 sent anything.
                prop_assert_eq!(record.poses.is_some(), record.samples.iter().any(|sample| sample.stream == 0));
            }
            prop_assert_eq!(covered, messages.len() as u64, "every message, once");
        }
    }

    #[test]
    fn a_stretch_reads_as_the_whole_step_does_between_its_ends(
        gaps in prop::collection::vec((0..30_000_000_i64, any::<bool>(), -1e6..1e6_f64), 1..2000),
        from in 0.0..1.0_f64,
        length in 0.0..1.0_f64,
    ) {
        let folder = TempDir::new("view-stretch");
        let path = folder.0.join("episode.view");
        let messages = messages(&gaps);
        write(&path, &messages, contents());
        let mut view = View::open(&path).unwrap();
        let (start, end) = (view.about().start_utc_ns, view.about().end_utc_ns);
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, reason = "a test's window")]
        let at = |share: f64| start + ((end - start) as f64 * share) as i64;
        let (from, to) = (at(from), at((from + length).min(1.0)));
        for level in 0..view.about().steps.len() {
            let whole = view.records(level, i64::MIN, i64::MAX).unwrap();
            let wanted: Vec<Record> = whole
                .into_iter()
                .filter(|record| (from..=to).contains(&record.received_utc_ns))
                .collect();
            prop_assert_eq!(view.records(level, from, to).unwrap(), wanted);
        }
    }
}

#[test]
fn a_view_says_what_it_shows_and_the_events_of_its_episode() {
    let folder = TempDir::new("view-about");
    let path = folder.0.join("episode.view");
    let event = Event {
        kind: "intervention".to_owned(),
        at_utc_ns: 1_000,
        until_utc_ns: None,
        phase: Phase::Begin,
        id: Some("i1".to_owned()),
        source: "cli".to_owned(),
        text: None,
        attributes: BTreeMap::new(),
    };
    let mut with_event = contents();
    with_event.events = vec![event.clone()];
    write(&path, &[(0, 1_000, 0.5), (1, 2_000, 0.25)], with_event);
    let view = View::open(&path).unwrap();
    assert_eq!(view.about().links, ["base_link"]);
    assert_eq!(view.about().streams, contents().streams);
    assert_eq!(view.about().events, [event]);
}

#[test]
fn a_view_carries_its_robots_scene_and_each_mesh_as_given() {
    let folder = TempDir::new("view-robot");
    let path = folder.0.join("episode.view");
    let scene = RobotScene {
        name: "arm7".to_owned(),
        root: "base_link".to_owned(),
        meshes: 2,
        ..RobotScene::default()
    };
    let mut with_robot = contents();
    with_robot.scene = Some(scene.clone());
    // The second couldn't be read when the view was made.
    with_robot.meshes = vec![b"KMSH first mesh".to_vec(), Vec::new()];
    write(&path, &[(0, 1_000, 0.5), (1, 2_000, 0.25)], with_robot);
    let mut view = View::open(&path).unwrap();
    assert_eq!(view.about().scene, Some(scene));
    assert_eq!(view.mesh(0).unwrap(), b"KMSH first mesh");
    assert_eq!(
        view.mesh(1).unwrap_err().code(),
        ErrorCode::RobotMeshUnreadable
    );
    assert_eq!(
        view.mesh(2).unwrap_err().code(),
        ErrorCode::ViewerMeshNotFound
    );
    // The records read as before.
    assert_eq!(view.records(0, i64::MIN, i64::MAX).unwrap().len(), 1);

    // A view of an episode without a robot has neither.
    write(&path, &[(1, 2_000, 0.25)], contents());
    let mut view = View::open(&path).unwrap();
    assert_eq!(view.about().scene, None);
    assert_eq!(
        view.mesh(0).unwrap_err().code(),
        ErrorCode::ViewerMeshNotFound
    );
}

#[test]
fn a_view_of_another_version_or_cut_off_is_unreadable() {
    let folder = TempDir::new("view-unreadable");
    let path = folder.0.join("episode.view");
    write(&path, &[(0, 1_000, 0.5)], contents());
    let whole = std::fs::read(&path).unwrap();

    let mut other = whole.clone();
    other[4..6].copy_from_slice(&99_u16.to_le_bytes());
    let cut = &whole[..whole.len() - 5];
    // A footer length past the end of the file, as damage could leave it.
    let mut damaged = whole.clone();
    let length_at = whole.len() - 8;
    damaged[length_at..length_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    for (name, bytes) in [
        ("other.view", other.as_slice()),
        ("cut.view", cut),
        ("damaged.view", damaged.as_slice()),
        ("empty.view", &[][..]),
    ] {
        let path = folder.write(name, bytes);
        let error = View::open(&path).err().unwrap();
        assert_eq!(error.code(), ErrorCode::ViewerReplayUnreadable, "{name}");
    }
    let error = View::open(&folder.write("not.view", b"KVRX\x01\x00\x00\x00"))
        .err()
        .unwrap();
    assert_eq!(error.code(), ErrorCode::ViewerReplayUnreadable);
    assert!(whole.starts_with(VIEW_MAGIC) && whole.ends_with(VIEW_MAGIC));
}

#[test]
fn a_window_is_merged_from_the_coarsest_step_that_still_has_the_points_asked_for() {
    let folder = TempDir::new("view-window");
    let path = folder.0.join("episode.view");
    // A minute of messages, one every millisecond, their value the
    // millisecond: steps of 10, 40, 160 and 640 ms, and so on.
    let messages: Vec<Message> = (0..60_000)
        .map(|ms| (0, 1_000_000 * ms, f64::from(u32::try_from(ms).unwrap())))
        .collect();
    write(&path, &messages, contents());
    let mut view = View::open(&path).unwrap();
    let minute = 59_999_000_000;
    // 600 ms a part: the 160 ms step has more than 100 parts, 640 ms fewer.
    assert_eq!(
        view.about().steps[view.step_for(0, minute, 100)].step_ns,
        160_000_000
    );
    // Finer than the finest step: the finest.
    assert_eq!(view.step_for(0, 1_000_000, 100), 0);
    // The whole step over a second.
    assert_eq!(
        view.about().steps[view.step_for(0, minute, 1)].step_ns,
        40_960_000_000
    );

    let window = view.window(0, minute, 100).unwrap();
    assert_eq!(window.span, Some((0, minute)));
    let parts = &window.streams[0].1;
    assert!(parts.len() <= 100 && parts.len() >= 99, "{}", parts.len());
    assert_eq!(parts.iter().map(|part| part.messages).sum::<u64>(), 60_000);
    assert_eq!(parts[0].extremes[0].0, 0.0);
    assert_eq!(parts.last().unwrap().extremes[0].1, 59_999.0);
    assert!(
        parts
            .windows(2)
            .all(|pair| pair[0].extremes[0].1 < pair[1].extremes[0].0)
    );
    // Posed with stream 0's messages, the robot's newest pose in each part.
    assert_eq!(window.poses.len(), parts.len());
}
