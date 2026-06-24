// SPDX-License-Identifier: Apache-2.0

//! `kapto showcase`: the demo, narrated, finds what the misbehaving robot did.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::{free_tcp_ports, free_udp_port, stop_gracefully};
use kapto_testkit::scratch;

#[test]
fn the_showcase_records_ordinary_episodes_then_finds_what_the_robot_did_wrong() {
    let folder = scratch("showcase");
    let [port, stream] = free_tcp_ports();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args([
            "showcase",
            "--unattended",
            "--episodes",
            "3",
            "--seconds",
            "3",
        ])
        .args(["--robot-port", &free_udp_port()])
        .args(["--port", &port, "--viewer-stream-port", &stream])
        .arg("--data-dir")
        .arg(folder.join("data"))
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (said, heard) = mpsc::channel();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in stdout.lines().map_while(Result::ok) {
            if said.send(line).is_err() {
                return;
            }
        }
    });
    let mut lines = Vec::new();
    while let Ok(line) = heard.recv_timeout(Duration::from_secs(180)) {
        let done = line.starts_with("The showcase is done");
        lines.push(line);
        if done {
            break;
        }
    }
    stop_gracefully(child);
    let told = lines.join("\n");
    assert!(told.contains("The showcase is done"), "{told}");
    assert!(told.contains("episode 3 of 3: accepted"), "{told}");
    // The shoulder's stuck reading, found: it jumped, and stopped dead.
    assert!(told.contains("- motion.jump: jumped faster than"), "{told}");
    assert!(told.contains("- motion.frozen: stopped dead"), "{told}");
    // Its own configuration, never the user's.
    assert!(
        folder
            .join("data/showcase/config/tasks/pick_and_place.toml")
            .exists()
    );
}
