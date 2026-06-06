// SPDX-License-Identifier: Apache-2.0

//! An episode's replay, made from real recordings of the sample arm and
//! read back from its replay view: what it holds, how often the robot is
//! posed, over the episode's own times, and the robot it was recorded with.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kapto_capture::{Batch, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, ErrorCode, KaptoError, RecvTime};
use kapto_robot::{is_mesh_or_texture, load_package};
use kapto_store::records::EPISODE_RECORD;
use kapto_testkit::TempDir;
use kapto_viz::page_scene::MESH_MAGIC;
use kapto_viz::view::View;
use kapto_viz::{DrawnRobots, ReplaySettings, Replayed, write_replay};

/// When the episode's first message was received, in UTC nanoseconds.
const START_UTC_NS: i64 = 1_800_000_000_000_000_000;
/// The sample arm's rate: a message every half millisecond.
const PERIOD_NS: u64 = 500_000;

fn arm7(path: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/arm7")
            .join(path),
    )
    .unwrap()
}

/// The sample arm's package in `folder`, its first link with a mesh
/// besides its own shapes when `with_mesh`.
fn package(folder: &TempDir, with_mesh: bool) -> PathBuf {
    let mut urdf = String::from_utf8(arm7("urdf/arm7.urdf")).unwrap();
    if with_mesh {
        urdf = urdf.replacen(
            "<link name=\"base_link\">",
            "<link name=\"base_link\"><visual><geometry><mesh filename=\"meshes/tetrahedron.stl\"/></geometry></visual>",
            1,
        );
        let mesh = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/meshes/handmade/tetrahedron.stl");
        folder.write(
            "arm7/urdf/meshes/tetrahedron.stl",
            &std::fs::read(mesh).unwrap(),
        );
    }
    folder.write("arm7/urdf/arm7.urdf", urdf.as_bytes());
    for file in ["package.toml", "streams.toml", "mapping.toml"] {
        folder.write(&format!("arm7/{file}"), &arm7(file));
    }
    folder.0.join("arm7")
}

/// The sample arm's message: every joint at `angle`, the gripper half open.
fn message(sequence: u32, angle: f64) -> Vec<u8> {
    let mut payload = sequence.to_le_bytes().to_vec();
    payload.extend(7_u64.to_le_bytes());
    for _ in 0..7 {
        payload.extend(angle.to_le_bytes());
    }
    payload.extend(0.02_f64.to_le_bytes());
    payload
}

/// Records `messages` of the sample arm's stream into `folder`, carrying
/// the package in `package` but its meshes, as recordings do by default;
/// with no package, carrying its `streams.toml` alone.
fn record(folder: &TempDir, package: Option<&Path>, messages: u32) -> PathBuf {
    let path = folder.0.join("recording.mcap");
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: START_UTC_NS,
        uncertainty: Duration::ZERO,
    });
    let streams = [ChannelSpec::from((0, "arm_joints".parse().unwrap()))];
    let mut recording =
        Recording::create(&path, &streams, &clock, &WriterConfig::default()).unwrap();
    match package.map(|folder| load_package(folder).unwrap()) {
        Some(package) => {
            let record = BTreeMap::from([
                ("package.id".to_owned(), package.manifest.id.clone()),
                ("package.hash".to_owned(), package.hash.clone()),
            ]);
            recording.write_record(EPISODE_RECORD, record).unwrap();
            let carried = package.contents(|file| !is_mesh_or_texture(&file.path));
            for (file, data) in carried.unwrap() {
                recording
                    .attach(&file.path, "text/plain", 0, &data)
                    .unwrap();
            }
        }
        None => recording
            .attach("streams.toml", "text/plain", 0, &arm7("streams.toml"))
            .unwrap(),
    }
    let counters = LossCounters::new(1);
    for n in 0..messages {
        let payload = message(n, f64::from(n) / 1000.0);
        let mut batch = Batch::new(1, payload.len());
        batch.push(0, &payload);
        batch.set_recv_time(RecvTime::from_nanos(u64::from(n) * PERIOD_NS));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();
    path
}

