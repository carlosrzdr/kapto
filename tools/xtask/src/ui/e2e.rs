// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask ui e2e` and `cargo xtask ui screenshots`: the web UI in a
//! real browser, against a real Kapto.
//!
//! Kapto runs here as `kapto demo` on free ports with its own data folder.
//! Playwright and its browser run in Docker from the image pinned below,
//! on the host's network so they can reach it. Both run in two steps: with
//! everything up, then, once the capture process has been killed (and isn't
//! started again for an hour), what the page says without it. The flows
//! check the page; the screenshots save a picture of each tab and state
//! into `target/screenshots/`, for a pull request's description.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use super::user;

/// Playwright v1.63.0 with its browsers; the same version as
/// `@playwright/test` in `ui/package.json`.
pub(crate) const PLAYWRIGHT_IMAGE: &str = "mcr.microsoft.com/playwright:v1.63.0-noble@sha256:eff16c30e6f3f4af0a03fa4b706120d5e9b0891c344a27d64559aff5900a4a27";

/// How long Kapto may take to start.
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// What to run against the demo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Run {
    /// The flows, which check the page.
    Flows,
    /// The screenshots, saved into `target/screenshots/`.
    Screenshots,
}

impl Run {
    /// The spec files of the two steps: with capture up, then without it.
    fn specs(self) -> [&'static str; 2] {
        match self {
            Self::Flows => ["recording.spec.ts", "capture-stopped.spec.ts"],
            Self::Screenshots => ["screens.spec.ts", "screens-without-capture.spec.ts"],
        }
    }
}

pub(crate) fn run(root: &Path, what: Run) -> ExitCode {
    let result = if what == Run::Screenshots {
        screenshots(root)
    } else {
        against_demo(root, what, None)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Takes the screenshots into a fresh `target/screenshots/`.
fn screenshots(root: &Path) -> Result<(), String> {
    let folder = root.join("target/screenshots");
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("can't create {}: {error}", folder.display()))?;
    against_demo(root, Run::Screenshots, Some(&folder))?;
    let mut taken: Vec<String> = std::fs::read_dir(&folder)
        .map_err(|error| format!("can't read {}: {error}", folder.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    taken.sort();
    eprintln!("{} screenshots in {}:", taken.len(), folder.display());
    for name in taken {
        eprintln!("  {name}");
    }
    Ok(())
}

/// Builds the UI and Kapto, starts the demo and runs `what`'s two steps,
/// saving screenshots into `screenshots` if given.
fn against_demo(root: &Path, what: Run, screenshots: Option<&Path>) -> Result<(), String> {
    if super::run(root, super::Task::Build) != ExitCode::SUCCESS {
        return Err("the UI didn't build".into());
    }
    eprintln!("Building kapto with the UI just built");
    let built = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["build", "--bin", "kapto"])
        .status()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if !built.success() {
        return Err("kapto didn't build".into());
    }

    let folder = std::env::temp_dir().join(format!("kapto-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let config = sample_config(&folder)?;
    let mut kapto = Kapto::start(&root.join("target/debug/kapto"), &folder, &config)?;
    let [up, without_capture] = what.specs();
    let result = (|| {
        playwright(root, &kapto.url, up, &docker_args(screenshots))?;
        eprintln!("Killing the capture process (pid {})", kapto.capture);
        kill("-KILL", kapto.capture)?;
        playwright(root, &kapto.url, without_capture, &docker_args(screenshots))
    })();
    kapto.stop();
    let _ = std::fs::remove_dir_all(&folder);
    result
}

/// A configuration folder of the flows' own, so they never read the
/// user's: two upload destinations, a profile for the demo's robot and one
/// for another, and a task. Returns its `kapto.toml`.
fn sample_config(folder: &Path) -> Result<PathBuf, String> {
    let config = folder.join("config");
    // A folder episodes go up to, and one that can never be made, under a
    // file, so the page has an upload that fails to show.
    let destinations = format!(
        "[[upload.destination]]\nname = \"archive\"\nurl = \"file://{archive}\"\n\n\
         [[upload.destination]]\nname = \"nas\"\nurl = \"file://{nas}\"\n",
        archive = folder.join("archive").display(),
        nas = config.join("kapto.toml/nas").display(),
    );
    let files = [
        ("kapto.toml", destinations.as_str()),
        (
            "profiles/lab_ana.toml",
            "package = \"arm7\"\noperator = \"Ana\"\nmode = \"teleoperated\"\ntags = [\"shift-a\"]\n",
        ),
        (
            "profiles/cobot_day.toml",
            "package = \"cobot\"\noperator = \"Ben\"\n",
        ),
        (
            "tasks/pick_and_place.toml",
            "name = \"Pick and place\"\nprompt = \"Pick up the red cube and put it in the bin\"\n\
             subtasks = [\"reach\", \"grasp\", \"lift\", \"place\"]\n",
        ),
    ];
    for (path, contents) in files {
        let path = config.join(path);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, contents));
        written.map_err(|error| format!("can't write {}: {error}", path.display()))?;
    }
    Ok(config.join("kapto.toml"))
}

/// `kapto demo` running for the flows.
struct Kapto {
    child: Child,
    url: String,
    capture: u32,
}

impl Kapto {
    fn start(binary: &Path, folder: &Path, config: &Path) -> Result<Self, String> {
        let port = free_tcp_port()?;
        let mut child = Command::new(binary)
            .arg("demo")
            .args([
                "--rate",
                "200",
                "--robot-port",
                &free_udp_port()?.to_string(),
            ])
            .args(["--port", &port.to_string()])
            .args(["--viewer-stream-port", &free_tcp_port()?.to_string()])
            .arg("--data-dir")
            .arg(folder)
            .arg("--config")
            .arg(config)
            // Once killed, capture stays down for the second step.
            .args([
                "--supervisor-restart-delay",
                "3600",
                "--supervisor-restart-max-delay",
                "3600",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("can't start kapto: {error}"))?;
        let stderr = child.stderr.take();
        // Stopped when dropped, so a start that fails leaves nothing
        // running.
        let mut kapto = Self {
            child,
            url: format!("http://localhost:{port}"),
            capture: 0,
        };
        let stderr = stderr.ok_or("kapto's output is missing")?;
        let (lines, received) = mpsc::channel();
        // Echoed, so a failing flow comes with Kapto's side of it.
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("  kapto | {line}");
                let _ = lines.send(line);
            }
        });
        let mut capture = None;
        loop {
            // Ended before it said it runs: why is in its output above.
            if let Ok(Some(status)) = kapto.child.try_wait() {
                return Err(format!("kapto ended as it started ({status})"));
            }
            let line = match received.recv_timeout(START_TIMEOUT) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Err("kapto didn't start".to_owned()),
            };
            if let Some(pid) = line
                .strip_prefix("[serve] started capture (pid ")
                .and_then(|rest| rest.strip_suffix(')'))
            {
                capture = pid.parse().ok();
            }
            if line.starts_with("Kapto is running: ") {
                break;
            }
        }
        kapto.capture = capture.ok_or("kapto didn't say which process is capture")?;
        Ok(kapto)
    }

    /// Stops it as Ctrl-C would, so it stops its children too, unless it
    /// has ended already.
    fn stop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill("-INT", self.child.id());
        }
        let _ = self.child.wait();
    }
}

