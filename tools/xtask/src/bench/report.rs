// SPDX-License-Identifier: Apache-2.0

//! The benchmark report: which machine, what load, what was measured, and
//! whether each target was met.

use std::fmt::Write as _;

use serde::Serialize;

use super::measure::MachineMeasured;

/// The machine a benchmark ran on, so results from different machines are
/// never compared by mistake.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Machine {
    pub(crate) cpu: String,
    pub(crate) threads: usize,
    pub(crate) memory_gib: f64,
    pub(crate) kernel: String,
    pub(crate) cpu_governor: String,
    /// Where `kapto record` ran: directly, or in a container and how limited.
    pub(crate) runs_in: String,
}

impl Machine {
    /// Reads this machine's description; parts that can't be read are
    /// `unknown`.
    pub(crate) fn this(place: super::Place) -> Self {
        let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
        let mut machine = Self::from_proc(
            &read("/proc/cpuinfo"),
            &read("/proc/meminfo"),
            &read("/proc/sys/kernel/osrelease"),
            &read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        );
        machine.runs_in = place.describe();
        machine
    }

    fn from_proc(cpuinfo: &str, meminfo: &str, kernel: &str, governor: &str) -> Self {
        let known = |text: &str| {
            let text = text.trim();
            if text.is_empty() {
                "unknown".to_owned()
            } else {
                text.to_owned()
            }
        };
        let cpu = cpuinfo
            .lines()
            .find(|line| line.starts_with("model name"))
            .and_then(|line| line.split_once(':'))
            .map_or("", |(_, model)| model);
        let memory_kib = super::measure::status_kib(meminfo, "MemTotal:").unwrap_or(0);
        Self {
            cpu: known(cpu),
            threads: cpuinfo
                .lines()
                .filter(|line| line.starts_with("processor"))
                .count(),
            memory_gib: memory_kib as f64 / (1024.0 * 1024.0),
            kernel: known(kernel),
            cpu_governor: known(governor),
            runs_in: super::Place::Native.describe(),
        }
    }
}

/// The load the synthetic robot sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Load {
    pub(crate) streams: u16,
    /// How many of the streams arrive over TCP; the rest over UDP.
    pub(crate) tcp_streams: u16,
    /// Cameras sent beside the streams, if any.
    pub(crate) cameras: Option<super::Cameras>,
    pub(crate) rate_hz: u32,
    pub(crate) seconds: u32,
    pub(crate) messages_sent: u64,
}

/// What was measured while recording.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Measured {
    pub(crate) written: u64,
    pub(crate) dropped_by_us: u64,
    pub(crate) lost_upstream: u64,
    pub(crate) buffer_peak_batches: u64,
    pub(crate) buffer_batches: u64,
    /// 99 % of messages were on disk within this, received to synced.
    pub(crate) p99_unsynced_ms: f64,
    /// The longest any message waited to be on disk.
    pub(crate) max_unsynced_ms: f64,
    /// CPU of the recording process while the load ran, averaged.
    pub(crate) cpu_cores_mean: f64,
    /// The busiest one-second window.
    pub(crate) cpu_cores_peak: f64,
    /// Average CPU per thread, by thread name.
    pub(crate) threads: Vec<(String, f64)>,
    pub(crate) memory_peak_mib: f64,
    /// What the machine was doing meanwhile: the same work costs more on a
    /// slower clock, a hot CPU or a busy machine.
    pub(crate) machine: MachineMeasured,
}

/// One target and whether it was met.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Check {
    pub(crate) target: &'static str,
    pub(crate) measured: String,
    pub(crate) met: bool,
}

/// Which targets a run is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Targets {
    /// The reference load: drops, buffers, time to disk, CPU and memory.
    Reference,
    /// Twice the reference load: recorded completely, with buffers to spare.
    Headroom,
    /// The reference load as ROS 2 topics, with the live view open: the
    /// reference targets, and those of the live view added to them.
    Ros2,
}

