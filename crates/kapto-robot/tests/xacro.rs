// SPDX-License-Identifier: Apache-2.0

//! Robot descriptions in xacro, expanded to URDF.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kapto_core::ErrorCode;
use kapto_robot::{RobotModel, expand_xacro, import_package, load_package, parse_urdf};
use kapto_testkit::TempDir;

fn testdata(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(path)
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// Whether two numbers are the same, however each tool wrote it.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(1.0)
}

/// Asserts `model` is the robot `reference` is: the same links, drawn with
/// the same meshes, and the same joints, placed, turning and limited alike.
fn assert_same_robot(model: &RobotModel, reference: &RobotModel) {
    let names = |model: &RobotModel| {
        model
            .links
            .iter()
            .map(|link| link.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(model), names(reference));
    for (link, expected) in model.links.iter().zip(&reference.links) {
        assert_eq!(
            format!(
                "{:?}",
                link.visuals
                    .iter()
                    .map(|shape| &shape.geometry)
                    .collect::<Vec<_>>()
            ),
            format!(
                "{:?}",
                expected
                    .visuals
                    .iter()
                    .map(|shape| &shape.geometry)
                    .collect::<Vec<_>>()
            ),
            "link {}",
            link.name
        );
    }
    assert_eq!(model.joints.len(), reference.joints.len());
    for (joint, expected) in model.joints.iter().zip(&reference.joints) {
        let at = &joint.name;
        assert_eq!(joint.name, expected.name);
        assert_eq!(joint.kind, expected.kind, "{at}");
        assert_eq!(
            (joint.parent, joint.child),
            (expected.parent, expected.child),
            "{at}"
        );
        let pose = |j: &kapto_robot::Joint| {
            let (t, r) = (j.origin.xyz, j.origin.rpy);
            [t[0], t[1], t[2], r[0], r[1], r[2]]
        };
        for (a, b) in pose(joint).iter().zip(pose(expected)) {
            assert!(
                close(*a, b),
                "{at}: origin {:?} against {:?}",
                pose(joint),
                pose(expected)
            );
        }
        for (a, b) in joint.axis.iter().zip(expected.axis) {
            assert!(close(*a, b), "{at}: axis");
        }
        assert_eq!(
            format!("{:.9?}", joint.limits),
            format!("{:.9?}", expected.limits),
            "{at}"
        );
    }
}

#[test]
fn universal_robots_ur5e_expands_to_the_robot_ros_own_xacro_makes_of_it() {
    let xacro = testdata("xacro/ur_description/urdf/ur.urdf.xacro");
    let text = expand_xacro(&xacro, &args(&[("ur_type", "ur5e"), ("name", "ur5e")]), &[]).unwrap();
    let model = parse_urdf(&text, &xacro.with_extension(""), &[]).unwrap();
    let reference_path = testdata("xacro/ur5e.urdf");
    let reference = parse_urdf(
        &std::fs::read_to_string(&reference_path).unwrap(),
        &reference_path,
        &[],
    )
    .unwrap();
    assert_eq!(model.name, "ur5e");
    assert_eq!(model.joints.len(), 12);
    assert_same_robot(&model, &reference);
}

#[test]
fn a_description_given_by_a_relative_path_includes_from_its_own_package() {
    let dir = TempDir::new("xacro-relative");
    // Deeper than where the test runs, so that a path joined twice can't
    // climb back to the root and land on the file by chance.
    dir.write(
        "deep/a/b/c/d/e/f/g/h/arm_description/package.xml",
        br#"<?xml version="1.0"?><package format="3"><name>arm_description</name></package>"#,
    );
    dir.write(
        "deep/a/b/c/d/e/f/g/h/arm_description/urdf/link.xacro",
        br#"<robot xmlns:xacro="http://www.ros.org/wiki/xacro"><link name="forearm"/></robot>"#,
    );
    let arm = dir.write(
        "deep/a/b/c/d/e/f/g/h/arm_description/urdf/arm.urdf.xacro",
        br#"<robot name="arm" xmlns:xacro="http://www.ros.org/wiki/xacro">
             <xacro:include filename="$(find arm_description)/urdf/link.xacro"/>
           </robot>"#,
    );
    // As given on the command line to Kapto run from here.
    let up = "../".repeat(std::env::current_dir().unwrap().components().count() - 1);
    let relative = PathBuf::from(format!(
        "{up}{}",
        arm.display().to_string().trim_start_matches('/')
    ));
    assert!(relative.is_relative());
    let text = expand_xacro(&relative, &args(&[]), &[]).unwrap();
    let model = parse_urdf(&text, &arm.with_extension(""), &[]).unwrap();
    assert_eq!(model.links.len(), 1);
    assert_eq!(model.links[0].name, "forearm");
}

#[test]
fn find_names_the_folder_kapto_finds_a_package_in_and_what_isnt_found_says_where() {
    let dir = TempDir::new("xacro-find");
    // Cloned into folders named otherwise than the packages they declare.
    dir.write(
        "clone/parts/package.xml",
        br#"<?xml version="1.0"?><package format="3"><name>gripper_description</name></package>"#,
    );
    dir.write(
        "clone/parts/finger.xacro",
        br#"<robot xmlns:xacro="http://www.ros.org/wiki/xacro">
             <xacro:macro name="finger" params="side reach">
               <link name="${side}_finger"/>
               <joint name="${side}_slide" type="prismatic">
                 <parent link="hand"/><child link="${side}_finger"/>
                 <origin xyz="0 ${reach / 2} 0"/><axis xyz="0 1 0"/>
                 <limit lower="${min(0, reach)}" upper="${max(0, reach)}" effort="10" velocity="0.1"/>
               </joint>
             </xacro:macro>
           </robot>"#,
    );
    let hand = dir.write(
        "clone/hand/hand.urdf.xacro",
        br#"<robot name="hand" xmlns:xacro="http://www.ros.org/wiki/xacro">
             <xacro:arg name="reach" default="0.04"/>
             <xacro:include filename="$(find gripper_description)/finger.xacro"/>
             <link name="hand"/>
             <xacro:finger side="left" reach="$(arg reach)"/>
             <xacro:finger side="right" reach="${-1 * $(arg reach)}"/>
           </robot>"#,
    );
    let text = expand_xacro(&hand, &args(&[("reach", "0.08")]), &[dir.0.join("clone")]).unwrap();
    let model = parse_urdf(&text, &hand.with_extension(""), &[]).unwrap();
    let slide = |name: &str| {
        model
            .joints
            .iter()
            .find(|joint| joint.name == name)
            .unwrap()
    };
    assert!(close(slide("left_slide").origin.xyz[1], 0.04));
    assert!(close(slide("right_slide").origin.xyz[1], -0.04));

    // A package that isn't anywhere: the cause code, and the file it is in.
    dir.write(
        "clone/hand/lost.urdf.xacro",
        br#"<robot name="lost" xmlns:xacro="http://www.ros.org/wiki/xacro">
             <xacro:include filename="$(find nowhere_description)/part.xacro"/>
           </robot>"#,
    );
    let error =
        expand_xacro(&dir.0.join("clone/hand/lost.urdf.xacro"), &args(&[]), &[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::RobotXacroFailed);
    assert!(
        error.message().contains("nowhere_description"),
        "{}",
        error.message()
    );
    assert!(
        error.report().contains("lost.urdf.xacro"),
        "{}",
        error.report()
    );
}

#[test]
fn a_xacro_description_is_imported_as_the_urdf_it_expands_to_with_its_meshes() {
    let dir = TempDir::new("xacro-import");
    dir.write("arm_description/meshes/base.stl", b"base");
    let xacro = dir.write(
        "arm_description/urdf/arm.urdf.xacro",
        br#"<robot name="arm" xmlns:xacro="http://www.ros.org/wiki/xacro">
             <xacro:arg name="prefix" default=""/>
             <link name="$(arg prefix)base"><visual><geometry><mesh filename="package://arm_description/meshes/base.stl"/></geometry></visual></link>
             <link name="$(arg prefix)upper"/>
             <joint name="$(arg prefix)shoulder" type="continuous"><parent link="$(arg prefix)base"/><child link="$(arg prefix)upper"/></joint>
           </robot>"#,
    );
    let packages = dir.0.join("packages");
    let imported = import_package(
        &xacro,
        &args(&[("prefix", "left_")]),
        &[],
        &packages.join("arm"),
        "arm",
        None,
    )
    .unwrap();
    assert_eq!(imported.urdf, PathBuf::from("arm.urdf"));
    assert_eq!(imported.joints, ["left_shoulder"]);
    assert_eq!(imported.copied.len(), 1);
    let package = load_package(&packages.join("arm")).unwrap();
    let urdf = std::fs::read_to_string(package.urdf()).unwrap();
    assert!(
        urdf.contains(r#"name="left_shoulder""#) && !urdf.contains("xacro:"),
        "{urdf}"
    );
    let manifest = std::fs::read_to_string(packages.join("arm/package.toml")).unwrap();
    assert!(
        manifest.contains("from arm.urdf.xacro prefix:=left_"),
        "{manifest}"
    );

    // Arguments are a xacro description's: a URDF given them is refused.
    let urdf_path = dir.write("plain/arm.urdf", urdf.as_bytes());
    let error = import_package(
        &urdf_path,
        &args(&[("prefix", "x")]),
        &[],
        &packages.join("plain"),
        "plain",
        None,
    )
    .unwrap_err();
    assert_eq!(error[0].code(), ErrorCode::ConfigInvalidValue);
}
