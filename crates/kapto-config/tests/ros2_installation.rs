// SPDX-License-Identifier: Apache-2.0

//! A ROS 2 topic's type put together from the ROS 2 installation Kapto runs
//! in, which only the ROS 2 image has: `cargo xtask ros2` runs it there; and
//! a recording's, from what it carries, wherever it is read.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;

use kapto_config::carried::{Attachments, carried_manifest, carried_streams};
use kapto_config::{ConfigFile, parse_streams};

#[test]
#[ignore = "needs a ROS 2 installation: `cargo xtask ros2` runs it in the ROS 2 image"]
fn a_topics_type_comes_from_the_installation_as_rosbag2_stores_it() {
    // No `msg/` folder beside it: every `.msg` file is the installation's.
    let streams = parse_streams(&ConfigFile {
        path: std::env::temp_dir().join("kapto-no-package/streams.toml"),
        contents: "[streams.joints]\n\
                   ros2 = { topic = \"/joint_states\", type = \"sensor_msgs/msg/JointState\" }\n"
            .to_owned(),
    })
    .unwrap();
    let reference = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/cdr/reference.json");
    let reference: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(reference).unwrap()).unwrap();
    // What rosbag2 itself stored for the type, in a bag it wrote.
    assert_eq!(
        streams[0].ros2.as_ref().unwrap().definition,
        reference["messages"]["joint_state"]["definition"]
            .as_str()
            .unwrap()
    );
}

#[test]
#[ignore = "needs a ROS 2 installation: `cargo xtask ros2` runs it in the ROS 2 image"]
fn the_sample_arms_msg_copies_are_the_installed_files_so_it_loads_where_ros_2_is() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/arm7_ros2/streams.toml");
    let contents = std::fs::read_to_string(&path).unwrap();
    let streams = parse_streams(&ConfigFile { path, contents });
    assert!(streams.is_ok(), "{:?}", streams.err());
}

/// Run on every machine, and by `cargo xtask ros2` where ROS 2 is installed
/// too, with a `std_msgs/msg/Header` of its own.
#[test]
fn a_recording_is_read_back_with_the_msg_files_it_carries_not_the_installations() {
    // Recorded with a Header unlike the installed one: what it carries
    // decides how its messages read.
    let carried = "builtin_interfaces/Time stamp\nstring frame_id\nuint32 seq\n";
    let attachments = Attachments::from([
        (
            "package.toml".to_owned(),
            b"id = \"arm\"\nversion = \"0.1.0\"\nname = \"Arm\"\nurdf = \"arm.urdf\"\n".to_vec(),
        ),
        (
            "streams.toml".to_owned(),
            b"[streams.status]\nros2 = { topic = \"/status\", type = \"std_msgs/msg/Header\" }\n"
                .to_vec(),
        ),
        (
            "msg/std_msgs/msg/Header.msg".to_owned(),
            carried.as_bytes().to_vec(),
        ),
        (
            "msg/builtin_interfaces/msg/Time.msg".to_owned(),
            b"int32 sec\nuint32 nanosec\n".to_vec(),
        ),
    ]);
    let recording = std::env::temp_dir().join("kapto-carried/recording.mcap");
    let manifest = carried_manifest(&attachments, &recording).unwrap();
    let streams = carried_streams(&attachments, &recording, manifest.as_ref()).unwrap();
    let definition = &streams[0].ros2.as_ref().unwrap().definition;
    assert!(definition.starts_with(carried), "{definition}");
}
