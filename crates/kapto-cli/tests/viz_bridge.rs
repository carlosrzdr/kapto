// SPDX-License-Identifier: Apache-2.0

//! `kapto viz-bridge`, run as a real process.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::{free_tcp_ports, root};
use kapto_testkit::scratch;

fn bridge(folder: &Path, mapping: &Path, [stream_port]: &[String; 1]) -> Output {
    let arm7 = root().join("packages/arm7");
    Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("viz-bridge")
        .arg("--robot")
        .arg(arm7.join("urdf/arm7.urdf"))
        .arg("--streams")
        .arg(arm7.join("streams.toml"))
        .arg("--mapping")
        .arg(mapping)
        .args(["--duration", "1", "--viewer-stream-port", stream_port])
        .arg("--tap-socket")
        .arg(folder.join("tap.sock"))
        .arg("--data-dir")
        .arg(folder)
        .env_clear()
        .env("HOME", folder)
        .output()
        .unwrap()
}

#[test]
fn the_bridge_streams_the_sample_arm_and_stops_after_its_duration() {
    let folder = scratch("arm7");
    let ports = free_tcp_ports();
    let output = bridge(&folder, &root().join("packages/arm7/mapping.toml"), &ports);
    let port = &ports[0];
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stdout}{stderr}");
    assert_eq!(stderr, "", "no warnings for the sample arm");
    assert!(
        stdout.starts_with(&format!("Streaming arm7 to the live view on port {port}\n")),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("Read 0 live samples; skipped 0 of another version and 0 unreadable\n")
    );
    assert!(
        !folder.join("tap.sock").exists(),
        "the socket is removed on exit"
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_mapping_to_a_joint_the_robot_lacks_is_refused_before_anything_starts() {
    let folder = scratch("bad-mapping");
    let mapping = folder.join("mapping.toml");
    std::fs::write(
        &mapping,
        "[joints]\njoint_9 = \"arm_joints.joint_positions[0]\"\n",
    )
    .unwrap();
    let output = bridge(&folder, &mapping, &free_tcp_ports());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr.starts_with("error[mapping.unknown_joint]"),
        "{stderr}"
    );
    assert!(!folder.join("tap.sock").exists());
    std::fs::remove_dir_all(folder).unwrap();
}
