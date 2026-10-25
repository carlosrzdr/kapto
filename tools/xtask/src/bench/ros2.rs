// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench ros2`: the reference load as a ROS 2 robot sends it,
//! a `sensor_msgs/msg/JointState` topic per stream at 2 kHz, recorded by
//! `kapto record` while the live view's bridge moves the sample arm with
//! the first and streams to a page that reads every frame. It is held to
//! the targets the same load over UDP is, and the whole of Kapto with the
//! live view open to at most two cores.
//!
//! Kapto speaks ROS 2 only in the ROS 2 image, so outside it the image is
//! built and the benchmark runs itself there, over the middleware asked
//! for, the repository mounted and the binaries built into `target/ros2/`;
//! the reports land in `target/bench/`, as the others' do. The load
//! generator waits for Kapto's subscriptions before its first message, so
//! every message sent can be asked to be written; CPU is measured from its
//! first second of sending.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Args;
use serde::Serialize;

use super::measure::{self, GeneratorTotals, Sample, cores_between, generator_totals};
use super::page::{Delays, Page, percentiles};
use super::place::Place;
use super::report::{Check, Load, Machine, Report, Targets};
use super::viewer::{Bridged, DELAY_P95_MS, mapping_of_first_stream, start_bridge};
use super::{
    CAMERA_MAX_BYTES, Cameras, Spec, free_ports, measured, start_recording, work_dir, write_reports,
};
use crate::ros2::{self as image, Middleware};

/// The reference load as ROS 2 topics, with the live view open.
#[derive(Args)]
pub(crate) struct Ros2Args {
    /// Number of topics, each a stream.
    #[arg(long, default_value_t = 16)]
    streams: u16,
    /// How long the load runs, in seconds.
    #[arg(long, default_value_t = 60)]
    seconds: u32,
    /// The middleware Kapto and the synthetic robot talk over.
    #[arg(long, value_enum, default_value_t = Middleware::Fastrtps)]
    middleware: Middleware,
    /// Cameras published beside the joint states, each a
    /// `sensor_msgs/msg/CompressedImage` topic of JPEG frames.
    #[arg(long, default_value_t = 0)]
    cameras: u16,
    /// Frames a second from each camera.
    #[arg(long, default_value_t = 60)]
    camera_fps: u32,
    /// Each camera's frame size, e.g. 1920x1080.
    #[arg(long, default_value = "1920x1080", value_parser = super::frame_size)]
    camera_size: (u16, u16),
    /// Keep the recording in `target/bench/` once it is measured. Without
    /// it the recording is removed, as one with cameras takes gigabytes; the
    /// reports stay.
    #[arg(long)]
    keep_recordings: bool,
    /// An environment variable for the run, `KEY=VALUE`, set for Kapto and
    /// the synthetic robot alike, e.g. a middleware's settings
    /// (`ZENOH_CONFIG_OVERRIDE='transport/shared_memory/enabled=true'`);
    /// the report names each. May be given more than once.
    #[arg(long = "env", value_name = "KEY=VALUE", value_parser = environment_variable)]
    env: Vec<String>,
}

/// `text` if it is `KEY=VALUE` with a key a shell takes.
fn environment_variable(text: &str) -> Result<String, String> {
    let (key, _) = text.split_once('=').ok_or("expected KEY=VALUE")?;
    let valid = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if valid {
        Ok(text.to_owned())
    } else {
        Err(format!(
            "`{key}` isn't a name an environment variable can have"
        ))
    }
}

/// `text` quoted for a POSIX shell.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

impl Ros2Args {
    /// The cameras asked for, if any.
    fn cameras(&self) -> Option<Cameras> {
        (self.cameras > 0).then_some(Cameras {
            count: self.cameras,
            fps: self.camera_fps,
            width: self.camera_size.0,
            height: self.camera_size.1,
        })
    }
}

