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
use std::net::{TcpListener, UdpSocket};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
        // Pre-flight measures the streams' rates and the disk's free space,
        // which depend on how busy and full the test machine is: its
        // failures are only warned of, so they can't refuse the start.
        .args(["--capture-preflight", "warn"])
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
    let (code, body) = http(&port, "POST", "/api/episodes/start", "{}");
    assert_eq!(code, 201, "{body}");
    wait_until("messages written", || {
        number(&http(&port, "GET", "/api/status", "").1, "written") > 100
    });
    // The wrist camera's pictures, beside the arm's joints.
    wait_until("the camera's frames written", || {
        let (_, body) = http(&port, "GET", "/api/status", "");
        let status: serde_json::Value = serde_json::from_str(&body).unwrap();
        status["capture"]["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|stream| {
                stream["stream"] == "wrist_camera" && stream["written"].as_u64() > Some(5)
            })
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

/// A loopback port for the demo's robot, free over UDP, and the one after
/// it, where its camera sends, taken here over TCP.
#[cfg(target_os = "linux")]
fn robot_and_camera_ports() -> (u16, TcpListener) {
    loop {
        let camera = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = camera.local_addr().unwrap().port();
        if UdpSocket::bind(("127.0.0.1", port - 1)).is_ok() {
            return (port - 1, camera);
        }
    }
}

/// The bytes the loopback TCP connection from `port` holds unsent, as the
/// system lists its connections (`tx_queue`).
#[cfg(target_os = "linux")]
fn unsent(port: u16) -> u64 {
    let connections = std::fs::read_to_string("/proc/net/tcp").unwrap();
    connections
        .lines()
        .skip(1)
        .find_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let local = u16::from_str_radix(fields[1].split(':').nth(1)?, 16).ok()?;
            let tx_queue = fields[4].split(':').next()?;
            (local == port).then(|| u64::from_str_radix(tx_queue, 16).unwrap())
        })
        .unwrap_or(0)
}

#[test]
#[cfg(target_os = "linux")]
fn the_demo_exits_when_stopped_while_its_camera_is_held_up_by_a_receiver_that_doesnt_read() {
    let folder: PathBuf = std::env::temp_dir().join(format!("kdemo-held-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    // A receiver that takes the camera's connection and never reads it, as
    // a stalled capture would: once the socket's buffers are full, every
    // write the camera makes waits.
    let (robot_port, receiver) = robot_and_camera_ports();
    let [port, stream_port] = free_tcp_ports();
    let child = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["demo", "--robot-port", &robot_port.to_string()])
        .args(["--rate", "100", "--port", &port])
        .args(["--viewer-stream-port", &stream_port])
        .args(["--capture-preflight", "warn"])
        .arg("--data-dir")
        .arg(&folder)
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut demo = Demo(Some(child));
    let deadline = Instant::now() + Duration::from_mins(1);
    receiver.set_nonblocking(true).unwrap();
    let connection = loop {
        if let Ok((connection, _)) = receiver.accept() {
            break connection;
        }
        assert!(Instant::now() < deadline, "the camera never connected");
        std::thread::sleep(Duration::from_millis(50));
    };
    // Held up once neither what waits here to be read nor what waits on
    // the camera's side to be sent grows for longer than the camera takes
    // between two frames: the camera's own buffer goes on filling for a
    // while after this side's is full, and only then does its write wait.
    let camera = connection.peer_addr().unwrap().port();
    connection.set_nonblocking(true).unwrap();
    let mut waiting = vec![0; 64 << 20];
    let mut queued = (0, 0);
    let mut since = Instant::now();
    while since.elapsed() < Duration::from_millis(500) {
        assert!(
            Instant::now() < deadline,
            "the camera never filled the connection"
        );
        std::thread::sleep(Duration::from_millis(50));
        let now = (
            connection.peek(&mut waiting).unwrap_or(queued.0),
            unsent(camera),
        );
        if now != queued {
            queued = now;
            since = Instant::now();
        }
    }
    assert!(queued.0 > 0 && queued.1 > 0, "the camera sent nothing");

    let mut child = demo.0.take().unwrap();
    assert!(
        child.try_wait().unwrap().is_none(),
        "the demo ended before it was stopped"
    );
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let stopping = Instant::now();
    let exited = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if stopping.elapsed() > Duration::from_secs(5) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_dir_all(&folder);
    assert!(
        exited,
        "the demo hadn't exited {:?} after SIGTERM",
        stopping.elapsed()
    );
}
