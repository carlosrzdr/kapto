// SPDX-License-Identifier: Apache-2.0

//! A ROS 2 topic's type put together from the ROS 2 installation Kapto runs
//! in, which only the ROS 2 image has: `cargo xtask ros2` runs it there.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;

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
