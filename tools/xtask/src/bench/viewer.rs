// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench viewer`: the reference load recorded while the live
//! view's bridge streams to a page, is stopped, has its page stop reading
//! and is killed, one phase after another in the same episode. Recording
//! must not notice: no message dropped or lost, and capture's CPU in each
//! phase as in the first, when no bridge runs. While the page reads, each
//! frame's delay from its newest message's arrival to it being sent is
//! measured from the frame itself.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::Args;
use serde::Serialize;

use super::measure::{Sample, cores_between, generator_totals};
use super::page::{Delays, Mode, Page, percentiles};
use super::place::Place;
use super::report::Machine;
use super::{Spec, build, free_ports, measured, start_recording, work_dir, write_streams};

/// The reference load, with the live view's bridge started, stopped and
/// killed and its page not reading while it records.
#[derive(Args)]
pub(crate) struct ViewerArgs {
    /// How long each phase lasts, in seconds; the load runs five of them.
    #[arg(long, default_value_t = 12)]
    phase_seconds: u32,
}

/// What the viewer is doing in each phase, in order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No bridge: what recording costs without a viewer.
    None,
    /// The bridge streams to a page that reads every frame.
    Watched,
    /// The bridge is stopped (SIGSTOP), as a process that hangs, its page
    /// still connected.
    Stalled,
    /// The bridge runs again and its page stops reading.
    Unread,
    /// The bridge is killed (SIGKILL).
    Killed,
}

impl Phase {
    const ALL: [Self; 5] = [
        Self::None,
        Self::Watched,
        Self::Stalled,
        Self::Unread,
        Self::Killed,
    ];

    fn describe(self) -> &'static str {
        match self {
            Self::None => "no bridge",
            Self::Watched => "the bridge streaming to a page that reads",
            Self::Stalled => "the bridge stopped",
            Self::Unread => "the bridge running again, its page not reading",
            Self::Killed => "the bridge killed",
        }
    }
}

/// Capture may use this much more CPU in a phase than without a bridge,
/// as a fraction, or `CPU_NOISE_CORES`, whichever is more.
const CPU_TOLERANCE: f64 = 0.10;
const CPU_NOISE_CORES: f64 = 0.05;

/// The live view's whole delay, from a message's arrival to it on screen,
/// should stay under this at the 95th percentile; the bridge's part alone
/// must.
pub(super) const DELAY_P95_MS: f64 = 150.0;

#[derive(Serialize)]
struct ViewerReport {
    machine: Machine,
    streams: u16,
    rate_hz: u32,
    phase_seconds: u32,
    messages_sent: u64,
    generator_kept_up: bool,
    written: u64,
    dropped_by_us: u64,
    lost_upstream: u64,
    phases: Vec<PhaseResult>,
    frames: usize,
    delay_ms: Option<Delays>,
}

#[derive(Serialize)]
struct PhaseResult {
    viewer: &'static str,
    capture_cpu_cores: f64,
    within_tolerance: bool,
}

impl ViewerReport {
    fn all_met(&self) -> bool {
        self.generator_kept_up
            && self.written == self.messages_sent
            && self.dropped_by_us == 0
            && self.lost_upstream == 0
            && self.phases.iter().all(|phase| phase.within_tolerance)
            && self
                .delay_ms
                .as_ref()
                .is_some_and(|delay| delay.p95 < DELAY_P95_MS)
    }