/// Messages per second on each topic.
const RATE_HZ: u32 = 2_000;

/// How long the load generator may wait for Kapto's subscriptions; the
/// recording lasts this much longer than the load.
const DISCOVERY_SECONDS: u32 = 10;

/// The whole of Kapto while recording with the live view open, in cores.
const WHOLE_CORES: f64 = 2.0;

#[derive(Serialize)]
struct Ros2Report {
    #[serde(flatten)]
    report: Report,
    middleware: &'static str,
    bridge: BridgeMeasured,
    /// Each stream: what the synthetic robot sent on it and what was written.
    streams: Vec<StreamCounted>,
}

/// One stream's messages, sent and written.
#[derive(Serialize)]
struct StreamCounted {
    stream: String,
    sent: u64,
    written: u64,
}

/// Each stream's messages sent and written: every joint topic was sent the
/// same share of `joints_sent`, every camera the same of `camera_frames`.
fn counted(written: &[(String, u64)], joints_sent: u64, camera_frames: u64) -> Vec<StreamCounted> {
    let cameras = written
        .iter()
        .filter(|(id, _)| id.starts_with("camera_"))
        .count() as u64;
    let joints = written.len() as u64 - cameras;
    written
        .iter()
        .map(|(id, written)| StreamCounted {
            stream: id.clone(),
            sent: if id.starts_with("camera_") {
                camera_frames / cameras.max(1)
            } else {
                joints_sent / joints.max(1)
            },
            written: *written,
        })
        .collect()
}

/// The live view's bridge, over the same seconds as recording.
#[derive(Serialize)]
struct BridgeMeasured {
    cpu_cores_mean: f64,
    memory_peak_mib: f64,
    frames: usize,
    delay_ms: Option<Delays>,
}

impl Ros2Report {
    /// The report of a run in which the joint topics were sent `joints_sent`
    /// messages and the cameras `camera_frames`.
    fn new(
        mut report: Report,
        middleware: Middleware,
        bridge: BridgeMeasured,
        (joints_sent, camera_frames): (u64, u64),
    ) -> Self {
        let streams = counted(&report.measured.streams_written, joints_sent, camera_frames);
        // The robot is on this machine and every message it sent can be
        // counted, so anything counted lost before Kapto never was.
        let lost = report.measured.lost_upstream;
        report.checks.push(Check {
            target: "Nothing counted lost before Kapto, every message having arrived",
            measured: format!("{lost} counted lost"),
            met: lost == 0,
        });
        let recording = report.measured.cpu_cores_mean;
        let whole = recording + bridge.cpu_cores_mean;
        report.checks.push(Check {
            target: "Recording with the live view open uses at most two cores",
            measured: format!(
                "{whole:.2} cores on average: recording {recording:.2}, the live view's bridge {:.2}",
                bridge.cpu_cores_mean
            ),
            met: whole <= WHOLE_CORES,
        });
        report.checks.push(Check {
            target: "95 % of frames sent within 150 ms of their newest message's arrival",
            measured: bridge.delay_ms.as_ref().map_or_else(
                || "no frame read".to_owned(),
                |delay| format!("{:.1} ms for 95 % of {} frames", delay.p95, bridge.frames),
            ),
            met: bridge
                .delay_ms
                .as_ref()
                .is_some_and(|delay| delay.p95 < DELAY_P95_MS),
        });
        Self {
            report,
            middleware: middleware.name(),
            bridge,
            streams,
        }
    }

