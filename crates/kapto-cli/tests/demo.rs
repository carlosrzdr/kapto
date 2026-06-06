// SPDX-License-Identifier: Apache-2.0

//! `kapto demo`: Kapto, the sample arm and a synthetic robot in one command.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use common::{free_tcp_ports, free_udp_port, http, stop_gracefully};
use kapto_testkit::wait_until;

/// The first number after `"field":` in a JSON text.
fn number(json: &str, field: &str) -> u64 {
    let key = format!("\"{field}\":");
    let start = json.find(&key).unwrap() + key.len();
    let digits: String = json[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

/// The demo process, stopped with SIGTERM when the test ends however it
/// ends, so its children never outlive the test.
struct Demo(Option<std::process::Child>);

impl Drop for Demo {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            stop_gracefully(child);
        }
    }
}

#[test]
fn the_demo_receives_its_robot_and_records_an_episode_through_the_api() {
    let folder: PathBuf = std::env::temp_dir().join(format!("kdemo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let [port, stream_port] = free_tcp_ports();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["demo", "--robot-port", &free_udp_port(), "--rate", "500"])
        .args(["--port", &port])
        .args(["--viewer-stream-port", &stream_port])
        .arg("--data-dir")
        .arg(&folder)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let stderr = BufReader::new(child.stderr.take().unwrap());
    let mut demo = Demo(Some(child));
    std::thread::spawn({
        let lines = Arc::clone(&lines);
        move || {
            stderr
                .lines()
                .for_each(|line| lines.lock().unwrap().push(line.unwrap()));
        }
    });
    wait_until("Kapto to run", || {
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("Kapto is running: "))
    });

    wait_until("the robot's messages", || {
        let (code, body) = http(&port, "GET", "/api/status", "");
        code == 200 && number(&body, "received") > 0
    });
    assert_eq!(http(&port, "POST", "/api/episodes/start", "{}").0, 201);
    wait_until("messages written", || {
        number(&http(&port, "GET", "/api/status", "").1, "written") > 100
    });
    assert_eq!(http(&port, "POST", "/api/episodes/stop", "{}").0, 200);
    let (_, episodes) = http(&port, "GET", "/api/episodes", "");
    assert!(number(&episodes, "written") > 100, "{episodes}");
    assert_eq!(number(&episodes, "lost_upstream"), 0, "{episodes}");
    // Recorded with the sample arm's package, which the demo writes out.
    assert!(
        episodes.contains(r#""package":{"id":"arm7","#),
        "{episodes}"
    );
    assert!(folder.join("demo/arm7/urdf/arm7.urdf").is_file());
    // The stream is declared at the rate the synthetic robot sends.
    let (_, robot) = http(&port, "GET", "/api/robot", "");
    assert!(robot.contains("\"rate_hz\":500"), "{robot}");

    let mut child = demo.0.take().unwrap();
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(&folder).unwrap();
}
