// SPDX-License-Identifier: Apache-2.0

//! Loading URDF files: a real third-party robot, a humanoid, how mesh files
//! are found, and every way a description can be broken.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{Geometry, JointKind, RobotModel, load_urdf, parse_urdf};
use kapto_testkit::TempDir;

fn testdata(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/robots")
        .join(path)
}

fn names<'a>(items: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    items.collect()
}

/// The children of the link called `name`.
fn children<'a>(robot: &'a RobotModel, name: &str) -> Vec<&'a str> {
    let (index, _) = robot.link(name).unwrap();
    names(
        robot
            .joints
            .iter()
            .filter(|joint| joint.parent == index)
            .map(|joint| robot.links[joint.child].name.as_str()),
    )
}

/// A URDF wrapped in a `<robot>` element.
fn robot(body: &str) -> String {
    format!("<robot name=\"test\">{body}</robot>")
}

fn problems(urdf: &str) -> Vec<String> {
    parse_urdf(urdf, Path::new("robot.urdf"), &[])
        .unwrap_err()
        .iter()
        .map(|error| format!("{}: {}", error.code(), error.message()))
        .collect()
}

const REVOLUTE_LIMIT: &str = r#"<limit lower="-1" upper="1" effort="1" velocity="1"/>"#;

fn revolute(name: &str, parent: &str, child: &str) -> String {
    format!(
        r#"<joint name="{name}" type="revolute"><parent link="{parent}"/><child link="{child}"/>{REVOLUTE_LIMIT}</joint>"#
    )
}

#[test]
fn the_franka_panda_loads_with_its_arm_joints_and_mimicking_finger() {
    let panda = load_urdf(&testdata("panda/panda.urdf"), &[]).unwrap();
    assert_eq!(panda.name, "panda");
    assert_eq!((panda.links.len(), panda.joints.len()), (13, 12));
    assert_eq!(panda.links[panda.root].name, "panda_link0");
    assert_eq!(
        names(panda.actuated_joints().map(|joint| joint.name.as_str())),
        [
            "panda_joint1",
            "panda_joint2",
            "panda_joint3",
            "panda_joint4",
            "panda_joint5",
            "panda_joint6",
            "panda_joint7",
            "panda_finger_joint1"
        ]
    );
    let (_, joint4) = panda.joint("panda_joint4").unwrap();
    let limits = joint4.limits.unwrap();
    assert_eq!((limits.lower, limits.upper), (-3.0718, -0.0698));
    let (finger1, _) = panda.joint("panda_finger_joint1").unwrap();
    let (_, finger2) = panda.joint("panda_finger_joint2").unwrap();
    let mimic = finger2.mimic.unwrap();
    assert_eq!(
        (mimic.joint, mimic.multiplier, mimic.offset),
        (finger1, 1.0, 0.0)
    );
}

#[test]
fn meshes_that_are_not_there_are_listed_and_the_robot_still_loads() {
    let panda = load_urdf(&testdata("panda/panda.urdf"), &[]).unwrap();
    let missing: Vec<_> = panda.missing_files().collect();
    // A visual and a collision mesh for each of its ten bodies.
    assert_eq!(missing.len(), 20);
    let (link, resource) = missing[0];
    assert_eq!(link.name, "panda_link0");
    assert_eq!(
        resource.uri,
        "package://example-robot-data/robots/panda_description/meshes/visual/link0.dae"
    );
}

#[test]
fn a_humanoid_is_one_tree_branching_from_its_pelvis() {
    let humanoid = load_urdf(&testdata("humanoid/humanoid.urdf"), &[]).unwrap();
    assert_eq!(humanoid.links[humanoid.root].name, "pelvis");
    assert_eq!(
        children(&humanoid, "pelvis"),
        ["torso", "left_thigh", "right_thigh"]
    );
    assert_eq!(
        children(&humanoid, "torso"),
        ["head", "left_upper_arm", "right_upper_arm"]
    );
    // 16 joints, two of them fingers copying the other finger of their hand.
    assert_eq!(humanoid.joints.len(), 16);
    assert_eq!(humanoid.actuated_joints().count(), 14);
    assert!(humanoid.missing_files().next().is_none());
    // Shapes given by a top-level material's name get its colour.
    let torso = humanoid.link("torso").unwrap().1;
    let material = torso.visuals[0].material.as_ref().unwrap();
    assert_eq!(material.name.as_deref(), Some("grey"));
    assert_eq!(material.rgba, Some([0.6, 0.6, 0.6, 1.0]));
    assert!(matches!(
        humanoid.link("left_upper_arm").unwrap().1.visuals[0].geometry,
        Geometry::Capsule {
            radius: 0.04,
            length: 0.3
        }
    ));
}

