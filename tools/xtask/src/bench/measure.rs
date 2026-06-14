// SPDX-License-Identifier: Apache-2.0

//! What the benchmark reads: the `/proc` files of the recording process, the
//! machine's clock, temperature and load, and the load generator's output. Parsing is kept here,
//! free of I/O, so it is tested on captured text.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Linux reports CPU time in `/proc` in ticks of this many per second
/// (`USER_HZ`), the same on every architecture Kapto supports.
const TICKS_PER_SECOND: f64 = 100.0;

/// One reading of the recording process.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Sample {
    /// Seconds since the benchmark started.
    pub(crate) at: f64,
    /// CPU ticks used so far, per thread name (threads sharing a name are
    /// added together).
    pub(crate) ticks: BTreeMap<String, u64>,
    /// The process's peak resident memory so far, in KiB.
    pub(crate) peak_rss_kib: u64,
    /// The whole machine at the same moment.
    pub(crate) machine: MachineSample,
}

/// The whole machine at one moment: what can make the same work cost more
/// from one run to the next. Each part is `None` where it can't be read, as
/// in a virtual machine.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MachineSample {
    /// The cores' clock, averaged over them, in MHz.
    pub(crate) cpu_mhz: Option<f64>,
    /// The CPU's temperature, in °C.
    pub(crate) cpu_celsius: Option<f64>,
    /// CPU ticks the machine has spent busy, and in all, since it booted.
    pub(crate) ticks: Option<(u64, u64)>,
}

/// The machine over a run, from its samples.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub(crate) struct MachineMeasured {
    /// The cores' clock, averaged over the run, in MHz.
    pub(crate) cpu_mhz_mean: Option<f64>,
    /// Its lowest reading.
    pub(crate) cpu_mhz_lowest: Option<f64>,
    /// The CPU's hottest reading, in °C.
    pub(crate) cpu_celsius_peak: Option<f64>,
    /// How many cores the whole machine kept busy on average, recording
    /// included.
    pub(crate) busy_cores_mean: Option<f64>,
}

/// Reads `/proc/<pid>`, or `None` once the process is gone.
pub(crate) fn sample(pid: u32, at: Duration) -> Option<Sample> {
    let proc = Path::new("/proc").join(pid.to_string());
    let status = std::fs::read_to_string(proc.join("status")).ok()?;
    let mut ticks = BTreeMap::new();
    for task in std::fs::read_dir(proc.join("task")).ok()?.flatten() {
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        let stat = std::fs::read_to_string(task.path().join("stat")).unwrap_or_default();
        if let Some(used) = stat_ticks(&stat) {
            *ticks.entry(name.trim().to_owned()).or_insert(0) += used;
        }
    }
    Some(Sample {
        at: at.as_secs_f64(),
        ticks,
        peak_rss_kib: status_kib(&status, "VmHWM:").unwrap_or(0),
        machine: machine_sample(),
    })
}

/// Reads the cores' clocks, the thermal zones and `/proc/stat`.
fn machine_sample() -> MachineSample {
    let read = |path: &Path| std::fs::read_to_string(path).unwrap_or_default();
    let in_dir = |dir: &str, prefix: &str| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .collect()
    };
    let clocks: Vec<String> = in_dir("/sys/devices/system/cpu", "cpu")
        .iter()
        .map(|cpu| read(&cpu.join("cpufreq/scaling_cur_freq")))
        .collect();
    let zones: Vec<(String, String)> = in_dir("/sys/class/thermal", "thermal_zone")
        .iter()
        .map(|zone| (read(&zone.join("type")), read(&zone.join("temp"))))
        .collect();
    MachineSample {
        cpu_mhz: mean_mhz(&clocks),
        cpu_celsius: cpu_celsius(&zones),
        ticks: machine_ticks(&read(Path::new("/proc/stat"))),
    }
}

/// The mean of `scaling_cur_freq` readings, in kHz each, as MHz; files that
/// are empty or not a number (a core without frequency scaling) left out.
pub(crate) fn mean_mhz(readings: &[String]) -> Option<f64> {
    let khz: Vec<f64> = readings
        .iter()
        .filter_map(|reading| reading.trim().parse::<f64>().ok())
        .collect();
    (!khz.is_empty()).then(|| khz.iter().sum::<f64>() / khz.len() as f64 / 1_000.0)
}

