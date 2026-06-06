// SPDX-License-Identifier: Apache-2.0

//! A ROS 2 robot through all of Kapto: `kapto serve` with the sample arm's
//! ROS 2 package (`packages/arm7_ros2`), fed by the synthetic robot
//! publishing its joint states on `/joint_states`. The arm moves live, an
//! episode is recorded, checked and replayed with the arm's poses, and
//! every episode starts with the fixed transforms published once, before
//! Kapto started. Only
//! with the `ros2` feature, run in the ROS 2 image over each middleware by
//! `cargo xtask ros2`.

#![cfg(all(unix, feature = "ros2"))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::root;
use common::serve::Serve;
use kapto_loadgen::{Config, Transport, run_until};
use kapto_testkit::{scratch, wait_until};
use serde_json::Value;

fn json(serve: &Serve, method: &str, path: &str) -> Value {
    let body = (method == "POST").then_some("{}");
    let (code, body) = serve.http(method, path, body);
    assert!((200..300).contains(&code), "{method} {path}: {code} {body}");
    serde_json::from_str(&body).unwrap()
}

/// The arm's newest joint positions and gripper opening, as the live view
/// decoded them.
fn live_values(serve: &Serve) -> Option<Vec<f64>> {
    let status = json(serve, "GET", "/api/status");
    let values = status["capture"]["values"].as_array()?.first()?.clone();
    assert_eq!(values["stream"], "arm_joints");
    serde_json::from_value(values["values"].clone()).ok()
}

/// The sample arm's package copied into `folder`, also declaring the
/// fixed transforms on `topic`, kept for late subscribers as
/// `robot_state_publisher` sends them.
fn package_with_fixed_transforms(folder: &Path, topic: &str) -> std::path::PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let package = folder.join("arm7_ros2");
    copy(&root().join("packages/arm7_ros2"), &package);
    let streams = std::fs::read_to_string(package.join("streams.toml")).unwrap();
    std::fs::write(
        package.join("streams.toml"),
        format!(
            "{streams}\n[streams.tf_static]\n\
             ros2 = {{ topic = \"{topic}\", type = \"tf2_msgs/msg/TFMessage\", \
             qos = {{ reliability = \"reliable\", durability = \"transient_local\" }} }}\n"
        ),
    )
    .unwrap();
    package
}

