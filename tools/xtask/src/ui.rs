// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask ui`: builds, checks and tests the web UI in `ui/` with
//! Node.js running in Docker, so the host needs nothing but Docker. The Node
//! image is the one the Dockerfile pins, so the UI is built the same way
//! here and in the image.

use std::path::Path;
use std::process::{Command, ExitCode};

pub(crate) mod e2e;

/// What to do with the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Task {
    /// Install the pinned dependencies and update the lockfile.
    Install,
    /// Build it into `ui/dist`, which `cargo build` then embeds.
    Build,
    /// Type-check it.
    Check,
    /// Run its component tests.
    Test,
    /// Run its flows in a browser against a running Kapto (Playwright).
    E2e,
    /// Take a screenshot of each tab and state against a running Kapto,
    /// into `target/screenshots/`, for a pull request's description.
    Screenshots,
}

impl Task {
    /// What the user is told before it starts.
    fn announce(self) -> &'static str {
        match self {
            Self::Install => "Installing the UI's dependencies",
            Self::Build => "Building the UI into ui/dist",
            Self::Check => "Type-checking the UI",
            Self::Test => "Running the UI's tests",
            Self::E2e => "Running the UI's flows in a browser",
            Self::Screenshots => "Taking screenshots of the UI in a browser",
        }
    }

    /// The shell command run in the Node container. Everything but
    /// `install` installs exactly what the lockfile says first.
    fn script(self) -> &'static str {
        match self {
            Self::Install => "npm install --no-audit --no-fund",
            Self::Build => "npm ci --no-audit --no-fund && npm run build",
            Self::Check => "npm ci --no-audit --no-fund && npm run check",
            Self::Test => "npm ci --no-audit --no-fund && npm test",
            // In the Playwright image, given the spec to run.
            Self::E2e | Self::Screenshots => "npm ci --no-audit --no-fund && npm run e2e --",
        }
    }
}

pub(crate) fn run(root: &Path, task: Task) -> ExitCode {
    match task {
        Task::E2e => return e2e::run(root, e2e::Run::Flows),
        Task::Screenshots => return e2e::run(root, e2e::Run::Screenshots),
        Task::Install | Task::Build | Task::Check | Task::Test => {}
    }
    let Some(image) = node_image(root) else {
        eprintln!("error: can't find `ARG NODE_IMAGE=` in docker/Dockerfile");
        return ExitCode::FAILURE;
    };
    let ui = root.join("ui");
    eprintln!(
        "{} in Docker ({image}). The first run downloads the image and the dependencies, which takes a minute.",
        task.announce()
    );
    let status = Command::new("docker")
        .args(["run", "--rm", "--user", &user(), "-e", "HOME=/tmp"])
        .args(["-e", "npm_config_cache=/ui/.npm-cache", "-e", "CI=1"])
        .arg("-v")
        .arg(format!("{}:/ui", ui.display()))
        .args(["-w", "/ui", &image, "sh", "-c", task.script()])
        .status();
    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("error: the UI {task:?} step failed ({status})");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("error: can't run docker: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The Node image pinned in the Dockerfile.
fn node_image(root: &Path) -> Option<String> {
    let dockerfile = std::fs::read_to_string(root.join("docker/Dockerfile")).ok()?;
    dockerfile
        .lines()
        .find_map(|line| line.strip_prefix("ARG NODE_IMAGE="))
        .map(str::to_owned)
}

/// The host user, so files written into `ui/` belong to it.
pub(crate) fn user() -> String {
    let id = |flag| {
        Command::new("id")
            .arg(flag)
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map_or_else(|| "1000".to_owned(), |id| id.trim().to_owned())
    };
    format!("{}:{}", id("-u"), id("-g"))
}
