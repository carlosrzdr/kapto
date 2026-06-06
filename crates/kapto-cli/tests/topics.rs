// SPDX-License-Identifier: Apache-2.0

//! `kapto topics`, run as a real process: built without ROS 2 it says which
//! image has it; built with it (in the ROS 2 image), it lists topics.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::process::{Command, Output};

fn topics(wait: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["topics", "--wait", wait])
        .output()
        .unwrap()
}

#[cfg(not(feature = "ros2"))]
#[test]
fn without_ros_2_listing_topics_says_which_image_has_it() {
    let output = topics("1");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr.starts_with("error[transport.ros2.not_built]"),
        "{stderr}"
    );
    assert!(stderr.contains("kapto-ros2"), "{stderr}");
}

#[cfg(feature = "ros2")]
#[test]
fn with_ros_2_a_topic_published_by_another_process_is_listed_with_its_type() {
    let name = format!("/kapto_cli_test_{}", std::process::id());
    let mut publisher = Command::new("ros2")
        .args(["topic", "pub", "--rate", "5", &name])
        .args(["std_msgs/msg/String", "{data: kapto}"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Long enough for the publisher, a Python program, to start.
    let output = topics("4");
    let _ = publisher.kill();
    let _ = publisher.wait();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    let line = stdout
        .lines()
        .find(|line| line.starts_with(&format!("{name} ")));
    assert!(
        line.is_some_and(|line| line.ends_with(" std_msgs/msg/String")),
        "{stdout}"
    );
}
