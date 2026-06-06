// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench upload --while-recording`: the reference load
//! recorded by `kapto record` in two phases, the first with nothing else
//! running, the second while episodes go up to the RustFS server of
//! `compose.yaml` one after another, as the worker uploads them: with the
//! default settings, at its priority (nice 10, the idle I/O class), each
//! episode's checksums worked out first. Recording must not notice: every
//! message written, none dropped, buffers at most half full, and capture's
//! CPU while uploads run as without them.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::measure::{self, cores_between, generator_totals};
use super::place::Place;
use super::report::Machine;
use super::upload::{Run, Upload, program, upload_once};
use super::{Spec, build, free_ports, measured, start_recording, work_dir, write_streams};
use crate::storage;

/// Capture may use this much more CPU while uploads run than without, as
/// a fraction, or `CPU_NOISE_CORES`, whichever is more; as the viewer
/// benchmark allows its bridge.
const CPU_TOLERANCE: f64 = 0.10;
const CPU_NOISE_CORES: f64 = 0.05;

#[derive(Serialize)]
struct UploadingReport {
    machine: Machine,
    streams: u16,
    rate_hz: u32,
    phase_seconds: u32,
    messages_sent: u64,
    generator_kept_up: bool,
    written: u64,
    dropped_by_us: u64,
    buffer_peak_percent: f64,
    capture_cpu_cores_alone: f64,
    capture_cpu_cores_uploading: f64,
    episode_mib: u32,
    uploads: Vec<Run>,
}

impl UploadingReport {
    fn cpu_within_tolerance(&self) -> bool {
        let allowed = (self.capture_cpu_cores_alone * CPU_TOLERANCE).max(CPU_NOISE_CORES);
        self.capture_cpu_cores_uploading - self.capture_cpu_cores_alone <= allowed
    }

    fn all_met(&self) -> bool {
        self.generator_kept_up
            && self.written == self.messages_sent
            && self.dropped_by_us == 0
            && self.buffer_peak_percent <= 50.0
            && self.cpu_within_tolerance()
            && !self.uploads.is_empty()
    }

    fn markdown(&self) -> String {
        let machine = &self.machine;
        let mut page = String::from(
            "# Recording while uploading\n\n\
             Provisional: measured on this machine, not the reference PC, uploading to a \
             server on the same machine, its disk the recording's. Compare runs only with \
             runs on the same machine.\n\n",
        );
        let _ = writeln!(
            page,
            "## Machine\n\n| | |\n|---|---|\n| CPU | {} |\n| Threads | {} |\n| Memory | {:.1} GiB |\n| Kernel | {} |\n| CPU governor | {} |\n",
            machine.cpu, machine.threads, machine.memory_gib, machine.kernel, machine.cpu_governor
        );
        let uploaded_mib: f64 = self.uploads.iter().map(Run::mib).sum();
        let uploading_seconds: f64 = self.uploads.iter().map(|run| run.seconds).sum();
        let _ = writeln!(
            page,
            "## Recording\n\n{} streams at {} Hz over UDP, a phase of {} s with nothing else \
             running and one while {} episodes of {} MiB went up one after another, as the \
             worker uploads them ({:.0} MiB at {:.0} MiB/s); the load generator {}.\n\n\
             | Target | Measured | Met |\n|---|---|---|",
            self.streams,
            self.rate_hz,
            self.phase_seconds,
            self.uploads.len(),
            self.episode_mib,
            uploaded_mib,
            uploaded_mib / uploading_seconds.max(f64::EPSILON),
            if self.generator_kept_up {
                "kept up"
            } else {
                "fell behind"
            }
        );
        let met = |met: bool| if met { "yes" } else { "**no**" };
        let _ = writeln!(
            page,
            "| Every message sent is written; none dropped by Kapto | {} of {} written, {} dropped | {} |",
            self.written,
            self.messages_sent,
            self.dropped_by_us,
            met(self.written == self.messages_sent && self.dropped_by_us == 0)
        );
        let _ = writeln!(
            page,
            "| Buffers never more than half full | {:.1} % at peak | {} |",
            self.buffer_peak_percent,
            met(self.buffer_peak_percent <= 50.0)
        );
        let _ = writeln!(
            page,
            "| Capture's CPU while uploading as without, within {:.0} % or {CPU_NOISE_CORES} cores | \
             {:.3} cores without, {:.3} while uploading | {} |",
            CPU_TOLERANCE * 100.0,
            self.capture_cpu_cores_alone,
            self.capture_cpu_cores_uploading,
            met(self.cpu_within_tolerance())
        );
        page
    }
}

