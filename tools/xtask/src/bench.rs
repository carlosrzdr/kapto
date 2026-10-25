// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench`: records the synthetic robot with `kapto record`,
//! measures the recording process from outside, and checks the result
//! against Kapto's performance targets.
//!
//! - `system`: the reference load, checked against every target.
//! - `headroom`: twice the reference load, which must still be recorded
//!   completely.
//! - `capacity`: how many streams this machine records completely.
//! - `browser`: the live view in a browser at the reference load.
//! - `viewer`: the reference load while the live view's bridge fails.
//! - `ros2`: the reference load as ROS 2 topics, with the live view open.
//! - `upload`: an episode uploaded to RustFS for each part size and
//!   number of parts at once, or the reference load recorded while
//!   episodes go up.
//!
//! Release binaries are built first. For each load, the load generator sends
//! the streams for a set time and `kapto record` records them, with two
//! seconds of margin, into a folder under `target/bench/`. Once a second the
//! harness reads the recording process's per-thread CPU and peak memory from
//! `/proc`. At the end it reads the finished episode's `episode.json`.

// CPU ticks, bytes and message counts become fractions for the report; the
// precision lost far beyond the 15th digit doesn't matter there.
#![allow(
    clippy::cast_precision_loss,
    reason = "counts shown as fractions in a report"
)]

mod browser;
mod capacity;
mod measure;
mod model;
mod page;
mod place;
pub(crate) mod profile;
mod report;
mod ros2;
mod upload;
mod uploading;
mod viewer;

use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Args;
use kapto_store::Episode;

pub(crate) use browser::BrowserArgs;
use capacity::{CapacityReport, Limit, Step, next_streams};
use measure::{GeneratorTotals, Sample, cores_between};
pub(crate) use model::ModelArgs;
use place::{Place, PlaceArgs};
use report::{Load, Machine, Measured, Report, Targets};
pub(crate) use ros2::Ros2Args;
pub(crate) use upload::UploadArgs;
pub(crate) use viewer::ViewerArgs;

/// The reference load: 16 streams at 2 kHz.
#[derive(Args)]
pub(crate) struct SystemArgs {
    /// Number of streams.
    #[arg(long, default_value_t = 16)]
    streams: u16,
    /// How many of the streams are sent over TCP (the generator connects,
    /// Kapto listens); the rest over UDP.
    #[arg(long, default_value_t = 0)]
    tcp: u16,
    /// Messages per second on each stream.
    #[arg(long, default_value_t = 2_000)]
    rate: u32,
    /// How long the load runs, in seconds.
    #[arg(long, default_value_t = 60)]
    seconds: u32,
    /// Cameras sent beside the streams over TCP, each its JPEG frames.
    #[arg(long, default_value_t = 0)]
    cameras: u16,
    /// Frames a second from each camera.
    #[arg(long, default_value_t = 60)]
    camera_fps: u32,
    /// Each camera's frame size, e.g. 1920x1080.
    #[arg(long, default_value = "1920x1080", value_parser = frame_size)]
    camera_size: (u16, u16),
    /// Keep the recording in `target/bench/` once it is measured. Without
    /// it the recording is removed, as one with cameras takes gigabytes; the
    /// reports stay.
    #[arg(long)]
    keep_recordings: bool,
    #[command(flatten)]
    place: PlaceArgs,
}

/// A frame size written `WIDTHxHEIGHT`.
pub(super) fn frame_size(text: &str) -> Result<(u16, u16), String> {
    let (width, height) = text
        .split_once('x')
        .ok_or_else(|| format!("`{text}` isn't WIDTHxHEIGHT, e.g. 1920x1080"))?;
    let side = |side: &str| {
        side.parse::<u16>()
            .map_err(|error| format!("`{side}`: {error}"))
    };
    Ok((side(width)?, side(height)?))
}

/// Twice the reference load: 32 streams at 2 kHz.
#[derive(Args)]
pub(crate) struct HeadroomArgs {
    /// How long the load runs, in seconds.
    #[arg(long, default_value_t = 60)]
    seconds: u32,
    /// Keep the recording in `target/bench/` once it is measured. Without
    /// it the recording is removed, as one with cameras takes gigabytes; the
    /// reports stay.
    #[arg(long)]
    keep_recordings: bool,
    #[command(flatten)]
    place: PlaceArgs,
}

