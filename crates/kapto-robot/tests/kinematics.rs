// SPDX-License-Identifier: Apache-2.0

//! Forward kinematics against ROS 2's `robot_state_publisher`, plus the
//! properties every pose must have.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_core::ErrorCode;
use kapto_robot::{Kinematics, RobotModel, Transform, load_urdf, parse_urdf};
use proptest::prelude::*;
use serde_json::Value;

fn robot_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/robots")
        .join(name)
}

fn load(name: &str, urdf: &str) -> RobotModel {
    load_urdf(&robot_dir(name).join(urdf), &[]).unwrap()
}

fn poses(robot: &RobotModel, kinematics: &Kinematics, values: &[f64]) -> Vec<Transform> {
    let mut poses = vec![Transform::IDENTITY; robot.links.len()];
    kinematics
        .poses(values, &[], &Transform::IDENTITY, &mut poses)
        .unwrap();
    poses
}

/// Compares every link of every case in `fk_reference.json`, made by
/// `cargo xtask golden fk`, with Kapto's poses.
fn matches_robot_state_publisher(name: &str, urdf: &str) {
    let robot = load(name, urdf);
    matches_reference(name, &robot);
}

fn matches_reference(name: &str, robot: &RobotModel) {
    let kinematics = Kinematics::new(robot).unwrap();
    let text = std::fs::read_to_string(robot_dir(name).join("fk_reference.json")).unwrap();
    let reference: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reference["root"], robot.links[robot.root].name.as_str());
    let cases = reference["cases"].as_array().unwrap();
    assert!(cases.len() >= 8);
    for (number, case) in cases.iter().enumerate() {
        let values: Vec<f64> = kinematics
            .joint_names()
            .iter()
            .map(|joint| case["joints"][joint].as_f64().unwrap())
            .collect();
        let poses = poses(robot, &kinematics, &values);
        for (index, link) in robot.links.iter().enumerate() {
            let expected = &case["links"][&link.name];
            let numbers = |key: &str| -> Vec<f64> {
                expected[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap())
                    .collect()
            };
            let translation = numbers("translation");
            let error = translation
                .iter()
                .zip(poses[index].translation())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            assert!(error < 1e-9, "case {number}, {}: {error} m off", link.name);
            // q and -q are the same rotation.
            let quaternion = numbers("rotation_xyzw");
            let ours = poses[index].quaternion();
            let dot: f64 = quaternion.iter().zip(ours).map(|(a, b)| a * b).sum();
            assert!(
                (dot.abs() - 1.0).abs() < 1e-9,
                "case {number}, {}: rotation {ours:?}, expected {quaternion:?}",
                link.name
            );
        }
    }
}

#[test]
fn panda_link_poses_match_robot_state_publisher() {
    matches_robot_state_publisher("panda", "panda.urdf");
}

#[test]
fn humanoid_link_poses_match_robot_state_publisher() {
    matches_robot_state_publisher("humanoid", "humanoid.urdf");
}

#[test]
fn sample_arm_link_poses_match_robot_state_publisher() {
    let urdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7/urdf/arm7.urdf");
    matches_reference("arm7", &load_urdf(&urdf, &[]).unwrap());
}

#[test]
fn the_inputs_are_the_joints_a_robot_reports_and_mimics_follow_them() {
    let panda = load("panda", "panda.urdf");
    let kinematics = Kinematics::new(&panda).unwrap();
    assert_eq!(kinematics.joint_names().len(), 8);
    assert_eq!(kinematics.joint_names()[7], "panda_finger_joint1");
    assert_eq!(kinematics.free_joint_names(), &[] as &[String]);
}

#[test]
fn many_samples_at_once_give_the_same_poses_as_one_at_a_time() {
    let panda = load("panda", "panda.urdf");
    let kinematics = Kinematics::new(&panda).unwrap();
    let rows: Vec<f64> = (0..3 * 8).map(|i| f64::from(i) * 0.05).collect();
    let mut batch = vec![Transform::IDENTITY; 3 * panda.links.len()];
    kinematics.poses_for_rows(&rows, &mut batch).unwrap();
    for (sample, values) in rows.chunks(8).enumerate() {
        let one = poses(&panda, &kinematics, values);
        let links = panda.links.len();
        assert_eq!(&batch[sample * links..(sample + 1) * links], one.as_slice());
    }
}

