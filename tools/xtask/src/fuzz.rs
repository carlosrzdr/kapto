// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask fuzz`: the fuzz targets in `fuzz/`, run with cargo-fuzz in
//! the Dockerfile's `fuzz` stage, since libFuzzer's instrumentation needs a
//! nightly toolchain. The repository is mounted into it, with a build
//! folder, the corpora and any crash found in `target/fuzz/`.
//!
//! Each target starts from seeds written here before it runs: the
//! reference messages in `testdata/`, and layouts, tap datagrams and
//! recordings made by Kapto's own code, so nothing generated is committed.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::ui::user;

mod seeds;

const IMAGE: &str = "kapto-fuzz";

/// Every target in `fuzz/fuzz_targets`, most valuable first.
pub(crate) const TARGETS: [&str; 10] = [
    "tcp_deframer",
    "cdr",
    "msg_definition",
    "struct_layout",
    "path_layout",
    "protobuf",
    "camera_frame",
    "tap_wire",
    "mcap_salvage",
    "mcap_seek",
];

#[derive(clap::Args)]
pub(crate) struct FuzzArgs {
    /// The target to fuzz; without one, every target in turn.
    #[arg(value_parser = clap::builder::PossibleValuesParser::new(TARGETS))]
    target: Option<String>,
    /// How long to fuzz each target: 60 s for one, 10 s each for all.
    #[arg(long)]
    seconds: Option<u64>,
    /// List the targets and stop.
    #[arg(long, conflicts_with_all = ["target", "seconds", "input"])]
    list: bool,
    /// Run the target once on this file, e.g. a crash found before,
    /// instead of fuzzing.
    #[arg(long, requires = "target", conflicts_with = "seconds")]
    input: Option<PathBuf>,
}

pub(crate) fn run(root: &Path, args: &FuzzArgs) -> ExitCode {
    if args.list {
        for target in TARGETS {
            println!("{target}");
        }
        return ExitCode::SUCCESS;
    }
    let targets: Vec<&str> = args
        .target
        .as_deref()
        .map_or_else(|| TARGETS.to_vec(), |target| vec![target]);
    let seconds = args
        .seconds
        .unwrap_or(if targets.len() == 1 { 60 } else { 10 });
    eprintln!("Building {IMAGE} (the Dockerfile's fuzz stage)");
    let built = Command::new("docker")
        .args(["build", "--target", "fuzz", "--tag", IMAGE, "--file"])
        .arg(root.join("docker/Dockerfile"))
        .arg(root)
        .status();
    if !built.is_ok_and(|status| status.success()) {
        eprintln!("error: the fuzzing image didn't build");
        return ExitCode::FAILURE;
    }
    let work = root.join("target/fuzz");
    if let Err(error) = seeds::write(root, &work.join("seeds")) {
        eprintln!(
            "error: can't write the seeds into {}: {error}",
            work.display()
        );
        return ExitCode::FAILURE;
    }
    let mut failed = Vec::new();
    for target in targets {
        let inputs = if let Some(input) = &args.input {
            match copy_in(&work, input) {
                Ok(inside) => vec![inside],
                Err(error) => {
                    eprintln!("error: can't read {}: {error}", input.display());
                    return ExitCode::FAILURE;
                }
            }
        } else {
            let corpus = format!("target/fuzz/corpus/{target}");
            if let Err(error) = std::fs::create_dir_all(root.join(&corpus)) {
                eprintln!("error: can't make {corpus}: {error}");
                return ExitCode::FAILURE;
            }
            vec![corpus, format!("target/fuzz/seeds/{target}")]
        };
        let artifacts = format!("target/fuzz/artifacts/{target}/");
        let _ = std::fs::create_dir_all(root.join(&artifacts));
        let mut libfuzzer = vec![
            format!("-artifact_prefix={artifacts}"),
            // An input that takes this long is reported as a hang.
            "-timeout=10".to_owned(),
            "-rss_limit_mb=2048".to_owned(),
        ];
        if args.input.is_none() {
            libfuzzer.push(format!("-max_total_time={seconds}"));
            libfuzzer.push("-print_final_stats=1".to_owned());
            eprintln!("Fuzzing {target} for {seconds} s");
        }
        if !in_image(root, target, &inputs, &libfuzzer) {
            eprintln!("error: {target} failed; what it found is in {artifacts}");
            failed.push(target);
        }
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("error: {} found a problem", failed.join(", "));
        ExitCode::FAILURE
    }
}

/// Copies a file to run once into the mounted build folder, where the
/// container sees it; returns its path from the repository root.
fn copy_in(work: &Path, input: &Path) -> std::io::Result<String> {
    let folder = work.join("inputs");
    std::fs::create_dir_all(&folder)?;
    std::fs::copy(input, folder.join("input"))?;
    Ok("target/fuzz/inputs/input".to_owned())
}

/// Runs `cargo fuzz run` in the image with the repository mounted.
fn in_image(root: &Path, target: &str, inputs: &[String], libfuzzer: &[String]) -> bool {
    Command::new("docker")
        .args(["run", "--rm", "--user", &user(), "-e", "HOME=/tmp"])
        .args(["-e", "CARGO_HOME=/src/target/fuzz/cargo"])
        .arg("-v")
        .arg(format!("{}:/src", root.display()))
        .args([
            "-w",
            "/src",
            IMAGE,
            "cargo",
            "fuzz",
            "run",
            "--fuzz-dir",
            "fuzz",
        ])
        .args(["--target-dir", "target/fuzz/build", target])
        .args(inputs)
        .arg("--")
        .args(libfuzzer)
        .status()
        .is_ok_and(|status| status.success())
}
