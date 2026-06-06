// SPDX-License-Identifier: Apache-2.0

//! Mapping decoded fields to joints: the sample arm's mapping, and every
//! way a mapping can name something that isn't there.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_config::{ConfigFile, StreamDeclaration, parse_mapping, parse_streams};
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::{JointMap, Kinematics, RobotModel, ValueSource, load_urdf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn config(path: &str, contents: &str) -> ConfigFile {
    ConfigFile {
        path: PathBuf::from(path),
        contents: contents.to_owned(),
    }
}

fn read(path: &Path) -> ConfigFile {
    ConfigFile {
        path: path.to_path_buf(),
        contents: std::fs::read_to_string(path).unwrap(),
    }
}

fn map(
    robot: &RobotModel,
    streams: &[StreamDeclaration],
    mapping: &str,
) -> Result<JointMap, Vec<KaptoError>> {
    let kinematics = Kinematics::new(robot).unwrap();
    let mapping = parse_mapping(&config("mapping.toml", mapping)).unwrap();
    JointMap::new(
        robot,
        &kinematics,
        streams,
        &mapping,
        Path::new("mapping.toml"),
    )
}

fn problems(robot: &RobotModel, streams: &[StreamDeclaration], mapping: &str) -> Vec<String> {
    map(robot, streams, mapping)
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

/// Streams with a decoder: `arm` (a sequence number, then seven positions
/// in rad and seven efforts in N·m, a flag and a value in degrees) and
/// `camera`, without a decoder.
fn streams() -> Vec<StreamDeclaration> {
    parse_streams(&config(
        "streams.toml",
        "[streams.camera]\n\
         [streams.arm.decoder]\n\
         kind = \"struct\"\n\
         fields = [\n\
           { name = \"sequence\", type = \"u32\", offset = 0 },\n\
           { name = \"positions\", type = \"f64\", offset = 4, count = 7, unit = \"rad\" },\n\
           { name = \"efforts\", type = \"f32\", offset = 60, count = 7, unit = \"N·m\" },\n\
           { name = \"enabled\", type = \"bool\", offset = 88 },\n\
           { name = \"wrist_deg\", type = \"f32\", offset = 89, unit = \"deg\" },\n\
         ]\n",
    ))
    .unwrap()
}

fn panda() -> RobotModel {
    load_urdf(&root().join("testdata/robots/panda/panda.urdf"), &[]).unwrap()
}

#[test]
fn the_sample_arms_mapping_drives_every_joint_from_the_synthetic_robot() {
    let package = root().join("packages/arm7");
    let robot = load_urdf(&package.join("urdf/arm7.urdf"), &[]).unwrap();
    let kinematics = Kinematics::new(&robot).unwrap();
    let streams = parse_streams(&read(&package.join("streams.toml"))).unwrap();
    let mapping = parse_mapping(&read(&package.join("mapping.toml"))).unwrap();
    let map = JointMap::new(&robot, &kinematics, &streams, &mapping, &package).unwrap();
    assert!(map.warnings().is_empty());
    // The joint positions follow the sequence number and the source time,
    // and the gripper opening follows them.
    let positions: Vec<_> = map.joints().iter().map(|joint| joint.position).collect();
    let expected: Vec<_> = (2..10)
        .map(|value| Some(ValueSource { stream: 0, value }))
        .collect();
    assert_eq!(positions, expected);
}

#[test]
fn positions_and_efforts_are_found_among_the_decoded_values() {
    let mapping = "[joints]\n\
        panda_joint1 = \"arm.positions[0]\"\n\
        panda_joint7 = { position = \"arm.positions[6]\", effort = \"arm.efforts[6]\" }\n";
    let map = map(&panda(), &streams(), mapping).unwrap();
    let joint7 = &map.joints()[6];
    assert_eq!(joint7.joint, "panda_joint7");
    // Values: sequence (0), positions (1 to 7), efforts (8 to 14).
    assert_eq!(
        joint7.position,
        Some(ValueSource {
            stream: 1,
            value: 7
        })
    );
    assert_eq!(
        joint7.effort,
        Some(ValueSource {
            stream: 1,
            value: 14
        })
    );
    assert_eq!(joint7.velocity, None);
}

#[test]
fn joints_without_a_position_are_warned_about_not_refused() {
    let map = map(
        &panda(),
        &streams(),
        "[joints]\npanda_joint1 = \"arm.positions[0]\"\n",
    )
    .unwrap();
    let unmapped: Vec<_> = map.warnings().iter().map(KaptoError::message).collect();
    assert_eq!(unmapped.len(), 7);
    assert!(unmapped[0].contains("panda_joint2"), "{}", unmapped[0]);
    assert!(
        map.warnings()
            .iter()
            .all(|warning| warning.code() == ErrorCode::MappingJointUnmapped)
    );
}

#[test]
fn every_wrong_joint_and_field_is_reported_with_its_line() {
    let mapping = "[joints]\n\
        panda_jiont1 = \"arm.positions[0]\"\n\
        panda_finger_joint2 = \"arm.positions[1]\"\n\
        panda_joint8 = \"arm.positions[2]\"\n\
        panda_joint2 = \"arms.positions[1]\"\n\
        panda_joint3 = \"camera.positions[2]\"\n\
        panda_joint4 = \"arm.postions[3]\"\n\
        panda_joint5 = \"arm.positions\"\n\
        panda_joint6 = \"arm.positions[7]\"\n\
        panda_joint7 = \"arm.sequence[0]\"\n\
        panda_finger_joint1 = { position = \"arm.enabled\", effort = \"arm.positions[0]\" }\n";
    assert_eq!(
        problems(&panda(), &streams(), mapping),
        [
            "mapping.unknown_joint 2: The mapping names a joint that can't be driven: the robot has no joint `panda_jiont1` (did you mean `panda_joint1`?)",
            "mapping.unknown_joint 3: The mapping names a joint that can't be driven: joint `panda_finger_joint2` copies joint `panda_finger_joint1`; map `panda_finger_joint1` instead",
            "mapping.unknown_joint 4: The mapping names a joint that can't be driven: joint `panda_joint8` is fixed, so no single value says where it is",
            "mapping.unknown_field 5: The mapping names a field that isn't decoded: `arms.positions[1]`: streams.toml declares no stream `arms` (did you mean `arm`?)",
            "mapping.unknown_field 6: The mapping names a field that isn't decoded: `camera.positions[2]`: stream `camera` has no decoder, so it has no fields",
            "mapping.unknown_field 7: The mapping names a field that isn't decoded: `arm.postions[3]`: stream `arm` decodes no field `postions` (did you mean `positions`?)",
            "mapping.unknown_field 8: The mapping names a field that isn't decoded: `arm.positions`: `positions` holds 7 values; say which, e.g. `arm.positions[0]`",
            "mapping.unknown_field 9: The mapping names a field that isn't decoded: `arm.positions[7]`: `positions` holds 7 values, numbered 0 to 6",
            "mapping.unknown_field 10: The mapping names a field that isn't decoded: `arm.sequence[0]`: `sequence` is a single value; leave out the index",
            "mapping.unknown_field 11: The mapping names a field that isn't decoded: `arm.enabled`: `enabled` is true/false, not a number",
            "mapping.unit_mismatch 11: The mapped field's unit doesn't fit the joint: `arm.positions[0]` is in rad, but joint `panda_finger_joint1` needs N",
        ]
    );
}

#[test]
fn a_position_in_degrees_is_refused_with_how_to_convert_it() {
    let errors = map(
        &panda(),
        &streams(),
        "[joints]\npanda_joint7 = \"arm.wrist_deg\"\n",
    )
    .unwrap_err();
    assert_eq!(errors[0].code(), ErrorCode::MappingUnitMismatch);
    assert!(
        errors[0]
            .message()
            .contains("is in deg, but joint `panda_joint7` needs rad")
    );
    assert!(ErrorCode::MappingUnitMismatch.remedy().contains("scale"));
}

#[test]
fn a_role_may_be_any_decoded_value_and_one_that_isnt_decoded_is_reported_with_its_line() {
    // A whole array, an element, a flag and a value in degrees all play
    // roles: roles say what a value is for, not that it drives a joint.
    let mapping = "[roles]\n\
                   \"arm.positions\" = \"arm.positions\"\n\
                   \"arm.first\" = \"arm.positions[0]\"\n\
                   \"safety.enabled\" = \"arm.enabled\"\n\
                   \"wrist.angle\" = \"arm.wrist_deg\"\n";
    assert!(map(&panda(), &streams(), mapping).is_ok());
    assert_eq!(
        problems(
            &panda(),
            &streams(),
            "[roles]\n\
             \"camera.head\" = \"camera.image\"\n\
             \"arm.last\" = \"arm.positions[7]\"\n\
             \"arm.sequence\" = \"arm.sequence[0]\"\n\
             \"arm.effort\" = \"arm.effort\"\n"
        ),
        [
            "mapping.unknown_field 2: The mapping names a field that isn't decoded: `camera.image`: stream `camera` has no decoder, so it has no fields",
            "mapping.unknown_field 3: The mapping names a field that isn't decoded: `arm.positions[7]`: `positions` holds 7 values, numbered 0 to 6",
            "mapping.unknown_field 4: The mapping names a field that isn't decoded: `arm.sequence[0]`: `sequence` is a single value; leave out the index",
            "mapping.unknown_field 5: The mapping names a field that isn't decoded: `arm.effort`: stream `arm` decodes no field `effort` (did you mean `efforts`?)",
        ]
    );
}