    fn markdown(&self) -> String {
        let mut page = self.report.markdown();
        let short: Vec<&StreamCounted> = self
            .streams
            .iter()
            .filter(|stream| stream.written != stream.sent)
            .collect();
        if short.is_empty() {
            page.push_str("\n## Each stream\n\nEvery stream wrote every message sent on it.\n");
        } else {
            page.push_str(
                "\n## Each stream\n\nThe streams that didn't write every message sent on them:\n\n\
                 | Stream | Sent | Written | Missing |\n|---|---|---|---|\n",
            );
            for stream in short {
                let _ = writeln!(
                    page,
                    "| `{}` | {} | {} | {} |",
                    stream.stream,
                    stream.sent,
                    stream.written,
                    i128::from(stream.sent) - i128::from(stream.written)
                );
            }
        }
        let bridge = &self.bridge;
        let _ = write!(
            page,
            "\n\n## The live view's bridge\n\nCPU {:.2} cores on average; memory peaked at {:.0} MiB. \
             The page read {} frames",
            bridge.cpu_cores_mean, bridge.memory_peak_mib, bridge.frames
        );
        match &bridge.delay_ms {
            Some(delay) => {
                let _ = writeln!(
                    page,
                    ", sent p50 {:.2} ms, p95 {:.2} ms, at most {:.2} ms after their newest message arrived.",
                    delay.p50, delay.p95, delay.max
                );
            }
            None => page.push_str(".\n"),
        }
        page
    }
}