/// Publishes the camera's fixed transform on `topic` once, kept for late
/// subscribers, as `robot_state_publisher` does.
fn publish_fixed_transform(topic: &str) -> Child {
    Command::new("ros2")
        .args(["topic", "pub", "--once", "--keep-alive", "600"])
        .args([
            "--qos-reliability",
            "reliable",
            "--qos-durability",
            "transient_local",
        ])
        .args([topic, "tf2_msgs/msg/TFMessage"])
        .arg(
            "{transforms: [{header: {frame_id: base_link}, child_frame_id: camera_link, \
             transform: {translation: {x: 0.1}, rotation: {w: 1.0}}}]}",
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// The first message of stream `tf_static` in episode `id`'s recording.
fn first_fixed_transform(folder: &Path, id: &str) -> (u64, Vec<u8>) {
    let bytes = std::fs::read(folder.join("data").join(id).join("recording.mcap")).unwrap();
    let message = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .find(|message| message.channel.topic == "tf_static")
        .unwrap();
    (message.log_time, message.data.to_vec())
}

#[test]
fn a_ros_2_robot_moves_live_and_its_episode_is_recorded_checked_and_replayed_with_its_poses() {
    let folder = scratch("ros2-serve");
    let topic = format!("/kapto_tf_static_{}", std::process::id());
    let package = package_with_fixed_transforms(&folder, &topic);
    // Sent, and done sending, before Kapto starts.
    let mut fixed = publish_fixed_transform(&topic);
    std::thread::sleep(Duration::from_secs(3));
    let serve = Serve::start_robot(
        &folder,
        &package.join("urdf/arm7.urdf"),
        &package.join("streams.toml"),
        &package.join("mapping.toml"),
        &[],
    );
    serve.wait_for("Kapto is running: ", 1);

    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let robot = scope.spawn(|| {
            let config = Config {
                target: "127.0.0.1:9000".parse().unwrap(),
                streams: 1,
                rate_hz: 2_000,
                duration: None,
                spin: Duration::from_micros(100),
                transport: Transport::Ros2,
                wait_for_subscribers: None,
                camera: None,
            };
            run_until(&config, &stop, |_, _| {}).unwrap()
        });

        // Live: the arm's values arrive and change.
        wait_until("the arm's values live", || live_values(&serve).is_some());
        let first = live_values(&serve).unwrap();
        assert_eq!(first.len(), 8, "seven joints and the gripper: {first:?}");
        wait_until("the arm moving live", || {
            live_values(&serve).is_some_and(|now| now != first)
        });

        let started = json(&serve, "POST", "/api/episodes/start");
        let id = started["episode"].as_str().unwrap().to_owned();
        std::thread::sleep(Duration::from_secs(2));
        json(&serve, "POST", "/api/episodes/stop");
        stop.store(true, Ordering::Relaxed);
        let (sent, _) = robot.join().unwrap();
        assert_eq!(sent.send_errors, 0);

        // Recorded with the package, every message received written.
        let episode = json(&serve, "GET", &format!("/api/episodes/{id}"));
        assert_eq!(episode["package"]["id"], "arm7_ros2");
        let stream = &episode["streams"][0];
        assert_eq!(stream["stream"], "arm_joints");
        assert!(stream["written"].as_u64().unwrap() > 2_000, "{stream}");
        assert_eq!(stream["decoded"], stream["received"], "{stream}");
        // It starts with the fixed transform, sent long before, as does
        // the next episode.
        let again = json(&serve, "POST", "/api/episodes/start");
        let again = again["episode"].as_str().unwrap().to_owned();
        std::thread::sleep(Duration::from_secs(1));
        json(&serve, "POST", "/api/episodes/stop");
        for id in [&id, &again] {
            let episode = json(&serve, "GET", &format!("/api/episodes/{id}"));
            let kept = &episode["streams"][1];
            assert_eq!(kept["stream"], "tf_static");
            assert_eq!(
                (&kept["carried"], &kept["received"]),
                (&1.into(), &0.into()),
                "{kept}"
            );
            let (at, data) = first_fixed_transform(&folder, id);
            assert_eq!(at, episode["started_utc_ns"].as_u64().unwrap());
            assert!(data.windows(11).any(|bytes| bytes == b"camera_link"));
        }

        // Checked: the package's gate passes, nothing having been dropped.
        let quality = || json(&serve, "GET", &format!("/api/episodes/{id}/qa"));
        wait_until("the episode checked", || !quality()["accepted"].is_null());
        assert_eq!(quality()["accepted"], true, "{}", quality());

        // Replayed: the arm's links posed through the episode.
        json(&serve, "POST", &format!("/api/episodes/{id}/replay"));
        let replay = || json(&serve, "GET", &format!("/api/episodes/{id}/replay"));
        wait_until("the replay made", || replay()["state"] == "ready");
        assert_eq!(replay()["lacks"], serde_json::json!([]), "{}", replay());
        let (from, to) = (&episode["started_utc_ns"], &episode["ended_utc_ns"]);
        let window = reqwest::blocking::get(format!(
            "{}/episodes/{id}/replay/window?from={from}&to={to}&points=100",
            serve.api()
        ))
        .unwrap()
        .bytes()
        .unwrap();
        assert_eq!(&window[..4], b"KVHI");
        let count =
            |offset: usize| u32::from_le_bytes(window[offset..offset + 4].try_into().unwrap());
        assert_eq!(count(28), 11, "the arm's links");
        assert!(count(32) > 0, "no poses");
    });
    let _ = fixed.kill();
    let _ = fixed.wait();
}