#[test]
fn mesh_files_are_found_by_package_relative_and_absolute_paths() {
    let dir = TempDir::new("meshes");
    let base = dir.write("arm_description/meshes/base.stl", b"mesh");
    let link = dir.write("arm_description/meshes/link.obj", b"mesh");
    let gripper = dir.write("elsewhere/gripper_description/meshes/finger.dae", b"mesh");
    let texture = dir.write("arm_description/textures/metal.png", b"mesh");
    let absolute = dir.write("absolute/part.stl", b"mesh");
    let urdf_path = dir.write("arm_description/urdf/arm.urdf", b"mesh");
    let mesh =
        |uri: &str| format!(r#"<visual><geometry><mesh filename="{uri}"/></geometry></visual>"#);
    let urdf = robot(&format!(
        r#"<link name="base">{}</link>
           <link name="link">{}{}</link>
           <link name="finger">
             <visual><geometry><mesh filename="package://gripper_description/meshes/finger.dae" scale="0.001 0.001 0.001"/></geometry>
               <material name="metal"><texture filename="package://arm_description/textures/metal.png"/></material>
             </visual>
           </link>
           {}{}"#,
        mesh("package://arm_description/meshes/base.stl"),
        mesh("../meshes/link.obj"),
        mesh(&format!("file://{}", absolute.display())),
        revolute("a", "base", "link"),
        revolute("b", "link", "finger"),
    ));
    std::fs::write(&urdf_path, &urdf).unwrap();
    let search = [dir.0.join("elsewhere")];
    let robot = load_urdf(&urdf_path, &search).unwrap();

    let path = |link: &str, shape: usize| match &robot.link(link).unwrap().1.visuals[shape].geometry
    {
        Geometry::Mesh { file, .. } => file.path.clone().unwrap(),
        other => panic!("not a mesh: {other:?}"),
    };
    assert_eq!(path("base", 0), base);
    assert_eq!(
        path("link", 0),
        urdf_path.parent().unwrap().join("../meshes/link.obj")
    );
    assert_eq!(
        std::fs::canonicalize(path("link", 0)).unwrap(),
        std::fs::canonicalize(link).unwrap()
    );
    assert_eq!(path("link", 1), absolute);
    assert_eq!(path("finger", 0), gripper);
    let finger = &robot.link("finger").unwrap().1.visuals[0];
    assert!(matches!(
        finger.geometry,
        Geometry::Mesh {
            scale: [0.001, 0.001, 0.001],
            ..
        }
    ));
    let material = finger.material.as_ref().unwrap();
    assert_eq!(
        material.texture.as_ref().unwrap().path.as_deref(),
        Some(texture.as_path())
    );
    assert!(robot.missing_files().next().is_none());
}

#[test]
fn a_package_is_found_by_the_name_its_package_xml_declares_and_through_a_link() {
    let dir = TempDir::new("package-names");
    // Cloned into a folder named otherwise than the package it declares.
    let pelvis = dir.write("clone/robots/h1/meshes/pelvis.stl", b"mesh");
    dir.write(
        "clone/robots/h1/package.xml",
        br#"<?xml version="1.0"?><package format="3"><name>h1_description</name><version>1.0.0</version></package>"#,
    );
    let urdf_path = dir.write(
        "clone/robots/h1/urdf/h1.urdf",
        robot(r#"<link name="pelvis"><visual><geometry><mesh filename="package://h1_description/meshes/pelvis.stl"/></geometry></visual></link>"#)
            .as_bytes(),
    );
    let pelvis_of = |robot: &RobotModel| match &robot.link("pelvis").unwrap().1.visuals[0].geometry
    {
        Geometry::Mesh { file, .. } => file.path.clone(),
        other => panic!("not a mesh: {other:?}"),
    };
    let found = load_urdf(&urdf_path, &[]).unwrap();
    assert_eq!(pelvis_of(&found), Some(pelvis.clone()));

    // Linked into a packages folder under the robot's own ID, as a package
    // is: the folder it links to declares the name.
    #[cfg(unix)]
    {
        std::fs::create_dir_all(dir.0.join("packages")).unwrap();
        std::os::unix::fs::symlink(dir.0.join("clone/robots/h1"), dir.0.join("packages/h1"))
            .unwrap();
        let linked = load_urdf(
            &dir.0.join("packages/h1/urdf/h1.urdf"),
            &[dir.0.join("packages")],
        )
        .unwrap();
        let linked_pelvis = pelvis_of(&linked).map(|path| std::fs::canonicalize(path).unwrap());
        assert_eq!(linked_pelvis, Some(std::fs::canonicalize(&pelvis).unwrap()));
        assert!(linked.missing_files().next().is_none());
    }

    // A package.xml declaring another name doesn't make the folder this one.
    std::fs::write(
        dir.0.join("clone/robots/h1/package.xml"),
        r#"<?xml version="1.0"?><package format="3"><name>g1_description</name></package>"#,
    )
    .unwrap();
    let missing = load_urdf(&urdf_path, &[]).unwrap();
    assert_eq!(pelvis_of(&missing), None);
}

#[test]
fn defaults_fill_in_what_the_urdf_leaves_out() {
    let urdf = robot(&format!(
        r#"<link name="a"/><link name="b"/><link name="c"/>
           <joint name="spin" type="continuous"><parent link="a"/><child link="b"/></joint>
           <joint name="slide" type="prismatic"><parent link="b"/><child link="c"/>
             <axis xyz="0 0 2"/>{REVOLUTE_LIMIT}</joint>"#
    ));
    let robot = parse_urdf(&urdf, Path::new("robot.urdf"), &[]).unwrap();
    let spin = &robot.joints[0];
    // Continuous joints have no limits, and the axis defaults to x.
    assert_eq!(
        (spin.kind, spin.limits, spin.axis),
        (JointKind::Continuous, None, [1.0, 0.0, 0.0])
    );
    assert_eq!(spin.origin.xyz, [0.0; 3]);
    // Axes are scaled to length 1.
    assert_eq!(robot.joints[1].axis, [0.0, 0.0, 1.0]);
}

#[test]
fn every_problem_with_the_tree_is_reported_at_once() {
    let urdf = robot(&format!(
        r#"<link name="base"/><link name="base"/><link name="arm"/><link name="hand"/>
           {}{}
           <joint name="twisted" type="revolute"><parent link="arm"/><child link="hand"/>
             <axis xyz="0 0 0"/>{REVOLUTE_LIMIT}</joint>
           <joint name="backwards" type="prismatic"><parent link="base"/><child link="ghost"/>
             <limit lower="1" upper="-1" effort="1" velocity="1"/></joint>
           <joint name="copy" type="revolute"><parent link="hand"/><child link="hand"/>
             {REVOLUTE_LIMIT}<mimic joint="nobody"/></joint>"#,
        revolute("shoulder", "base", "arm"),
        revolute("shoulder", "base", "arm"),
    ));
    assert_eq!(
        problems(&urdf),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: link `base` is declared more than once",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `shoulder` is declared more than once",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `twisted` has the axis [0.0, 0.0, 0.0], which has no direction",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `backwards` has child link `ghost`, which isn't declared",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `backwards` has a lower limit (1) above its upper limit (-1)",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `copy` connects link `hand` to itself",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `copy` mimics joint `nobody`, which isn't declared",
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: link `arm` is the child of both `shoulder` and `shoulder`: a link has one parent joint",
        ]
    );
}

#[test]
fn a_robot_must_have_exactly_one_root_and_no_loops() {
    let two_roots = robot(&format!(
        r#"<link name="a"/><link name="b"/><link name="c"/>{}"#,
        revolute("j", "a", "b")
    ));
    assert_eq!(
        problems(&two_roots),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: links `a`, `c` have no parent joint: the robot must be one tree with one root link"
        ]
    );
    let looped = robot(&format!(
        r#"<link name="root"/><link name="a"/><link name="b"/>{}{}{}"#,
        revolute("j1", "root", "a"),
        revolute("j2", "a", "b"),
        revolute("j3", "b", "a"),
    ));
    assert_eq!(
        problems(&looped),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: link `a` is the child of both `j1` and `j3`: a link has one parent joint"
        ]
    );
    let ring = robot(&format!(
        r#"<link name="a"/><link name="b"/>{}{}"#,
        revolute("j1", "a", "b"),
        revolute("j2", "b", "a"),
    ));
    assert_eq!(
        problems(&ring),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: every link has a parent joint, so the joints form a loop: one link must be the root"
        ]
    );
    let detached_loop = robot(&format!(
        r#"<link name="root"/><link name="a"/><link name="b"/>{}{}"#,
        revolute("j1", "a", "b"),
        revolute("j2", "b", "a"),
    ));
    assert_eq!(
        problems(&detached_loop),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: link `a` is part of a loop of joints"
        ]
    );
    assert_eq!(
        problems(&robot("")),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: the robot has no links"
        ]
    );
}

