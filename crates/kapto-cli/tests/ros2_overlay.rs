// SPDX-License-Identifier: Apache-2.0

//! A robot's own ROS 2 message type, from a package built for the image and
//! mounted as an overlay: recorded and decoded with it, refused without it.
//! `cargo xtask ros2` builds the tests' own package
//! (`testdata/ros2/kapto_test_msgs`) and mounts it at
//! `/overlays/kapto_test_msgs`, where the image's entry point sources it.

#![cfg(feature = "ros2")]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const OVERLAY: &str = "/overlays/kapto_test_msgs";

/// A folder of its own for `test`, with a `streams.toml` declaring one
/// topic of the overlay's type and its decoder, and no `.msg` files: the
/// installation has them.
fn package(test: &str, topic: &str) -> PathBuf {
    let folder =
        std::env::temp_dir().join(format!("kapto-ros2-overlay-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("streams.toml"),
        format!(
            "[streams.gripper]\nrate_hz = 20\n\
             ros2 = {{ topic = \"{topic}\", type = \"kapto_test_msgs/msg/Gripper\", qos = {{ reliability = \"reliable\", depth = 100 }} }}\n\
             decoder = {{ kind = \"cdr\", fields = [\n\
               {{ name = \"opening\", type = \"f64\", path = \"opening\" }},\n\
               {{ name = \"grasped\", type = \"bool\", path = \"grasped\" }} ] }}\n"
        ),
    )
    .unwrap();
    folder
}

/// `kapto record` for `seconds`, with `env` changed.
fn record(folder: &Path, seconds: &str, env: &[(&str, OsString)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["record", "--duration", seconds, "--streams"])
        .arg(folder.join("streams.toml"))
        .arg("--data-dir")
        .arg(folder.join("data"))
        .envs(env.iter().map(|(key, value)| (key, value)))
        .output()
        .unwrap()
}

/// The search path `key` without the overlay's folders, as if it weren't
/// mounted.
fn without_overlay(key: &str) -> (&str, OsString) {
    let paths = std::env::var_os(key).unwrap_or_default();
    let kept: Vec<PathBuf> = std::env::split_paths(&paths)
        .filter(|path| !path.starts_with(OVERLAY))
        .collect();
    (key, std::env::join_paths(kept).unwrap())
}

/// The overlay is sourced: its package is on the installation's path.
fn assert_overlay_sourced() {
    let paths = std::env::var("AMENT_PREFIX_PATH").unwrap_or_default();
    assert!(
        paths.contains(OVERLAY),
        "{OVERLAY} isn't on AMENT_PREFIX_PATH ({paths}): run through `cargo xtask ros2`"
    );
}

#[test]
fn a_topic_of_a_type_mounted_as_an_overlay_is_recorded_and_decoded() {
    assert_overlay_sourced();
    let topic = format!("/kapto_overlay_test_{}", std::process::id());
    let folder = package("recorded", &topic);
    let mut publisher = Command::new("ros2")
        .args([
            "topic",
            "pub",
            "--rate",
            "20",
            "--qos-reliability",
            "reliable",
        ])
        .args([
            &topic,
            "kapto_test_msgs/msg/Gripper",
            "{opening: 0.25, grasped: true, tool: suction}",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let output = record(&folder, "6", &[]);
    let _ = publisher.kill();
    let _ = publisher.wait();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");

    let episode = std::fs::read_dir(folder.join("data"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("_FINALISED").exists())
        .unwrap();
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(episode.join("episode.json")).unwrap()).unwrap();
    let written = summary["streams"][0]["written"].as_u64().unwrap();
    assert!(written > 20, "{written} written\n{stderr}");

    // The channel's schema is the installed definition, its own file and
    // that of the standard type it uses.
    let bytes = std::fs::read(episode.join("recording.mcap")).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let schema = summary
        .channels
        .values()
        .find(|channel| channel.topic == "gripper")
        .and_then(|channel| channel.schema.clone())
        .unwrap();
    assert_eq!(schema.name, "kapto_test_msgs/msg/Gripper");
    let definition = String::from_utf8(schema.data.to_vec()).unwrap();
    assert!(definition.contains("float64 opening\n"), "{definition}");
    assert!(
        definition.contains("MSG: builtin_interfaces/Time\n"),
        "{definition}"
    );

    // Decoded with the layout the recording carries.
    let decoded = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["decode", "--format", "csv", "--stream", "gripper"])
        .arg(&episode)
        .output()
        .unwrap();
    let csv = String::from_utf8(decoded.stdout).unwrap();
    assert!(
        decoded.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    let rows: Vec<&str> = csv.lines().skip(1).collect();
    assert!(rows.len() > 20, "{csv}");
    assert!(rows.iter().all(|row| row.ends_with(",0.25,true")), "{csv}");
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn the_same_topic_is_refused_without_the_overlays_support_library() {
    assert_overlay_sourced();
    let topic = format!("/kapto_overlay_refused_{}", std::process::id());
    let folder = package("refused", &topic);

    // Its definition found, but not the library to subscribe with: as for a
    // package built for another distribution or none.
    let output = record(&folder, "1", &[without_overlay("LD_LIBRARY_PATH")]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error[transport.ros2.type_unavailable]"),
        "{stderr}"
    );
    assert!(
        stderr.contains("libkapto_test_msgs__rosidl_typesupport_c.so"),
        "{stderr}"
    );

    // Not mounted at all: where ROS 2 is installed, its definition comes
    // from the installation alone, so the file is refused when read.
    let output = record(
        &folder,
        "1",
        &[
            without_overlay("LD_LIBRARY_PATH"),
            without_overlay("AMENT_PREFIX_PATH"),
        ],
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("error[decoder.msg_not_found]"), "{stderr}");
    assert!(
        stderr.contains("kapto_test_msgs/msg/Gripper.msg"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn topics_says_which_types_load_here_with_the_remedy_for_one_that_doesnt() {
    assert_overlay_sourced();
    let topic = format!("/kapto_overlay_topics_{}", std::process::id());
    let mut publisher = Command::new("ros2")
        .args(["topic", "pub", "--rate", "5", &topic])
        .args(["kapto_test_msgs/msg/Gripper", "{opening: 0.5}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let topics = |env: &[(&str, OsString)]| {
        Command::new(env!("CARGO_BIN_EXE_kapto"))
            .args(["topics", "--wait", "4"])
            .envs(env.iter().map(|(key, value)| (key, value)))
            .output()
            .unwrap()
    };
    let mounted = topics(&[]);
    let unmounted = topics(&[without_overlay("LD_LIBRARY_PATH")]);
    let _ = publisher.kill();
    let _ = publisher.wait();
    let line_of = |output: &Output| {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(output.status.success(), "{stdout}");
        stdout
            .lines()
            .find(|line| line.starts_with(&format!("{topic} ")))
            .unwrap_or_else(|| panic!("{topic} not listed: {stdout}"))
            .to_owned()
    };

    let line = line_of(&mounted);
    assert!(line.ends_with(" kapto_test_msgs/msg/Gripper"), "{line}");
    let stderr = String::from_utf8_lossy(&mounted.stderr);
    assert!(!stderr.contains("kapto_test_msgs"), "{stderr}");

    let line = line_of(&unmounted);
    assert!(
        line.ends_with(" kapto_test_msgs/msg/Gripper  (its type doesn't load here)"),
        "{line}"
    );
    let stderr = String::from_utf8_lossy(&unmounted.stderr);
    assert!(
        stderr.contains("warning[transport.ros2.type_unavailable]"),
        "{stderr}"
    );
    assert!(
        stderr.contains("libkapto_test_msgs__rosidl_typesupport_c.so"),
        "{stderr}"
    );
    assert!(stderr.contains("/overlays/<name>"), "{stderr}");
}