/// A whole benchmark run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Report {
    pub(crate) targets: Targets,
    pub(crate) machine: Machine,
    pub(crate) load: Load,
    pub(crate) measured: Measured,
    pub(crate) checks: Vec<Check>,
}

impl Report {
    pub(crate) fn new(targets: Targets, machine: Machine, load: Load, measured: Measured) -> Self {
        let mut checks = checks(&load, &measured);
        if targets == Targets::Headroom {
            checks.truncate(2);
        }
        Self {
            targets,
            machine,
            load,
            measured,
            checks,
        }
    }

    pub(crate) fn all_met(&self) -> bool {
        self.checks.iter().all(|check| check.met)
    }

    pub(crate) fn markdown(&self) -> String {
        let (machine, load, measured) = (&self.machine, &self.load, &self.measured);
        let mut page = String::from(match self.targets {
            Targets::Reference => "# System benchmark\n\n",
            Targets::Headroom => "# Headroom benchmark: twice the reference load\n\n",
            Targets::Ros2 => {
                "# ROS 2 benchmark: the reference load as topics, the live view open\n\n"
            }
        });
        page.push_str(
            "Provisional: measured on this machine, not the reference PC. Compare runs \
             only with runs on the same machine.\n\n",
        );
        let _ = writeln!(
            page,
            "## Machine\n\n| | |\n|---|---|\n| CPU | {} |\n| Threads | {} |\n| Memory | {:.1} GiB |\n| Kernel | {} |\n| CPU governor | {} |\n| Kapto ran | {} |\n",
            machine.cpu,
            machine.threads,
            machine.memory_gib,
            machine.kernel,
            machine.cpu_governor,
            machine.runs_in
        );
        let over = if self.targets == Targets::Ros2 {
            "ROS 2, each a `sensor_msgs/msg/JointState` topic,".to_owned()
        } else if load.tcp_streams == 0 {
            "UDP".to_owned()
        } else {
            format!(
                "UDP ({}) and TCP ({})",
                load.streams - load.tcp_streams,
                load.tcp_streams
            )
        };
        let _ = writeln!(
            page,
            "## Load\n\n{} streams over {over} at {} Hz for {} s: {} messages/s{}, {} sent.\n",
            load.streams,
            load.rate_hz,
            load.seconds,
            u64::from(load.streams) * u64::from(load.rate_hz),
            load.cameras.map_or_else(String::new, |cameras| format!(
                ", and {} cameras of {}x{} JPEG frames at {} fps {}",
                cameras.count,
                cameras.width,
                cameras.height,
                cameras.fps,
                if self.targets == Targets::Ros2 {
                    "as `sensor_msgs/msg/CompressedImage` topics"
                } else {
                    "over TCP"
                }
            )),
            load.messages_sent
        );
        page.push_str("## Targets\n\n| Target | Measured | Met |\n|---|---|---|\n");
        for check in &self.checks {
            let met = if check.met { "yes" } else { "**no**" };
            let _ = writeln!(page, "| {} | {} | {met} |", check.target, check.measured);
        }
        let _ = writeln!(
            page,
            "\n## Recording process\n\nWritten {}, dropped by Kapto {}, lost upstream {}. \
             Buffers peaked at {} of {} batches. CPU {:.2} cores on average, {:.2} at peak. \
             Memory peaked at {:.0} MiB.\n\n| Thread | CPU (cores) |\n|---|---|",
            measured.written,
            measured.dropped_by_us,
            measured.lost_upstream,
            measured.buffer_peak_batches,
            measured.buffer_batches,
            measured.cpu_cores_mean,
            measured.cpu_cores_peak,
            measured.memory_peak_mib
        );
        for (thread, cores) in &measured.threads {
            let _ = writeln!(page, "| `{thread}` | {cores:.3} |");
        }
        page.push_str(&machine_while_recording(measured));
        page
    }
}

