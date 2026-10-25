// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask profile record`: where `kapto record`'s time goes under a
//! load. Kapto runs on this machine, built with frame pointers; `perf`
//! samples every thread from the profiler container, the only place given
//! `CAP_PERFMON`; each thread's time running, waiting for a core and blocked
//! is read from `/proc` meanwhile, so a thread that waits shows even when it
//! uses no CPU.

mod flamegraph;
mod stacks;
mod timeline;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::time::Duration;

use clap::{Args, ValueEnum};

use super::place::{Place, current_ids};
use super::{Cameras, Spec, record_load_watched, tidy, work_dir};
use stacks::{Reading, ThreadTimes};

/// The image `perf` runs in, the Dockerfile's `profiler` stage.
const IMAGE: &str = "kapto-profiler:dev";

/// Where the binaries built with frame pointers go, apart from the release
/// build, which they would otherwise replace.
const TARGET_DIR: &str = "target/profiling";

/// Samples a second of each thread's time on a core, timed by the clock
/// rather than the CPU's cycles, whose period adapts slowly and samples
/// threads that wake often far more than their time: often enough for a
/// 20-second run to name functions that take 1 % of a thread, rarely
/// enough to cost it little.
const FREQUENCY: &str = "999";

/// How often each thread's times are read.
const READ_EVERY: Duration = Duration::from_millis(200);

/// Arguments of `profile record` and `profile trace`.
#[derive(Args, Clone, Copy, Debug)]
pub(crate) struct ScenarioArgs {
    /// The load to record.
    #[arg(long, value_enum)]
    scenario: Scenario,
    /// How long the load runs, in seconds.
    #[arg(long, default_value_t = 20)]
    seconds: u32,
    /// Keep the recording in `target/bench/` once it is profiled or traced.
    #[arg(long)]
    keep_recordings: bool,
}

/// Arguments of `profile trace`.
#[derive(Args, Clone, Copy, Debug)]
pub(crate) struct TraceArgs {
    #[command(flatten)]
    scenario: ScenarioArgs,
    /// Also upload the episode to the RustFS server of `compose.yaml`,
    /// started for it in Docker, and say what that costs per GB.
    #[arg(long)]
    upload: bool,
}

/// The loads a profile is taken under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Scenario {
    /// The reference load: 16 streams at 2 kHz over UDP.
    Reference,
    /// The reference load with two 1080p cameras at 60 fps beside it.
    Cameras,
}

impl Scenario {
    fn spec(self, seconds: u32) -> Spec {
        Spec {
            streams: 16,
            tcp_streams: 0,
            cameras: (self == Self::Cameras).then_some(Cameras {
                count: 2,
                fps: 60,
                width: 1920,
                height: 1080,
            }),
            rate: 2_000,
            seconds,
            max_message_bytes: None,
            place: Place::Native,
            tap_socket: None,
            decoded: false,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Cameras => "cameras",
        }
    }
}