    fn markdown(&self) -> String {
        let machine = &self.machine;
        let mut page = String::from(
            "# Viewer benchmark: recording while the live view fails\n\n\
             Provisional: measured on this machine, not the reference PC. Compare runs \
             only with runs on the same machine.\n\n",
        );
        let _ = writeln!(
            page,
            "## Machine\n\n| | |\n|---|---|\n| CPU | {} |\n| Threads | {} |\n| Memory | {:.1} GiB |\n| Kernel | {} |\n| CPU governor | {} |\n",
            machine.cpu, machine.threads, machine.memory_gib, machine.kernel, machine.cpu_governor
        );
        let _ = writeln!(
            page,
            "## Recording\n\n{} streams at {} Hz over UDP, {} phases of {} s; the load generator {}.\n\n\
             | | |\n|---|---|\n| Sent | {} |\n| Written | {} |\n| Dropped by Kapto | {} |\n| Lost before Kapto | {} |\n",
            self.streams,
            self.rate_hz,
            self.phases.len(),
            self.phase_seconds,
            if self.generator_kept_up {
                "kept up"
            } else {
                "fell behind"
            },
            self.messages_sent,
            self.written,
            self.dropped_by_us,
            self.lost_upstream
        );
        let baseline = self
            .phases
            .first()
            .map_or(0.0, |phase| phase.capture_cpu_cores);
        page.push_str(
            "## Capture's CPU in each phase\n\n| Viewer | Cores | Against no bridge | Within tolerance |\n|---|---|---|---|\n",
        );
        for phase in &self.phases {
            let _ = writeln!(
                page,
                "| {} | {:.3} | {:+.1} % | {} |",
                phase.viewer,
                phase.capture_cpu_cores,
                100.0 * (phase.capture_cpu_cores - baseline) / baseline.max(f64::EPSILON),
                if phase.within_tolerance { "yes" } else { "no" }
            );
        }
        let _ = writeln!(
            page,
            "\nWithin tolerance: at most {:.0} % or {CPU_NOISE_CORES} cores more than with no bridge.\n",
            CPU_TOLERANCE * 100.0
        );
        page.push_str("## Delay from a message's arrival to its frame sent\n\n");
        match &self.delay_ms {
            Some(delay) => {
                let _ = writeln!(
                    page,
                    "Over {} frames read by the page: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms \
                     (p95 must be under {DELAY_P95_MS} ms, the whole live view's budget to the screen).",
                    self.frames, delay.p50, delay.p95, delay.max
                );
            }
            None => page.push_str("The page read no frames.\n"),
        }
        page
    }
}

