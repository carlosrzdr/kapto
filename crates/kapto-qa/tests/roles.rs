// SPDX-License-Identifier: Apache-2.0

//! An episode's values by role, read from real recordings: decoded with the
//! layouts stored in them and named by the `mapping.toml` they carry.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use kapto_capture::{Batch, ChannelSchema, ChannelSpec, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, ErrorCode, KaptoError, RecvTime, Role};
use kapto_decode::{
    FieldSpec, JSON_ENCODING, PathField, PathFormat, PathLayout, STRUCT_ENCODING, ScalarType,
    StructLayout,
};
use kapto_qa::roles::read_roles;

/// A recording for one test, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kapto-roles-{}-{test}", std::process::id()));
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

const MAPPING: &str = "[joints]\n\
                       shoulder = \"arm_joints.positions[0]\"\n\
                       elbow = \"arm_joints.positions[1]\"\n\
                       [roles]\n\
                       \"arm.positions\" = \"arm_joints.positions\"\n\
                       \"safety.enabled\" = \"arm_joints.enabled\"\n\
                       \"camera.head\" = \"camera.image\"\n\
                       \"wrench.left\" = \"ft_sensor.force\"\n";

/// The arm's message: a sequence number, two positions and a flag.
fn arm(sequence: u32, positions: [f64; 2], enabled: bool) -> Vec<u8> {
    let mut payload = sequence.to_le_bytes().to_vec();
    for position in positions {
        payload.extend(position.to_le_bytes());
    }
    payload.push(u8::from(enabled));
    payload
}

/// Records `arm_joints`, with its layout, and `camera`, without one,
/// carrying `mapping` if given; one message is too short to decode.
/// Messages are received a millisecond apart from UTC 1 000 005 s.
fn record(file: &TempFile, mapping: Option<&str>, messages: u32) {
    let layout = StructLayout {
        fields: vec![
            FieldSpec::new("sequence", ScalarType::U32, 0),
            FieldSpec {
                count: 2,
                ..FieldSpec::new("positions", ScalarType::F64, 4)
            },
            FieldSpec::new("enabled", ScalarType::Bool, 20),
        ],
    };
    let streams = [
        ChannelSpec {
            stream: 0,
            id: "arm_joints".parse().unwrap(),
            schema: Some(ChannelSchema {
                message_encoding: STRUCT_ENCODING.to_owned(),
                name: "arm_joints".to_owned(),
                encoding: STRUCT_ENCODING.to_owned(),
                data: layout.to_schema_json().into_bytes(),
                metadata: BTreeMap::new(),
            }),
        },
        ChannelSpec::from((1, "camera".parse().unwrap())),
    ];
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 1_000_005_000_000_000,
        uncertainty: Duration::ZERO,
    });
    let mut recording =
        Recording::create(&file.0, &streams, &clock, &WriterConfig::default()).unwrap();
    if let Some(mapping) = mapping {
        recording
            .attach("mapping.toml", "application/toml", 0, mapping.as_bytes())
            .unwrap();
    }
    let counters = LossCounters::new(2);
    let send = |recording: &mut Recording, at_ms: u64, stream, payload: &[u8]| {
        let mut batch = Batch::new(1, payload.len());
        batch.push(stream, payload);
        batch.set_recv_time(RecvTime::from_nanos(at_ms * 1_000_000));
        recording.write(&batch, &counters).unwrap();
    };
    for n in 0..messages {
        let at = u64::from(n);
        send(
            &mut recording,
            at,
            0,
            &arm(n, [0.5 * f64::from(n), -1.0], n % 2 == 0),
        );
        send(&mut recording, at, 1, b"jpeg");
    }
    send(&mut recording, u64::from(messages), 0, b"short");
    recording.finish(&clock).unwrap();
}

fn role(text: &str) -> Role {
    text.parse().unwrap()
}

