// SPDX-License-Identifier: Apache-2.0

//! An episode's statistics by role, from values with known speeds, efforts
//! and gripper cycles.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::Path;

use kapto_core::Role;
use kapto_qa::episode_statistics::{Statistic, from_stored, of_episode, to_stored};
use kapto_qa::{EpisodeView, RoleColumn};
use kapto_store::StreamSummary;

const START: i64 = 1_772_700_400_000_000_000;

/// A 10 s episode of one stream that received 20 000 messages, lost 100
/// on the way and had 20 dropped by Kapto, with `roles`.
fn episode(roles: BTreeMap<Role, RoleColumn>) -> EpisodeView {
    let stream = StreamSummary {
        written: 19_980,
        dropped_by_us: 20,
        lost_upstream: 100,
        ..kapto_testkit::stream("arm_joints", 20_000)
    };
    let summary = kapto_testkit::episode(
        "20260305T192345.123Z-1a2b3c4d",
        (START, START + 10_000_000_000),
        vec![stream],
    );
    EpisodeView::new(Path::new("/data/episode"), summary).with_roles(roles)
}

/// A column sampled every 10 ms, with `value(i)` in row `i`.
fn column(rows: u32, value: impl Fn(u32) -> f64) -> RoleColumn {
    RoleColumn {
        times_utc_ns: (0..rows)
            .map(|i| START + i64::from(i) * 10_000_000)
            .collect(),
        width: 1,
        values: (0..rows).map(value).collect(),
    }
}

/// The distribution `name`, which must be one.
fn distribution<'a>(
    statistics: &'a BTreeMap<String, Statistic>,
    name: &str,
) -> &'a kapto_qa::statistics::Distribution {
    match &statistics[name] {
        Statistic::Distribution(distribution) => Some(distribution),
        Statistic::Value(_) => None,
    }
    .unwrap()
}

/// The single value `name`, which must be one.
fn value(statistics: &BTreeMap<String, Statistic>, name: &str) -> f64 {
    match &statistics[name] {
        Statistic::Value(value) => Some(*value),
        Statistic::Distribution(_) => None,
    }
    .unwrap()
}

fn role(text: &str) -> Role {
    text.parse().unwrap()
}

#[test]
fn each_episode_has_its_duration_and_each_streams_rate_and_loss() {
    let statistics = of_episode(&episode(BTreeMap::new()));
    assert_eq!(value(&statistics, "episode.duration_s"), 10.0);
    assert_eq!(value(&statistics, "streams.arm_joints.rate_hz"), 2000.0);
    assert!(
        (value(&statistics, "streams.arm_joints.lost_upstream_ratio") - 100.0 / 20_100.0).abs()
            < 1e-12
    );
    assert_eq!(
        value(&statistics, "streams.arm_joints.dropped_ratio"),
        0.001
    );
    assert_eq!(statistics.len(), 4);
}

#[test]
fn an_episode_ending_before_it_starts_has_no_duration_and_no_rate() {
    let episode = kapto_testkit::episode(
        "20260305T192345.123Z-1a2b3c4d",
        (START, START - 1_000_000_000),
        vec![kapto_testkit::stream("arm_joints", 100)],
    );
    let statistics = of_episode(&EpisodeView::new(Path::new("/data/episode"), episode));
    assert_eq!(value(&statistics, "episode.duration_s"), 0.0);
    assert_eq!(value(&statistics, "streams.arm_joints.rate_hz"), 0.0);
}

