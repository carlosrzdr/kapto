// SPDX-License-Identifier: Apache-2.0

//! The demo as a new user starts it: `docker compose --profile demo up`,
//! checked end to end. A page opens it in a browser, sees the arm move,
//! records an episode and sees it uploaded; then the episode is looked for
//! in the bucket, `_SUCCESS` last. It runs as a Compose project of its own,
//! on free ports, and is taken down with its volumes however it ends.

use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use crate::ui::e2e::playwright;

pub(crate) fn run(root: &Path) -> ExitCode {
    let project = format!("kapto-demo-{}", std::process::id());
    let ports = (free_port(), free_port(), free_port());
    let checked = match &ports {
        (Ok(page), Ok(stream), Ok(storage)) => {
            let compose = Compose {
                root,
                project: &project,
                ports: [page.as_str(), stream.as_str(), storage.as_str()],
            };
            let result = check(root, &compose, page);
            let down = compose.run(&["--profile", "demo", "down", "--volumes"]);
            result.and(down)
        }
        _ => Err("no free port".to_owned()),
    };
    match checked {
        Ok(()) => {
            eprintln!("The demo started by Compose recorded an episode and uploaded it.");
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("error: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn check(root: &Path, compose: &Compose<'_>, page: &str) -> Result<(), String> {
    eprintln!("Starting the demo with `docker compose --profile demo up --build`");
    compose.run(&["--profile", "demo", "up", "--build", "--detach"])?;
    let url = format!("http://127.0.0.1:{page}");
    answers(&format!("127.0.0.1:{page}"))?;
    playwright(root, &url, "compose-demo.spec.ts", &[])?;
    let listing = compose.output(&[
        "exec",
        "-T",
        "rustfs",
        "curl",
        "--silent",
        "--aws-sigv4",
        "aws:amz:us-east-1:s3",
        "--user",
        "kapto:kapto-storage",
        "http://localhost:9000/kapto?list-type=2",
    ])?;
    if listing.contains("/_SUCCESS</Key>") {
        Ok(())
    } else {
        Err(format!(
            "no episode with its _SUCCESS in the bucket: {listing}"
        ))
    }
}

/// `docker compose` as the project `project`, the demo's page, its stream
/// and the storage published on `ports`.
struct Compose<'a> {
    root: &'a Path,
    project: &'a str,
    ports: [&'a str; 3],
}

impl Compose<'_> {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("docker");
        command
            .args(["compose", "--project-name", self.project])
            .args(args)
            .current_dir(self.root)
            .env("KAPTO_DEMO_PORT", self.ports[0])
            .env("KAPTO_DEMO_STREAM_PORT", self.ports[1])
            .env("KAPTO_STORAGE_PORT", self.ports[2]);
        command
    }

    fn run(&self, args: &[&str]) -> Result<(), String> {
        let status = self
            .command(args)
            .status()
            .map_err(|error| format!("can't run docker: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "`docker compose {}` failed ({status}); is Docker running?",
                args.join(" ")
            ))
        }
    }

    fn output(&self, args: &[&str]) -> Result<String, String> {
        let output = self
            .command(args)
            .output()
            .map_err(|error| format!("can't run docker: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(format!(
                "`docker compose {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

/// Waits for the demo's API to take connections, up to two minutes.
fn answers(address: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(address).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "the demo didn't answer on {address} within two minutes"
    ))
}

fn free_port() -> Result<String, String> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port().to_string())
        .map_err(|error| format!("no free port: {error}"))
}