impl Drop for Kapto {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What `docker run` needs for the screenshots: their folder mounted where
/// the spec saves them, if given.
fn docker_args(screenshots: Option<&Path>) -> Vec<String> {
    screenshots.map_or_else(Vec::new, |folder| {
        vec![
            "-e".to_owned(),
            "KAPTO_SCREENSHOTS=/screenshots".to_owned(),
            "-v".to_owned(),
            format!("{}:/screenshots", folder.display()),
        ]
    })
}

/// Runs one spec file in the Playwright image against Kapto at `url`, with
/// `extra` arguments to `docker run` (folders, devices, settings).
pub(crate) fn playwright(
    root: &Path,
    url: &str,
    spec: &str,
    extra: &[String],
) -> Result<(), String> {
    eprintln!("Running {spec} in Docker ({PLAYWRIGHT_IMAGE})");
    let ui: PathBuf = root.join("ui");
    let mut docker = Command::new("docker");
    docker
        .args(["run", "--rm", "--network", "host", "--ipc", "host"])
        .args(["--user", &user(), "-e", "HOME=/tmp", "-e", "CI=1"])
        .args(["-e", "npm_config_cache=/ui/.npm-cache"])
        .args(["-e", &format!("KAPTO_URL={url}")])
        .args(extra);
    let status = docker
        .arg("-v")
        .arg(format!("{}:/ui", ui.display()))
        .args(["-w", "/ui", PLAYWRIGHT_IMAGE, "sh", "-c"])
        .arg(format!("{} {spec}", super::Task::E2e.script()))
        .status()
        .map_err(|error| format!("can't run docker: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{spec} failed ({status})"))
    }
}

fn kill(signal: &str, pid: u32) -> Result<(), String> {
    let status = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()
        .map_err(|error| format!("can't run kill: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("can't signal process {pid}"))
    }
}

fn free_tcp_port() -> Result<u16, String> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| format!("no free port: {error}"))
}

fn free_udp_port() -> Result<u16, String> {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .map(|address| address.port())
        .map_err(|error| format!("no free port: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_playwright_image_matches_the_package_the_specs_use() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let package: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("ui/package.json")).unwrap())
                .unwrap();
        let version = package["devDependencies"]["@playwright/test"]
            .as_str()
            .unwrap();
        assert!(
            PLAYWRIGHT_IMAGE.contains(&format!(":v{version}-")),
            "ui/package.json has @playwright/test {version}, the image is {PLAYWRIGHT_IMAGE}"
        );
    }
}
