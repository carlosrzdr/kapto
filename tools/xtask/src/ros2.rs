// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask ros2`: the `ros2` feature linted and tested in the ROS 2
//! image, since building it needs a ROS 2 installation and libclang that the
//! host checks don't. The `ros2-dev` stage of the Dockerfile is built, then
//! the repository is mounted into it, with a build folder of its own in
//! `target/ros2/` (another C library, other features). The tests run once
//! per middleware the image has, each in a container of its own, so one
//! middleware's nodes never see another's, with the tests' own message
//! package built for the image and mounted as an overlay, as a robot's is;
//! and an overlay without its setup script is checked to stop the image's
//! entry point.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::ValueEnum;

use crate::ui::user;

pub(crate) const IMAGE: &str = "kapto-ros2-dev";

/// The image's entry point, which sets up ROS 2's environment and the
/// overlays' over it, as for Kapto, then runs the command it is given.
const ENTRYPOINT: &str = "/usr/local/bin/kapto-entrypoint";

/// The tests' own message package, a type no installation has.
const OVERLAY_PACKAGE: &str = "testdata/ros2/kapto_test_msgs";

/// Where the tests mount it, as a robot's built package is mounted.
const OVERLAY_MOUNT: &str = "/overlays/kapto_test_msgs";

/// Lints the crates with ROS 2 code.
const LINTS: &str = "cargo clippy --locked -p kapto-transport -p kapto-cli -p kapto-loadgen \
    --all-targets --features kapto-cli/ros2,kapto-loadgen/ros2 -- --deny warnings";

/// The ROS 2 tests, against real nodes, and reading types from the
/// installation, which the host checks skip; and the synthetic robot
/// publishing on ROS 2 topics.
const TESTS: &str = "cargo test --locked -p kapto-transport --features ros2 --test ros2 \
    && cargo test --locked -p kapto-cli --features ros2 --test topics --test ros2_record \
    --test ros2_cameras --test ros2_overlay \
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
            // Ready once its port answers, which takes from a few
            // milliseconds to a second; never more than five.
            Self::Zenoh => {
                "/opt/ros/$ROS_DISTRO/lib/rmw_zenoh_cpp/rmw_zenohd >/dev/null 2>&1 & \
                 for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/7447) 2>/dev/null && break; \
                 sleep 0.1; done &&"
            }
        }
    }
}

pub(crate) fn run(root: &Path) -> ExitCode {
    if let Err(problem) = build(root)
        .and_then(|()| overlay_without_setup_stops(root))
        .and_then(|()| build_overlay(root))
    {
        eprintln!("error: {problem}");
        return ExitCode::FAILURE;
    }
    eprintln!("Linting the ros2 feature in {IMAGE}");
    if let Err(problem) = in_image(root, "rmw_fastrtps_cpp", LINTS, &[]) {
        eprintln!("error: the ros2 feature's lints failed ({problem})");
        return ExitCode::FAILURE;
    }
    let mut failed = Vec::new();
    for middleware in Middleware::ALL {
        let name = middleware.name();
        eprintln!("Testing the ros2 feature over {name}");
        let script = format!("{} {TESTS}", middleware.before());
        let overlay = [format!("{}:{OVERLAY_MOUNT}:ro", overlay(root).display())];
        if let Err(problem) = in_image(root, name, &script, &overlay) {
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

/// The tests' message package as colcon installs it, built in the image:
/// the folder mounted as its overlay.
fn overlay(root: &Path) -> PathBuf {
    root.join("target/ros2/overlay/install")
}

/// Builds the tests' message package with colcon for the image's ROS 2,
/// as a robot's own is built to be mounted.
fn build_overlay(root: &Path) -> Result<(), String> {
    eprintln!("Building {OVERLAY_PACKAGE} for {IMAGE}");
    let script = format!(
        "colcon --log-base target/ros2/overlay/log build --base-paths {OVERLAY_PACKAGE} \
         --build-base target/ros2/overlay/build --install-base target/ros2/overlay/install"
    );
    in_image(root, "rmw_fastrtps_cpp", &script, &[])
        .map_err(|problem| format!("{OVERLAY_PACKAGE} didn't build ({problem})"))
}

/// An empty folder mounted as an overlay stops the entry point before the
/// command it was given, naming the folder.
fn overlay_without_setup_stops(root: &Path) -> Result<(), String> {
    eprintln!("Checking that an overlay without setup.sh stops {IMAGE}'s entry point");
    let empty = root.join("target/ros2/overlay/without-setup");
    std::fs::create_dir_all(&empty)
        .map_err(|error| format!("can't create {}: {error}", empty.display()))?;
    let output = Command::new("docker")
        .args(["run", "--rm", "--user", &user(), "-v"])
        .arg(format!("{}:/overlays/robot_msgs:ro", empty.display()))
        .args([IMAGE, ENTRYPOINT, "echo", "started"])
        .output()
        .map_err(|error| format!("can't run docker: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && !stdout.contains("started")
        && stderr.contains("/overlays/robot_msgs has no setup.sh")
    {
        Ok(())
    } else {
        Err(format!(
            "an overlay without setup.sh didn't stop the entry point ({}): {stdout}{stderr}",
            output.status
        ))
    }
}

/// Runs `script` in the image over `middleware`, with ROS 2 set up and
/// what the middleware needs started first.
pub(crate) fn run_in(root: &Path, middleware: Middleware, script: &str) -> Result<(), String> {
    let script = format!("{} {script}", middleware.before());
    in_image(root, middleware.name(), &script, &[])
}

/// Runs `script` in the image through its entry point, with the repository
/// mounted and the `mounts` given (`-v` values), over `middleware`.
fn in_image(root: &Path, middleware: &str, script: &str, mounts: &[String]) -> Result<(), String> {
    let status = Command::new("docker")
        .args(["run", "--rm", "--user", &user(), "-e", "HOME=/tmp"])
        .args(["-e", "CARGO_HOME=/src/target/ros2/cargo"])
        .args(["-e", "CARGO_TARGET_DIR=/src/target/ros2"])
        .args(["-e", &format!("RMW_IMPLEMENTATION={middleware}")])
        // How hard the build may work the machine, as on the host.
        .args(
            [
                "CARGO_BUILD_JOBS",
                "CARGO_INCREMENTAL",
                "NEXTEST_TEST_THREADS",
            ]
            .into_iter()
            .filter(|key| std::env::var_os(key).is_some())
            .flat_map(|key| ["-e", key]),
        )
        .arg("-v")
        .arg(format!("{}:/src", root.display()))
        .args(mounts.iter().flat_map(|mount| ["-v", mount.as_str()]))
        .args(["-w", "/src", IMAGE, ENTRYPOINT, "bash", "-c", script])
        .status()
        .map_err(|error| format!("can't run docker: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(status.to_string())
    }
}
