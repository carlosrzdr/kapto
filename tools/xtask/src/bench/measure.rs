// SPDX-License-Identifier: Apache-2.0

//! What the benchmark reads: the `/proc` files of the recording process and
//! the load generator's output. Parsing is kept here,
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
    })
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
}
