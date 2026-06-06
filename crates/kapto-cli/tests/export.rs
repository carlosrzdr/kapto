// SPDX-License-Identifier: Apache-2.0

//! `kapto export rosbag2`, run as a real process on a recording made with
//! Kapto's own writer: the ROS 2 topics alone, each on its own name.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use kapto_capture::{Batch, ChannelSchema, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, RecvTime};
use kapto_testkit::TempDir;

const SECOND: u64 = 1_000_000_000;
const DEFINITION: &[u8] = b"float64 data\n";

/// A recording of a ROS 2 topic (stream `force`, from `/gripper/force`)
/// beside a UDP stream, unless `with_topic` is false: two messages each.
fn recording(dir: &Path, with_topic: bool) -> std::path::PathBuf {
    let path = dir.join("recording.mcap");
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 1_800_000_000 * 1_000_000_000,
        uncertainty: Duration::ZERO,
    });
    let mut force = ChannelSpec::from((0, "force".parse().unwrap()));
    if with_topic {
        force.schema = Some(ChannelSchema {
            message_encoding: "cdr".to_owned(),
            name: "std_msgs/msg/Float64".to_owned(),
            encoding: "ros2msg".to_owned(),
            data: DEFINITION.to_vec(),
            metadata: BTreeMap::from([("ros2.topic".to_owned(), "/gripper/force".to_owned())]),
        });
    }
    let streams = [force, ChannelSpec::from((1, "arm".parse().unwrap()))];
    let mut writer = Recording::create(&path, &streams, &clock, &WriterConfig::default()).unwrap();
    let counters = LossCounters::new(2);
    for n in 0..2_u64 {
        let mut batch = Batch::new(2, 64);
        batch.push_with_source_time(
            0,
            &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x40],
            1_700_000_000 * SECOND + n,
        );
        batch.push(1, b"udp");
        batch.set_recv_time(RecvTime::from_nanos(n * SECOND));
        writer.write(&batch, &counters).unwrap();
    }
    writer.finish(&clock).unwrap();
    path
}

fn export(recording: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["export", "rosbag2"])
        .arg(recording)
        .arg("--output")
        .arg(output)
        .output()
        .unwrap()
}

#[test]
fn a_recordings_ros_2_topics_are_exported_alone_on_their_own_names_with_both_times() {
    let dir = TempDir::new("export");
    let recorded = recording(&dir.0, true);
    let bag = dir.0.join("bag.mcap");
    let output = export(&recorded, &bag);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("/gripper/force: 2 messages"), "{stdout}");

    let bytes = std::fs::read(&bag).unwrap();
    let messages: Vec<_> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        messages.len(),
        2,
        "the UDP stream and Kapto's events left out"
    );
    let channel = &messages[0].channel;
    assert_eq!(
        (channel.topic.as_str(), channel.message_encoding.as_str()),
        ("/gripper/force", "cdr")
    );
    let schema = channel.schema.as_ref().unwrap();
    assert_eq!(
        (
            schema.name.as_str(),
            schema.encoding.as_str(),
            &schema.data[..]
        ),
        ("std_msgs/msg/Float64", "ros2msg", DEFINITION)
    );
    let times: Vec<(u64, u64)> = messages
        .iter()
        .map(|message| (message.log_time, message.publish_time))
        .collect();
    let received = 1_800_000_000 * SECOND;
    assert_eq!(
        times,
        [
            (received, 1_700_000_000 * SECOND),
            (received + SECOND, 1_700_000_000 * SECOND + 1)
        ]
    );
    // The file's header record (after the 8-byte magic: opcode, length,
    // then the profile as a length-prefixed string) names ROS 2's profile.
    let profile_len = u32::from_le_bytes(bytes[17..21].try_into().unwrap()) as usize;
    assert_eq!(
        (bytes[8], &bytes[21..21 + profile_len]),
        (0x01, &b"ros2"[..])
    );
}

#[test]
fn an_export_never_overwrites_a_file_and_needs_a_ros_2_topic() {
    let dir = TempDir::new("export-refused");
    let bag = dir.0.join("bag.mcap");
    std::fs::write(&bag, b"keep me").unwrap();
    let output = export(&recording(&dir.0, true), &bag);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("error[export.output_exists]"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&bag).unwrap(), b"keep me");

    let other = TempDir::new("export-no-topics");
    let none = other.0.join("bag.mcap");
    let output = export(&recording(&other.0, false), &none);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("error[export.no_ros2_topics]"),
        "{stderr}"
    );
    assert!(!none.exists(), "no partial bag left behind");
}