#[test]
fn the_base_pose_moves_the_whole_robot() {
    let humanoid = load("humanoid", "humanoid.urdf");
    let kinematics = Kinematics::new(&humanoid).unwrap();
    let values = vec![0.3; kinematics.joint_names().len()];
    let at_origin = poses(&humanoid, &kinematics, &values);
    let base = Transform::new(
        Transform::rotation_about([0.0, 0.0, 1.0], 0.5).rotation(),
        [1.0, 2.0, 0.9],
    );
    let mut walked = vec![Transform::IDENTITY; humanoid.links.len()];
    kinematics.poses(&values, &[], &base, &mut walked).unwrap();
    for (moved, still) in walked.iter().zip(&at_origin) {
        let expected = base.then(still);
        assert!(close(moved.translation(), expected.translation()));
    }
}

#[test]
fn wrong_numbers_of_values_or_poses_fail_with_joint_values_mismatch() {
    let panda = load("panda", "panda.urdf");
    let kinematics = Kinematics::new(&panda).unwrap();
    let mut poses = vec![Transform::IDENTITY; panda.links.len()];
    let identity = Transform::IDENTITY;
    let short = kinematics.poses(&[0.0; 7], &[], &identity, &mut poses);
    assert_eq!(
        short.unwrap_err().code(),
        ErrorCode::RobotJointValuesMismatch
    );
    let small = kinematics.poses(&[0.0; 8], &[], &identity, &mut poses[..3]);
    assert_eq!(
        small.unwrap_err().code(),
        ErrorCode::RobotJointValuesMismatch
    );
    let ragged = kinematics.poses_for_rows(&[0.0; 12], &mut poses);
    assert_eq!(
        ragged.unwrap_err().code(),
        ErrorCode::RobotJointValuesMismatch
    );
}

#[test]
fn mimic_joints_copying_each_other_in_a_loop_are_refused() {
    let limit = r#"<limit lower="-1" upper="1" effort="1" velocity="1"/>"#;
    let urdf = format!(
        r#"<robot name="r"><link name="a"/><link name="b"/><link name="c"/>
           <joint name="j1" type="revolute"><parent link="a"/><child link="b"/>{limit}<mimic joint="j2"/></joint>
           <joint name="j2" type="revolute"><parent link="b"/><child link="c"/>{limit}<mimic joint="j1"/></joint>
           </robot>"#
    );
    let robot = parse_urdf(&urdf, Path::new("r.urdf"), &[]).unwrap();
    let error = Kinematics::new(&robot).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotKinematicTreeInvalid);
    assert!(error.message().contains("in a loop"), "{}", error.message());
}

#[test]
fn a_mimic_of_a_mimic_applies_both_multipliers_and_offsets() {
    let limit = r#"<limit lower="-9" upper="9" effort="1" velocity="1"/>"#;
    let urdf = format!(
        r#"<robot name="r"><link name="a"/><link name="b"/><link name="c"/><link name="d"/>
           <joint name="driver" type="prismatic"><parent link="a"/><child link="b"/>{limit}</joint>
           <joint name="first" type="prismatic"><parent link="a"/><child link="c"/>{limit}
             <mimic joint="driver" multiplier="2" offset="1"/></joint>
           <joint name="second" type="prismatic"><parent link="a"/><child link="d"/>{limit}
             <mimic joint="first" multiplier="-1" offset="0.5"/></joint>
           </robot>"#
    );
    let robot = parse_urdf(&urdf, Path::new("r.urdf"), &[]).unwrap();
    let kinematics = Kinematics::new(&robot).unwrap();
    let poses = poses(&robot, &kinematics, &[3.0]);
    // first = 2 × 3 + 1 = 7; second = −7 + 0.5 = −6.5, both along x.
    assert!(close(poses[2].translation(), [7.0, 0.0, 0.0]));
    assert!(close(poses[3].translation(), [-6.5, 0.0, 0.0]));
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-9)
}