/// Runs the benchmark, in the ROS 2 image unless ROS 2 is set up here.
pub(crate) fn run(root: &Path, args: &Ros2Args) -> ExitCode {
    if std::env::var_os("ROS_DISTRO").is_none() {
        // Exported before the run, so the synthetic robot and Kapto both
        // have them; passed on as flags too, for the report to name them.
        let (mut exported, mut flags) = (String::new(), String::new());
        for variable in &args.env {
            let _ = write!(exported, "export {} && ", quoted(variable));
            let _ = write!(flags, " --env {}", quoted(variable));
        }
        let script = format!(
            "{exported}cargo xtask bench ros2 --streams {} --seconds {} --middleware {} \
             --cameras {} --camera-fps {} --camera-size {}x{}{}{flags}",
            args.streams,
            args.seconds,
            args.middleware.name(),
            args.cameras,
            args.camera_fps,
            args.camera_size.0,
            args.camera_size.1,
            if args.keep_recordings {
                " --keep-recordings"
            } else {
                ""
            }
        );
        return match image::build(root).and_then(|()| image::run_in(root, args.middleware, &script))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(problem) => {
                eprintln!("The ROS 2 benchmark in {} failed: {problem}", image::IMAGE);
                ExitCode::FAILURE
            }
        };
    }
    match measure_load(root, args) {
        Ok(report) => {
            println!("{}", report.markdown());
            if report.report.all_met() {
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

fn measure_load(root: &Path, args: &Ros2Args) -> Result<Ros2Report, String> {
    let binaries = build(root)?;
    let work = work_dir(root, "ros2");
    std::fs::create_dir_all(&work)
        .map_err(|error| format!("can't create {}: {error}", work.display()))?;
    // The message types' definitions, read from beside streams.toml.
    copy_folder(&root.join("packages/arm7_ros2/msg"), &work.join("msg"))?;
    let streams = work.join("streams.toml");
    std::fs::write(&streams, streams_toml(args.streams, args.cameras()))
        .map_err(|error| error.to_string())?;
    let mapping = work.join("mapping.toml");
    std::fs::write(&mapping, mapping_of_first_stream()).map_err(|error| error.to_string())?;
    // A Unix socket's path has at most 107 bytes, which one deep in a
    // checkout's `target/` can exceed.
    let tap_socket = std::env::temp_dir().join(format!("kapto-bench-{}.sock", std::process::id()));
    let spec = Spec {
        streams: args.streams,
        tcp_streams: 0,
        cameras: None,
        rate: RATE_HZ,
        seconds: args.seconds + DISCOVERY_SECONDS,
        max_message_bytes: None,
        place: Place::Native,
        tap_socket: Some(tap_socket.clone()),
        decoded: true,
    };

    let (mut record, pid, _, _) = start_recording(&binaries, &streams, &work, &spec, &[])?;
    let stream_port = free_ports(1)?.to_string();
    let bridged = Bridged {
        streams: &streams,
        mapping: &mapping,
        tap_socket: &tap_socket,
    };
    let mut bridge = start_bridge(&binaries, root, &work, bridged, &stream_port)?;
    let page = Page::connect(&stream_port).map_err(|problem| {
        let _ = bridge.kill();
        let _ = record.kill();
        format!(
            "{problem}; what the bridge said is in {}",
            work.join("bridge/bridge.log").display()
        )
    })?;
    let sent = send(&binaries, args, pid, bridge.id());
    let _ = bridge.kill();
    let _ = bridge.wait();
    let recorded = record
        .wait()
        .map_err(|error| format!("kapto record failed: {error}"))?;
    let _ = std::fs::remove_file(&tap_socket);
    let Sent {
        totals,
        camera_frames,
        recording,
        bridge: bridged_samples,
    } = sent?;
    if !recorded.success() {
        return Err("kapto record failed".to_owned());
    }

    let (Some(first), Some(last)) = (bridged_samples.first(), bridged_samples.last()) else {
        return Err("the load ran under a second".to_owned());
    };
    let delays = page.delays_ms();
    let bridge = BridgeMeasured {
        cpu_cores_mean: cores_between(first, last).1,
        memory_peak_mib: last.peak_rss_kib as f64 / 1024.0,
        frames: delays.len(),
        delay_ms: percentiles(delays),
    };
    let mut machine = Machine::this(Place::Native);
    machine.runs_in = format!(
        "the {} image, over {}{}",
        image::IMAGE,
        args.middleware.name(),
        if args.env.is_empty() {
            String::new()
        } else {
            format!(", with {}", args.env.join(", "))
        }
    );
    let load = Load {
        streams: args.streams,
        tcp_streams: 0,
        cameras: args.cameras(),
        rate_hz: RATE_HZ,
        seconds: args.seconds,
        messages_sent: totals.sent,
    };
    let report = Report::new(
        Targets::Ros2,
        machine,
        load,
        measured(&work.join("data"), &recording)?,
    );
    let sent = (totals.sent - camera_frames, camera_frames);
    let report = Ros2Report::new(report, args.middleware, bridge, sent);
    write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    super::tidy(&work, args.keep_recordings);
    Ok(report)
}

/// What the load generator sent, and recording's and the bridge's samples
/// over the same seconds.
struct Sent {
    totals: GeneratorTotals,
    /// Of `totals.sent`, the cameras' frames.
    camera_frames: u64,
    recording: Vec<Sample>,
    bridge: Vec<Sample>,
}

/// Runs the load generator, sampling recording (`pid`) and the bridge
/// (`bridge_pid`) together once a second from its first second of sending.
fn send(binaries: &Path, args: &Ros2Args, pid: u32, bridge_pid: u32) -> Result<Sent, String> {
    let start = Instant::now();
    let cameras = start_cameras(binaries, args)?;
    let mut generator = Command::new(binaries.join("kapto-loadgen"))
        .args(["--transport", "ros2"])
        .args(["--streams", &args.streams.to_string()])
        .args(["--rate", &RATE_HZ.to_string()])
        .args(["--duration", &args.seconds.to_string()])
        .args(["--wait-for-subscribers", &DISCOVERY_SECONDS.to_string()])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start the load generator: {error}"))?;
    let stdout = generator
        .stdout
        .take()
        .ok_or("the load generator has no stdout")?;
    let (lines_tx, lines) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = lines_tx.send(line);
        }
    });
    let (mut recording, mut bridge) = (Vec::new(), Vec::new());
    let mut sending = false;
    while matches!(generator.try_wait(), Ok(None)) {
        sending = sending || lines.try_iter().any(|line| reports_a_second(&line));
        if sending {
            let at = start.elapsed();
            let (Some(kapto), Some(viewer)) =
                (measure::sample(pid, at), measure::sample(bridge_pid, at))
            else {
                let _ = generator.kill();
                return Err(
                    "kapto record or its live view's bridge exited during the benchmark".to_owned(),
                );
            };
            recording.push(kapto);
            bridge.push(viewer);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = reader.join();
    let printed: Vec<String> = lines.try_iter().collect();
    let status = generator.wait().map_err(|error| error.to_string())?;
    let mut totals = match generator_totals(&printed.join("\n")) {
        Some(totals) if status.success() => totals,
        _ => return Err(format!("the load generator failed: {printed:#?}")),
    };
    let mut camera_frames = 0;
    if let Some((mut started, reader, lines, mut printed)) = cameras {
        let status = started.wait().map_err(|error| error.to_string())?;
        let _ = reader.join();
        printed.extend(lines.try_iter());
        let printed = printed.join("\n");
        let sent = match generator_totals(&printed) {
            Some(sent) if status.success() => sent,
            _ => return Err(format!("the cameras failed: {printed}")),
        };
        if !sent.kept_up() {
            return Err(format!(
                "the cameras fell behind ({} frames skipped, {} send errors), so the load wasn't the one asked for",
                sent.skipped, sent.send_errors
            ));
        }
        totals.sent += sent.sent;
        camera_frames = sent.sent;
    }
    if !totals.kept_up() {
        // The seconds it fell behind in, to tell a slow start from a stall.
        let behind: Vec<&str> = printed
            .iter()
            .filter(|line| reports_a_second(line) && !line.contains("skipped 0 "))
            .map(|line| line.trim())
            .collect();
        return Err(format!(
            "the joint states fell behind ({} ticks skipped, {} send errors), so the load wasn't the one asked for: {behind:#?}",
            totals.skipped, totals.send_errors
        ));
    }
    Ok(Sent {
        totals,
        camera_frames,
        recording,
        bridge,
    })
}

/// The cameras' generator, its reader thread, its output and what it
/// printed so far.
type Sending = (
    std::process::Child,
    std::thread::JoinHandle<()>,
    mpsc::Receiver<String>,
    Vec<String>,
);

/// Starts the cameras, if any, and returns once they are sending.
fn start_cameras(binaries: &Path, args: &Ros2Args) -> Result<Option<Sending>, String> {
    // The cameras start first, and the joint states once they are sending:
    // encoding a second of frames takes every core for a moment, which
    // would hold the joint states up as they start. They run on two
    // seconds longer, so every second the joint states are measured has
    // both.
    let Some(cameras) = args.cameras() else {
        return Ok(None);
    };
    let mut started = Command::new(binaries.join("kapto-loadgen"))
        .args(["--transport", "ros2"])
        .args(["--streams", &cameras.count.to_string()])
        .args(["--rate", &cameras.fps.to_string()])
        .args(["--duration", &(args.seconds + 2).to_string()])
        .args(["--wait-for-subscribers", &DISCOVERY_SECONDS.to_string()])
        .args(["--camera", &format!("{}x{}", cameras.width, cameras.height)])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start the cameras: {error}"))?;
    let stdout = started.stdout.take().ok_or("the cameras have no stdout")?;
    let (lines_tx, lines) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = lines_tx.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(u64::from(DISCOVERY_SECONDS) + 5);
    let mut printed = Vec::new();
    while !printed.iter().any(|line: &String| reports_a_second(line)) {
        let Ok(line) = lines.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        else {
            let _ = started.kill();
            return Err(format!("the cameras never started sending: {printed:#?}"));
        };
        printed.push(line);
    }
    Ok(Some((started, reader, lines, printed)))
}

/// Builds Kapto and the load generator with ROS 2, returning where they are.
fn build(root: &Path) -> Result<PathBuf, String> {
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--quiet", "--locked"])
        .args(["-p", "kapto-cli", "-p", "kapto-loadgen"])
        .args(["--features", "kapto-cli/ros2,kapto-loadgen/ros2"])
        .current_dir(root)
        .status()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if !built.success() {
        return Err("the release build with ROS 2 failed".to_owned());
    }
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    Ok(target.join("release"))
}

/// Whether `line` is one of the load generator's once-a-second reports,
/// `     1 s  …`, which it prints only once it is sending.
fn reports_a_second(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next().is_some_and(|word| word.parse::<u64>().is_ok()) && words.next() == Some("s")
}

/// A streams.toml of `streams` topics, named as the load generator
/// publishes them and with its quality of service, each decoded as the sample arm's
/// joints and gripper, as `packages/arm7_ros2` declares them; then each
/// camera's compressed frames, on the topics the generator names them.
fn streams_toml(streams: u16, cameras: Option<Cameras>) -> String {
    let mut text = String::new();
    for stream in 0..streams {
        let topic = match stream {
            0 => "/joint_states".to_owned(),
            _ => format!("/joint_states_{stream}"),
        };
        let _ = write!(
            text,
            "[streams.joints_{stream}]\nrate_hz = {RATE_HZ}\n\
             ros2 = {{ topic = \"{topic}\", type = \"sensor_msgs/msg/JointState\", \
             qos = {{ reliability = \"reliable\", depth = 1000 }} }}\n\
             decoder = {{ kind = \"cdr\", fields = [\
               {{ name = \"joint_positions\", type = \"f64\", path = \"position\", count = 7 }}, \
               {{ name = \"gripper\", type = \"f64\", path = \"position[7]\" }} ] }}\n\n"
        );
    }
    for camera in 0..cameras.map_or(0, |cameras| cameras.count) {
        let topic = match camera {
            0 => "/camera/image_raw/compressed".to_owned(),
            _ => format!("/camera_{camera}/image_raw/compressed"),
        };
        let _ = write!(
            text,
            "[streams.camera_{camera}]\nrate_hz = {}\nmax_message_bytes = {CAMERA_MAX_BYTES}\n\
             ros2 = {{ topic = \"{topic}\", type = \"sensor_msgs/msg/CompressedImage\", \
             qos = {{ reliability = \"reliable\", depth = 10 }} }}\n\n",
            cameras.map_or(0, |cameras| cameras.fps)
        );
    }
    text
}

/// Copies the folder `from`, and everything in it, to `to`.
fn copy_folder(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to)
        .map_err(|error| format!("can't create {}: {error}", to.display()))?;
    for entry in std::fs::read_dir(from)
        .map_err(|error| format!("can't read {}: {error}", from.display()))?
    {
        let entry = entry.map_err(|error| error.to_string())?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|error| format!("can't copy to {}: {error}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_topics_are_declared_as_the_generator_publishes_them_and_decode_with_the_arms_messages() {
        let folder = std::env::temp_dir().join(format!("kapto-bench-ros2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        copy_folder(&root().join("packages/arm7_ros2/msg"), &folder.join("msg")).unwrap();
        let path = folder.join("streams.toml");
        std::fs::write(&path, streams_toml(3, None)).unwrap();
        let file = kapto_config::ConfigFile {
            contents: std::fs::read_to_string(&path).unwrap(),
            path,
        };
        let streams = kapto_config::parse_streams(&file).unwrap();
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(streams.len(), 3);
        let text = streams_toml(3, None);
        assert!(text.contains("topic = \"/joint_states\""));
        assert!(text.contains("topic = \"/joint_states_2\""));
    }

    #[test]
    fn sending_starts_with_the_generators_first_report() {
        assert!(reports_a_second("     1 s  32000 msg  late 0"));
        assert!(!reports_a_second(
            "Sending 16 stream(s) at 2000 Hz over ros2 to topics …"
        ));
        assert!(!reports_a_second("total   1920000 msg  late 12"));
    }

    fn report(recording: f64, bridge: f64, delay: Option<f64>) -> Ros2Report {
        report_losing(0, recording, bridge, delay)
    }

    fn report_losing(lost: u64, recording: f64, bridge: f64, delay: Option<f64>) -> Ros2Report {
        let report = Report::new(
            Targets::Ros2,
            Machine::this(Place::Native),
            Load {
                streams: 16,
                tcp_streams: 0,
                cameras: None,
                rate_hz: RATE_HZ,
                seconds: 60,
                messages_sent: 10,
            },
            super::super::report::Measured {
                written: 10,
                dropped_by_us: 0,
                lost_upstream: lost,
                buffer_peak_batches: 1,
                buffer_batches: 100,
                p99_unsynced_ms: 100.0,
                max_unsynced_ms: 200.0,
                cpu_cores_mean: recording,
                cpu_cores_peak: recording,
                threads: Vec::new(),
                memory_peak_mib: 100.0,
                machine: measure::MachineMeasured::default(),
                streams_written: Vec::new(),
            },
        );
        let delay_ms = delay.map(|p95| Delays {
            p50: p95 / 2.0,
            p95,
            max: p95,
        });
        Ros2Report::new(
            report,
            Middleware::Fastrtps,
            BridgeMeasured {
                cpu_cores_mean: bridge,
                memory_peak_mib: 50.0,
                frames: usize::from(delay.is_some()),
                delay_ms,
            },
            (0, 0),
        )
    }

    #[test]
    fn recording_and_the_bridge_share_two_cores_frames_are_prompt_and_nothing_is_counted_lost() {
        assert!(report(0.6, 0.3, Some(20.0)).report.all_met());
        assert!(
            !report(0.9, 1.2, Some(20.0)).report.all_met(),
            "over two cores"
        );
        assert!(
            !report(0.6, 0.3, Some(160.0)).report.all_met(),
            "late frames"
        );
        assert!(!report(0.6, 0.3, None).report.all_met(), "no frames read");
        assert!(
            !report_losing(14, 0.6, 0.3, Some(20.0)).report.all_met(),
            "counted lost"
        );
    }

    #[test]
    fn an_environment_variable_is_a_name_then_its_value_quoted_for_the_shell() {
        assert_eq!(
            environment_variable("ZENOH_CONFIG_OVERRIDE=transport/shared_memory/enabled=true"),
            Ok("ZENOH_CONFIG_OVERRIDE=transport/shared_memory/enabled=true".to_owned())
        );
        assert!(environment_variable("no-equals").is_err());
        assert!(environment_variable("1BAD=x").is_err());
        assert!(environment_variable("A B=x").is_err());
        assert_eq!(quoted("A=it's"), "'A=it'\\''s'");
    }

    #[test]
    fn each_stream_is_held_to_its_share_of_what_was_sent_and_a_short_one_is_named() {
        let written = [
            ("joints_0".to_owned(), 100),
            ("joints_1".to_owned(), 98),
            ("camera_0".to_owned(), 60),
            ("camera_1".to_owned(), 57),
        ];
        let sent: Vec<u64> = counted(&written, 200, 120)
            .iter()
            .map(|stream| stream.sent)
            .collect();
        assert_eq!(sent, [100, 100, 60, 60]);
        let mut measured = report(0.5, 0.2, None).report;
        measured.measured.streams_written = written.to_vec();
        let page = Ros2Report::new(
            measured,
            Middleware::Zenoh,
            BridgeMeasured {
                cpu_cores_mean: 0.1,
                memory_peak_mib: 50.0,
                frames: 0,
                delay_ms: None,
            },
            (200, 120),
        )
        .markdown();
        assert!(page.contains("| `joints_1` | 100 | 98 | 2 |"), "{page}");
        assert!(page.contains("| `camera_1` | 60 | 57 | 3 |"), "{page}");
        assert!(!page.contains("| `joints_0`"), "{page}");
    }
}
