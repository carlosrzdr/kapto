// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask ros2`: the `ros2` feature linted and tested in the ROS 2
//! image, since building it needs a ROS 2 installation and libclang that the
//! host checks don't. The `ros2-dev` stage of the Dockerfile is built, then
//! the repository is mounted into it, with a build folder of its own in
//! `target/ros2/` (another C library, other features). The tests run once
//! per middleware the image has, each in a container of its own, so one
//! middleware's nodes never see another's.

use std::path::Path;
use std::process::{Command, ExitCode};

use clap::ValueEnum;

use crate::ui::user;

pub(crate) const IMAGE: &str = "kapto-ros2-dev";

/// ROS 2's environment, as the image's entry point sets it up for Kapto.
const SETUP: &str = ". /opt/ros/$ROS_DISTRO/setup.sh";

/// Lints the crates with ROS 2 code.
const LINTS: &str = "cargo clippy --locked -p kapto-transport -p kapto-cli -p kapto-loadgen \
    --all-targets --features kapto-cli/ros2,kapto-loadgen/ros2 -- --deny warnings";

/// The ROS 2 tests, against real nodes, and reading types from the
/// installation, which the host checks skip; and the synthetic robot
/// publishing on ROS 2 topics.
const TESTS: &str = "cargo test --locked -p kapto-transport --features ros2 --test ros2 \
    && cargo test --locked -p kapto-cli --features ros2 --test topics --test ros2_record \
    --test ros2_cameras \
    --test ros2_serve \
    && cargo test --locked -p kapto-loadgen --features ros2 --lib --test ros2 \
    && cargo test --locked -p kapto-config --test ros2_installation -- --ignored";

/// A middleware the image has.
#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Middleware {
    #[value(name = "rmw_fastrtps_cpp")]
    Fastrtps,
    #[value(name = "rmw_cyclonedds_cpp")]
    Cyclonedds,
    #[value(name = "rmw_zenoh_cpp")]
    Zenoh,
}

impl Middleware {
    const ALL: [Self; 3] = [Self::Fastrtps, Self::Cyclonedds, Self::Zenoh];

    /// Its name, as `RMW_IMPLEMENTATION` takes it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fastrtps => "rmw_fastrtps_cpp",
            Self::Cyclonedds => "rmw_cyclonedds_cpp",
            Self::Zenoh => "rmw_zenoh_cpp",
        }
    }

    /// What has to run before its nodes find each other: Zenoh's router;
    /// the DDS ones discover peers by themselves.
    const fn before(self) -> &'static str {
        match self {
            Self::Fastrtps | Self::Cyclonedds => "",
            Self::Zenoh => {
                "/opt/ros/$ROS_DISTRO/lib/rmw_zenoh_cpp/rmw_zenohd >/dev/null 2>&1 & sleep 1 &&"
            }
        }
    }
}

pub(crate) fn run(root: &Path) -> ExitCode {
    if let Err(problem) = build(root) {
        eprintln!("error: {problem}");
        return ExitCode::FAILURE;
    }
    eprintln!("Linting the ros2 feature in {IMAGE}");
    if let Err(problem) = in_image(root, "rmw_fastrtps_cpp", &format!("{SETUP} && {LINTS}")) {
        eprintln!("error: the ros2 feature's lints failed ({problem})");
        return ExitCode::FAILURE;
    }
    let mut failed = Vec::new();
    for middleware in Middleware::ALL {
        let name = middleware.name();
        eprintln!("Testing the ros2 feature over {name}");
        if let Err(problem) = run_in(root, middleware, TESTS) {
            eprintln!("error: the tests over {name} failed ({problem})");
            failed.push(name);
        }
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("error: the ros2 feature failed over {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// Builds the image from the working tree.
pub(crate) fn build(root: &Path) -> Result<(), String> {
    eprintln!("Building {IMAGE} (the Dockerfile's ros2-dev stage)");
    let built = Command::new("docker")
        .args(["build", "--target", "ros2-dev", "--tag", IMAGE, "--file"])
        .arg(root.join("docker/Dockerfile"))
        .arg(root)
        .status();
    if built.is_ok_and(|status| status.success()) {
        Ok(())
    } else {
        Err("the ROS 2 image didn't build".to_owned())
    }
}

/// Runs `script` in the image over `middleware`, with ROS 2 set up and
/// what the middleware needs started first.
pub(crate) fn run_in(root: &Path, middleware: Middleware, script: &str) -> Result<(), String> {
    let script = format!("{SETUP} && {} {script}", middleware.before());
    in_image(root, middleware.name(), &script)
}

/// Runs `script` in the image with the repository mounted, over `middleware`.
fn in_image(root: &Path, middleware: &str, script: &str) -> Result<(), String> {
    let status = Command::new("docker")
        .args(["run", "--rm", "--user", &user(), "-e", "HOME=/tmp"])
        .args(["-e", "CARGO_HOME=/src/target/ros2/cargo"])
        .args(["-e", "CARGO_TARGET_DIR=/src/target/ros2"])
        .args(["-e", &format!("RMW_IMPLEMENTATION={middleware}")])
        .arg("-v")
        .arg(format!("{}:/src", root.display()))
        .args(["-w", "/src", IMAGE, "bash", "-c", script])
        .status()
        .map_err(|error| format!("can't run docker: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(status.to_string())
    }
}
