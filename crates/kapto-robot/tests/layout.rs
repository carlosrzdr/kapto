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
fn the_sample_arm_is_shown_moving_with_its_joints_and_gripper_plotted_but_not_its_counters() {
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
    assert!(
        side.iter().all(|panel| panel.kind != PanelKind::Health),
        "{text}"
    );
}

#[test]
fn two_arms_on_streams_of_their_own_get_a_plot_each_of_their_joints() {
    let arm = |id: &str| DeclaredStream {
        id: id.to_owned(),
        camera: false,
        fields: vec![("joint_positions".to_owned(), 2), ("gripper".to_owned(), 1)],
        unplotted: Vec::new(),
    };
    let role = |role: &str, field: &str| (role.parse().unwrap(), field.parse().unwrap());
    let declared = Declared {
        robot_moves: true,
        streams: vec![arm("right_arm"), arm("left_arm")],
        roles: vec![
            role(
                "joints.right_shoulder.position",
                "right_arm.joint_positions[0]",
            ),
            role(
                "joints.right_elbow.position",
                "right_arm.joint_positions[1]",
            ),
            role(
                "joints.left_shoulder.position",
                "left_arm.joint_positions[0]",
            ),
            role("joints.left_elbow.position", "left_arm.joint_positions[1]"),
            // One group across both: still a plot each.
            role("grippers.right.opening", "right_arm.gripper"),
            role("grippers.left.opening", "left_arm.gripper"),
        ],
    };
    let made = layout(&made(&declared));
    let plots: Vec<String> = made.columns[1]
        .panels
        .iter()
        .map(|panel| match &panel.kind {
            PanelKind::Plot { fields } => fields
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
            other => panic!("not a plot: {other:?}"),
        })
        .collect();
    assert_eq!(
        plots,
        [
            "role:joints.right_shoulder.position role:joints.right_elbow.position",
            "role:joints.left_shoulder.position role:joints.left_elbow.position",
            "role:grippers.right.opening",
            "role:grippers.left.opening",
        ]
    );
}

/// A robot that doesn't move, two cameras, and a stream of 40 fields.
fn rig() -> Declared {
    let camera = |id: &str| DeclaredStream {
        id: id.to_owned(),
        camera: true,
        fields: Vec::new(),
        unplotted: Vec::new(),
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
                unplotted: Vec::new(),
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
        ["camera", "camera", "plot of 16", "plot of 16", "plot"]
    );
}

#[test]
fn a_package_declaring_more_than_a_page_can_draw_keeps_the_first_panels() {
    let mut many = rig();
    many.robot_moves = true;
    many.streams.extend((0..30).map(|n| DeclaredStream {
        id: format!("camera_{n}"),
        camera: true,
        fields: Vec::new(),
        unplotted: Vec::new(),
    }));
    let made = made_layout(&many);
    let panels: Vec<&PanelKind> = made
        .columns
        .iter()
        .flat_map(|column| column.panels.iter().map(|panel| &panel.kind))
        .collect();
    assert_eq!(panels.len(), MAX_PANELS);
    assert_eq!(panels[0], &PanelKind::Robot);
    assert!(matches!(panels.last(), Some(PanelKind::Camera { .. })));
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
  { kind = \"plot\", fields = [\"arm_joints.sequence\"] },
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
            Missing {
                column: 0,
                panel: 6,
                what: vec!["`arm_joints.sequence` only ever climbs, so isn't plotted".to_owned()],
            },
        ]
    );
}