/// The CPU's temperature from thermal zones' `type` and `temp` (in m°C): the
/// package's sensor if there is one, else a zone named for the CPU.
pub(crate) fn cpu_celsius(zones: &[(String, String)]) -> Option<f64> {
    let reading = |wanted: &dyn Fn(&str) -> bool| {
        zones
            .iter()
            .filter(|(kind, _)| wanted(&kind.trim().to_ascii_lowercase()))
            .find_map(|(_, temp)| temp.trim().parse::<f64>().ok())
    };
    reading(&|kind| kind == "x86_pkg_temp")
        .or_else(|| reading(&|kind| kind.contains("cpu")))
        .map(|millidegrees| millidegrees / 1_000.0)
}

/// Busy and total ticks from the `cpu` line of `/proc/stat`: idle and
/// waiting for I/O are not busy; guest time is already in user time.
pub(crate) fn machine_ticks(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [user, nice, system, idle, iowait, irq, softirq, steal] = fields[..] else {
        return None;
    };
    let busy = user + nice + system + irq + softirq + steal;
    Some((busy, busy + idle + iowait))
}

/// The machine over the samples of a run.
pub(crate) fn machine_between(samples: &[Sample]) -> MachineMeasured {
    let clocks: Vec<f64> = samples.iter().filter_map(|s| s.machine.cpu_mhz).collect();
    let busy_cores = match (samples.first(), samples.last()) {
        (Some(first), Some(last)) => first
            .machine
            .ticks
            .zip(last.machine.ticks)
            .filter(|_| last.at > first.at)
            .map(|((busy_from, _), (busy_to, _))| {
                busy_to.saturating_sub(busy_from) as f64 / TICKS_PER_SECOND / (last.at - first.at)
            }),
        _ => None,
    };
    MachineMeasured {
        cpu_mhz_mean: (!clocks.is_empty())
            .then(|| clocks.iter().sum::<f64>() / clocks.len() as f64),
        cpu_mhz_lowest: clocks.iter().copied().reduce(f64::min),
        cpu_celsius_peak: samples
            .iter()
            .filter_map(|s| s.machine.cpu_celsius)
            .reduce(f64::max),
        busy_cores_mean: busy_cores,
    }
}

/// User plus system CPU ticks from a `/proc/.../stat` line. The thread name
/// comes in parentheses and may itself contain spaces or parentheses, so
/// fields are counted from the last `)`.
pub(crate) fn stat_ticks(stat: &str) -> Option<u64> {
    let after_name = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    // After the name: state is field 3, utime field 14, stime field 15.
    let user: u64 = fields.get(11)?.parse().ok()?;
    let system: u64 = fields.get(12)?.parse().ok()?;
    Some(user + system)
}

/// A `Name:   1234 kB` value from `/proc/.../status`, in KiB.
pub(crate) fn status_kib(status: &str, name: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with(name))?;
    line[name.len()..].split_whitespace().next()?.parse().ok()
}

/// CPU used between two samples, in cores, per thread name and in total.
pub(crate) fn cores_between(from: &Sample, to: &Sample) -> (BTreeMap<String, f64>, f64) {
    let seconds = (to.at - from.at).max(f64::EPSILON);
    let per_thread: BTreeMap<String, f64> = to
        .ticks
        .iter()
        .map(|(name, &ticks)| {
            let before = from.ticks.get(name).copied().unwrap_or(0);
            let used = ticks.saturating_sub(before) as f64 / TICKS_PER_SECOND;
            (name.clone(), used / seconds)
        })
        .collect();
    let total = per_thread.values().sum();
    (per_thread, total)
}

/// What the load generator says it did, from its `total` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GeneratorTotals {
    /// Messages sent.
    pub(crate) sent: u64,
    /// Ticks it skipped because it fell behind its schedule.
    pub(crate) skipped: u64,
    /// Messages the operating system refused to send.
    pub(crate) send_errors: u64,
}

impl GeneratorTotals {
    /// Whether it sent the whole load on schedule. If not, the load measured
    /// isn't the load asked for, and a shortfall is the generator's.
    pub(crate) fn kept_up(&self) -> bool {
        self.skipped == 0 && self.send_errors == 0
    }
}

