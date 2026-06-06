// SPDX-License-Identifier: Apache-2.0

//! `kapto decode`, run as a real process on recordings written by the
//! recorder's own writer.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use kapto_capture::{
    Batch, ChannelSchema, ChannelSpec, LossCounters, Recording, StreamIndex, WriterConfig,
};
use kapto_core::{ClockMap, ClockSample, RecvTime};
use kapto_decode::{
    CdrLayout, DecoderDeclaration, FieldSpec, PathField, PathFormat, PathLayout, STRUCT_ENCODING,
    ScalarType, StructLayout,
};
use kapto_testkit::TempDir;

/// Receive time 2026-03-09T14:33:32Z for every message.
const UTC_NS: u64 = 1_773_066_812_000_000_000;

/// `arm`: u16 sequence + 2 × i16 joints in thousandths of a radian, then
/// `status`: raw bytes. Arm messages: (sequence, joint 0, joint 1); a
/// two-byte message is too short for the layout.
fn record(dir: &Path) -> PathBuf {
    let layout = StructLayout {
        fields: vec![
            FieldSpec::new("sequence", ScalarType::U16, 0),
            FieldSpec {
                count: 2,
                scale: Some(0.001),
                unit: Some("rad".to_owned()),
                ..FieldSpec::new("joints", ScalarType::I16, 2)
            },
        ],
    };
    let mut arm = ChannelSpec::from((0, "arm".parse().unwrap()));
    arm.schema = Some(ChannelSchema {
        message_encoding: STRUCT_ENCODING.to_owned(),
        name: "arm".to_owned(),
        encoding: STRUCT_ENCODING.to_owned(),
        data: layout.to_schema_json().into_bytes(),
        metadata: BTreeMap::new(),
    });
    let channels = [arm, ChannelSpec::from((1, "status".parse().unwrap()))];
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: i64::try_from(UTC_NS).unwrap(),
        uncertainty: Duration::ZERO,
    });
    let path = dir.join("recording.mcap");
    let mut recording =
        Recording::create(&path, &channels, &clock, &WriterConfig::default()).unwrap();
    let arm_message = |sequence: u16, a: i16, b: i16| {
        let mut bytes = sequence.to_le_bytes().to_vec();
        bytes.extend(a.to_le_bytes());
        bytes.extend(b.to_le_bytes());
        bytes
    };
    let messages: Vec<(StreamIndex, Vec<u8>)> = vec![
        (0, arm_message(1, 1500, -250)),
        (1, b"\x01\x02\xff".to_vec()),
        (0, arm_message(2, 1600, -260)),
        (0, vec![9, 9]),
        (0, arm_message(3, 1700, -270)),
    ];
    let mut batch = Batch::new(messages.len(), 32);
    for (stream, payload) in &messages {
        batch.push(*stream, payload);
    }
    batch.set_recv_time(RecvTime::from_nanos(0));
    recording.write(&batch, &LossCounters::new(2)).unwrap();
    recording.finish(&clock).unwrap();
    path
}

fn kapto(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(args)
        .env_clear()
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn the_table_shows_decoded_values_and_raw_bytes_per_stream() {
    let dir = TempDir::new("table");
    record(&dir.0);
    // The episode folder works as well as the file.
    let output = kapto(&["decode", dir.0.to_str().unwrap()]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let out = text(&output.stdout);
    assert!(
        out.contains("arm (kapto.struct/v1); joints in rad: showing 4 of 4 messages"),
        "{out}"
    );
    let lines: Vec<Vec<&str>> = out
        .lines()
        .map(|line| line.split_whitespace().collect())
        .collect();
    let has = |cells: &[&str]| lines.iter().any(|line| line.as_slice() == cells);
    assert!(
        has(&[
            "RECEIVED",
            "(UTC)",
            "INDEX",
            "sequence",
            "joints[0]",
            "joints[1]"
        ]),
        "{out}"
    );
    assert!(has(&[
        "2026-03-09T14:33:32.000000000Z",
        "0",
        "1",
        "1.5",
        "-0.25"
    ]));
    assert!(out.contains("2026-03-09T14:33:32.000000000Z  2      not decoded: the message has 2 bytes; the layout needs 6"));
    assert!(out.contains("status (kapto.raw): showing 1 of 1 messages"));
    assert!(has(&[
        "2026-03-09T14:33:32.000000000Z",
        "0",
        "3",
        "01",
        "02",
        "ff"
    ]));
}

#[test]
fn csv_has_every_decoded_message_of_one_stream() {
    let dir = TempDir::new("csv");
    let path = record(&dir.0);
    let output = kapto(&[
        "decode",
        path.to_str().unwrap(),
        "--format",
        "csv",
        "--stream",
        "arm",
    ]);
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout),
        "received_utc_ns,received_utc,index,sequence,joints[0],joints[1]\n\
         1773066812000000000,2026-03-09T14:33:32.000000000Z,0,1,1.5,-0.25\n\
         1773066812000000000,2026-03-09T14:33:32.000000000Z,1,2,1.6,-0.26\n\
         1773066812000000000,2026-03-09T14:33:32.000000000Z,3,3,1.7,-0.27\n"
    );
    assert_eq!(
        text(&output.stderr),
        "1 message(s) of `arm` weren't decoded and are left out; the first: the message has 2 bytes; the layout needs 6.\n"
    );
}