/// Replays `recording` into `folder`'s `name.view`, with local packages in
/// `packages` and the robot kept in `robots`: the view, what went into it
/// and its problems' codes.
fn viewed(
    recording: &Path,
    packages: &Path,
    robots: &DrawnRobots,
    name: &str,
) -> (View, Replayed, Vec<ErrorCode>) {
    let settings = ReplaySettings {
        display_rate: 60,
        packages: packages.to_owned(),
        robots: robots.clone(),
    };
    let path = recording.with_file_name(format!("{name}.view"));
    let replayed = write_replay(recording, &path, &settings, &mut || Ok(())).unwrap();
    let codes = replayed.problems.iter().map(KaptoError::code).collect();
    (View::open(&path).unwrap(), replayed, codes)
}

/// The replay of `recording` drawn afresh, as [`viewed`].
fn replayed(recording: &Path, packages: &Path) -> (View, Replayed, Vec<ErrorCode>) {
    viewed(recording, packages, &DrawnRobots::default(), "episode")
}

#[test]
fn every_message_is_kept_and_the_robot_posed_at_the_display_rate_over_the_episodes_times() {
    let folder = TempDir::new("replay-arm");
    let package = package(&folder, false);
    let recording = record(&folder, Some(&package), 1000);
    let (mut view, replayed, problems) = replayed(&recording, &folder.0.join("elsewhere"));
    assert_eq!(problems, []);

    let last_ns = START_UTC_NS + 999 * 500_000;
    let about = view.about().clone();
    assert_eq!(
        (about.start_utc_ns, about.end_utc_ns),
        (START_UTC_NS, last_ns)
    );
    assert_eq!((about.messages, replayed.messages), (1000, 1000));
    // The sequence number and the robot's clock only climb: not plotted.
    let plotted: Vec<(&str, bool)> = about.streams[0]
        .values
        .iter()
        .map(|value| (value.name.as_str(), value.plotted))
        .collect();
    assert_eq!(plotted.len(), 10, "{plotted:?}");
    assert_eq!(&plotted[..2], [("sequence", false), ("source_time", false)]);
    assert!(plotted[2..].iter().all(|(_, plotted)| *plotted));

    // A pose with the first message, then with the first message at least
    // a sixtieth of a second after the one before (every 34th, 17 ms
    // apart).
    let records = view.records(0, i64::MIN, i64::MAX).unwrap();
    let posed: Vec<i64> = records
        .iter()
        .filter(|record| record.poses.is_some())
        .map(|record| record.received_utc_ns)
        .collect();
    assert_eq!(u64::try_from(posed.len()).unwrap(), replayed.poses);
    assert_eq!(replayed.poses, 30);
}

#[test]
fn meshes_the_recording_leaves_out_come_from_the_local_package_with_the_same_content() {
    let folder = TempDir::new("replay-mesh");
    let package = package(&folder, true);
    let recording = record(&folder, Some(&package), 10);
    let (mut view, _, problems) = replayed(&recording, &folder.0);
    assert_eq!(problems, []);
    assert_eq!(view.about().scene.as_ref().unwrap().meshes, 1);
    assert!(view.mesh(0).unwrap().starts_with(MESH_MAGIC));
}

#[test]
fn without_its_meshes_the_replay_says_why_and_still_shows_the_rest() {
    let folder = TempDir::new("replay-no-mesh");
    let package = package(&folder, true);
    let recording = record(&folder, Some(&package), 10);
    // The package changed since: its content no longer matches.
    std::fs::write(package.join("README.md"), "changed").unwrap();
    let (view, replayed, problems) = replayed(&recording, &folder.0);
    assert_eq!(problems, [ErrorCode::RobotMeshUnreadable]);
    assert_eq!(view.about().scene.as_ref().unwrap().meshes, 0);
    assert!(replayed.poses > 0);
    assert_eq!(view.about().messages, 10);
}