#[test]
fn only_joints_with_one_value_can_mimic_or_be_mimicked() {
    let urdf = robot(&format!(
        r#"<link name="a"/><link name="b"/><link name="c"/>
           <joint name="weld" type="fixed"><parent link="a"/><child link="b"/></joint>
           <joint name="copy" type="revolute"><parent link="b"/><child link="c"/>
             {REVOLUTE_LIMIT}<mimic joint="weld" multiplier="2"/></joint>"#
    ));
    assert_eq!(
        problems(&urdf),
        [
            "robot.kinematic_tree_invalid: The robot's links and joints don't form one tree: joint `copy` mimics joint `weld`, but only revolute, continuous and prismatic joints can mimic or be mimicked"
        ]
    );
}

#[test]
fn a_mimic_on_a_fixed_joint_is_ignored_as_it_has_no_value_to_copy_into() {
    let urdf = robot(&format!(
        r#"<link name="a"/><link name="b"/><link name="c"/>
           {}
           <joint name="weld" type="fixed"><parent link="b"/><child link="c"/>
             <mimic joint="driven" multiplier="2"/></joint>"#,
        revolute("driven", "a", "b")
    ));
    let robot = parse_urdf(&urdf, Path::new("robot.urdf"), &[]).unwrap();
    let (_, weld) = robot.joint("weld").unwrap();
    assert_eq!(weld.kind, JointKind::Fixed);
    assert!(weld.mimic.is_none());
}