/// The capacity search.
#[derive(Args)]
pub(crate) struct CapacityArgs {
    /// Messages per second on each stream.
    #[arg(long, default_value_t = 2_000)]
    rate: u32,
    /// How long each step records, in seconds.
    #[arg(long, default_value_t = 20)]
    seconds: u32,
    /// Streams in the first step.
    #[arg(long, default_value_t = 16)]
    start: u16,
    /// The most streams to try.
    #[arg(long, default_value_t = 1_024)]
    max_streams: u16,
    #[command(flatten)]
    place: PlaceArgs,
}

/// One load to record.
struct Spec {
    streams: u16,
    /// How many of them over TCP: the last ones.
    tcp_streams: u16,
    /// Cameras sent beside the streams, if any.
    cameras: Option<Cameras>,
    rate: u32,
    seconds: u32,
    /// Declared for every stream; the default slot size when `None`.
    max_message_bytes: Option<u16>,
    /// Where `kapto record` runs.
    place: Place,
    /// Where it sends live samples, if not the default.
    tap_socket: Option<PathBuf>,
    /// Whether each stream declares its values, the load generator's
    /// joints and gripper, so live samples carry them, and each camera its
    /// pictures' format, so the live view shows them.
    decoded: bool,
}

/// Cameras sent beside a load's streams: TCP streams of JPEG frames, Kapto
/// listening, each declaring [`CAMERA_MAX_BYTES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Cameras {
    pub(crate) count: u16,
    pub(crate) fps: u32,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

/// The largest frame a benchmark's camera declares: 1 MiB, above a
/// datagram, so its frames go through a ring of their own.
pub(super) const CAMERA_MAX_BYTES: u32 = 1 << 20;

/// What recording one load gave.
struct Run {
    work: PathBuf,
    generator: GeneratorTotals,
    measured: Measured,
}

/// The reference load, checked against every target.
pub(crate) fn system(root: &Path, args: &SystemArgs) -> ExitCode {
    if args.tcp > args.streams {
        eprintln!("--tcp {} is more than --streams {}", args.tcp, args.streams);
        return ExitCode::FAILURE;
    }
    let spec = Spec {
        streams: args.streams,
        tcp_streams: args.tcp,
        cameras: (args.cameras > 0).then_some(Cameras {
            count: args.cameras,
            fps: args.camera_fps,
            width: args.camera_size.0,
            height: args.camera_size.1,
        }),
        rate: args.rate,
        seconds: args.seconds,
        max_message_bytes: None,
        place: args.place.into(),
        tap_socket: None,
        decoded: false,
    };
    checked(
        root,
        "system",
        &spec,
        Targets::Reference,
        args.keep_recordings,
    )
}

/// Twice the reference load, which must still be recorded completely.
pub(crate) fn headroom(root: &Path, args: &HeadroomArgs) -> ExitCode {
    let spec = Spec {
        streams: 32,
        tcp_streams: 0,
        cameras: None,
        rate: 2_000,
        seconds: args.seconds,
        max_message_bytes: None,
        place: args.place.into(),
        tap_socket: None,
        decoded: false,
    };
    checked(
        root,
        "headroom",
        &spec,
        Targets::Headroom,
        args.keep_recordings,
    )
}

fn checked(root: &Path, kind: &str, spec: &Spec, targets: Targets, keep: bool) -> ExitCode {
    let outcome = build(root, spec.place).and_then(|()| {
        let run = record_load(root, &work_dir(root, kind), spec)?;
        if !run.generator.kept_up() {
            return Err(format!(
                "the load generator fell behind ({} ticks skipped, {} send errors), so the load wasn't the one asked for",
                run.generator.skipped, run.generator.send_errors
            ));
        }
        let load = Load {
            streams: spec.streams,
            tcp_streams: spec.tcp_streams,
            cameras: spec.cameras,
            rate_hz: spec.rate,
            seconds: spec.seconds,
            messages_sent: run.generator.sent,
        };
        let report = Report::new(targets, Machine::this(spec.place), load, run.measured);
        write_reports(&run.work, &serde_json::to_string_pretty(&report), &report.markdown())?;
        tidy(&run.work, keep);
        Ok(report)
    });
    match outcome {
        Ok(report) => {
            println!("{}", report.markdown());
            if report.all_met() {
                ExitCode::SUCCESS
            } else {
                eprintln!("Some targets were not met.");
                ExitCode::FAILURE
            }
        }
        Err(problem) => {
            eprintln!("The benchmark couldn't run: {problem}");
            ExitCode::FAILURE
        }
    }
}