/// The load generator's totals, from its `total <n> msg  late …  skipped
/// <n>  send errors <n> …` line.
pub(crate) fn generator_totals(output: &str) -> Option<GeneratorTotals> {
    let line = output.lines().find(|line| line.starts_with("total"))?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let after = |word: &str| -> Option<u64> {
        let at = words.iter().position(|candidate| *candidate == word)?;
        words.get(at + 1)?.parse().ok()
    };
    Some(GeneratorTotals {
        sent: words.get(1)?.parse().ok()?,
        skipped: after("skipped")?,
        send_errors: after("errors")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_ticks_add_user_and_system_time_after_a_name_with_spaces() {
        let line = "4242 (cap rx) (0)) S 1 4242 4242 0 -1 4194560 120 0 0 0 731 45 0 0 20 0 3 0 9";
        assert_eq!(stat_ticks(line), Some(776));
        assert_eq!(stat_ticks("garbage"), None);
    }

    #[test]
    fn status_values_are_read_in_kib() {
        let status = "Name:\tkapto\nVmHWM:\t   48216 kB\nVmRSS:\t   47000 kB\n";
        assert_eq!(status_kib(status, "VmHWM:"), Some(48_216));
        assert_eq!(status_kib(status, "VmSwap:"), None);
    }

    #[test]
    fn cpu_between_samples_is_ticks_over_time_per_thread() {
        let sample = |at, writer, receiver| Sample {
            at,
            ticks: BTreeMap::from([
                ("cap-writer".to_owned(), writer),
                ("cap-rx-udp-0".to_owned(), receiver),
            ]),
            peak_rss_kib: 0,
            machine: MachineSample::default(),
        };
        let (threads, total) = cores_between(&sample(1.0, 100, 50), &sample(3.0, 140, 110));
        assert!((threads["cap-writer"] - 0.2).abs() < 1e-9);
        assert!((threads["cap-rx-udp-0"] - 0.3).abs() < 1e-9);
        assert!((total - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_generators_totals_are_read_from_its_last_line() {
        let output = "     1 s  32000 msg  late 0\n\
                      total   1920000 msg  late 12  skipped 3  send errors 1  lateness p50 0 µs\n";
        let totals = generator_totals(output).unwrap();
        assert_eq!(
            totals,
            GeneratorTotals {
                sent: 1_920_000,
                skipped: 3,
                send_errors: 1
            }
        );
        assert!(!totals.kept_up());
        assert_eq!(generator_totals("     1 s  32000 msg"), None);
    }

    #[test]
    fn the_cores_clock_is_their_mean_leaving_out_cores_that_say_nothing() {
        let readings = ["2800000\n", "1200000\n", "", "n/a"].map(str::to_owned);
        assert_eq!(mean_mhz(&readings), Some(2_000.0));
        assert_eq!(mean_mhz(&[String::new()]), None);
    }

    #[test]
    fn the_cpus_temperature_is_its_packages_sensor_else_one_named_for_it() {
        let zone = |kind: &str, temp: &str| (format!("{kind}\n"), format!("{temp}\n"));
        let laptop = [
            zone("acpitz", "90000"),
            zone("TCPU", "61000"),
            zone("x86_pkg_temp", "64500"),
        ];
        assert_eq!(cpu_celsius(&laptop), Some(64.5));
        let board = [zone("soc-thermal", "70000"), zone("cpu-thermal", "52000")];
        assert_eq!(cpu_celsius(&board), Some(52.0));
        assert_eq!(cpu_celsius(&[zone("iwlwifi_1", "40000")]), None);
    }

    #[test]
    fn the_machines_busy_ticks_leave_out_idle_and_waiting_for_io() {
        let stat = "cpu  100 20 30 1000 40 5 6 7 0 0\ncpu0 50 10 15 500 20 2 3 3 0 0\n";
        assert_eq!(machine_ticks(stat), Some((168, 1208)));
        assert_eq!(machine_ticks("intr 1 2 3"), None);
    }

    #[test]
    fn the_machine_over_a_run_is_its_mean_clock_lowest_clock_hottest_reading_and_busy_cores() {
        let sample = |at, mhz, celsius, busy| Sample {
            at,
            machine: MachineSample {
                cpu_mhz: Some(mhz),
                cpu_celsius: Some(celsius),
                ticks: Some((busy, busy * 4)),
            },
            ..Sample::default()
        };
        let run = [
            sample(1.0, 3_000.0, 60.0, 1_000),
            sample(2.0, 1_400.0, 81.0, 1_150),
            sample(3.0, 2_200.0, 75.0, 1_300),
        ];
        let machine = machine_between(&run);
        assert_eq!(machine.cpu_mhz_mean, Some(2_200.0));
        assert_eq!(machine.cpu_mhz_lowest, Some(1_400.0));
        assert_eq!(machine.cpu_celsius_peak, Some(81.0));
        assert_eq!(machine.busy_cores_mean, Some(1.5));
        assert_eq!(
            machine_between(&[Sample::default()]),
            MachineMeasured::default()
        );
    }
}