/// What the machine was doing while recording, each part `unknown` where it
/// couldn't be read.
fn machine_while_recording(measured: &Measured) -> String {
    let machine = &measured.machine;
    let ghz = |mhz: Option<f64>| {
        mhz.map_or("unknown".to_owned(), |mhz| {
            format!("{:.2} GHz", mhz / 1_000.0)
        })
    };
    let celsius = machine
        .cpu_celsius_peak
        .map_or("unknown".to_owned(), |celsius| format!("{celsius:.0} °C"));
    let busy = machine
        .busy_cores_mean
        .map_or("unknown".to_owned(), |busy| {
            format!(
                "{busy:.2} cores, {:.2} of them besides recording",
                (busy - measured.cpu_cores_mean).max(0.0)
            )
        });
    format!(
        "\n## The machine while recording\n\nCPU clock {} on average, {} at its lowest. \
         CPU at {celsius} at its hottest. Busy on average: {busy}.\n",
        ghz(machine.cpu_mhz_mean),
        ghz(machine.cpu_mhz_lowest),
    )
}

fn checks(load: &Load, measured: &Measured) -> Vec<Check> {
    let buffer_share = measured.buffer_peak_batches as f64 / measured.buffer_batches.max(1) as f64;
    vec![
        Check {
            target: "Every message sent is written; none dropped by Kapto",
            measured: format!(
                "{} of {} written, {} dropped",
                measured.written, load.messages_sent, measured.dropped_by_us
            ),
            met: measured.dropped_by_us == 0 && measured.written == load.messages_sent,
        },
        Check {
            target: "Buffers never more than half full",
            measured: format!("{:.1} % at peak", buffer_share * 100.0),
            met: buffer_share <= 0.5,
        },
        Check {
            target: "99 % of messages on disk within 1 s of being received",
            measured: format!(
                "{:.0} ms for 99 %, {:.0} ms at most",
                measured.p99_unsynced_ms, measured.max_unsynced_ms
            ),
            met: measured.p99_unsynced_ms < 1_000.0,
        },
        Check {
            target: "Recording uses at most one core",
            measured: format!("{:.2} cores on average", measured.cpu_cores_mean),
            met: measured.cpu_cores_mean <= 1.0,
        },
        Check {
            target: "Recording uses at most 1 GiB of memory",
            measured: format!("{:.0} MiB at peak", measured.memory_peak_mib),
            met: measured.memory_peak_mib <= 1024.0,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(dropped: u64, buffer_peak: u64, cores: f64, memory_mib: f64) -> Report {
        Report::new(
            Targets::Reference,
            Machine::from_proc(
                "processor\t: 0\nmodel name\t: Test CPU 3000\nprocessor\t: 1\n",
                "MemTotal:       16384000 kB\n",
                "6.10.0\n",
                "",
            ),
            Load {
                streams: 16,
                tcp_streams: 0,
                cameras: None,
                rate_hz: 2_000,
                seconds: 60,
                messages_sent: 1_920_000,
            },
            Measured {
                written: 1_920_000 - dropped,
                dropped_by_us: dropped,
                lost_upstream: 0,
                buffer_peak_batches: buffer_peak,
                buffer_batches: 1_002,
                p99_unsynced_ms: 990.0,
                max_unsynced_ms: 1_004.0,
                cpu_cores_mean: cores,
                cpu_cores_peak: cores * 1.5,
                threads: vec![("cap-rx-udp-0".to_owned(), cores * 0.6)],
                memory_peak_mib: memory_mib,
                machine: MachineMeasured {
                    cpu_mhz_mean: Some(2_150.0),
                    cpu_mhz_lowest: Some(1_400.0),
                    cpu_celsius_peak: Some(81.4),
                    busy_cores_mean: Some(1.5),
                },
            },
        )
    }

    #[test]
    fn the_machine_is_described_from_proc_and_unknowns_are_named() {
        let machine = report(0, 1, 0.1, 100.0).machine;
        assert_eq!(machine.cpu, "Test CPU 3000");
        assert_eq!(machine.threads, 2);
        assert!((machine.memory_gib - 15.625).abs() < 1e-9);
        assert_eq!(machine.kernel, "6.10.0");
        assert_eq!(machine.cpu_governor, "unknown");
    }

    #[test]
    fn a_run_within_every_target_meets_them_all() {
        assert!(report(0, 7, 0.3, 120.0).all_met());
    }

    #[test]
    fn each_target_fails_on_its_own() {
        let unmet = |report: Report| -> Vec<&'static str> {
            report
                .checks
                .iter()
                .filter(|check| !check.met)
                .map(|check| check.target)
                .collect()
        };
        assert_eq!(
            unmet(report(1, 7, 0.3, 120.0)),
            ["Every message sent is written; none dropped by Kapto"]
        );
        assert_eq!(
            unmet(report(0, 502, 0.3, 120.0)),
            ["Buffers never more than half full"]
        );
        assert_eq!(
            unmet(report(0, 7, 1.2, 120.0)),
            ["Recording uses at most one core"]
        );
        assert_eq!(
            unmet(report(0, 7, 0.3, 1_500.0)),
            ["Recording uses at most 1 GiB of memory"]
        );
    }

    #[test]
    fn messages_slower_than_a_second_to_reach_disk_miss_their_target() {
        let mut slow = report(0, 7, 0.3, 120.0);
        slow.measured.p99_unsynced_ms = 1_100.0;
        let slow = Report::new(Targets::Reference, slow.machine, slow.load, slow.measured);
        let unmet: Vec<_> = slow.checks.iter().filter(|check| !check.met).collect();
        assert_eq!(unmet.len(), 1);
        assert_eq!(
            unmet[0].target,
            "99 % of messages on disk within 1 s of being received"
        );
        assert_eq!(unmet[0].measured, "1100 ms for 99 %, 1004 ms at most");
    }

    #[test]
    fn a_headroom_run_is_held_only_to_recording_everything_with_buffers_to_spare() {
        // Over the CPU and time-to-disk targets, which don't apply to it.
        let mut busy = report(0, 7, 1.6, 120.0);
        busy.measured.p99_unsynced_ms = 1_200.0;
        let headroom = Report::new(Targets::Headroom, busy.machine, busy.load, busy.measured);
        let targets: Vec<_> = headroom.checks.iter().map(|check| check.target).collect();
        assert_eq!(
            targets,
            [
                "Every message sent is written; none dropped by Kapto",
                "Buffers never more than half full"
            ]
        );
        assert!(headroom.all_met());
        assert!(headroom.markdown().starts_with("# Headroom benchmark"));
    }

    #[test]
    fn the_markdown_report_says_it_is_provisional_and_marks_unmet_targets() {
        let page = report(1, 7, 0.3, 120.0).markdown();
        assert!(page.contains("Provisional"));
        assert!(page.contains("| CPU | Test CPU 3000 |"));
        assert!(
            page.contains(
                "16 streams over UDP at 2000 Hz for 60 s: 32000 messages/s, 1920000 sent."
            )
        );
        assert!(page.contains("| Every message sent is written; none dropped by Kapto | 1919999 of 1920000 written, 1 dropped | **no** |"));
        assert!(page.contains("| `cap-rx-udp-0` | 0.180 |"));
        assert!(page.contains(
            "CPU clock 2.15 GHz on average, 1.40 GHz at its lowest. CPU at 81 °C at its \
             hottest. Busy on average: 1.50 cores, 1.20 of them besides recording."
        ));
    }

    #[test]
    fn the_machines_state_that_couldnt_be_read_is_said_to_be_unknown() {
        let mut run = report(0, 7, 0.3, 120.0);
        run.measured.machine = MachineMeasured::default();
        assert!(run.markdown().contains(
            "CPU clock unknown on average, unknown at its lowest. CPU at unknown at its \
             hottest. Busy on average: unknown."
        ));
    }
}
