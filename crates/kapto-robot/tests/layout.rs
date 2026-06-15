// SPDX-License-Identifier: Apache-2.0

//! The layout made for a package without one of its own, from what the
//! sample packages declare, and what a layout names that its package lacks.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_config::{
    ConfigFile, Layout, MAX_PANELS, PanelKind, parse_layout, parse_mapping, parse_streams,
};
use kapto_robot::{
    Declared, DeclaredStream, Missing, load_package, load_urdf, made_layout, missing,
};

fn file(path: PathBuf) -> ConfigFile {
    let contents = std::fs::read_to_string(&path).unwrap();
    ConfigFile { path, contents }
}

/// What the sample package `name` declares.
fn declared(name: &str) -> Declared {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages")
        .join(name);
    let package = load_package(&folder).unwrap();
    let robot = load_urdf(&package.urdf(), &[]).unwrap();
    let streams = parse_streams(&file(package.streams())).unwrap();
    let mapping = parse_mapping(&file(package.mapping())).unwrap();
    Declared::new(Some(&robot), &streams, &mapping)
}

fn layout(text: &str) -> Layout {
    parse_layout(&ConfigFile {
        path: PathBuf::from("layout.toml"),
        contents: text.to_owned(),
    })
    .unwrap()
}

/// The layout made for `declared`, checked to read back the same and to
/// name nothing the package lacks.
fn made(declared: &Declared) -> String {
    let made = made_layout(declared);
    let text = made.to_toml();
    assert_eq!(layout(&text), made);
    assert_eq!(missing(&made, declared), []);
    text
}

#[test]
fn the_sample_arm_is_shown_moving_with_its_joints_gripper_and_other_fields_plotted() {
    assert_eq!(
        made(&declared("arm7")),
        "[[columns]]
width = 3
panels = [
  { kind = \"robot\", height = 1 },
]

[[columns]]
width = 2
panels = [
  { kind = \"plot\", fields = [\"role:joints.joint_1.position\", \"role:joints.joint_2.position\", \"role:joints.joint_3.position\", \"role:joints.joint_4.position\", \"role:joints.joint_5.position\", \"role:joints.joint_6.position\", \"role:joints.joint_7.position\", \"role:joints.gripper.position\"], height = 1 },
  { kind = \"plot\", fields = [\"role:gripper.opening\"], height = 1 },
  { kind = \"plot\", fields = [\"arm_joints.sequence\", \"arm_joints.source_time\"], height = 1 },
  { kind = \"health\", height = 1 },
]
"
    );
}

#[test]
fn the_ros2_sample_arm_is_made_the_same_way_from_its_topics() {
    let text = made(&declared("arm7_ros2"));
    let made = layout(&text);
    assert_eq!(made.columns[0].panels[0].kind, PanelKind::Robot);
    let side = &made.columns[1].panels;
    assert!(
        matches!(&side[0].kind, PanelKind::Plot { fields } if fields[0].to_string() == "role:joints.joint_1.position"),
        "{text}"
    );
    assert_eq!(side.last().unwrap().kind, PanelKind::Health);
}

/// A robot that doesn't move, two cameras, and a stream of 40 fields.
fn rig() -> Declared {
    let camera = |id: &str| DeclaredStream {
        id: id.to_owned(),
        camera: true,
        fields: Vec::new(),
    };
    Declared {
        robot_moves: false,
        streams: vec![
            camera("head_camera"),
            camera("wrist_camera"),
            DeclaredStream {
                id: "rig".to_owned(),
                camera: false,
                fields: (0..40).map(|n| (format!("load_{n}"), 1)).collect(),
            },
        ],
        roles: Vec::new(),
    }
}

#[test]
fn a_robot_that_doesnt_move_gets_no_3d_view_and_each_camera_comes_first() {
    let made = layout(&made(&rig()));
    assert_eq!(made.columns.len(), 1);
    let kinds: Vec<&str> = made.columns[0]
        .panels
        .iter()
        .map(|panel| match &panel.kind {
            PanelKind::Camera { .. } => "camera",
            PanelKind::Plot { fields } if fields.len() == 16 => "plot of 16",
            PanelKind::Plot { .. } => "plot",
            PanelKind::Health => "health",
            PanelKind::Robot | PanelKind::Events => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "camera",
            "camera",
            "plot of 16",
            "plot of 16",
            "plot",
            "health"
        ]
    );
}

#[test]
fn a_package_declaring_more_than_a_page_can_draw_keeps_the_first_panels_and_its_health() {
    let mut many = rig();
    many.robot_moves = true;
    many.streams.extend((0..30).map(|n| DeclaredStream {
        id: format!("camera_{n}"),
        camera: true,
        fields: Vec::new(),
    }));
    let made = made_layout(&many);
    let panels: Vec<&PanelKind> = made
        .columns
        .iter()
        .flat_map(|column| column.panels.iter().map(|panel| &panel.kind))
        .collect();
    assert_eq!(panels.len(), MAX_PANELS);
    assert_eq!(panels[0], &PanelKind::Robot);
    assert_eq!(panels.last(), Some(&&PanelKind::Health));
}

#[test]
fn each_panel_naming_what_the_package_lacks_says_what() {
    let named = layout(
        "[[columns]]
panels = [
  { kind = \"robot\" },
  { kind = \"camera\", stream = \"wrist_camera\" },
  { kind = \"camera\", stream = \"arm_joints\" },
  { kind = \"plot\", fields = [\"role:joints.joint_1.position\", \"role:gripper.force\"] },
  { kind = \"plot\", fields = [\"arm_joints.torques\", \"base.speed\", \"arm_joints.joint_positions[7]\"] },
  { kind = \"plot\", fields = [\"arm_joints.joint_positions[6]\"] },
  { kind = \"health\" },
  { kind = \"events\" },
]
",
    );
    assert_eq!(
        missing(&named, &declared("arm7")),
        [
            Missing {
                column: 0,
                panel: 1,
                what: vec!["no stream `wrist_camera`".to_owned()],
            },
            Missing {
                column: 0,
                panel: 2,
                what: vec!["`arm_joints` isn't a camera".to_owned()],
            },
            Missing {
                column: 0,
                panel: 3,
                what: vec!["no field plays `gripper.force`".to_owned()],
            },
            Missing {
                column: 0,
                panel: 4,
                what: vec![
                    "`arm_joints` decodes no field `torques`".to_owned(),
                    "no stream `base`".to_owned(),
                    "`arm_joints.joint_positions` holds 7 values, so no `[7]`".to_owned(),
                ],
            },
        ]
    );
}