/// Runs the viewer benchmark and prints its report.
pub(crate) fn run(root: &Path, args: &ViewerArgs) -> ExitCode {
    match measure(root, args) {
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

fn measure(root: &Path, args: &ViewerArgs) -> Result<ViewerReport, String> {
    build(root, Place::Native)?;
    let work = work_dir(root, "viewer");
    std::fs::create_dir_all(&work)
        .map_err(|error| format!("can't create {}: {error}", work.display()))?;
    let phase_count = u32::try_from(Phase::ALL.len()).unwrap_or(u32::MAX);
    // A Unix socket's path has at most 107 bytes, which one deep in a
    // checkout's `target/` can exceed.
    let tap_socket = std::env::temp_dir().join(format!("kapto-bench-{}.sock", std::process::id()));
    let spec = Spec {
        streams: 16,
        tcp_streams: 0,
        cameras: None,
        rate: 2_000,
        seconds: args.phase_seconds * phase_count,
        max_message_bytes: None,
        place: Place::Native,
        tap_socket: Some(tap_socket.clone()),
        decoded: true,
    };
    let port = free_ports(spec.streams)?;
    let streams = write_streams(&work, port, &spec)?;
    let mapping = work.join("mapping.toml");
    std::fs::write(&mapping, mapping_of_first_stream()).map_err(|error| error.to_string())?;
    let binaries = root.join("target/release");

    let (mut record, pid, _, _) = start_recording(&binaries, &streams, &work, &spec, &[])?;
    let start = Instant::now();
    let mut generator = Command::new(binaries.join("kapto-loadgen"))
        .args(["--streams", &spec.streams.to_string()])
        .args(["--rate", &spec.rate.to_string()])
        .args(["--duration", &spec.seconds.to_string()])
        .args(["--target", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start the load generator: {error}"))?;

    let mut viewer = Viewer {
        binaries: &binaries,
        root,
        work: &work,
        streams: &streams,
        mapping: &mapping,
        tap_socket: &tap_socket,
        bridge: None,
        page: None,
    };
    let (phases, samples) = match viewer.run_phases(pid, start, args.phase_seconds) {
        Ok(measured) => measured,
        Err(problem) => {
            viewer.enter(Phase::Killed)?;
            let _ = generator.kill();
            let _ = record.kill();
            return Err(problem);
        }
    };

    let finished = generator
        .wait_with_output()
        .map_err(|error| format!("the load generator failed: {error}"))?;
    let totals = generator_totals(&String::from_utf8_lossy(&finished.stdout))
        .ok_or("the load generator printed no total")?;
    let status = record
        .wait()
        .map_err(|error| format!("kapto record failed: {error}"))?;
    // The bridge was killed, so nothing else removes it.
    let _ = std::fs::remove_file(&tap_socket);
    if !status.success() {
        return Err("kapto record failed".to_owned());
    }
    let recorded = measured(&work.join("data"), &samples)?;
    let delays = viewer.page.map(|page| page.delays_ms()).unwrap_or_default();

    let baseline = phases.first().map_or(0.0, |(_, cores)| *cores);
    let allowed = (baseline * CPU_TOLERANCE).max(CPU_NOISE_CORES);
    let report = ViewerReport {
        machine: Machine::this(Place::Native),
        streams: spec.streams,
        rate_hz: spec.rate,
        phase_seconds: args.phase_seconds,
        messages_sent: totals.sent,
        generator_kept_up: totals.kept_up(),
        written: recorded.written,
        dropped_by_us: recorded.dropped_by_us,
        lost_upstream: recorded.lost_upstream,
        phases: phases
            .into_iter()
            .map(|(phase, cores)| PhaseResult {
                viewer: phase.describe(),
                capture_cpu_cores: cores,
                within_tolerance: cores - baseline <= allowed,
            })
            .collect(),
        frames: delays.len(),
        delay_ms: percentiles(delays),
    };
    super::write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    Ok(report)
}

/// The sample arm driven by the first stream, whose messages carry its
/// joints; the other streams are only load.
pub(super) fn mapping_of_first_stream() -> String {
    let mut text = String::from("[joints]\n");
    for joint in 1..=7 {
        let _ = writeln!(
            text,
            "joint_{joint} = \"joints_0.joint_positions[{}]\"",
            joint - 1
        );
    }
    text.push_str("gripper = \"joints_0.gripper\"\n");
    text
}

/// What the bridge shows: the streams recorded, the mapping onto the
/// sample arm, and the live tap recording sends to.
#[derive(Clone, Copy)]
pub(super) struct Bridged<'a> {
    pub(super) streams: &'a Path,
    pub(super) mapping: &'a Path,
    pub(super) tap_socket: &'a Path,
}

/// Starts `kapto viz-bridge` from `binaries` for the sample arm, streaming
/// to pages on `stream_port`; what it says is in `work/bridge/bridge.log`.
pub(super) fn start_bridge(
    binaries: &Path,
    root: &Path,
    work: &Path,
    bridged: Bridged<'_>,
    stream_port: &str,
) -> Result<Child, String> {
    let bridge_dir: PathBuf = work.join("bridge");
    std::fs::create_dir_all(&bridge_dir).map_err(|error| error.to_string())?;
    // What it says, for when it doesn't start.
    let log =
        std::fs::File::create(bridge_dir.join("bridge.log")).map_err(|error| error.to_string())?;
    Command::new(binaries.join("kapto"))
        .arg("viz-bridge")
        .arg("--robot")
        .arg(root.join("packages/arm7/urdf/arm7.urdf"))
        .arg("--streams")
        .arg(bridged.streams)
        .arg("--mapping")
        .arg(bridged.mapping)
        .args(["--viewer-stream-port", stream_port])
        .arg("--viewer-tap-socket")
        .arg(bridged.tap_socket)
        .arg("--data-dir")
        .arg(&bridge_dir)
        .env_clear()
        .envs(super::ros2_environment())
        .env("HOME", &bridge_dir)
        .stdout(log.try_clone().map_err(|error| error.to_string())?)
        .stderr(log)
        .spawn()
        .map_err(|error| format!("can't start kapto viz-bridge: {error}"))
}

/// Capture's CPU in cores in each phase, and the samples it came from.
type Phased = (Vec<(Phase, f64)>, Vec<Sample>);

/// The bridge and the page watching it, taken from phase to phase.
struct Viewer<'a> {
    binaries: &'a Path,
    root: &'a Path,
    work: &'a Path,
    streams: &'a Path,
    mapping: &'a Path,
    tap_socket: &'a Path,
    bridge: Option<Child>,
    page: Option<Page>,
}

impl Viewer<'_> {
    /// Goes through the phases, each `phase_seconds` from `start`, and
    /// measures the recording process `pid` in each: its CPU in cores, and
    /// the samples taken.
    fn run_phases(
        &mut self,
        pid: u32,
        start: Instant,
        phase_seconds: u32,
    ) -> Result<Phased, String> {
        let mut samples = Vec::new();
        let mut phases = Vec::new();
        for (number, phase) in (0_u32..).zip(Phase::ALL) {
            eprintln!("{}…", phase.describe());
            self.enter(phase)?;
            // A second to settle, then the rest of the phase measured.
            let ends = Duration::from_secs(u64::from(phase_seconds * (number + 1)));
            std::thread::sleep(Duration::from_secs(1));
            let first = super::measure::sample(pid, start.elapsed());
            while start.elapsed() < ends {
                std::thread::sleep(Duration::from_millis(200));
            }
            let last = super::measure::sample(pid, start.elapsed());
            let (Some(first), Some(last)) = (first, last) else {
                return Err("kapto record exited during the benchmark".to_owned());
            };
            phases.push((phase, cores_between(&first, &last).1));
            samples.extend([first, last]);
        }
        Ok((phases, samples))
    }

    fn enter(&mut self, phase: Phase) -> Result<(), String> {
        match phase {
            Phase::None => {}
            Phase::Watched => {
                let stream_port = free_ports(1)?.to_string();
                self.bridge = Some(self.start_bridge(&stream_port)?);
                self.page = Some(Page::connect(&stream_port).map_err(|problem| {
                    format!(
                        "{problem}; what the bridge said is in {}",
                        self.work.join("bridge/bridge.log").display()
                    )
                })?);
            }
            Phase::Stalled => {
                // A bridge that exited would pass for one running.
                if let Some(bridge) = &mut self.bridge
                    && !matches!(bridge.try_wait(), Ok(None))
                {
                    return Err(format!(
                        "kapto viz-bridge exited; what it said is in {}",
                        self.work.join("bridge/bridge.log").display()
                    ));
                }
                self.set_page(Mode::Reading);
                self.signal("-STOP");
            }
            Phase::Unread => {
                self.set_page(Mode::NotReading);
                self.signal("-CONT");
            }
            Phase::Killed => {
                if let Some(mut running) = self.bridge.take() {
                    let _ = running.kill();
                    let _ = running.wait();
                }
            }
        }
        Ok(())
    }

    fn start_bridge(&self, stream_port: &str) -> Result<Child, String> {
        start_bridge(
            self.binaries,
            self.root,
            self.work,
            Bridged {
                streams: self.streams,
                mapping: self.mapping,
                tap_socket: self.tap_socket,
            },
            stream_port,
        )
    }

    fn set_page(&self, mode: Mode) {
        if let Some(page) = &self.page {
            page.set(mode);
        }
    }

    fn signal(&self, which: &str) {
        if let Some(process) = &self.bridge {
            let _ = Command::new("kill")
                .args([which, &process.id().to_string()])
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_arm_follows_the_first_stream() {
        let mapping = mapping_of_first_stream();
        assert!(mapping.contains("joint_7 = \"joints_0.joint_positions[6]\"\n"));
        assert!(mapping.ends_with("gripper = \"joints_0.gripper\"\n"));
    }
}