/// Fits each recording stage's cost from the stage benchmarks and checks it
/// against `perf-budget.toml`.
pub(crate) fn model(root: &Path, args: &ModelArgs) -> ExitCode {
    model::run(root, args)
}

/// Watches the live view in a browser at the reference load.
pub(crate) fn browser(root: &Path, args: &BrowserArgs) -> ExitCode {
    browser::run(root, args)
}

/// Records the reference load while the live view's bridge streams to a
/// page, is stopped, has its page stop reading and is killed.
pub(crate) fn viewer(root: &Path, args: &ViewerArgs) -> ExitCode {
    viewer::run(root, args)
}

/// Records the reference load as ROS 2 topics with the live view open.
pub(crate) fn ros2(root: &Path, args: &Ros2Args) -> ExitCode {
    ros2::run(root, args)
}

/// Uploads an episode to RustFS for each part size and parts at once.
pub(crate) fn upload(root: &Path, args: &UploadArgs) -> ExitCode {
    upload::run(root, args)
}

/// Searches for the most streams recorded completely.
pub(crate) fn capacity(root: &Path, args: &CapacityArgs) -> ExitCode {
    match search(root, args) {
        Ok(report) => {
            println!("{}", report.markdown());
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("The capacity search couldn't run: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn search(root: &Path, args: &CapacityArgs) -> Result<CapacityReport, String> {
    let place = Place::from(args.place);
    build(root, place)?;
    let work = work_dir(root, "capacity");
    let (mut lo, mut hi) = (0, None);
    let mut steps = Vec::new();
    let mut limit = Limit::Kapto;
    let mut next = Some(args.start.clamp(1, args.max_streams.max(1)));
    while let Some(streams) = next {
        eprintln!(
            "Recording {streams} streams at {} Hz for {} s…",
            args.rate, args.seconds
        );
        let spec = Spec {
            streams,
            tcp_streams: 0,
            cameras: None,
            rate: args.rate,
            seconds: args.seconds,
            // The load generator's messages are 68 bytes. Declaring it keeps
            // the buffers small, so the search measures throughput, not RAM.
            max_message_bytes: Some(128),
            place,
            tap_socket: None,
            decoded: false,
        };
        let run = record_load(root, &work.join(format!("{streams}-streams")), &spec)?;
        // At hundreds of streams a step records gigabytes; only its numbers
        // are kept.
        let _ = std::fs::remove_dir_all(run.work.join("data"));
        let measured = &run.measured;
        let step = Step {
            streams,
            sent: run.generator.sent,
            written: measured.written,
            dropped_by_us: measured.dropped_by_us,
            lost_upstream: measured.lost_upstream,
            buffer_peak_percent: 100.0 * measured.buffer_peak_batches as f64
                / measured.buffer_batches.max(1) as f64,
            cpu_cores: measured.cpu_cores_mean,
            generator_kept_up: run.generator.kept_up(),
        };
        let (passed, kept_up) = (step.passed(), step.generator_kept_up);
        steps.push(step);
        if !kept_up {
            limit = Limit::Generator;
            break;
        }
        if passed {
            lo = streams;
        } else {
            hi = Some(streams);
        }
        next = next_streams(lo, hi, args.max_streams);
        if next.is_none() && hi.is_none() {
            limit = Limit::Maximum;
        }
    }
    let report = CapacityReport {
        machine: Machine::this(place),
        rate_hz: args.rate,
        step_seconds: args.seconds,
        steps,
        streams: lo,
        limit,
    };
    write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    Ok(report)
}

/// Builds the release binaries, and the image when recording in the container.
fn build(root: &Path, place: Place) -> Result<(), String> {
    if let Place::Container { .. } = place {
        place::build_image(root)?;
    }
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
        .current_dir(root)
        .status()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if built.success() {
        Ok(())
    } else {
        Err("the release build failed".to_owned())
    }
}

/// Removes the recording a run left in `work` once it is measured, unless
/// asked to keep it; the reports beside it stay. A minute with cameras
/// records gigabytes, and runs add up.
pub(super) fn tidy(work: &Path, keep: bool) {
    let data = work.join("data");
    if keep {
        eprintln!("The recording is kept in {}", data.display());
    } else if std::fs::remove_dir_all(&data).is_ok() {
        eprintln!(
            "The recording is removed now it is measured (--keep-recordings keeps it); the reports are in {}",
            work.display()
        );
    }
}

/// `target/bench/<kind>-<unix time>`.
fn work_dir(root: &Path, kind: &str) -> PathBuf {
    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    root.join(format!("target/bench/{kind}-{started_at}"))
}

/// Records one load into `work` and measures it.
fn record_load(root: &Path, work: &Path, spec: &Spec) -> Result<Run, String> {
    record_load_watched(&root.join("target/release"), work, spec, &[], |_| {})
}

/// Records one load into `work` with the binaries in `binaries` and
/// measures it, `kapto record` run natively with `environment` besides
/// what ROS 2 needs; `watch` is given the recording process's ID once it
/// records, before the load starts.
fn record_load_watched(
    binaries: &Path,
    work: &Path,
    spec: &Spec,
    environment: &[(&str, &Path)],
    watch: impl FnOnce(u32),
) -> Result<Run, String> {
    std::fs::create_dir_all(work)
        .map_err(|error| format!("can't create {}: {error}", work.display()))?;
    let cameras = spec.cameras.map_or(0, |cameras| cameras.count);
    let port = free_ports(spec.streams + cameras)?;
    let streams = write_streams(work, port, spec)?;

    let (mut record, pid, lines, mut output) =
        start_recording(binaries, &streams, work, spec, environment)?;
    watch(pid);
    let start = Instant::now();
    // UDP streams from `port`, then TCP streams, each from its own generator.
    let udp_streams = spec.streams - spec.tcp_streams;
    let mut generators = Vec::new();
    for (count, first_port, transport) in [
        (udp_streams, port, "udp"),
        (spec.tcp_streams, port + udp_streams, "tcp-connect"),
    ] {
        if count == 0 {
            continue;
        }
        let generator = Command::new(binaries.join("kapto-loadgen"))
            .args(["--streams", &count.to_string()])
            .args(["--rate", &spec.rate.to_string()])
            .args(["--duration", &spec.seconds.to_string()])
            .args(["--target", &format!("127.0.0.1:{first_port}")])
            .args(["--transport", transport])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("can't start the load generator: {error}"))?;
        generators.push(generator);
    }
    if let Some(sent) = spec.cameras {
        let generator = Command::new(binaries.join("kapto-loadgen"))
            .args(["--streams", &sent.count.to_string()])
            .args(["--rate", &sent.fps.to_string()])
            .args(["--duration", &spec.seconds.to_string()])
            .args(["--target", &format!("127.0.0.1:{}", port + spec.streams)])
            .args(["--transport", "tcp-connect"])
            .args(["--camera", &format!("{}x{}", sent.width, sent.height)])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("can't start the cameras: {error}"))?;
        generators.push(generator);
    }
    let samples = sample_while(pid, start, || {
        generators
            .iter_mut()
            .any(|generator| matches!(generator.try_wait(), Ok(None)))
    });
    let mut generator = GeneratorTotals {
        sent: 0,
        skipped: 0,
        send_errors: 0,
    };
    for running in generators {
        let finished = running
            .wait_with_output()
            .map_err(|error| format!("the load generator failed: {error}"))?;
        let totals = measure::generator_totals(&String::from_utf8_lossy(&finished.stdout))
            .ok_or("the load generator printed no total")?;
        generator.sent += totals.sent;
        generator.skipped += totals.skipped;
        generator.send_errors += totals.send_errors;
    }
    let status = record
        .wait()
        .map_err(|error| format!("kapto record failed: {error}"))?;
    output.extend(lines.try_iter());
    if !status.success() {
        return Err(format!("kapto record failed: {output:#?}"));
    }
    let measured = measured(&work.join("data"), &samples)?;
    eprintln!("The recorded episode is in {}", work.display());
    Ok(Run {
        work: work.to_owned(),
        generator,
        measured,
    })
}

/// Starts `kapto record`, natively or in the container, with two seconds of
/// margin over the load, and returns once it has started recording: the child
/// to wait for, the process ID to measure, its stderr lines so far and a
/// channel for the rest.
fn start_recording(
    binaries: &Path,
    streams: &Path,
    work: &Path,
    spec: &Spec,
    environment: &[(&str, &Path)],
) -> Result<(Child, u32, mpsc::Receiver<String>, Vec<String>), String> {
    let mut kapto_args = vec![
        "record".to_owned(),
        "--duration".to_owned(),
        (spec.seconds + 2).to_string(),
        "--streams".to_owned(),
        streams.display().to_string(),
        "--data-dir".to_owned(),
        work.join("data").display().to_string(),
    ];
    if let Some(tap_socket) = &spec.tap_socket {
        kapto_args.extend([
            "--viewer-tap-socket".to_owned(),
            tap_socket.display().to_string(),
        ]);
    }
    let container = format!("kapto-bench-{}", std::process::id());
    let mut command = match spec.place {
        Place::Native => {
            let mut command = Command::new(binaries.join("kapto"));
            command
                .args(&kapto_args)
                .env_clear()
                .envs(ros2_environment())
                .envs(environment.iter().copied());
            command
        }
        Place::Container { limited } => {
            let (uid, gid) = place::current_ids()?;
            let mut command = Command::new("docker");
            command.args(place::docker_run_args(
                &container,
                limited,
                uid,
                gid,
                work,
                &kapto_args,
            ));
            command
        }
    };
    let mut record = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start kapto record: {error}"))?;
    let stderr = record.stderr.take().ok_or("kapto record has no stderr")?;
    let (lines_tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = lines_tx.send(line);
        }
    });
    let mut output = Vec::new();
    while !output
        .last()
        .is_some_and(|line: &String| line.ends_with("Started."))
    {
        let line = lines
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("kapto record didn't start: {output:#?}"))?;
        output.push(line);
    }
    // In the container, the child is the docker client; the process to
    // measure is kapto itself, which Docker reports by its host process ID.
    let pid = match spec.place {
        Place::Native => record.id(),
        Place::Container { .. } => place::container_pid(&container)?,
    };
    Ok((record, pid, lines, output))
}