/// Runs the benchmark and prints its report.
pub(crate) fn run(root: &Path, phase_seconds: u32, mib: u32) -> ExitCode {
    match measure_phases(root, phase_seconds, mib) {
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

fn measure_phases(root: &Path, phase_seconds: u32, mib: u32) -> Result<UploadingReport, String> {
    build(root, Place::Native)?;
    let program = program(root)?;
    let work = work_dir(root, "uploading");
    std::fs::create_dir_all(&work)
        .map_err(|error| format!("can't create {}: {error}", work.display()))?;
    let folder = work.join("episode");
    let report = storage::with_server(root, |address| {
        let upload = Upload {
            program: &program,
            address,
            folder: &folder,
            mib,
            setting: None,
            low_priority: true,
        };
        // Made, and the server warmed, before recording starts.
        eprintln!("Making a {mib} MiB episode to upload");
        upload_once(&upload, "warm-up")?;
        record_while(root, &work, &upload, phase_seconds)
    });
    let _ = std::fs::remove_dir_all(&folder);
    let report = report?;
    super::write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    Ok(report)
}

/// Records the reference load for two phases, uploading `upload`'s episode
/// over and over in the second.
fn record_while(
    root: &Path,
    work: &Path,
    upload: &Upload<'_>,
    phase_seconds: u32,
) -> Result<UploadingReport, String> {
    let spec = Spec {
        streams: 16,
        tcp_streams: 0,
        cameras: None,
        rate: 2_000,
        seconds: phase_seconds * 2,
        max_message_bytes: None,
        place: Place::Native,
        tap_socket: None,
        decoded: false,
    };
    let port = free_ports(spec.streams)?;
    let streams = write_streams(work, port, &spec)?;
    let binaries = root.join("target/release");
    let (mut record, pid, _, _) = start_recording(&binaries, &streams, work, &spec)?;
    let start = Instant::now();
    let generator = Command::new(binaries.join("kapto-loadgen"))
        .args(["--streams", &spec.streams.to_string()])
        .args(["--rate", &spec.rate.to_string()])
        .args(["--duration", &spec.seconds.to_string()])
        .args(["--target", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't start the load generator: {error}"))?;

    let phase = |number: u32| -> Result<(measure::Sample, measure::Sample), String> {
        // A second to settle, then the rest of the phase measured.
        std::thread::sleep(Duration::from_secs(1));
        let first = measure::sample(pid, start.elapsed());
        let ends = Duration::from_secs(u64::from(phase_seconds * number));
        while start.elapsed() < ends {
            std::thread::sleep(Duration::from_millis(200));
        }
        let last = measure::sample(pid, start.elapsed());
        first
            .zip(last)
            .ok_or_else(|| "kapto record exited during the benchmark".to_owned())
    };
    eprintln!("Recording with nothing else running…");
    let alone = phase(1);
    eprintln!("Recording while uploading…");
    let stop = AtomicBool::new(false);
    let (uploading, uploads) = std::thread::scope(|scope| {
        let uploader = scope.spawn(|| {
            let mut uploads = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                match upload_once(upload, &format!("while-{}", uploads.len())) {
                    Ok(run) => uploads.push(run),
                    Err(problem) => return Err(problem),
                }
            }
            Ok(uploads)
        });
        let measured = phase(2);
        stop.store(true, Ordering::Relaxed);
        let uploads = uploader
            .join()
            .map_err(|_| "the uploads stopped unexpectedly".to_owned())
            .and_then(|uploads| uploads);
        (measured, uploads)
    });

    let finished = generator
        .wait_with_output()
        .map_err(|error| format!("the load generator failed: {error}"))?;
    let totals = generator_totals(&String::from_utf8_lossy(&finished.stdout))
        .ok_or("the load generator printed no total")?;
    let status = record
        .wait()
        .map_err(|error| format!("kapto record failed: {error}"))?;
    if !status.success() {
        return Err("kapto record failed".to_owned());
    }
    let (alone, uploading, uploads) = (alone?, uploading?, uploads?);
    let recorded = measured(&work.join("data"), &[alone.0.clone(), uploading.1.clone()])?;
    #[allow(clippy::cast_precision_loss, reason = "batches shown as a percentage")]
    let buffer_peak_percent =
        100.0 * recorded.buffer_peak_batches as f64 / recorded.buffer_batches.max(1) as f64;
    Ok(UploadingReport {
        machine: Machine::this(Place::Native),
        streams: spec.streams,
        rate_hz: spec.rate,
        phase_seconds,
        messages_sent: totals.sent,
        generator_kept_up: totals.kept_up(),
        written: recorded.written,
        dropped_by_us: recorded.dropped_by_us,
        buffer_peak_percent,
        capture_cpu_cores_alone: cores_between(&alone.0, &alone.1).1,
        capture_cpu_cores_uploading: cores_between(&uploading.0, &uploading.1).1,
        episode_mib: upload.mib,
        uploads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(dropped: u64, alone: f64, uploading: f64, uploads: usize) -> UploadingReport {
        let run: Run = serde_json::from_str(
            "{\"part_size_mib\":16,\"parallel_parts\":4,\"max_in_flight\":32,\"defaults\":true,\
             \"bytes\":268435456,\"seconds\":2.0,\"cpu_cores\":0.3,\"peak_memory_mib\":100.0}",
        )
        .unwrap();
        UploadingReport {
            machine: Machine::this(Place::Native),
            streams: 16,
            rate_hz: 2_000,
            phase_seconds: 30,
            messages_sent: 1_920_000,
            generator_kept_up: true,
            written: 1_920_000 - dropped,
            dropped_by_us: dropped,
            buffer_peak_percent: 3.0,
            capture_cpu_cores_alone: alone,
            capture_cpu_cores_uploading: uploading,
            episode_mib: 256,
            uploads: vec![run; uploads],
        }
    }

    #[test]
    fn recording_must_not_notice_the_uploads() {
        let fine = report(0, 0.20, 0.24, 3);
        assert!(fine.all_met(), "within 0.05 cores");
        assert!(fine.markdown().contains("3 episodes of 256 MiB went up"));
        assert!(fine.markdown().contains("(768 MiB at 128 MiB/s)"));
        assert!(!report(0, 1.0, 1.15, 3).all_met(), "15 % more");
        assert!(report(0, 1.0, 1.09, 3).all_met(), "9 % more");
        assert!(!report(1, 0.20, 0.20, 3).all_met(), "a message dropped");
        assert!(!report(0, 0.20, 0.20, 0).all_met(), "nothing uploaded");
    }
}