/// Records the scenario's load while sampling every thread of `kapto
/// record`, and writes the profile beside the recording.
pub(crate) fn record(root: &Path, args: ScenarioArgs) -> ExitCode {
    match profiled(root, args) {
        Ok((work, report)) => {
            println!("{report}");
            eprintln!(
                "The profile is in {}: profile.md, flamegraph.svg, and stacks.folded for other tools",
                work.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Records the scenario's load with a profiling build, has the worker do
/// the episode's jobs, and writes their timeline and its report.
pub(crate) fn trace(root: &Path, args: TraceArgs) -> ExitCode {
    match timeline::traced(root, args) {
        Ok((work, report)) => {
            println!("{report}");
            eprintln!(
                "The trace is in {}: trace.md, and trace.json for Perfetto",
                work.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn profiled(root: &Path, args: ScenarioArgs) -> Result<(PathBuf, String), String> {
    build(root)?;
    let work = work_dir(root, &format!("profile-{}", args.scenario.name()));
    let spec = args.scenario.spec(args.seconds);
    let mut perf = None;
    let mut watcher = None;
    let run = record_load_watched(
        &root.join(TARGET_DIR).join("release"),
        &work,
        &spec,
        &[],
        |pid| {
            perf = Some(sample(&work, pid));
            watcher = Some(std::thread::spawn(move || watch(pid)));
        },
    );
    let sampled = perf
        .ok_or("kapto record never started")?
        .and_then(|mut child| child.wait().map_err(|error| error.to_string()));
    let threads = watcher
        .ok_or("kapto record never started")?
        .join()
        .map_err(|_| "reading the threads' times failed")?;
    let run = run?;
    if !sampled.is_ok_and(|status| status.success()) {
        return Err(format!(
            "perf couldn't sample kapto record; its output is in {}",
            work.join("perf.log").display()
        ));
    }
    let script = perf_script(&work)?;
    let profile = stacks::Profile::new(&script, &threads);
    let report = profile.markdown(args.scenario.name(), args.seconds);
    let write = |name: &str, text: &str| {
        std::fs::write(work.join(name), text)
            .map_err(|error| format!("can't write {name}: {error}"))
    };
    let title = format!(
        "kapto record, the {} load, {} s",
        args.scenario.name(),
        args.seconds
    );
    write("flamegraph.svg", &profile.flamegraph(&title))?;
    write("stacks.folded", &profile.folded())?;
    write("profile.md", &report)?;
    tidy(&run.work, args.keep_recordings);
    Ok((work, report))
}

/// Builds Kapto and the load generator with frame pointers, so `perf`
/// walks their stacks, and the profiler image.
fn build(root: &Path) -> Result<(), String> {
    let built = Command::new(env!("CARGO"))
        .args([
            "build",
            "--release",
            "--quiet",
            "-p",
            "kapto-cli",
            "-p",
            "kapto-loadgen",
        ])
        .env("RUSTFLAGS", "-C force-frame-pointers=yes")
        .env("CARGO_TARGET_DIR", root.join(TARGET_DIR))
        .current_dir(root)
        .status()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if !built.success() {
        return Err("the build with frame pointers failed".to_owned());
    }
    let image = Command::new("docker")
        .args([
            "build", "--quiet", "--target", "profiler", "--tag", IMAGE, "--file",
        ])
        .arg(root.join("docker/Dockerfile"))
        .arg(root)
        .status()
        .map_err(|error| format!("can't run docker: {error}"))?;
    if image.success() {
        Ok(())
    } else {
        Err(format!("the {IMAGE} image didn't build"))
    }
}

/// Starts `perf record` on `pid` in the profiler container, sharing the
/// host's processes; it ends when the process does. The file it writes is
/// handed back to this user.
fn sample(work: &Path, pid: u32) -> Result<Child, String> {
    let (uid, gid) = current_ids()?;
    let data = work.join("perf.data");
    let log = std::fs::File::create(work.join("perf.log")).map_err(|error| error.to_string())?;
    let script = format!(
        "perf record -e cpu-clock -F {FREQUENCY} -g -p {pid} -o '{data}' 2>&1; status=$?; chown {uid}:{gid} '{data}'; exit $status",
        data = data.display()
    );
    Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-add",
            "PERFMON",
            "--pid",
            "host",
            "--volume",
        ])
        .arg(format!("{0}:{0}", work.display()))
        .args([IMAGE, "sh", "-c", &script])
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("can't run docker: {error}"))
}

/// Each sample's thread and stack, symbols looked up in this machine's
/// files, mounted read-only.
fn perf_script(work: &Path) -> Result<String, String> {
    let (uid, gid) = current_ids()?;
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--user",
            &format!("{uid}:{gid}"),
            "--volume",
            "/:/host:ro",
        ])
        .args([
            IMAGE,
            "perf",
            "script",
            "--symfs",
            "/host",
            "-F",
            "comm,tid,ip,sym,dso",
            "-i",
        ])
        .arg(
            Path::new("/host")
                .join(work.strip_prefix("/").unwrap_or(work))
                .join("perf.data"),
        )
        .output()
        .map_err(|error| format!("can't run docker: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "perf script failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Reads every thread of `pid`'s times until it ends: the first and last
/// reading of each.
fn watch(pid: u32) -> Vec<ThreadTimes> {
    let tasks = Path::new("/proc").join(pid.to_string()).join("task");
    let mut threads: Vec<ThreadTimes> = Vec::new();
    let start = std::time::Instant::now();
    while let Ok(entries) = std::fs::read_dir(&tasks) {
        let at = start.elapsed();
        for task in entries.flatten() {
            let read = |file: &str| std::fs::read_to_string(task.path().join(file));
            let (Ok(name), Ok(schedstat)) = (read("comm"), read("schedstat")) else {
                continue;
            };
            let Some(reading) = Reading::parse(at, &schedstat) else {
                continue;
            };
            let tid = task.file_name().to_string_lossy().into_owned();
            match threads.iter_mut().find(|thread| thread.tid == tid) {
                Some(thread) => thread.last = reading,
                None => threads.push(ThreadTimes {
                    tid,
                    name: name.trim().to_owned(),
                    first: reading,
                    last: reading,
                }),
            }
        }
        std::thread::sleep(READ_EVERY);
    }
    threads
}