/// The pose of each link with every joint at zero, worked out by walking
/// the joint origins from the root, independently of `Kinematics`.
fn chained_origins(robot: &RobotModel) -> Vec<Transform> {
    let mut poses = vec![None; robot.links.len()];
    poses[robot.root] = Some(Transform::IDENTITY);
    while poses.iter().any(Option::is_none) {
        for joint in &robot.joints {
            if let (Some(parent), None) = (poses[joint.parent], poses[joint.child]) {
                poses[joint.child] = Some(parent.then(&Transform::from_pose(&joint.origin)));
            }
        }
    }
    poses.into_iter().map(Option::unwrap).collect()
}

#[test]
fn at_zero_every_link_sits_at_its_chained_joint_origins() {
    for (name, urdf) in [("panda", "panda.urdf"), ("humanoid", "humanoid.urdf")] {
        let robot = load(name, urdf);
        let kinematics = Kinematics::new(&robot).unwrap();
        let zero = vec![0.0; kinematics.joint_names().len()];
        let poses = poses(&robot, &kinematics, &zero);
        for (pose, expected) in poses.iter().zip(chained_origins(&robot)) {
            assert!(close(pose.translation(), expected.translation()), "{name}");
        }
    }
}

proptest! {
    #[test]
    fn every_rotation_stays_a_rotation(values in prop::collection::vec(-3.0_f64..3.0, 14)) {
        let humanoid = load("humanoid", "humanoid.urdf");
        let kinematics = Kinematics::new(&humanoid).unwrap();
        for pose in poses(&humanoid, &kinematics, &values) {
            let r = pose.rotation();
            for i in 0..3 {
                for j in 0..3 {
                    let dot: f64 = (0..3).map(|k| r[i][k] * r[j][k]).sum();
                    let expected = if i == j { 1.0 } else { 0.0 };
                    prop_assert!((dot - expected).abs() < 1e-9);
                }
            }
            let determinant = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1])
                - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0])
                + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
            prop_assert!((determinant - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_prismatic_joint_moves_its_link_along_its_axis_by_its_value(
        axis in (-1.0_f64..1.0, -1.0_f64..1.0, -1.0_f64..1.0)
            .prop_filter("not zero", |(x, y, z)| x * x + y * y + z * z > 0.01),
        distance in -2.0_f64..2.0,
    ) {
        let (x, y, z) = axis;
        let urdf = format!(
            r#"<robot name="r"><link name="a"/><link name="b"/>
               <joint name="slide" type="prismatic"><parent link="a"/><child link="b"/>
                 <origin xyz="0.1 0.2 0.3" rpy="0.4 0.5 0.6"/><axis xyz="{x} {y} {z}"/>
                 <limit lower="-2" upper="2" effort="1" velocity="1"/></joint></robot>"#
        );
        let robot = parse_urdf(&urdf, Path::new("r.urdf"), &[]).unwrap();
        let kinematics = Kinematics::new(&robot).unwrap();
        let rest = poses(&robot, &kinematics, &[0.0])[1];
        let moved = poses(&robot, &kinematics, &[distance])[1];
        let travelled: f64 = (0..3)
            .map(|i| (moved.translation()[i] - rest.translation()[i]).powi(2))
            .sum::<f64>()
            .sqrt();
        prop_assert!((travelled - distance.abs()).abs() < 1e-9);
        let along = rest.then(&Transform::translation_along(robot.joints[0].axis, distance));
        prop_assert!(close(moved.translation(), along.translation()));
    }

    #[test]
    fn a_revolute_joint_turns_its_link_without_moving_its_origin(angle in -6.0_f64..6.0) {
        let humanoid = load("humanoid", "humanoid.urdf");
        let kinematics = Kinematics::new(&humanoid).unwrap();
        let joints = kinematics.joint_names().len();
        let rest = poses(&humanoid, &kinematics, &vec![0.0; joints]);
        let mut values = vec![0.0; joints];
        values[0] = angle; // waist_yaw, about z
        let turned = poses(&humanoid, &kinematics, &values);
        let torso = humanoid.link("torso").unwrap().0;
        prop_assert!(close(turned[torso].translation(), rest[torso].translation()));
        let head = humanoid.link("head").unwrap().0;
        let height = |pose: &Transform| pose.translation()[2];
        prop_assert!((height(&turned[head]) - height(&rest[head])).abs() < 1e-9);
    }
}