/// Of this process's environment, only what ROS 2 needs to load its
/// libraries and find its middleware, when it is set up here: a run
/// otherwise starts with none, so nothing else changes what it measures.
pub(crate) fn ros2_environment() -> impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os().filter(|(name, _)| {
        let name = name.to_string_lossy();
        ["ROS_", "RMW_", "AMENT_", "ZENOH_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
            || name == "LD_LIBRARY_PATH"
    })
}

fn write_reports(
    work: &Path,
    json: &Result<String, serde_json::Error>,
    markdown: &str,
) -> Result<(), String> {
    let json = json.as_ref().map_err(ToString::to_string)?;
    std::fs::write(work.join("report.json"), json).map_err(|error| error.to_string())?;
    std::fs::write(work.join("report.md"), markdown).map_err(|error| error.to_string())?;
    eprintln!("Reports are in {}", work.display());
    Ok(())
}

/// Reads the recording process once a second while `running` holds.
fn sample_while(pid: u32, start: Instant, mut running: impl FnMut() -> bool) -> Vec<Sample> {
    let mut samples = Vec::new();
    while running() {
        if let Some(sample) = measure::sample(pid, start.elapsed()) {
            samples.push(sample);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    samples.extend(measure::sample(pid, start.elapsed()));
    samples
}

fn measured(data: &Path, samples: &[Sample]) -> Result<Measured, String> {
    let episode_dir = std::fs::read_dir(data)
        .map_err(|error| error.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| !path.ends_with(".recording"))
        .ok_or("no finished episode")?;
    let episode = Episode::read(&episode_dir).map_err(|error| error.report())?;
    // Windows of one second; the first starts the clock.
    let windows: Vec<_> = samples
        .windows(2)
        .map(|pair| cores_between(&pair[0], &pair[1]))
        .collect();
    let (first, last) = (
        samples.first().ok_or("no samples")?,
        samples.last().ok_or("no samples")?,
    );
    let (threads, cpu_mean) = cores_between(first, last);
    let cpu_peak = windows.iter().map(|(_, total)| *total).fold(0.0, f64::max);
    let total = |field: fn(&kapto_store::StreamSummary) -> u64| -> u64 {
        episode.streams.iter().map(field).sum()
    };
    Ok(Measured {
        written: total(|stream| stream.written),
        dropped_by_us: total(|stream| stream.dropped_by_us),
        lost_upstream: total(|stream| stream.lost_upstream),
        buffer_peak_batches: episode.buffers.peak_batches,
        buffer_batches: episode.buffers.batches,
        p99_unsynced_ms: episode.durability.p99_unsynced_ns as f64 / 1e6,
        max_unsynced_ms: episode.durability.max_unsynced_ns as f64 / 1e6,
        cpu_cores_mean: cpu_mean,
        cpu_cores_peak: cpu_peak,
        threads: threads.into_iter().collect(),
        memory_peak_mib: last.peak_rss_kib as f64 / 1024.0,
        machine: measure::machine_between(samples),
        streams_written: episode
            .streams
            .iter()
            .map(|stream| (stream.id.clone(), stream.written))
            .collect(),
    })
}

/// The first of `count` consecutive loopback ports free now for UDP and TCP.
fn free_ports(count: u16) -> Result<u16, String> {
    (20_000..60_000_u16)
        .step_by(97)
        .filter(|&base| base.checked_add(count).is_some())
        .find(|&base| {
            (0..count).all(|offset| {
                let address = ("127.0.0.1", base + offset);
                UdpSocket::bind(address).is_ok() && TcpListener::bind(address).is_ok()
            })
        })
        .ok_or_else(|| format!("no {count} consecutive free ports on loopback"))
}

/// A streams.toml for the load generator's streams, numbered like its
/// messages: a 4-byte little-endian sequence number first. The last
/// `tcp_streams` arrive over TCP, each message after its 4-byte length.
fn write_streams(dir: &Path, port: u16, spec: &Spec) -> Result<PathBuf, String> {
    let mut text = String::new();
    for stream in 0..spec.streams {
        let address = format!("127.0.0.1:{}", port + stream);
        let input = if stream >= spec.streams - spec.tcp_streams {
            format!(
                "tcp = {{ listen = \"{address}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}"
            )
        } else {
            format!("udp = {{ bind = \"{address}\" }}")
        };
        let _ = write!(
            text,
            "[streams.joints_{stream}]\nrate_hz = {}\n{input}\n\
             sequence = {{ offset = 0, type = \"u32\" }}\n",
            spec.rate
        );
        if spec.decoded {
            text.push_str(
                "decoder = { kind = \"struct\", fields = [\
                   { name = \"joint_positions\", type = \"f64\", offset = 12, count = 7 }, \
                   { name = \"gripper\", type = \"f64\", offset = 68 } ] }\n",
            );
        }
        if let Some(bytes) = spec.max_message_bytes {
            let _ = writeln!(text, "max_message_bytes = {bytes}");
        }
        text.push('\n');
    }
    if let Some(cameras) = spec.cameras {
        for camera in 0..cameras.count {
            let address = format!("127.0.0.1:{}", port + spec.streams + camera);
            let _ = write!(
                text,
                "[streams.camera_{camera}]\nrate_hz = {}\nmax_message_bytes = {CAMERA_MAX_BYTES}\n\
                 tcp = {{ listen = \"{address}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}\n",
                cameras.fps
            );
            if spec.decoded {
                text.push_str("camera = { format = \"jpeg\" }\n");
            }
            text.push('\n');
        }
    }
    let path = dir.join("streams.toml");
    std::fs::write(&path, text).map_err(|error| error.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cameras_are_tcp_streams_after_the_others_declaring_a_mebibyte_frame() {
        let dir = std::env::temp_dir().join(format!("kapto-bench-cameras-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = Spec {
            streams: 2,
            tcp_streams: 0,
            cameras: Some(Cameras {
                count: 2,
                fps: 60,
                width: 1920,
                height: 1080,
            }),
            rate: 2_000,
            seconds: 1,
            max_message_bytes: None,
            place: Place::Native,
            tap_socket: None,
            decoded: false,
        };
        let path = write_streams(&dir, 30_000, &spec).unwrap();
        let file = kapto_config::ConfigFile {
            contents: std::fs::read_to_string(&path).unwrap(),
            path,
        };
        let streams = kapto_config::parse_streams(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let cameras: Vec<_> = streams.iter().skip(2).collect();
        assert_eq!(cameras.len(), 2);
        assert_eq!(cameras[1].id.as_str(), "camera_1");
        assert_eq!(cameras[1].rate_hz, Some(60));
        assert_eq!(cameras[1].max_message_bytes, Some(1 << 20));
        let tcp = cameras[1].tcp.as_ref().unwrap();
        assert_eq!(
            tcp.endpoint,
            kapto_config::TcpEndpointDeclaration::Listen("127.0.0.1:30003".parse().unwrap())
        );
    }
}