#[test]
fn the_limit_caps_messages_per_stream() {
    let dir = TempDir::new("limit");
    let path = record(&dir.0);
    let output = kapto(&[
        "decode",
        path.to_str().unwrap(),
        "--format",
        "csv",
        "--stream",
        "arm",
        "--limit",
        "1",
    ]);
    assert_eq!(text(&output.stdout).lines().count(), 2);
    let table = text(&kapto(&["decode", path.to_str().unwrap(), "--limit", "2"]).stdout);
    assert!(table.contains("showing 2 of 4 messages"));
}

#[test]
fn csv_needs_one_stream_with_a_layout() {
    let dir = TempDir::new("csv-errors");
    let path = record(&dir.0);
    let path = path.to_str().unwrap();
    let several = kapto(&["decode", path, "--format", "csv"]);
    assert_eq!(several.status.code(), Some(1));
    assert!(text(&several.stderr).starts_with(
        "error[decode.csv_needs_one_stream]: CSV output needs exactly one stream: 2 streams selected\n  streams in the recording: arm, status\n"
    ));
    let raw = kapto(&["decode", path, "--format", "csv", "--stream", "status"]);
    assert!(text(&raw.stderr).starts_with("error[decode.no_layout]"));
}

#[test]
fn unknown_streams_and_unreadable_files_are_reported() {
    let dir = TempDir::new("errors");
    let path = record(&dir.0);
    let unknown = kapto(&["decode", path.to_str().unwrap(), "--stream", "leg"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(text(&unknown.stderr).contains("`leg`\n  streams in the recording: arm, status"));
    let missing = kapto(&["decode", dir.0.join("nothing.mcap").to_str().unwrap()]);
    assert!(text(&missing.stderr).starts_with("error[recording.unreadable]"));
}

/// One stream each of JSON and MessagePack with the same layout and
/// messages; the third of each lacks its joints.
fn record_by_path(dir: &Path) -> PathBuf {
    let layout = |format| {
        DecoderDeclaration::Path(PathLayout {
            format,
            fields: vec![
                PathField::new("sequence", "header.seq", ScalarType::U32),
                PathField {
                    count: 2,
                    unit: Some("rad".to_owned()),
                    ..PathField::new("joints", "state.position", ScalarType::F64)
                },
            ],
        })
    };
    let channel = |index, name: &str, format| {
        let stored = layout(format).stored(name);
        let mut spec = ChannelSpec::from((index, name.parse().unwrap()));
        spec.schema = Some(ChannelSchema {
            message_encoding: stored.message_encoding.to_owned(),
            name: stored.schema_name,
            encoding: stored.schema_encoding.to_owned(),
            data: stored.schema_data,
            metadata: stored.channel_metadata,
        });
        spec
    };
    let channels = [
        channel(0, "json_arm", PathFormat::Json),
        channel(1, "msgpack_arm", PathFormat::MsgPack),
    ];
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: i64::try_from(UTC_NS).unwrap(),
        uncertainty: Duration::ZERO,
    });
    let path = dir.join("recording.mcap");
    let mut recording =
        Recording::create(&path, &channels, &clock, &WriterConfig::default()).unwrap();
    let messages = [
        serde_json::json!({"header": {"seq": 1}, "state": {"position": [0.5, -1.25], "name": "arm"}}),
        serde_json::json!({"state": {"position": [0.75, -1.0]}, "header": {"seq": 2}}),
        serde_json::json!({"header": {"seq": 3}, "state": {}}),
        serde_json::json!({"state": {}}),
    ];
    let mut batch = Batch::new(2 * messages.len(), 128);
    for message in &messages {
        batch.push(0, message.to_string().as_bytes());
        batch.push(1, &rmp_serde::to_vec_named(message).unwrap());
    }
    batch.set_recv_time(RecvTime::from_nanos(0));
    recording.write(&batch, &LossCounters::new(2)).unwrap();
    recording.finish(&clock).unwrap();
    path
}