#[test]
fn every_role_the_recording_has_is_read_back_as_recorded_and_the_others_are_absent() {
    let file = TempFile::new("columns");
    record(&file, Some(MAPPING), 3);
    let roles = read_roles(&file.0, &mut || Ok(())).unwrap();

    let names: Vec<&str> = roles.keys().map(Role::as_str).collect();
    assert_eq!(
        names,
        [
            "arm.positions",
            "joints.elbow.position",
            "joints.shoulder.position",
            "safety.enabled"
        ]
    );
    // One row per decoded message; the one too short to decode has none.
    let shoulder = &roles[&role("joints.shoulder.position")];
    assert_eq!(shoulder.width, 1);
    assert_eq!(shoulder.values, [0.0, 0.5, 1.0]);
    assert_eq!(
        shoulder.times_utc_ns,
        [
            1_000_005_000_000_000,
            1_000_005_001_000_000,
            1_000_005_002_000_000
        ]
    );
    // A whole array: a row per message, as wide as the array.
    let positions = &roles[&role("arm.positions")];
    assert_eq!((positions.rows(), positions.width), (3, 2));
    assert_eq!(positions.row(2), Some(&[1.0, -1.0][..]));
    assert_eq!(positions.row(3), None);
    // A flag as 1 and 0.
    assert_eq!(roles[&role("safety.enabled")].values, [1.0, 0.0, 1.0]);
}

#[test]
fn a_recording_without_a_mapping_has_no_roles() {
    let file = TempFile::new("unmapped");
    record(&file, None, 2);
    assert!(read_roles(&file.0, &mut || Ok(())).unwrap().is_empty());
}

#[test]
fn reading_waits_at_its_steps_and_stops_with_the_error_it_is_given() {
    let file = TempFile::new("between");
    record(&file, Some(MAPPING), 12_000);
    let mut steps = 0;
    let roles = read_roles(&file.0, &mut || {
        steps += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(roles[&role("joints.shoulder.position")].rows(), 12_000);
    assert!(steps >= 2, "{steps}");

    let stopped = read_roles(&file.0, &mut || {
        Err(KaptoError::new(ErrorCode::WorkerJobStopped))
    })
    .unwrap_err();
    assert_eq!(stopped.code(), ErrorCode::WorkerJobStopped);
}

#[test]
fn a_recording_that_isnt_one_is_reported_as_unreadable() {
    let file = TempFile::new("garbage");
    std::fs::write(&file.0, b"not a recording").unwrap();
    let error = read_roles(&file.0, &mut || Ok(())).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RecordingUnreadable);
}

#[test]
fn a_role_a_message_lacks_has_no_row_at_its_time_and_never_a_zero() {
    let file = TempFile::new("two-drivers");
    let layout = PathLayout {
        format: PathFormat::Json,
        fields: vec![
            PathField::new("shoulder", "position[name=shoulder]", ScalarType::F64),
            PathField::new("gripper", "position[name=gripper]", ScalarType::F64),
        ],
    };
    let streams = [ChannelSpec {
        stream: 0,
        id: "joints".parse().unwrap(),
        schema: Some(ChannelSchema {
            message_encoding: JSON_ENCODING.to_owned(),
            name: "joints".to_owned(),
            encoding: JSON_ENCODING.to_owned(),
            data: layout.to_schema_json().into_bytes(),
            metadata: BTreeMap::new(),
        }),
    }];
    let clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 0,
        uncertainty: Duration::ZERO,
    });
    let mut recording =
        Recording::create(&file.0, &streams, &clock, &WriterConfig::default()).unwrap();
    let mapping = "[joints]\nshoulder = \"joints.shoulder\"\n[roles]\n\"gripper.opening\" = \"joints.gripper\"\n";
    recording
        .attach("mapping.toml", "application/toml", 0, mapping.as_bytes())
        .unwrap();
    let counters = LossCounters::new(1);
    // The arm's driver and the gripper's take turns on one topic.
    let messages: [&[u8]; 3] = [
        br#"{"name":["shoulder"],"position":[0.5]}"#,
        br#"{"name":["gripper"],"position":[0.02]}"#,
        br#"{"name":["shoulder"],"position":[0.6]}"#,
    ];
    for (at, payload) in (1_u64..).zip(messages) {
        let mut batch = Batch::new(1, payload.len());
        batch.push(0, payload);
        batch.set_recv_time(RecvTime::from_nanos(at));
        recording.write(&batch, &counters).unwrap();
    }
    recording.finish(&clock).unwrap();

    let roles = read_roles(&file.0, &mut || Ok(())).unwrap();
    let shoulder = &roles[&role("joints.shoulder.position")];
    assert_eq!(shoulder.values, [0.5, 0.6]);
    assert_eq!(shoulder.times_utc_ns, [1, 3]);
    let gripper = &roles[&role("gripper.opening")];
    assert_eq!(
        (gripper.values.as_slice(), gripper.times_utc_ns.as_slice()),
        (&[0.02][..], &[2][..])
    );
}
