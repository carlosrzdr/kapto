// SPDX-License-Identifier: Apache-2.0

//! `kapto viz-bridge`, run as a real process.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{free_tcp_ports, root};
use kapto_capture::Batch;
use kapto_capture::tap::frames::{FrameConfig, frames};
use kapto_core::RecvTime;
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

#[cfg(unix)]
#[test]
fn a_camera_declared_beside_the_arm_has_its_frames_shown_to_a_page() {
    let folder = scratch("camera");
    let arm7 = root().join("packages/arm7");
    let mut streams = std::fs::read_to_string(arm7.join("streams.toml")).unwrap();
    streams.push_str(
        "\n[streams.wrist_camera]\nrate_hz = 30\nmax_message_bytes = 1048576\n\
         tcp = { listen = \"127.0.0.1:7010\", framing = { kind = \"length_prefix\", length_bytes = 4 } }\n\
         camera = { format = \"jpeg\" }\n",
    );
    std::fs::write(folder.join("streams.toml"), streams).unwrap();
    let [port] = free_tcp_ports();
    let socket = folder.join("frames.sock");
    let bridge = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .arg("viz-bridge")
        .arg("--robot")
        .arg(arm7.join("urdf/arm7.urdf"))
        .arg("--streams")
        .arg(folder.join("streams.toml"))
        .arg("--mapping")
        .arg(arm7.join("mapping.toml"))
        .args(["--duration", "3", "--viewer-stream-port", &port])
        .arg("--tap-socket")
        .arg(folder.join("tap.sock"))
        .arg("--frame-socket")
        .arg(&socket)
        .arg("--data-dir")
        .arg(&folder)
        .env_clear()
        .env("HOME", &folder)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut page = loop {
        if let Ok((page, _)) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")) {
            break page;
        }
        assert!(Instant::now() < deadline, "the bridge never listened");
        std::thread::sleep(Duration::from_millis(20));
    };
    let tungstenite::Message::Text(opening) = page.read().unwrap() else {
        panic!("no opening message");
    };
    assert!(opening.contains("\"cameras\":[1]"), "{opening}");

    // The stream after the arm's is the camera.
    let config = FrameConfig {
        cameras: vec![(1, 1 << 20)],
        min_interval: Duration::from_millis(30),
    };
    let (mut feed, mut sender, _) = frames(&config, &socket);
    // As recorded from the camera's TCP stream: the length prefix, then
    // the JPEG.
    let jpeg = [0xff, 0xd8, 0xff, 0xe0, 1, 2, 3];
    let mut message = 7_u32.to_le_bytes().to_vec();
    message.extend(jpeg);
    let mut batch = Batch::new(1, message.len());
    assert!(batch.push(1, &message));
    batch.set_recv_time(RecvTime::from_nanos(1));
    feed.offer(&batch);
    sender.send_until(Instant::now() + Duration::from_secs(5));
    let picture = loop {
        if let tungstenite::Message::Binary(bytes) = page.read().unwrap()
            && bytes.starts_with(b"KVPC")
        {
            break bytes;
        }
    };
    assert_eq!(&picture[6..8], &[1, 0], "camera stream 1");
    assert_eq!(&picture[40..], &jpeg, "the camera's own JPEG");
    drop((page, sender));

    let output = bridge.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "Cameras' frames from capture: {}\n",
            socket.display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains("Read 1 cameras' frames: 1 shown, 0 that can't be shown"),
        "{stdout}"
    );
    std::fs::remove_dir_all(folder).unwrap();
}
