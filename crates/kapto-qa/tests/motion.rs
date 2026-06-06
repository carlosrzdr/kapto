// SPDX-License-Identifier: Apache-2.0

//! The motion checks, on joints made to move smoothly, jump, freeze, and
//! come to rest.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::Path;

use kapto_core::Role;
use kapto_qa::checks::{FrozenJoints, GripperChatter, Idle, JointJumps};
use kapto_qa::conformance::check_meets_the_contract;
use kapto_qa::{Check, CheckKind, EpisodeView, RoleColumn, TimeRange, Verdict};

const ID: &str = "20260305T192345.123Z-1a2b3c4d";
const START: i64 = 1_772_700_400_000_000_000;
/// Ten milliseconds between messages: 100 Hz.
const STEP: i64 = 10_000_000;

fn at(row: u32) -> i64 {
    START + i64::from(row) * STEP
}

/// A joint's positions over 500 rows (5 s).
fn column(position: impl Fn(u32) -> f64) -> RoleColumn {
    RoleColumn {
        times_utc_ns: (0..500).map(at).collect(),
        width: 1,
        values: (0..500).map(position).collect(),
    }
}

/// An episode with these joints' positions.
fn episode(joints: &[(&str, RoleColumn)]) -> EpisodeView {
    let named: Vec<(String, RoleColumn)> = joints
        .iter()
        .map(|(joint, column)| (format!("joints.{joint}.position"), column.clone()))
        .collect();
    with_roles(&named)
}

/// An episode with these roles' values.
fn with_roles(roles: &[(String, RoleColumn)]) -> EpisodeView {
    let roles: BTreeMap<Role, RoleColumn> = roles
        .iter()
        .map(|(role, column)| (role.parse().unwrap(), column.clone()))
        .collect();
    EpisodeView::new(
        &Path::new("/data").join(ID),
        kapto_testkit::episode(ID, (START, at(500)), Vec::new()),
    )
    .with_roles(roles)
}

/// Turning back and forth at up to 1 rad/s.
fn swinging(row: u32) -> f64 {
    (f64::from(row) * 0.01).sin()
}

const JUMPS: JointJumps = JointJumps { max_speed: 20.0 };
const FROZEN: FrozenJoints = FrozenJoints {
    max_seconds: 1.0,
    moving_speed: 0.05,
};

#[test]
fn a_reading_that_jumps_is_flagged_where_it_jumped_and_smooth_motion_isnt() {
    // Half a radian away for one message at row 200, then back.
    let glitched = column(|row| swinging(row) + if row == 200 { 0.5 } else { 0.0 });
    let finding = JUMPS
        .run(&episode(&[
            ("shoulder", column(swinging)),
            ("elbow", glitched),
        ]))
        .unwrap();
    assert_eq!(finding.verdict, Verdict::Failed, "{}", finding.message);
    assert!(
        finding
            .message
            .starts_with("jumped faster than 20 between two messages: elbow 1 times"),
        "{}",
        finding.message
    );
    // Out and back, one range.
    assert_eq!(
        finding.ranges,
        [TimeRange {
            start_utc_ns: at(199),
            end_utc_ns: at(201),
        }]
    );
    assert_eq!(finding.values["elbow.jumps"], 1.0);
    assert!(finding.values["elbow.fastest"] > 49.0);
    assert!(!finding.values.contains_key("shoulder.jumps"));

    let smooth = JUMPS
        .run(&episode(&[("shoulder", column(swinging))]))
        .unwrap();
    assert_eq!(smooth.verdict, Verdict::Passed);
    assert_eq!(JUMPS.kind(), CheckKind::Advisory);
    assert_eq!(
        JUMPS.run(&episode(&[])).unwrap().verdict,
        Verdict::NotApplicable
    );
}

#[test]
fn a_joint_that_stops_dead_mid_motion_is_flagged_and_one_coming_to_rest_isnt() {
    // Moving, then exactly still for 1.5 s from row 100, then moving on.
    let frozen = column(|row| swinging(if (100..250).contains(&row) { 100 } else { row }));
    // Slowing down to rest, then holding: still, but not stopped dead.
    let resting = column(|row| {
        let t = f64::from(row.min(300)) / 300.0;
        1.0 - (1.0 - t).powi(3)
    });
    let finding = FROZEN
        .run(&episode(&[
            ("shoulder", column(swinging)),
            ("elbow", frozen),
            ("wrist", resting),
        ]))
        .unwrap();
    assert_eq!(finding.verdict, Verdict::Failed, "{}", finding.message);
    assert!(
        finding.message.contains("elbow 1 times, for up to 1.5 s"),
        "{}",
        finding.message
    );
    assert!(!finding.message.contains("wrist"), "{}", finding.message);
    assert_eq!(
        finding.ranges,
        [TimeRange {
            start_utc_ns: at(100),
            end_utc_ns: at(249),
        }]
    );

    // Too short to flag.
    let blink = column(|row| swinging(if (100..150).contains(&row) { 100 } else { row }));
    let short = FROZEN
        .run(&episode(&[
            ("shoulder", column(swinging)),
            ("elbow", blink),
        ]))
        .unwrap();
    assert_eq!(short.verdict, Verdict::Passed, "{}", short.message);
}