#[test]
fn a_ros_2_stream_is_replayed_with_the_msg_files_the_recording_carries() {
    let folder = TempDir::new("replay-cdr");
    let package = package(&folder, false);
    let mut streams = String::from_utf8(arm7("streams.toml")).unwrap();
    streams.push_str(concat!(
        "\n[streams.gripper_force]\nudp = { bind = \"0.0.0.0:9100\" }\n",
        "\n[streams.gripper_force.decoder]\nkind = \"cdr\"\n",
        "type = \"std_msgs/msg/Float64\"\n",
        "fields = [{ name = \"force\", type = \"f64\", path = \"data\", unit = \"N\" }]\n",
    ));
    folder.write("arm7/streams.toml", streams.as_bytes());
    folder.write("arm7/msg/std_msgs/msg/Float64.msg", b"float64 data\n");
    let recording = record(&folder, Some(&package), 10);
    // Not beside the package any more: only the recording has them.
    std::fs::remove_dir_all(&package).unwrap();

    let (view, replayed, problems) = replayed(&recording, &folder.0);
    assert_eq!(problems, []);
    assert_eq!(replayed.messages, 10);
    // Its field, read from the definition the recording carries.
    let force = &view.about().streams[1];
    let values: Vec<&str> = force
        .values
        .iter()
        .map(|value| value.name.as_str())
        .collect();
    assert_eq!(
        (force.name.as_str(), values),
        ("gripper_force", vec!["force"])
    );
}

#[test]
fn a_recording_made_without_a_package_is_replayed_as_values_without_a_robot() {
    let folder = TempDir::new("replay-unchecked");
    let recording = record(&folder, None, 10);
    let (mut view, replayed, problems) = replayed(&recording, &folder.0);
    assert_eq!(problems, [ErrorCode::ViewerReplayWithoutRobot]);
    assert_eq!((replayed.poses, view.about().messages), (0, 10));
    assert_eq!(view.about().scene, None);
    let records = view.records(0, i64::MIN, i64::MAX).unwrap();
    assert!(records.iter().all(|record| record.poses.is_none()));
}

#[test]
fn a_replay_appears_only_once_complete() {
    let folder = TempDir::new("replay-file");
    let recording = record(&folder, None, 10);
    let settings = ReplaySettings {
        display_rate: 60,
        packages: folder.0.clone(),
        robots: DrawnRobots::default(),
    };
    let view = folder.0.join("episode.view");
    write_replay(&recording, &view, &settings, &mut || Ok(())).unwrap();
    assert!(std::fs::metadata(&view).unwrap().len() > 0);
    assert!(!folder.0.join("episode.view.partial").exists());

    let missing = folder.0.join("missing.view");
    let error = write_replay(
        &folder.0.join("nothing.mcap"),
        &missing,
        &settings,
        &mut || Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::RecordingUnreadable);
    assert!(!missing.exists());
    assert!(!folder.0.join("missing.view.partial").exists());
}

#[test]
fn a_replays_view_poses_the_robot_at_the_display_rate_across_the_whole_episode() {
    let folder = TempDir::new("replay-view");
    let package = package(&folder, false);
    // Two seconds of the sample arm.
    let recording = record(&folder, Some(&package), 4000);
    let (mut view, replayed, _) = replayed(&recording, &folder.0.join("no-packages"));
    let about = view.about().clone();
    let last = START_UTC_NS + 3999 * i64::try_from(PERIOD_NS).unwrap();
    assert_eq!((about.start_utc_ns, about.end_utc_ns), (START_UTC_NS, last));
    assert_eq!(about.messages, 4000);
    assert!(
        about.links.iter().any(|link| link == "base_link"),
        "{:?}",
        about.links
    );
    assert_eq!(about.streams[0].name, "arm_joints");
    let records = view.records(0, i64::MIN, i64::MAX).unwrap();
    let posed: Vec<i64> = records
        .iter()
        .filter(|record| record.poses.is_some())
        .map(|record| record.received_utc_ns)
        .collect();
    // A pose in every sixtieth of a second, from the first to the last.
    assert_eq!(posed.len(), records.len());
    assert!((119..=121).contains(&posed.len()), "{}", posed.len());
    assert!(posed.windows(2).all(|pair| pair[1] - pair[0] <= 17_000_000));
    assert!(*posed.last().unwrap() > last - 17_000_000);
    assert_eq!(u64::try_from(posed.len()).unwrap(), replayed.poses);
}

