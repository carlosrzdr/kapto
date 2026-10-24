// SPDX-License-Identifier: Apache-2.0

//! Developer automation for the Kapto repository, run as `cargo xtask <command>`.
//!
//! Every check CI runs is a command here, so contributors run exactly the same
//! checks locally as CI does.

mod agents;
mod api_diff;
mod bench;
mod ci;
mod clean;
mod demo;
mod golden;
mod links;
mod reference;
mod ros2;
mod storage;
mod typescript;
mod ui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cargo xtask",
    about = "Developer automation for the Kapto repository"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the checks that must pass before a pull request is merged.
    Ci {
        /// Which group of checks to run.
        #[arg(value_enum, default_value_t = ci::Group::All)]
        group: ci::Group,
    },
    /// Measure Kapto against its performance targets.
    #[command(subcommand)]
    Bench(BenchCommand),
    /// Find where Kapto's time goes under a load.
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Regenerate reference values with independent tools.
    #[command(subcommand)]
    Golden(GoldenCommand),
    /// Build, check or test the web UI, with Node.js in Docker.
    Ui {
        /// What to do.
        #[arg(value_enum)]
        task: ui::Task,
    },
    /// Show what the build folder holds, and remove what is cheap to build
    /// again.
    Clean {
        /// Remove Kapto's own build outputs, keeping the dependencies built.
        #[arg(long, conflicts_with = "all")]
        workspace: bool,
        /// Remove the whole build folder.
        #[arg(long)]
        all: bool,
    },
    /// Check the API for changes that could break a client, against the
    /// OpenAPI document committed at an earlier revision (oasdiff, in Docker).
    ApiDiff {
        /// The git revision to compare with.
        #[arg(long, default_value = "main")]
        base: String,
    },
    /// Lint and test the ros2 feature in the ROS 2 image (Docker), which
    /// the other checks can't build without ROS 2 on the host.
    Ros2,
    /// Test uploads to S3-compatible storage against a RustFS server
    /// started for them (Docker).
    Storage,
    /// Start the demo as a new user does, `docker compose --profile demo
    /// up`, and check it end to end: the arm moving in a browser, an
    /// episode recorded and found uploaded in RustFS.
    DemoCompose,
    /// Regenerate the reference pages in docs/reference from the code.
    Gen {
        /// Only report pages that are out of date; don't write anything.
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum GoldenCommand {
    /// Link poses of the robots in testdata/robots from ROS 2's
    /// `robot_state_publisher`, in Docker, for checking forward kinematics.
    Fk,
    /// Messages serialised by ROS 2 itself, in Docker, with their
    /// definitions and values, for checking the CDR reader.
    Cdr,
    /// Protobuf messages encoded by buf, in Docker, for checking Protobuf
    /// decoding.
    Protobuf,
}

#[derive(Subcommand)]
enum BenchCommand {
    /// Record the synthetic robot with `kapto record` and check CPU, memory,
    /// buffers and drops against the targets.
    System(bench::SystemArgs),
    /// Record twice the reference load (32 streams at 2 kHz) and check that
    /// nothing is dropped and buffers stay at most half full.
    Headroom(bench::HeadroomArgs),
    /// Find how many streams this machine records completely.
    Capacity(bench::CapacityArgs),
    /// Run the stage benchmarks, fit each stage's cost per batch, per
    /// message and per KiB, and compare it with perf-budget.toml.
    Model(bench::ModelArgs),
    /// Record the reference load while the live view's bridge streams to a
    /// page, is stopped, has its page stop reading and is killed, check that
    /// recording doesn't notice, and measure the delay to each frame sent.
    Viewer(bench::ViewerArgs),
    /// Watch the live view in a browser drawing with the graphics card at
    /// the reference load: the delay to each frame drawn and the page's
    /// memory over time.
    Browser(bench::BrowserArgs),
    /// Record the reference load as ROS 2 `JointState` topics, in the ROS 2
    /// image, while the live view's bridge moves the arm with the first,
    /// and check it against the targets the same load over UDP is held to.
    Ros2(bench::Ros2Args),
    /// Upload an episode to RustFS in Docker for each part size and number
    /// of parts at once, with the CPU and memory each takes, and check the
    /// default settings come close to the fastest.
    Upload(bench::UploadArgs),
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Record a scenario's load while `perf` samples every thread of `kapto
    /// record` (in Docker, with `CAP_PERFMON`): each thread's time on a core,
    /// waiting for one and blocked, the functions its samples land in, and
    /// the stacks folded for a flamegraph.
    Record(bench::profile::RecordArgs),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Ci { group } => ci::run(group),
        Command::DemoCompose => demo::run(&repo_root()),
        Command::Bench(BenchCommand::System(args)) => bench::system(&repo_root(), &args),
        Command::Bench(BenchCommand::Headroom(args)) => bench::headroom(&repo_root(), &args),
        Command::Bench(BenchCommand::Capacity(args)) => bench::capacity(&repo_root(), &args),
        Command::Bench(BenchCommand::Model(args)) => bench::model(&repo_root(), &args),
        Command::Bench(BenchCommand::Viewer(args)) => bench::viewer(&repo_root(), &args),
        Command::Bench(BenchCommand::Browser(args)) => bench::browser(&repo_root(), &args),
        Command::Bench(BenchCommand::Ros2(args)) => bench::ros2(&repo_root(), &args),
        Command::Bench(BenchCommand::Upload(args)) => bench::upload(&repo_root(), &args),
        Command::Profile(ProfileCommand::Record(args)) => {
            bench::profile::record(&repo_root(), args)
        }
        Command::Ui { task } => ui::run(&repo_root(), task),
        Command::Clean { workspace, all } => clean::run(
            &repo_root(),
            match (workspace, all) {
                (true, _) => clean::Remove::Workspace,
                (_, true) => clean::Remove::All,
                _ => clean::Remove::Nothing,
            },
        ),
        Command::ApiDiff { base } => api_diff::run(&repo_root(), &base),
        Command::Gen { check } => reference::run(&repo_root(), check),
        Command::Ros2 => ros2::run(&repo_root()),
        Command::Storage => storage::run(&repo_root()),
        Command::Golden(GoldenCommand::Fk) => golden::fk(&repo_root()),
        Command::Golden(GoldenCommand::Cdr) => golden::cdr(&repo_root()),
        Command::Golden(GoldenCommand::Protobuf) => golden::protobuf(&repo_root()),
    }
}

/// The repository root: two levels above this crate (`tools/xtask`).
pub(crate) fn repo_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}
