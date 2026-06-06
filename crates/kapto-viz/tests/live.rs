// SPDX-License-Identifier: Apache-2.0

//! The robot moving with live samples: which joints a sample sets, and when
//! its link poses are worked out again.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_capture::tap::wire::TapSample;
use kapto_config::{ConfigFile, parse_mapping, parse_streams};
use kapto_core::ClockSample;
use kapto_robot::{JointMap, Kinematics, RobotModel, load_urdf};
use kapto_viz::LiveRobot;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn arm7() -> RobotModel {
    load_urdf(&root().join("packages/arm7/urdf/arm7.urdf"), &[]).unwrap()
}

fn live_arm7() -> LiveRobot {
    let robot = arm7();
    let kinematics = Kinematics::new(&robot).unwrap();
    let read = |name: &str| {
        let path = root().join("packages/arm7").join(name);
        ConfigFile {
            contents: std::fs::read_to_string(&path).unwrap(),
            path,
        }
    };
    let streams = parse_streams(&read("streams.toml")).unwrap();
    let mapping = parse_mapping(&read("mapping.toml")).unwrap();
    let map = JointMap::new(
        &robot,
        &kinematics,
        &streams,
        &mapping,
        Path::new("mapping.toml"),
    )
    .unwrap();
    LiveRobot::new(&robot, kinematics, &map, streams.len(), ClockSample::now())
}

/// A sample of the arm's stream: sequence, send time, seven positions and
/// the gripper opening.
fn arm_sample(recv_time_ns: u64, positions: [f64; 7]) -> TapSample {
    let mut values = vec![1.0, 2.0];
    values.extend(positions);
    values.push(0.02);
    TapSample::single(0, recv_time_ns, values)
}

#[test]
fn a_sample_sets_the_joints_mapped_to_its_values_and_other_streams_change_nothing() {
    let mut live = live_arm7();
    let positions = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7];
    assert!(live.apply(&arm_sample(5, positions)));
    let mut expected = positions.to_vec();
    expected.push(0.02);
    assert_eq!(live.values(), expected);

    let other_stream = TapSample::single(1, 6, vec![9.0; 9]);
    assert!(!live.apply(&other_stream));
    let too_short = TapSample {
        values: vec![1.0, 2.0],
        ..arm_sample(7, positions)
    };
    assert!(!live.apply(&too_short));
    let mut expected = positions.to_vec();
    expected.push(0.02);
    assert_eq!(live.values(), expected);
}

#[test]
fn a_value_the_sample_lacks_leaves_its_joint_where_it_was() {
    let mut live = live_arm7();
    let positions = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7];
    assert!(live.apply(&arm_sample(5, positions)));
    let mut partly = [f64::NAN; 7];
    partly[0] = 0.9;
    assert!(live.apply(&arm_sample(6, partly)));
    assert_eq!(live.values()[..2], [0.9, 0.2]);
    let none = TapSample {
        values: vec![f64::NAN; 10],
        ..arm_sample(7, positions)
    };
    assert!(!live.apply(&none), "nothing to move");
}

#[test]
fn the_poses_are_worked_out_at_rest_first_then_once_per_change() {
    let mut live = live_arm7();
    // At rest, before any sample, at the time the clock was read.
    assert_eq!(live.advance(), Some(live.clock().mono.as_nanos()));
    let at_rest = live.poses().to_vec();
    // Nothing moved since.
    assert_eq!(live.advance(), None);

    live.apply(&arm_sample(5, [0.5; 7]));
    live.apply(&arm_sample(9, [0.6; 7]));
    // Once, at the newest sample that moved it.
    assert_eq!(live.advance(), Some(9));
    assert_ne!(live.poses(), at_rest);
    assert_eq!(live.advance(), None);
}
