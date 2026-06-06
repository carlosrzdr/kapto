// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench upload`: how fast an episode goes up to S3-compatible
//! storage for each part size and number of parts at once, against the
//! RustFS server of `compose.yaml` started on a free port, and the CPU and
//! peak memory each upload takes. The uploads are made by the engine as the
//! worker makes them, by `kapto-upload`'s `rustfs` benchmark program; the
//! report says whether the default settings come close to the fastest.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::Args;
use serde::{Deserialize, Serialize};

use super::place::Place;
use super::report::Machine;
use super::{work_dir, write_reports};
use crate::storage;

/// The upload benchmark.
#[derive(Args)]
pub(crate) struct UploadArgs {
    /// The size of the episode's recording, in MiB.
    #[arg(long, default_value_t = 256)]
    mib: u32,
    /// How many times each setting uploads; the median one is reported.
    #[arg(long, default_value_t = 3)]
    repeats: u32,
    /// Instead, record the reference load with nothing else running, then
    /// while episodes go up as the worker uploads them, and check that
    /// recording doesn't notice.
    #[arg(long)]
    while_recording: bool,
    /// With --while-recording, how long each phase lasts, in seconds.
    #[arg(long, default_value_t = 30)]
    phase_seconds: u32,
}

/// Each part size, in MiB, and number of parts at once tried, with no
/// growth past it; then the defaults, growing while the store keeps up.
const PART_SIZES: [u64; 5] = [5, 8, 16, 32, 64];
const PARALLEL: [u32; 5] = [1, 2, 4, 8, 16];

/// The defaults must go at least this share as fast as the fastest run.
const DEFAULTS_SHARE: f64 = 0.9;

/// One upload, as the benchmark program prints it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct Run {
    part_size_mib: u64,
    parallel_parts: u32,
    max_in_flight: u32,
    defaults: bool,
    bytes: u64,
    pub(super) seconds: f64,
    cpu_cores: f64,
    peak_memory_mib: f64,
}

impl Run {
    /// What it uploaded, in MiB.
    #[allow(clippy::cast_precision_loss, reason = "bytes shown as MiB")]
    pub(super) fn mib(&self) -> f64 {
        self.bytes as f64 / f64::from(1 << 20)
    }

    fn mib_per_second(&self) -> f64 {
        self.mib() / self.seconds.max(f64::EPSILON)
    }
}

#[derive(Serialize)]
struct UploadReport {
    machine: Machine,
    recording_mib: u32,
    repeats: u32,
    runs: Vec<Run>,
}

impl UploadReport {
    fn fastest(&self) -> Option<&Run> {
        self.runs
            .iter()
            .max_by(|one, other| one.mib_per_second().total_cmp(&other.mib_per_second()))
    }

    fn defaults(&self) -> Option<&Run> {
        self.runs.iter().find(|run| run.defaults)
    }

    /// Whether the defaults go nearly as fast as the fastest settings.
    fn defaults_sensible(&self) -> bool {
        match (self.defaults(), self.fastest()) {
            (Some(defaults), Some(fastest)) => {
                defaults.mib_per_second() >= DEFAULTS_SHARE * fastest.mib_per_second()
            }
            _ => false,
        }
    }

    fn markdown(&self) -> String {
        let machine = &self.machine;
        let mut page = String::from(
            "# Upload benchmark: one episode to RustFS on this machine\n\n\
             Provisional: measured on this machine, not the reference PC, to a server on \
             the same machine, so the network is never what limits it. Compare runs only \
             with runs on the same machine.\n\n",
        );
        let _ = writeln!(
            page,
            "## Machine\n\n| | |\n|---|---|\n| CPU | {} |\n| Threads | {} |\n| Memory | {:.1} GiB |\n| Kernel | {} |\n| CPU governor | {} |\n",
            machine.cpu, machine.threads, machine.memory_gib, machine.kernel, machine.cpu_governor
        );
        let _ = writeln!(
            page,
            "## Uploads\n\nAn episode of a {} MiB recording and its sidecars, its checksums worked \
             out and uploaded, each part checked by the server's SHA-256, {} times for each \
             setting, each a process of its own; the median is shown. Each setting but the \
             defaults keeps its parts at once fixed (`max_in_flight` = `parallel_parts`); the \
             defaults start at theirs and grow while the server keeps up.\n\n\
             | Part size | Parts at once | Most in flight | MiB/s | CPU (cores) | Peak memory |\n\
             |---|---|---|---|---|---|",
            self.recording_mib, self.repeats
        );
        let fastest = self.fastest();
        for run in &self.runs {
            let mark = if run.defaults {
                " (defaults)"
            } else if fastest == Some(run) {
                " (fastest)"
            } else {
                ""
            };
            let _ = writeln!(
                page,
                "| {} MiB{mark} | {} | {} | {:.0} | {:.2} | {:.0} MiB |",
                run.part_size_mib,
                run.parallel_parts,
                run.max_in_flight,
                run.mib_per_second(),
                run.cpu_cores,
                run.peak_memory_mib
            );
        }
        if let (Some(defaults), Some(fastest)) = (self.defaults(), fastest) {
            let _ = writeln!(
                page,
                "\nThe defaults go {:.0} MiB/s, {:.0} % of the fastest ({} MiB parts, {} at once): \
                 {} (at least {:.0} % is).",
                defaults.mib_per_second(),
                100.0 * defaults.mib_per_second() / fastest.mib_per_second().max(f64::EPSILON),
                fastest.part_size_mib,
                fastest.parallel_parts,
                if self.defaults_sensible() {
                    "sensible on this machine"
                } else {
                    "**slower than they should be**"
                },
                DEFAULTS_SHARE * 100.0
            );
        }
        page
    }
}

