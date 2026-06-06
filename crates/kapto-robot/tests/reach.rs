// SPDX-License-Identifier: Apache-2.0

//! How far a robot reaches, against shapes whose sweep is known by hand.

#![allow(clippy::unwrap_used, reason = "tests fail loudly")]

use std::path::Path;

use kapto_robot::{parse_urdf, reach};

/// A 0.2 m cube on the ground, and a 1 m bar along x hanging from a joint
/// 1 m up that turns about z within `limits`.
fn pendulum(kind: &str, limits: &str) -> String {
    format!(
        r#"<robot name="pendulum">
             <link name="base"><visual><geometry><box size="0.2 0.2 0.2"/></geometry></visual></link>
             <link name="bar"><visual><origin xyz="0.5 0 0"/><geometry><box size="1 0.1 0.1"/></geometry></visual></link>
             <joint name="swing" type="{kind}"><parent link="base"/><child link="bar"/>
               <origin xyz="0 0 1"/><axis xyz="0 0 1"/>{limits}</joint>
           </robot>"#
    )
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter()
        .zip(&b)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f64>()
        .sqrt()
}

#[test]
fn a_bar_turning_all_the_way_round_reaches_a_disc_of_its_length() {
    let robot = parse_urdf(&pendulum("continuous", ""), Path::new("p.urdf"), &[]).unwrap();
    let reach = reach(&robot, |_| None).unwrap().unwrap();
    // Every point the bar's far corners sweep, at 1 m up, is inside.
    for step in 0..36 {
        let angle = f64::from(step) * std::f64::consts::TAU / 36.0;
        let tip = [angle.cos(), angle.sin(), 1.0];
        assert!(
            distance(tip, reach.center) <= reach.radius + 1e-9,
            "{tip:?} outside {reach:?}"
        );
    }
    // And the cube on the ground.
    assert!(distance([0.1, 0.1, -0.1], reach.center) <= reach.radius + 1e-9);
    // Not much more: the disc's corners, with the bar's own width.
    assert!(reach.radius < 1.3, "{reach:?}");
}

#[test]
fn a_bar_that_barely_moves_reaches_little_more_than_where_it_rests() {
    let limits = r#"<limit lower="0" upper="0.1" effort="1" velocity="1"/>"#;
    let robot = parse_urdf(&pendulum("revolute", limits), Path::new("p.urdf"), &[]).unwrap();
    let reach = reach(&robot, |_| None).unwrap().unwrap();
    // Leaning towards +x, where the bar lies: it never swings to -x.
    assert!(reach.center[0] > 0.3, "{reach:?}");
    assert!(reach.radius < 0.9, "{reach:?}");
}

#[test]
fn a_robot_without_shapes_has_no_reach() {
    let robot = parse_urdf(
        r#"<robot name="bare"><link name="a"/></robot>"#,
        Path::new("bare.urdf"),
        &[],
    )
    .unwrap();
    assert_eq!(reach(&robot, |_| None).unwrap(), None);
}

#[test]
fn a_mesh_counts_by_its_bounds_scaled_and_one_that_cannot_be_read_is_left_out() {
    let part = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/meshes/handmade/tetrahedron.stl");
    let urdf = format!(
        r#"<robot name="m">
             <link name="a"><visual><geometry><mesh filename="{}" scale="2 2 2"/></geometry></visual></link>
           </robot>"#,
        part.display()
    );
    let robot = parse_urdf(&urdf, Path::new("m.urdf"), &[]).unwrap();
    let reach = reach(&robot, |_| Some(([0.0; 3], [1.0; 3])))
        .unwrap()
        .unwrap();
    assert_eq!(reach.center, [1.0; 3]);
    assert!((reach.radius - 3.0_f64.sqrt()).abs() < 1e-12);
    assert_eq!(reach_of_unreadable(&robot), None);
}

fn reach_of_unreadable(robot: &kapto_robot::RobotModel) -> Option<kapto_robot::Reach> {
    reach(robot, |_| None).unwrap()
}