#[test]
fn a_replays_view_carries_the_robot_it_was_recorded_with_whatever_its_package_is_now() {
    let folder = TempDir::new("replay-view-robot");
    let package = package(&folder, true);
    let recording = record(&folder, Some(&package), 10);
    // Nothing kept from the replay before.
    let viewed = |name: &str| viewed(&recording, &folder.0, &DrawnRobots::default(), name).0;

    // The package as it was recorded with: the mesh from it.
    let mut view = viewed("unchanged");
    let scene = view.about().scene.clone().unwrap();
    assert_eq!(
        (scene.name.as_str(), scene.root.as_str()),
        ("arm7", "base_link")
    );
    assert_eq!(scene.links[0].name, "base_link");
    assert_eq!(scene.meshes, 1);
    assert!(view.mesh(0).unwrap().starts_with(MESH_MAGIC));

    // The package since renamed its robot: the replay keeps the recorded
    // description, without the mesh the recording didn't carry.
    let urdf = std::fs::read_to_string(package.join("urdf/arm7.urdf")).unwrap();
    std::fs::write(
        package.join("urdf/arm7.urdf"),
        urdf.replace("<robot name=\"arm7\">", "<robot name=\"arm7_v2\">"),
    )
    .unwrap();
    let mut view = viewed("changed");
    let scene = view.about().scene.clone().unwrap();
    assert_eq!(scene.name, "arm7");
    assert_eq!(scene.meshes, 0);
    assert_eq!(
        view.mesh(0).unwrap_err().code(),
        ErrorCode::ViewerMeshNotFound
    );
}

#[test]
fn a_robot_drawn_whole_is_kept_for_the_next_replay_of_the_same_package() {
    let folder = TempDir::new("replay-kept-robot");
    let package = package(&folder, true);
    let recording = record(&folder, Some(&package), 10);
    let kept = DrawnRobots::default();
    let replay_with = |robots: &DrawnRobots, name: &str| {
        let (mut view, _, codes) = viewed(&recording, &folder.0, robots, name);
        let meshes = view.about().scene.clone().unwrap().meshes;
        (codes, meshes, view.mesh(0).ok())
    };
    let (problems, meshes, mesh) = replay_with(&kept, "first");
    assert_eq!((problems, meshes), (vec![], 1));
    let first = mesh.unwrap();

    // The package changed since, so the recording's own files are used,
    // which leave its mesh out: drawn afresh, the mesh is missing...
    std::fs::write(package.join("README.md"), "changed").unwrap();
    let (problems, meshes, _) = replay_with(&DrawnRobots::default(), "afresh");
    assert_eq!(
        (problems, meshes),
        (vec![ErrorCode::RobotMeshUnreadable], 0)
    );
    // ...but the robot kept from the same recorded package still has it.
    let (problems, meshes, mesh) = replay_with(&kept, "kept");
    assert_eq!((problems, meshes), (vec![], 1));
    assert_eq!(mesh.unwrap(), first);
}

#[test]
fn a_replay_stopped_part_way_leaves_no_file_behind() {
    let folder = TempDir::new("replay-stopped");
    let recording = record(&folder, None, 10_000);
    let settings = ReplaySettings {
        display_rate: 60,
        packages: folder.0.clone(),
        robots: DrawnRobots::default(),
    };
    let out = folder.0.join("episode.view");
    let mut steps = 0;
    let error = write_replay(&recording, &out, &settings, &mut || {
        steps += 1;
        Err(KaptoError::new(ErrorCode::WorkerJobStopped))
    })
    .unwrap_err();
    assert_eq!((error.code(), steps), (ErrorCode::WorkerJobStopped, 1));
    assert!(!out.exists());
    assert!(!folder.0.join("episode.view.partial").exists());
}
