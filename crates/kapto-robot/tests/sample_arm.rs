// SPDX-License-Identifier: Apache-2.0

//! The sample arm in `packages/arm7` fits the synthetic robot: its stream
//! declaration decodes what the synthetic robot sends, into one value per
//! joint of the arm, always within the joint limits.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_config::{ConfigFile, DecoderDeclaration, StreamDeclaration, parse_streams};
use kapto_decode::{Decoder, Value};
use kapto_loadgen::payload::{JOINTS, SIZE, Sample, gripper_at, joints_at};
use kapto_robot::{Kinematics, RobotModel, load_urdf};

fn package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7")
}

fn arm() -> RobotModel {
    load_urdf(&package().join("urdf/arm7.urdf"), &[]).unwrap()
}

fn stream() -> StreamDeclaration {
    let path = package().join("streams.toml");
    let contents = std::fs::read_to_string(&path).unwrap();
    let mut streams = parse_streams(&ConfigFile { path, contents }).unwrap();
    assert_eq!(streams.len(), 1);
    streams.remove(0)
}

#[test]
fn the_arm_has_seven_joints_and_needs_no_mesh_files() {
    let arm = arm();
    let kinematics = Kinematics::new(&arm).unwrap();
    let joints: Vec<_> = kinematics
        .joint_names()
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(
        joints,
        [
            "joint_1", "joint_2", "joint_3", "joint_4", "joint_5", "joint_6", "joint_7", "gripper"
        ]
    );
    assert_eq!(joints.len(), JOINTS + 1, "the arm's joints and the gripper");
    assert!(arm.missing_files().next().is_none());
    assert!(arm.links.iter().all(|link| !link.visuals.is_empty()));
}

#[test]
fn the_stream_declaration_decodes_what_the_synthetic_robot_sends() {
    let stream = stream();
    assert_eq!(stream.max_message_bytes, Some(u32::try_from(SIZE).unwrap()));
    let Some(DecoderDeclaration::Struct(layout)) = &stream.decoder else {
        panic!("no struct decoder");
    };
    let decoder = layout.compile().unwrap();
    let sample = Sample {
        seq: 41,
        source_time_ns: 1_772_700_400_123_456_789,
        joints: joints_at(0, 12.5),
        gripper: gripper_at(0, 12.5),
    };
    let mut payload = [0; SIZE];
    sample.encode(&mut payload);
    let mut values = vec![Value::default(); decoder.value_count()];
    decoder.decode(&payload, &mut values).unwrap();
    // Values come field after field; the joints follow the fields before them.
    let fields = decoder.fields();
    let joints = fields
        .iter()
        .position(|field| field.name == "joint_positions")
        .unwrap();
    let start: usize = fields[..joints].iter().map(|field| field.count).sum();
    let joint_positions: Vec<f64> = values[start..start + fields[joints].count]
        .iter()
        .map(|value| match value {
            Value::Float(position) => *position,
            other => panic!("not a float: {other:?}"),
        })
        .collect();
    assert_eq!(joint_positions, sample.joints);
    assert_eq!(values[0], Value::UInt(41));
    assert_eq!(values.last(), Some(&Value::Float(sample.gripper)));
}

#[test]
fn the_synthetic_robots_joint_positions_stay_within_the_arms_limits() {
    let arm = arm();
    let limits: Vec<_> = arm
        .actuated_joints()
        .map(|joint| joint.limits.unwrap())
        .collect();
    for stream in 0..16 {
        for step in 0..2_000 {
            let seconds = f64::from(step) * 0.05;
            let mut positions = joints_at(stream, seconds).to_vec();
            // The mirrored finger isn't actuated: it follows the gripper.
            positions.push(gripper_at(stream, seconds));
            assert_eq!(positions.len(), limits.len());
            for (position, limit) in positions.iter().zip(&limits) {
                assert!(limit.lower <= *position && *position <= limit.upper);
            }
        }
    }
}
