// SPDX-License-Identifier: Apache-2.0

//! Episodes started with a session profile and a task: a running capture
//! process recording the sample arm's package, told to start by
//! `kapto episode start --profile … --task …`.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::{free_udp_port, root, stop_gracefully};
use kapto_store::episode::content_hash;
use kapto_store::manifest::FINALISED_FILE;
use kapto_store::records::{
    CAPTURE_RECORD, EPISODE_RECORD, PREFLIGHT_RECORD, SUMMARY_RECORD, flatten,
};
use kapto_store::{Episode, OperationMode};
use kapto_testkit::{scratch, wait_until};

fn copy_folder(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_folder(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The settings every command of the test runs with.
fn settings(folder: &Path) -> Vec<String> {
    let path = |path: PathBuf| path.to_str().unwrap().to_owned();
    vec![
        "--config".into(),
        path(folder.join("config/kapto.toml")),
        "--data-dir".into(),
        path(folder.join("data")),
    ]
}

fn episode_start(folder: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["episode", "start"])
        .args(args)
        .args(settings(folder))
        .env_clear()
        .output()
        .unwrap()
}

/// Checks that the recording in `folder` says, in its records, everything
/// `episode` says.
/// And that it carries every file of the package in `package` byte for
/// byte, but for its meshes, each named in `kapto.capture` with the SHA-256
/// `sha256sum` gives.
fn recording_carries(folder: &Path, episode: &Episode, package: &Path) {
    let bytes = std::fs::read(folder.join(&episode.recording)).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let record = |name: &str| {
        summary
            .metadata_indexes
            .iter()
            .map(|index| mcap::read::metadata(&bytes, index).unwrap())
            .find(|record| record.name == name)
            .unwrap()
            .metadata
    };
    let attached: Vec<(String, Vec<u8>)> = summary
        .attachment_indexes
        .iter()
        .map(|index| {
            let attachment = mcap::read::attachment(&bytes, index).unwrap();
            (attachment.name, attachment.data.into_owned())
        })
        .collect();
    let read = |path: &str| std::fs::read(package.join(path)).unwrap();
    let expected: Vec<(String, Vec<u8>)> = [
        "README.md",
        "mapping.toml",
        "package.toml",
        "qa.toml",
        "streams.toml",
        "urdf/arm7.urdf",
    ]
    .into_iter()
    .map(|path| (path.to_owned(), read(path)))
    .collect();
    assert_eq!(attached, expected);
    let sha256sum = Command::new("sha256sum")
        .arg("meshes/gripper.stl")
        .current_dir(package)
        .output()
        .unwrap();
    let sha256 = String::from_utf8(sha256sum.stdout).unwrap();
    assert_eq!(
        record(CAPTURE_RECORD).get("left_out.meshes/gripper.stl"),
        sha256.split_whitespace().next().map(str::to_owned).as_ref()
    );

    // Every value of `episode.json` is in the recording's records: what it
    // was recorded with when it started, and how it went when it ended.
    // Only how long messages waited to be on disk isn't, since that
    // includes closing the file; and the fields that describe
    // `episode.json` itself.
    let mut recorded = record(EPISODE_RECORD);
    recorded.extend(record(SUMMARY_RECORD));
    recorded.extend(
        record(PREFLIGHT_RECORD)
            .into_iter()
            .map(|(name, value)| (format!("preflight.{name}"), value)),
    );
    let mut expected = flatten(episode);
    expected.retain(|name, _| {
        !["format", "version", "recording", "streams"].contains(&name.as_str())
            && !name.starts_with("durability.")
    });
    for stream in &episode.streams {
        for (field, value) in flatten(stream) {
            if field != "id" {
                expected.insert(format!("streams.{}.{field}", stream.id), value);
            }
        }
    }
    for (name, value) in &expected {
        assert_eq!(recorded.get(name), Some(value), "{name}");
    }
    for name in [
        "package.hash",
        "profile.hash",
        "task.hash",
        "streams.arm_joints.written",
    ] {
        assert!(recorded.contains_key(name), "{name}");
    }
}

#[test]
fn an_episode_keeps_the_profile_and_task_it_was_started_with_as_they_were() {
    let folder = scratch("session");
    // The sample arm's package, on a free port.
    let package = folder.join("arm7");
    copy_folder(&root().join("packages/arm7"), &package);
    // A mesh, which recordings leave out by default.
    std::fs::create_dir_all(package.join("meshes")).unwrap();
    std::fs::write(package.join("meshes/gripper.stl"), b"solid g\nendsolid g\n").unwrap();
    let streams = package.join("streams.toml");
    let declared = std::fs::read_to_string(&streams).unwrap();
    std::fs::write(
        &streams,
        declared.replace("127.0.0.1:9000", &format!("127.0.0.1:{}", free_udp_port())),
    )
    .unwrap();
    let saved = |path: &str, contents: &str| {
        let path = folder.join("config").join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    saved("kapto.toml", "");
    saved(
        "profiles/lab_ana.toml",
        "package = \"arm7\"\noperator = \"Ana\"\nmode = \"teleoperated\"\n",
    );
    saved("profiles/cobot_day.toml", "package = \"cobot\"\n");
    saved(
        "tasks/pick_and_place.toml",
        "name = \"Pick and place\"\nsubtasks = [\"reach\", \"grasp\"]\n",
    );

    let capture = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["capture", "--streams"])
        .arg(&streams)
        .args(settings(&folder))
        // Its robot never sends: pre-flight's failed checks mustn't refuse.
        .args(["--capture-preflight", "warn"])
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let socket = folder.join("data/capture.sock");
    wait_until("capture to listen", || socket.exists());

    // For another robot package, or not there: refused, nothing recorded.
    let other = episode_start(&folder, &["--profile", "cobot_day"]);
    assert!(!other.status.success());
    let refusal = String::from_utf8(other.stderr).unwrap();
    assert!(refusal.contains("error[profile.unavailable]"), "{refusal}");
    assert!(refusal.contains("profile `cobot_day` records robot package `cobot`, and capture records robot package `arm7`"), "{refusal}");
    let missing = episode_start(&folder, &["--task", "stacking"]);
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("error[task.not_found]")
    );

    let started = episode_start(
        &folder,
        &[
            "--profile",
            "lab_ana",
            "--task",
            "pick_and_place",
            "--duration",
            "1",
        ],
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    // Changed while recording: the episode keeps the copy taken at start.
    saved(
        "profiles/lab_ana.toml",
        "package = \"arm7\"\noperator = \"Ben\"\n",
    );
    let data = folder.join("data");
    let finalised = || {
        std::fs::read_dir(&data)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.join(FINALISED_FILE).exists())
    };
    wait_until("the episode to be finalised", || finalised().is_some());
    let folder = finalised().unwrap();
    let episode = Episode::read(&folder).unwrap();

    // The recording says the same, on its own, and carries the package.
    recording_carries(&folder, &episode, &package);

    let profile = episode.profile.unwrap();
    assert_eq!(
        (
            profile.id.as_str(),
            profile.operator.as_deref(),
            profile.mode
        ),
        ("lab_ana", Some("Ana"), Some(OperationMode::Teleoperated))
    );
    // Each copy carries its content hash.
    assert_eq!(profile.hash.clone(), Some(content_hash(&profile)));
    let task = episode.task.unwrap();
    assert_eq!(
        (task.id.as_str(), task.subtasks.len()),
        ("pick_and_place", 2)
    );
    assert_eq!(task.hash.clone(), Some(content_hash(&task)));
    assert_eq!(episode.package.unwrap().id, "arm7");

    stop_gracefully(capture);
}