/// Runs the benchmark and prints its report.
pub(crate) fn run(root: &Path, args: &UploadArgs) -> ExitCode {
    if args.while_recording {
        return super::uploading::run(root, args.phase_seconds, args.mib);
    }
    match measure(root, args) {
        Ok(report) => {
            println!("{}", report.markdown());
            if report.defaults_sensible() {
                ExitCode::SUCCESS
            } else {
                eprintln!("The defaults are well short of the fastest settings.");
                ExitCode::FAILURE
            }
        }
        Err(problem) => {
            eprintln!("The benchmark couldn't run: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn measure(root: &Path, args: &UploadArgs) -> Result<UploadReport, String> {
    let work = work_dir(root, "upload");
    std::fs::create_dir_all(&work)
        .map_err(|error| format!("can't create {}: {error}", work.display()))?;
    let settings: Vec<Option<(u64, u32)>> = PART_SIZES
        .iter()
        .flat_map(|&part| PARALLEL.iter().map(move |&parallel| Some((part, parallel))))
        .chain([None])
        .collect();
    let program = program(root)?;
    let folder = work.join("episode");
    let runs = storage::with_server(root, |address| {
        let mut runs = Vec::new();
        for (number, setting) in settings.iter().enumerate() {
            eprintln!(
                "Uploading a {} MiB episode {} times with {}",
                args.mib,
                args.repeats,
                setting.map_or_else(
                    || "the defaults".to_owned(),
                    |(part, parallel)| format!("{part} MiB parts, {parallel} at once")
                )
            );
            let upload = Upload {
                program: &program,
                address,
                folder: &folder,
                mib: args.mib,
                setting: *setting,
                low_priority: false,
            };
            let tries = (0..args.repeats.max(1))
                .map(|repeat| upload_once(&upload, &format!("run-{number}-{repeat}")))
                .collect::<Result<Vec<_>, _>>()?;
            runs.extend(median(tries));
        }
        Ok(runs)
    })?;
    let _ = std::fs::remove_dir_all(&folder);
    let report = UploadReport {
        machine: Machine::this(Place::Native),
        recording_mib: args.mib,
        repeats: args.repeats.max(1),
        runs,
    };
    write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    Ok(report)
}

/// Builds the benchmark program and returns where it is, to be run
/// directly rather than through cargo, whose own work would count.
pub(super) fn program(root: &Path) -> Result<PathBuf, String> {
    let output = Command::new(env!("CARGO"))
        .args(["bench", "--no-run", "--quiet", "--message-format", "json"])
        .args([
            "-p",
            "kapto-upload",
            "--features",
            "testing",
            "--bench",
            "rustfs",
        ])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if !output.status.success() {
        return Err("the upload benchmark program didn't build".to_owned());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|message| message["target"]["name"] == "rustfs")
        .and_then(|message| message["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "cargo didn't say where the upload benchmark program is".to_owned())
}

/// What one upload by the benchmark program uploads and how.
pub(super) struct Upload<'a> {
    pub(super) program: &'a Path,
    /// The server's address.
    pub(super) address: &'a str,
    /// Where the episode is, made by the first upload.
    pub(super) folder: &'a Path,
    pub(super) mib: u32,
    /// Part size and parts at once; the defaults if `None`.
    pub(super) setting: Option<(u64, u32)>,
    /// At the worker's priority (nice 10, idle I/O), as the worker uploads.
    pub(super) low_priority: bool,
}

/// One upload of `upload`'s episode, to `key` in the bucket.
pub(super) fn upload_once(upload: &Upload<'_>, key: &str) -> Result<Run, String> {
    let mut command = if upload.low_priority {
        let mut lowered = Command::new("nice");
        lowered
            .args(["-n", "10", "ionice", "-c", "3"])
            .arg(upload.program);
        lowered
    } else {
        Command::new(upload.program)
    };
    command
        .arg("--folder")
        .arg(upload.folder)
        .args(["--mib", &upload.mib.to_string(), "--key", key])
        .stdout(Stdio::piped());
    if let Some((part, parallel)) = upload.setting {
        command.args([
            "--part",
            &part.to_string(),
            "--parallel",
            &parallel.to_string(),
        ]);
    }
    storage::reach(&mut command, upload.address);
    let output = command
        .output()
        .map_err(|error| format!("can't run the upload benchmark program: {error}"))?;
    if !output.status.success() {
        return Err(format!("an upload failed ({})", output.status));
    }
    runs_of(&String::from_utf8_lossy(&output.stdout))?
        .pop()
        .ok_or_else(|| "the benchmark printed no run".to_owned())
}

/// The try that took the median time, if there was one.
fn median(mut tries: Vec<Run>) -> Option<Run> {
    tries.sort_by(|one, other| one.seconds.total_cmp(&other.seconds));
    let middle = tries.len() / 2;
    tries.into_iter().nth(middle)
}

/// The runs the benchmark program printed, a line of JSON each.
fn runs_of(output: &str) -> Result<Vec<Run>, String> {
    let runs: Vec<Run> = output
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|error| format!("the benchmark printed a run that doesn't read: {error}"))?;
    if runs.is_empty() {
        return Err("the benchmark printed no runs".to_owned());
    }
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(part: u64, parallel: u32, defaults: bool, seconds: f64) -> String {
        format!(
            "{{\"part_size_mib\":{part},\"parallel_parts\":{parallel},\"max_in_flight\":{},\
             \"defaults\":{defaults},\"bytes\":536870912,\"seconds\":{seconds},\
             \"cpu_cores\":0.5,\"peak_memory_mib\":80.0}}",
            if defaults { 32 } else { parallel }
        )
    }

    fn report(lines: &[String]) -> UploadReport {
        UploadReport {
            machine: Machine::this(Place::Native),
            recording_mib: 512,
            repeats: 1,
            runs: runs_of(&lines.join("\n")).unwrap(),
        }
    }

    #[test]
    fn runs_are_read_from_the_programs_lines_and_nothing_else() {
        let output = format!("Running benches/rustfs.rs\n{}\n", run(16, 4, false, 2.0));
        let runs = runs_of(&output).unwrap();
        assert_eq!(runs.len(), 1);
        assert!((runs[0].mib_per_second() - 256.0).abs() < 1e-9);
        assert!(runs_of("nothing\n").is_err());
        assert!(runs_of("{\"part_size_mib\": 16}\n").is_err());
    }

    #[test]
    fn each_setting_reports_its_median_try() {
        let tries = runs_of(
            &[
                run(16, 4, false, 3.0),
                run(16, 4, false, 1.0),
                run(16, 4, false, 2.0),
            ]
            .join("\n"),
        )
        .unwrap();
        assert!((median(tries).unwrap().seconds - 2.0).abs() < 1e-9);
        assert_eq!(median(Vec::new()), None);
    }

    #[test]
    fn the_defaults_are_sensible_within_a_tenth_of_the_fastest() {
        let close = report(&[run(32, 8, false, 1.0), run(16, 4, true, 1.1)]);
        assert!(close.defaults_sensible());
        let page = close.markdown();
        assert!(
            page.contains("| 32 MiB (fastest) | 8 | 8 | 512 |"),
            "{page}"
        );
        assert!(
            page.contains("| 16 MiB (defaults) | 4 | 32 | 465 |"),
            "{page}"
        );
        assert!(page.contains("91 % of the fastest"), "{page}");

        let slow = report(&[run(32, 8, false, 1.0), run(16, 4, true, 1.2)]);
        assert!(!slow.defaults_sensible());
        assert!(slow.markdown().contains("**slower than they should be**"));
        assert!(!report(&[run(32, 8, false, 1.0)]).defaults_sensible());
    }
}