#[test]
fn json_and_msgpack_recordings_decode_by_their_stored_paths() {
    let dir = TempDir::new("by-path");
    let path = record_by_path(&dir.0);
    for (stream, encoding) in [
        ("json_arm", "kapto.json/v1"),
        ("msgpack_arm", "kapto.msgpack/v1"),
    ] {
        let output = kapto(&[
            "decode",
            path.to_str().unwrap(),
            "--format",
            "csv",
            "--stream",
            stream,
        ]);
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert_eq!(
            text(&output.stdout),
            // The joints the third message lacks are empty cells, never 0.
            "received_utc_ns,received_utc,index,sequence,joints[0],joints[1]\n\
             1773066812000000000,2026-03-09T14:33:32.000000000Z,0,1,0.5,-1.25\n\
             1773066812000000000,2026-03-09T14:33:32.000000000Z,1,2,0.75,-1\n\
             1773066812000000000,2026-03-09T14:33:32.000000000Z,2,3,,\n",
            "{stream}"
        );
        assert_eq!(
            text(&output.stderr),
            format!(
                "1 message(s) of `{stream}` weren't decoded and are left out; the first: `sequence` isn't in the message.\n"
            )
        );
        let table = kapto(&["decode", path.to_str().unwrap(), "--stream", stream]);
        assert!(
            text(&table.stdout).contains(&format!("{stream} ({encoding}); joints in rad")),
            "{}",
            text(&table.stdout)
        );
    }
}

/// A ROS 2 `JointState` as ROS 2 itself serialised it, with its definition as
/// rosbag2 stores it, from `testdata/cdr/reference.json`.
fn joint_state() -> (String, Vec<u8>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/cdr/reference.json");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let message = &file["messages"]["joint_state"];
    let hex = message["cdr"].as_str().unwrap();
    let cdr = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect();
    (message["definition"].as_str().unwrap().to_owned(), cdr)
}

#[test]
fn a_ros_2_recording_decodes_by_its_stored_type_and_reads_as_rosbag2_writes_it() {
    let dir = TempDir::new("cdr");
    let (definition, cdr) = joint_state();
    let stored = DecoderDeclaration::Cdr(CdrLayout {
        type_name: "sensor_msgs/msg/JointState".to_owned(),
        definition: definition.clone(),
        fields: vec![
            PathField::new("stamp", "header.stamp.sec", ScalarType::I64),
            PathField {
                count: 3,
                unit: Some("rad".to_owned()),
                ..PathField::new("position", "position", ScalarType::F64)
            },
        ],
    })
    .stored("joints");
    let mut spec = ChannelSpec::from((0, "joints".parse().unwrap()));
    spec.schema = Some(ChannelSchema {
        message_encoding: stored.message_encoding.to_owned(),
        name: stored.schema_name,
        encoding: stored.schema_encoding.to_owned(),
        data: stored.schema_data,
        metadata: stored.channel_metadata,
    });
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: i64::try_from(UTC_NS).unwrap(),
        uncertainty: Duration::ZERO,
    });
    let path = dir.0.join("recording.mcap");
    let mut recording =
        Recording::create(&path, &[spec], &clock, &WriterConfig::default()).unwrap();
    let mut batch = Batch::new(1, 256);
    batch.push(0, &cdr);
    batch.set_recv_time(RecvTime::from_nanos(0));
    recording.write(&batch, &LossCounters::new(1)).unwrap();
    recording.finish(&clock).unwrap();

    // Decoded from what the recording stores, with no package anywhere.
    let output = kapto(&[
        "decode",
        path.to_str().unwrap(),
        "--format",
        "csv",
        "--stream",
        "joints",
    ]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "received_utc_ns,received_utc,index,stamp,position[0],position[1],position[2]\n\
         1773066812000000000,2026-03-09T14:33:32.000000000Z,0,1773066812,0.5,-1.25,0.04\n"
    );

    // The channel as rosbag2 writes one, so ROS 2's tools and Foxglove read it.
    let bytes = std::fs::read(&path).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let channel = summary
        .channels
        .values()
        .find(|channel| channel.topic == "joints")
        .unwrap();
    let schema = channel.schema.as_ref().unwrap();
    assert_eq!(channel.message_encoding, "cdr");
    assert_eq!(
        (schema.name.as_str(), schema.encoding.as_str()),
        ("sensor_msgs/msg/JointState", "ros2msg")
    );
    assert_eq!(schema.data.as_ref(), definition.as_bytes());
    assert!(channel.metadata["kapto.fields"].contains(r#""path":"header.stamp.sec""#));
}