#[test]
fn a_joints_speed_comes_from_its_velocity_if_mapped_otherwise_from_its_positions() {
    let roles = BTreeMap::from([
        // Turning at 0.5 rad/s, then back at 2 rad/s.
        (
            role("joints.shoulder.position"),
            column(200, |i| {
                if i < 100 {
                    0.005 * f64::from(i)
                } else {
                    0.5 - 0.02 * f64::from(i - 100)
                }
            }),
        ),
        // Its velocity says otherwise: the mapped velocity wins.
        (role("joints.elbow.position"), column(100, |_| 1.0)),
        (
            role("joints.elbow.velocity"),
            column(100, |i| if i % 2 == 0 { 3.0 } else { -3.0 }),
        ),
        (
            role("joints.elbow.effort"),
            column(100, |i| f64::from(i) - 50.0),
        ),
    ]);
    let statistics = of_episode(&episode(roles));
    let shoulder = distribution(&statistics, "joints.shoulder.speed");
    assert_eq!(shoulder.count, 199);
    assert!((shoulder.quantile(0.25).unwrap() - 0.5).abs() < 0.01);
    assert!((shoulder.max.unwrap() - 2.0).abs() < 1e-9);
    let elbow = distribution(&statistics, "joints.elbow.speed");
    assert_eq!((elbow.min, elbow.max), (Some(3.0), Some(3.0)));
    let effort = distribution(&statistics, "joints.elbow.effort");
    assert_eq!(
        (effort.min, effort.max, effort.count),
        (Some(-50.0), Some(49.0), 100)
    );
}

#[test]
fn a_gripper_counts_how_many_times_it_closed_after_being_open() {
    // Closed, open, closed, open, closed, open: it closes twice after being
    // open, with noise that never crosses the other threshold.
    let opening = column(300, |i| {
        let open = (i / 50) % 2 == 1;
        let noise = if i % 3 == 0 { 0.003 } else { -0.003 };
        (if open { 0.08 } else { 0.0 }) + noise
    });
    let statistics = of_episode(&episode(BTreeMap::from([(
        role("gripper.opening"),
        opening,
    )])));
    assert_eq!(value(&statistics, "gripper.opening.cycles"), 2.0);
}

#[test]
fn statistics_read_back_as_stored_and_one_stored_in_another_form_is_left_out() {
    let statistics = of_episode(&episode(BTreeMap::new()));
    let mut stored = to_stored(statistics.clone());
    assert_eq!(from_stored(stored.clone()), statistics);
    stored.insert(
        "made.by.a.later.version".to_owned(),
        serde_json::json!({"histogram": [1, 2]}),
    );
    assert_eq!(from_stored(stored), statistics);
}

#[test]
fn a_joint_has_where_it_went_and_its_acceleration_and_jerk_from_its_positions() {
    // Speeding up at 2 rad/s² from rest; another at rest; another shaking
    // by a hundredth of a radian every row.
    let roles = BTreeMap::from([
        (
            role("joints.shoulder.position"),
            column(200, |i| (0.01 * f64::from(i)).powi(2)),
        ),
        (role("joints.elbow.position"), column(200, |_| 0.7)),
        (
            role("joints.wrist.position"),
            column(200, |i| if i % 2 == 0 { 0.01 } else { -0.01 }),
        ),
    ]);
    let statistics = of_episode(&episode(roles));
    let close = |a: Option<f64>, b: f64| (a.unwrap() - b).abs() < 1e-6;

    let reach = distribution(&statistics, "joints.shoulder.position");
    assert!(
        close(reach.min, 0.0) && close(reach.max, 3.9601),
        "{reach:?}"
    );
    let acceleration = distribution(&statistics, "joints.shoulder.acceleration");
    assert_eq!(acceleration.count, 198);
    assert!(close(acceleration.min, 2.0) && close(acceleration.max, 2.0));
    let jerk = distribution(&statistics, "joints.shoulder.jerk");
    assert!(close(jerk.max, 0.0), "{jerk:?}");

    for name in ["speed", "acceleration", "jerk"] {
        let still = distribution(&statistics, &format!("joints.elbow.{name}"));
        assert_eq!(still.max, Some(0.0), "{name}");
    }

    // Shaking: 2 rad/s, but 400 rad/s² and 80 000 rad/s³.
    let shaking = |name: &str| distribution(&statistics, &format!("joints.wrist.{name}")).max;
    assert!(close(shaking("speed"), 2.0));
    assert!(close(shaking("acceleration"), 400.0));
    assert!(close(shaking("jerk"), 80_000.0));
}