#[test]
fn a_joint_frozen_while_nothing_else_moves_isnt_flagged_nor_a_robot_of_one_joint() {
    let frozen = || column(|row| swinging(if (100..250).contains(&row) { 100 } else { row }));
    let held = column(|_| 0.3);
    let alone = FROZEN.run(&episode(&[("elbow", frozen())])).unwrap();
    assert_eq!(alone.verdict, Verdict::NotApplicable);
    let still = FROZEN
        .run(&episode(&[("elbow", frozen()), ("shoulder", held)]))
        .unwrap();
    assert_eq!(still.verdict, Verdict::Passed, "{}", still.message);
}

#[test]
fn the_motion_checks_meet_the_contract() {
    let episodes = [
        episode(&[("shoulder", column(swinging))]),
        episode(&[
            ("shoulder", column(swinging)),
            ("elbow", column(|row| swinging(row.min(200)))),
        ]),
    ];
    check_meets_the_contract(&JUMPS, &episodes);
    check_meets_the_contract(&FROZEN, &episodes);
}

const CHATTER: GripperChatter = GripperChatter {
    max_closings: 3,
    window_seconds: 2.0,
};
const IDLE: Idle = Idle {
    max_ratio: 0.5,
    min_seconds: 2.0,
    moving_speed: 0.05,
};

#[test]
fn a_gripper_closing_again_and_again_is_flagged_where_and_one_working_isnt() {
    // Closing once a second, then six times in a second from row 300.
    let opening = |row: u32| {
        let period = if (300..400).contains(&row) { 16 } else { 100 };
        if row % period < period / 2 { 0.08 } else { 0.0 }
    };
    let finding = CHATTER
        .run(&with_roles(&[(
            "gripper.opening".to_owned(),
            column(opening),
        )]))
        .unwrap();
    assert_eq!(finding.verdict, Verdict::Failed, "{}", finding.message);
    assert!(
        finding
            .message
            .starts_with("closed more than 3 times within 2 s: gripper.opening"),
        "{}",
        finding.message
    );
    let first = finding.ranges[0].start_utc_ns;
    assert!(first >= at(100) && first <= at(310), "{first}");
    let working = |row: u32| if row % 100 < 50 { 0.08 } else { 0.0 };
    let fine = CHATTER
        .run(&with_roles(&[(
            "gripper.opening".to_owned(),
            column(working),
        )]))
        .unwrap();
    assert_eq!(fine.verdict, Verdict::Passed, "{}", fine.message);
    assert_eq!(
        CHATTER
            .run(&episode(&[("shoulder", column(swinging))]))
            .unwrap()
            .verdict,
        Verdict::NotApplicable
    );
}

#[test]
fn a_robot_still_for_most_of_an_episode_is_flagged_with_its_still_spans() {
    // Moving for the first second, then still for four.
    let resting = || column(|row| swinging(row.min(100)));
    let finding = IDLE
        .run(&episode(&[("shoulder", resting()), ("elbow", resting())]))
        .unwrap();
    assert_eq!(finding.verdict, Verdict::Failed, "{}", finding.message);
    assert!(
        finding.message.starts_with("still for 80 % of the episode"),
        "{}",
        finding.message
    );
    assert_eq!(
        finding.ranges,
        [TimeRange {
            start_utc_ns: at(100),
            end_utc_ns: at(499),
        }]
    );
    // One joint moving is the robot moving.
    let working = IDLE
        .run(&episode(&[
            ("shoulder", resting()),
            ("elbow", column(swinging)),
        ]))
        .unwrap();
    assert_eq!(working.verdict, Verdict::Passed, "{}", working.message);
    assert_eq!(working.values["still_ratio"], 0.0);
    assert!(
        working.message.starts_with("still for 0 %"),
        "{}",
        working.message
    );
    check_meets_the_contract(&IDLE, &[episode(&[("shoulder", resting())])]);
    check_meets_the_contract(&CHATTER, &[episode(&[("shoulder", resting())])]);
}
