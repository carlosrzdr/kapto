// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask storage`: the tests that upload to S3-compatible storage,
//! against the RustFS server of `compose.yaml`'s `storage` profile, the one
//! the guide to trying uploads starts, with its bucket `kapto`. It runs as
//! a Compose project of its own, on a free port of this machine only, keeps
//! nothing once stopped, and is stopped however the tests end. The tests
//! are ignored by the other checks and find the server, and its
//! credentials, in their environment.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// The server's credentials, as `compose.yaml` gives them; they guard
/// nothing but the test server.
const ACCESS_KEY: &str = "kapto";
const SECRET_KEY: &str = "kapto-storage";

/// Every test that needs the server: each has `s3` in its name.
const TESTS: [&str; 9] = [
    "nextest",
    "run",
    "-p",
    "kapto-upload",
    "-p",
    "kapto-cli",
    "--run-ignored",
    "only",
    "-E",
];

pub(crate) fn run(root: &Path) -> ExitCode {
    match with_server(root, |address| test(root, address)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("error: {problem}");
            ExitCode::FAILURE
        }
    }
}

/// Starts the server, runs `using` with its address once it answers, and
/// stops it, removing what it kept, however `using` ends.
pub(crate) fn with_server<T>(
    root: &Path,
    using: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    let project = format!("kapto-storage-{}", std::process::id());
    let used = free_port().and_then(|port| {
        eprintln!("Starting RustFS from compose.yaml on port {port}");
        compose(root, &project, &port, &["run", "--rm", "rustfs-bucket"])?;
        let address = format!("127.0.0.1:{port}");
        ready(&address)?;
        using(&address)
    });
    let stopped = compose(
        root,
        &project,
        "0",
        &["--profile", "storage", "down", "--volumes"],
    );
    let used = used?;
    stopped.map(|()| used)
}

/// Points `command` at the server on `address`, with its credentials, and
/// away from any the machine has of its own.
pub(crate) fn reach(command: &mut Command, address: &str) {
    command
        .env("KAPTO_TEST_S3_ENDPOINT", format!("http://{address}"))
        .env("AWS_ACCESS_KEY_ID", ACCESS_KEY)
        .env("AWS_SECRET_ACCESS_KEY", SECRET_KEY)
        .env("AWS_REGION", "us-east-1")
        // The machine's own provider settings would point the tests
        // elsewhere.
        .env_remove("AWS_ENDPOINT")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("AWS_PROFILE");
}

/// Runs `docker compose` with `args` as the project `project`, the server
/// published on `port`.
fn compose(root: &Path, project: &str, port: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new("docker")
        .args(["compose", "--project-name", project])
        .args(args)
        .current_dir(root)
        .env("KAPTO_STORAGE_PORT", port)
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

fn free_port() -> Result<String, String> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port().to_string())
        .map_err(|error| format!("no free port: {error}"))
}

/// Waits for the server to answer its health check.
fn ready(address: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if healthy(address) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("RustFS didn't answer on {address} within a minute"))
}

fn healthy(address: &str) -> bool {
    let Ok(mut stream) = TcpStream::connect(address) else {
        return false;
    };
    let request = format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    let mut answer = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut answer).is_ok()
        && answer.starts_with("HTTP/1.1 200")
}

fn test(root: &Path, address: &str) -> Result<(), String> {
    eprintln!("Testing uploads to S3-compatible storage on {address}");
    let mut command = Command::new("cargo");
    command.args(TESTS).arg("test(s3)").current_dir(root);
    reach(&mut command, address);
    let status = command
        .status()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the storage tests failed ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credentials_are_the_ones_the_compose_file_gives_the_server() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let compose = std::fs::read_to_string(root.join("compose.yaml")).unwrap();
        assert!(compose.contains(&format!("RUSTFS_ACCESS_KEY: {ACCESS_KEY}\n")));
        assert!(compose.contains(&format!("RUSTFS_SECRET_KEY: {SECRET_KEY}\n")));
        assert!(compose.contains(&format!("--user {ACCESS_KEY}:{SECRET_KEY}")));
    }
}