/// Each error as `code line: message`.
fn located(urdf: &str) -> Vec<String> {
    parse_urdf(urdf, Path::new("robot.urdf"), &[])
        .unwrap_err()
        .iter()
        .map(|error| {
            let line = error
                .context_entries()
                .iter()
                .find(|(key, _)| *key == "line")
                .map_or("?", |(_, line)| line.as_str());
            format!("{} {line}: {}", error.code(), error.message())
        })
        .collect()
}

#[test]
fn values_that_cannot_be_read_are_reported_with_their_lines() {
    let urdf = "<robot name=\"test\">\n\
        <link name=\"a\"><visual><origin xyz=\"0 0\"/><geometry><box size=\"1 1 1\"/></geometry></visual></link>\n\
        <link name=\"b\"><visual><geometry><cone radius=\"1\"/></geometry></visual></link>\n\
        <link name=\"c\"><collision><geometry><sphere radius=\"big\"/></geometry></collision></link>\n\
        <link><visual/></link>\n\
        <joint name=\"j1\" type=\"hinge\"><parent link=\"a\"/><child link=\"b\"/></joint>\n\
        <joint name=\"j2\" type=\"revolute\"><parent link=\"a\"/><child link=\"c\"/></joint>\n\
        <joint name=\"j3\" type=\"fixed\"><parent link=\"a\"/></joint>\n\
        </robot>";
    assert_eq!(
        located(urdf),
        [
            "robot.urdf_invalid 2: The robot description isn't valid URDF: `xyz` of <origin> must be 3 numbers separated by spaces, not \"0 0\"",
            "robot.urdf_invalid 3: The robot description isn't valid URDF: <cone> isn't a shape: use <box>, <cylinder>, <capsule>, <sphere> or <mesh>",
            "robot.urdf_invalid 4: The robot description isn't valid URDF: `radius` of <sphere> must be a number, not \"big\"",
            "robot.urdf_invalid 5: The robot description isn't valid URDF: <link> needs `name`",
            "robot.urdf_invalid 6: The robot description isn't valid URDF: joint type `hinge` isn't one of revolute, continuous, prismatic, fixed, floating, planar or spherical",
            "robot.urdf_invalid 7: The robot description isn't valid URDF: joint `j2` moves within limits, so it needs a <limit>",
            "robot.urdf_invalid 8: The robot description isn't valid URDF: <joint> needs a <child>",
        ]
    );
}

