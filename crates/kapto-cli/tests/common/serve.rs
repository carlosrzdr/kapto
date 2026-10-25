// SPDX-License-Identifier: Apache-2.0

//! A `kapto serve` started for a test, on free ports and its own folder.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kapto_capture::control::ControlClient;

use super::{free_tcp_ports, free_udp_port, root, stop_gracefully};

/// The arm's streams.toml on a free UDP port, so tests can run together.
pub(crate) fn streams(folder: &Path) -> PathBuf {
    let port = free_udp_port();
    let original = std::fs::read_to_string(root().join("packages/arm7/streams.toml")).unwrap();
    let path = folder.join("streams.toml");
    std::fs::write(
        &path,
        original.replace("127.0.0.1:9000", &format!("127.0.0.1:{port}")),
    )
    .unwrap();
    path
}

/// ROS 2's environment, where the tests run in it, so serve finds its
/// libraries, message types and middleware as the test does; nothing on a
/// machine without ROS 2.
fn ros2_environment() -> impl Iterator<Item = (String, String)> {
    std::env::vars().filter(|(name, _)| {
        ["ROS_", "RMW_", "AMENT_", "CYCLONEDDS_", "ZENOH_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
            || name == "LD_LIBRARY_PATH"
    })
}

/// A running `kapto serve` and everything it printed so far.
pub(crate) struct Serve {
    pub(crate) child: Option<Child>,
    folder: PathBuf,
    pub(crate) lines: Arc<Mutex<Vec<String>>>,
    api_port: String,
}

impl Serve {
    pub(crate) fn start(folder: &Path, mapping: &Path) -> Self {
        Self::start_with(folder, mapping, &[])
    }

    /// The same, with more flags.
    pub(crate) fn start_with(folder: &Path, mapping: &Path, flags: &[&str]) -> Self {
        let robot = root().join("packages/arm7/urdf/arm7.urdf");
        Self::start_robot(folder, &robot, &streams(folder), mapping, flags)
    }

    /// The same, with another robot's files.
    pub(crate) fn start_robot(
        folder: &Path,
        robot: &Path,
        streams: &Path,
        mapping: &Path,
        flags: &[&str],
    ) -> Self {
        let [api_port, stream_port] = free_tcp_ports();
        let mut child = Command::new(env!("CARGO_BIN_EXE_kapto"))
            // A job of its own, as a shell starts it: Ctrl-C at its
            // terminal is SIGINT to its whole group.
            .process_group(0)
            .arg("serve")
            .arg("--robot")
            .arg(robot)
            .arg("--streams")
            .arg(streams)
            .arg("--mapping")
            .arg(mapping)
            .args(["--port", &api_port])
            .args(["--viewer-stream-port", &stream_port])
            .arg("--data-dir")
            .arg(folder.join("data"))
            // The robot is silent until a test sends: pre-flight's failed
            // checks mustn't refuse its episodes.
            .args(["--capture-preflight", "warn"])
            .args(flags)
            .env_clear()
            .envs(ros2_environment())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let stderr = BufReader::new(child.stderr.take().unwrap());
        std::thread::spawn({
            let lines = Arc::clone(&lines);
            move || {
                for line in stderr.lines() {
                    lines.lock().unwrap().push(line.unwrap());
                }
            }
        });
        Self {
            child: Some(child),
            folder: folder.to_path_buf(),
            lines,
            api_port,
        }
    }

    /// Waits until a line starting with `start` has been printed `times`
    /// times, and returns the last one.
    pub(crate) fn wait_for(&self, start: &str, times: usize) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let lines = self.lines.lock().unwrap();
            let matching: Vec<&String> = lines
                .iter()
                .filter(|line| line.starts_with(start))
                .collect();
            if matching.len() >= times {
                return matching[times - 1].clone();
            }
            assert!(Instant::now() < deadline, "no {start:?} in {lines:#?}");
            drop(lines);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn child_pid(&self, name: &str, times: usize) -> u32 {
        let line = self.wait_for(&format!("[serve] started {name} (pid "), times);
        line.trim_end_matches(')')
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    pub(crate) fn control(&self) -> ControlClient {
        let socket = self.folder.join("data/capture.sock");
        ControlClient::connect(&socket, Duration::from_secs(30)).unwrap()
    }

    /// One HTTP request to the API: the status code and the body.
    pub(crate) fn http(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        super::http(&self.api_port, method, path, body.unwrap_or_default())
    }

    /// Where the API answers, e.g. `http://127.0.0.1:41234/api`.
    pub(crate) fn api(&self) -> String {
        format!("http://127.0.0.1:{}/api", self.api_port)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }
}

impl Drop for Serve {
    /// Stops serve the way an operator would, so it stops its children too:
    /// killing it outright would leave capture and the bridge running,
    /// since they are made to survive their supervisor.
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            stop_gracefully(child);
        }
    }
}
