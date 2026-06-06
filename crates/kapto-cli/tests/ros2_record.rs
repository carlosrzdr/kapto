// SPDX-License-Identifier: Apache-2.0

//! `kapto record` on a ROS 2 topic, run as a real process: built without
//! ROS 2 it says which image has it; built with it (in the ROS 2 image), it
//! records a topic another process publishes into a recording ROS 2's own
//! tools read.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A folder of its own for `test`, with a `streams.toml` declaring one
/// `sensor_msgs/msg/JointState` topic and the `.msg` files it uses.
fn package(test: &str, topic: &str) -> PathBuf {
    let folder =
        std::env::temp_dir().join(format!("kapto-ros2-record-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let files = [
        (
            "msg/sensor_msgs/msg/JointState.msg",
            "std_msgs/Header header\nstring[] name\nfloat64[] position\nfloat64[] velocity\nfloat64[] effort\n",
        ),
        (
            "msg/std_msgs/msg/Header.msg",
            "builtin_interfaces/Time stamp\nstring frame_id\n",
        ),
        (
            "msg/builtin_interfaces/msg/Time.msg",
            "int32 sec\nuint32 nanosec\n",
        ),
        (
            "streams.toml",
            &format!(
                "[streams.joints]\nrate_hz = 50\n\
                 ros2 = {{ topic = \"{topic}\", type = \"sensor_msgs/msg/JointState\", qos = {{ reliability = \"reliable\", depth = 100 }} }}\n"
            ),
        ),
    ];
    for (path, contents) in files {
        let path = folder.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    folder
}

fn record(folder: &Path, seconds: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["record", "--duration", seconds, "--streams"])
        .arg(folder.join("streams.toml"))
        .arg("--data-dir")
        .arg(folder.join("data"))
        .output()
        .unwrap()
}

#[cfg(not(feature = "ros2"))]
#[test]
fn without_ros_2_recording_a_topic_says_which_image_has_it() {
    let folder = package("slim", "/joint_states");
    let output = record(&folder, "1");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error[transport.ros2.not_built]"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[cfg(feature = "ros2")]
#[test]
fn a_topic_is_recorded_as_ros_2_reads_a_bag_and_exported_replays_on_its_own_topic() {
    let topic = format!("/kapto_record_test_{}", std::process::id());
    let folder = package("ros2", &topic);
    let mut publisher = Command::new("ros2")
        .args([
            "topic",
            "pub",
            "--rate",
            "50",
            "--qos-reliability",
            "reliable",
            &topic,
        ])
        .args([
            "sensor_msgs/msg/JointState",
            "{name: [shoulder], position: [0.5]}",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let output = record(&folder, "6");
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
    assert!(written > 50, "{written} written\n{stderr}");

    // ROS 2's own tool reads the recording as a bag, the topic's messages
    // on the stream's channel, with every message Kapto says it wrote.
    let info = Command::new("ros2")
        .args(["bag", "info", "--storage", "mcap"])
        .arg(episode.join("recording.mcap"))
        .output()
        .unwrap();
    let info = String::from_utf8_lossy(&info.stdout).into_owned();
    let line = info
        .lines()
        .find(|line| line.contains("Topic: joints "))
        .unwrap_or_else(|| panic!("{info}"));
    assert!(line.contains("Type: sensor_msgs/msg/JointState"), "{line}");
    assert!(line.contains(&format!("Count: {written} ")), "{line}");
    assert!(line.contains("Serialization Format: cdr"), "{line}");

    // The channel names the topic it was received on.
    let bytes = std::fs::read(episode.join("recording.mcap")).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let joints = summary
        .channels
        .values()
        .find(|channel| channel.topic == "joints")
        .unwrap();
    assert_eq!(joints.metadata.get("ros2.topic"), Some(&topic));

    // Exported, ROS 2's own player replays it on the topic it came from,
    // with no flags.
    let bag = folder.join("episode.mcap");
    let exported = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["export", "rosbag2"])
        .arg(&episode)
        .arg("--output")
        .arg(&bag)
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let mut echo = Command::new("ros2")
        .args([
            "topic",
            "echo",
            "--once",
            &topic,
            "sensor_msgs/msg/JointState",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Long enough for the echo, a Python program, to subscribe.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let played = Command::new("ros2")
        .args(["bag", "play"])
        .arg(&bag)
        .output()
        .unwrap();
    assert!(
        played.status.success(),
        "{}",
        String::from_utf8_lossy(&played.stderr)
    );
    let echoed = printed_by(&mut echo, std::time::Duration::from_secs(10));
    assert!(
        echoed.contains("shoulder"),
        "nothing arrived on {topic}: {echoed}"
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[cfg(feature = "ros2")]
#[test]
fn static_transforms_published_before_recording_are_recorded_with_the_moving_ones() {
    let id = std::process::id();
    let (moving, fixed) = (format!("/kapto_tf_{id}"), format!("/kapto_tf_static_{id}"));
    let folder = std::env::temp_dir().join(format!("kapto-ros2-record-{id}-tf"));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    // The static transforms' topic keeps its last message for whoever
    // subscribes later, as robot_state_publisher's does; Kapto has to ask
    // for it.
    std::fs::write(
        folder.join("streams.toml"),
        format!(
            "[streams.tf]\nrate_hz = 20\n\
             ros2 = {{ topic = \"{moving}\", type = \"tf2_msgs/msg/TFMessage\" }}\n\
             [streams.tf_static]\n\
             ros2 = {{ topic = \"{fixed}\", type = \"tf2_msgs/msg/TFMessage\", \
             qos = {{ reliability = \"reliable\", durability = \"transient_local\" }} }}\n"
        ),
    )
    .unwrap();
    let transform = |child: &str| {
        format!(
            "{{transforms: [{{header: {{frame_id: base_link}}, child_frame_id: {child}, \
             transform: {{translation: {{x: 0.1}}, rotation: {{w: 1.0}}}}}}]}}"
        )
    };
    let publish = |topic: &str, args: &[&str], message: String| {
        Command::new("ros2")
            .args(["topic", "pub"])
            .args(args)
            .args([topic, "tf2_msgs/msg/TFMessage", &message])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let once = [
        "--once",
        "--keep-alive",
        "30",
        "--qos-reliability",
        "reliable",
        "--qos-durability",
        "transient_local",
    ];
    let mut publishers = [
        publish(&fixed, &once, transform("camera_link")),
        publish(&moving, &["--rate", "20"], transform("tool0")),
    ];
    // Published, and done publishing, well before Kapto subscribes.
    std::thread::sleep(std::time::Duration::from_secs(4));
    let output = record(&folder, "4");
    for publisher in &mut publishers {
        let _ = publisher.kill();
        let _ = publisher.wait();
    }
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");

    let episode = std::fs::read_dir(folder.join("data"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("_FINALISED").exists())
        .unwrap();
    let summary: serde_json::Value =
        serde_json::from_slice(&std::fs::read(episode.join("episode.json")).unwrap()).unwrap();
    let written = |stream: &str| {
        summary["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == stream)
            .and_then(|entry| entry["written"].as_u64())
            .unwrap_or_else(|| panic!("no {stream} in {summary}"))
    };
    assert_eq!(written("tf_static"), 1, "{stderr}");
    assert!(written("tf") > 20, "{stderr}");
    let bytes = std::fs::read(episode.join("recording.mcap")).unwrap();
    let fixed_message = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .find(|message| message.channel.topic == "tf_static")
        .unwrap();
    assert_eq!(
        fixed_message.channel.schema.as_ref().unwrap().name,
        "tf2_msgs/msg/TFMessage"
    );
    assert!(
        fixed_message
            .data
            .windows(11)
            .any(|bytes| bytes == b"camera_link"),
        "the static transform as published"
    );
    let _ = std::fs::remove_dir_all(folder);
}

/// What `child` printed, once it ends or after `limit`, when it is stopped.
#[cfg(feature = "ros2")]
fn printed_by(child: &mut std::process::Child, limit: std::time::Duration) -> String {
    use std::io::Read;

    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() && started.elapsed() < limit {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut printed = String::new();
    let _ = child.stdout.take().unwrap().read_to_string(&mut printed);
    printed
}