#[test]
fn tree_problems_point_at_the_joint_they_belong_to() {
    let urdf = format!(
        "<robot name=\"test\">\n<link name=\"a\"/>\n<link name=\"b\"/>\n{}\n{}\n</robot>",
        revolute("j1", "a", "b"),
        revolute("j2", "a", "nowhere"),
    );
    assert_eq!(
        located(&urdf),
        [
            "robot.kinematic_tree_invalid 5: The robot's links and joints don't form one tree: joint `j2` has child link `nowhere`, which isn't declared"
        ]
    );
}

#[test]
fn elements_kapto_does_not_use_are_ignored() {
    let urdf = robot(&format!(
        r#"<link name="a"><inertial><mass value="1"/></inertial></link><link name="b"/>{}
           <transmission name="t"><type>SimpleTransmission</type></transmission>
           <gazebo reference="a"><material>Gazebo/Grey</material></gazebo>
           <ros2_control name="arm" type="system"/>"#,
        revolute("j", "a", "b")
    ));
    let robot = parse_urdf(&urdf, Path::new("robot.urdf"), &[]).unwrap();
    assert_eq!(robot.joints.len(), 1);
}

#[test]
fn a_document_type_declaration_is_refused_so_entities_cannot_expand() {
    let urdf = "<?xml version=\"1.0\"?>\n<!DOCTYPE robot [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;\">]>\n<robot name=\"&b;\"><link name=\"a\"/></robot>";
    let errors = parse_urdf(urdf, Path::new("robot.urdf"), &[]).unwrap_err();
    assert_eq!(errors[0].code(), ErrorCode::RobotUrdfInvalid);
}

#[test]
fn a_document_that_is_not_a_robot_is_refused() {
    assert_eq!(
        located("<sdf version=\"1.9\"><model name=\"x\"/></sdf>"),
        [
            "robot.urdf_invalid 1: The robot description isn't valid URDF: the top element is <sdf>, but a URDF starts with <robot>"
        ]
    );
}

#[test]
fn text_that_is_not_urdf_fails_with_urdf_invalid() {
    let errors = parse_urdf("<robot name=\"x\">\n<link", Path::new("robot.urdf"), &[]).unwrap_err();
    assert_eq!(errors[0].code(), ErrorCode::RobotUrdfInvalid);
    let context = errors[0].context_entries();
    assert!(
        context.contains(&("file", "robot.urdf".to_owned())),
        "{context:?}"
    );
    assert!(context.iter().any(|(key, _)| *key == "line"), "{context:?}");
}

#[test]
fn a_file_that_cannot_be_read_fails_with_urdf_unreadable() {
    let errors = load_urdf(Path::new("/no/such/robot.urdf"), &[]).unwrap_err();
    let codes: Vec<_> = errors.iter().map(KaptoError::code).collect();
    assert_eq!(codes, [ErrorCode::RobotUrdfUnreadable]);
}

#[test]
fn namespace_prefixes_used_but_never_declared_are_read_past_as_urdfdom_does() {
    // As Fetch's description has it: a Gazebo sensor with an undeclared prefix.
    let urdf = format!(
        "<?xml version=\"1.0\"?>\n<robot name=\"fetch\">\n<link name=\"a\"/><link name=\"b\"/>\n{}\n<gazebo reference=\"b\">\n<sensor:camera name=\"rgb\"><controller:plugin name=\"p\"/></sensor:camera>\n</gazebo>\n<joint name=\"bad\" type=\"fixed\"><parent link=\"a\"/><child link=\"nowhere\"/></joint>\n</robot>",
        revolute("swing", "a", "b")
    );
    // Lines are still where they were in the file.
    assert_eq!(
        located(&urdf),
        [
            "robot.kinematic_tree_invalid 8: The robot's links and joints don't form one tree: joint `bad` has child link `nowhere`, which isn't declared"
        ]
    );
    let fixed = urdf.replace(
        "<joint name=\"bad\" type=\"fixed\"><parent link=\"a\"/><child link=\"nowhere\"/></joint>",
        "",
    );
    let robot = parse_urdf(&fixed, Path::new("robot.urdf"), &[]).unwrap();
    assert_eq!(robot.joints.len(), 1);
}
