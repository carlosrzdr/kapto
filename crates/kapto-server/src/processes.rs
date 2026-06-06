// SPDX-License-Identifier: Apache-2.0

//! What each of Kapto's processes is doing: whether it runs and since when,
//! how often it ended and was started again, and what it costs in CPU,
//! memory and threads.
//!
//! The supervisor's events say which processes run and with which IDs
//! ([`Processes::record`]). [`Processes::sample`], called once a second by
//! the `srv-stats` thread, reads each one's CPU time, memory and threads
//! from `/proc`; off Linux those stay unknown. CPU use is the CPU time a
//! process spent between two samples, in cores: 1.0 is one core kept busy.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::supervisor::Event;

/// How often [`sample_while`] samples.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(1);

/// What one process is doing, as the API reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProcessStatus {
    /// Its name: `serve` for the server itself, or the child's, such as
    /// `capture` or `viz-bridge`.
    pub name: String,
    /// Whether it runs.
    pub state: ProcessState,
    /// Its process ID while it runs.
    pub pid: Option<u32>,
    /// Seconds since it was started, while it runs.
    pub uptime_s: Option<f64>,
    /// CPU used over the last second, in cores: 1.0 is one core kept busy.
    /// Unknown until it has been sampled twice, and off Linux.
    pub cpu_cores: Option<f64>,
    /// Memory it holds (resident set), in bytes. Unknown off Linux.
    pub memory_bytes: Option<u64>,
    /// Its threads. Unknown off Linux.
    pub threads: Option<u32>,
    /// How many times it ended on its own, or couldn't start, and was to be
    /// started again.
    pub restarts: u32,
    /// How it ended last, e.g. `signal: 9 (SIGKILL)`, if it ever did.
    pub last_exit: Option<String>,
    /// While it waits to be started again: the seconds left.
    pub restart_in_s: Option<f64>,
}

/// Whether a process runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Not started yet.
    Starting,
    /// Running.
    Running,
    /// It ended and waits to be started again.
    Restarting,
    /// It was stopped with the server.
    Stopped,
}

/// The processes Kapto runs, kept up to date from the supervisor's events
/// and `/proc`.
#[derive(Debug)]
pub struct Processes {
    entries: Mutex<Vec<Entry>>,
}

#[derive(Debug)]
struct Entry {
    name: String,
    pid: Option<u32>,
    state: ProcessState,
    /// When it was started, or when it ended to be started again.
    since: Instant,
    restart_delay: Duration,
    restarts: u32,
    last_exit: Option<String>,
    usage: Usage,
    /// The last CPU time read, of which process, and when.
    previous: Option<(u32, u64, Instant)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Usage {
    cpu_cores: Option<f64>,
    memory_bytes: Option<u64>,
    threads: Option<u32>,
}

/// What `/proc` says about one process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    /// CPU time in user and kernel mode, in clock ticks.
    pub(crate) cpu_ticks: u64,
    pub(crate) threads: u32,
    pub(crate) memory_bytes: u64,
}

impl Processes {
    /// The server itself, running as `own` since now, and its `children`,
    /// not started yet.
    #[must_use]
    pub fn new(own: &str, children: &[&str]) -> Self {
        let now = Instant::now();
        let entry = |name: &str, pid, state| Entry {
            name: name.to_owned(),
            pid,
            state,
            since: now,
            restart_delay: Duration::ZERO,
            restarts: 0,
            last_exit: None,
            usage: Usage::default(),
            previous: None,
        };
        let mut entries = vec![entry(own, Some(std::process::id()), ProcessState::Running)];
        entries.extend(
            children
                .iter()
                .map(|name| entry(name, None, ProcessState::Starting)),
        );
        Self {
            entries: Mutex::new(entries),
        }
    }

    /// Notes what the supervisor reports about a child.
    pub fn record(&self, event: &Event) {
        let name = match event {
            Event::Started { name, .. }
            | Event::Restarting { name, .. }
            | Event::Stopped { name, .. } => name,
        };
        let mut entries = self.lock();
        let Some(entry) = entries.iter_mut().find(|entry| &entry.name == name) else {
            return;
        };
        entry.since = Instant::now();
        entry.usage = Usage::default();
        entry.previous = None;
        match event {
            Event::Started { pid, .. } => {
                entry.pid = Some(*pid);
                entry.state = ProcessState::Running;
            }
            Event::Restarting { reason, delay, .. } => {
                entry.pid = None;
                entry.state = ProcessState::Restarting;
                entry.restart_delay = *delay;
                entry.restarts += 1;
                entry.last_exit = Some(reason.clone());
            }
            Event::Stopped { .. } => {
                entry.pid = None;
                entry.state = ProcessState::Stopped;
            }
        }
    }

    /// Reads each running process's CPU time, memory and threads.
    pub fn sample(&self) {
        let pids: Vec<u32> = self.lock().iter().filter_map(|entry| entry.pid).collect();
        // Read without holding the lock: the API may ask meanwhile.
        let readings: Vec<(u32, Option<Reading>)> =
            pids.into_iter().map(|pid| (pid, read(pid))).collect();
        let now = Instant::now();
        let ticks = ticks_per_second();
        let mut entries = self.lock();
        for (pid, reading) in readings {
            let Some(entry) = entries.iter_mut().find(|entry| entry.pid == Some(pid)) else {
                continue;
            };
            let Some(reading) = reading else {
                entry.usage = Usage::default();
                continue;
            };
            let cpu_cores = entry.previous.and_then(|(was, ticks_before, at)| {
                let seconds = now.duration_since(at).as_secs_f64();
                (was == pid && seconds > 0.0 && ticks > 0).then(|| {
                    let spent = reading.cpu_ticks.saturating_sub(ticks_before);
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "CPU ticks stay far below 2^52 for any process"
                    )]
                    let cores = spent as f64 / ticks as f64 / seconds;
                    cores
                })
            });
            entry.previous = Some((pid, reading.cpu_ticks, now));
            entry.usage = Usage {
                cpu_cores,
                memory_bytes: Some(reading.memory_bytes),
                threads: Some(reading.threads),
            };
        }
    }

    /// What each process is doing now, the server first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ProcessStatus> {
        let now = Instant::now();
        self.lock()
            .iter()
            .map(|entry| {
                let elapsed = now.duration_since(entry.since);
                ProcessStatus {
                    name: entry.name.clone(),
                    state: entry.state,
                    pid: entry.pid,
                    uptime_s: (entry.state == ProcessState::Running)
                        .then_some(elapsed.as_secs_f64()),
                    cpu_cores: entry.usage.cpu_cores,
                    memory_bytes: entry.usage.memory_bytes,
                    threads: entry.usage.threads,
                    restarts: entry.restarts,
                    last_exit: entry.last_exit.clone(),
                    restart_in_s: (entry.state == ProcessState::Restarting)
                        .then(|| entry.restart_delay.saturating_sub(elapsed).as_secs_f64()),
                }
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Samples `processes` every [`SAMPLE_EVERY`] for as long as `running` is
/// set.
pub fn sample_while(processes: &Processes, running: &AtomicBool) {
    while running.load(Ordering::Relaxed) {
        processes.sample();
        let next = Instant::now() + SAMPLE_EVERY;
        while running.load(Ordering::Relaxed) && Instant::now() < next {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "linux")]
fn read(pid: u32) -> Option<Reading> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let (cpu_ticks, threads) = parse_stat(&stat)?;
    Some(Reading {
        cpu_ticks,
        threads,
        memory_bytes: parse_resident(&status)?,
    })
}

/// Without `/proc`, nothing is known.
#[cfg(not(target_os = "linux"))]
fn read(_pid: u32) -> Option<Reading> {
    None
}

#[cfg(target_os = "linux")]
fn ticks_per_second() -> u64 {
    rustix::param::clock_ticks_per_second()
}

#[cfg(not(target_os = "linux"))]
fn ticks_per_second() -> u64 {
    0
}

/// The CPU time (user and kernel, in clock ticks) and threads in
/// `/proc/<pid>/stat`. The process's name, in parentheses, may itself hold
/// spaces and parentheses, so the fields are counted from the last `)`.
pub(crate) fn parse_stat(stat: &str) -> Option<(u64, u32)> {
    let fields: Vec<&str> = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    // Numbered from 1 in proc(5), the state being field 3: `utime` is
    // field 14, `stime` 15 and `num_threads` 20.
    let field = |number: usize| fields.get(number - 3).copied();
    let user: u64 = field(14)?.parse().ok()?;
    let kernel: u64 = field(15)?.parse().ok()?;
    let threads = field(20)?.parse().ok()?;
    Some((user + kernel, threads))
}

/// The resident memory in `/proc/<pid>/status`, in bytes.
pub(crate) fn parse_resident(status: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line
        .trim_start_matches("VmRSS:")
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/proc/<pid>/stat` of a capture process, as Linux 7.0 wrote it.
    const STAT: &str = "41237 (kapto) S 41201 41201 9934 34817 41201 4194560 2208 0 0 0 1542 377 0 0 20 0 9 0 2201734 1214545920 21290 18446744073709551615 1 1 0 0 0 0 0 4096 17642 0 0 0 17 5 0 0 0 0 0 0 0 0 0 0 0 0 0\n";

    const STATUS: &str = "Name:\tkapto\nUmask:\t0022\nState:\tS (sleeping)\nVmPeak:\t 1186080 kB\nVmSize:\t 1186080 kB\nVmHWM:\t   85372 kB\nVmRSS:\t   85160 kB\nThreads:\t9\n";

    #[test]
    fn cpu_time_and_threads_are_read_from_stat() {
        assert_eq!(parse_stat(STAT), Some((1542 + 377, 9)));
    }

    #[test]
    fn a_process_name_with_spaces_and_parentheses_doesnt_shift_the_fields() {
        let odd = STAT.replace("(kapto)", "(a (b) c)");
        assert_eq!(parse_stat(&odd), Some((1919, 9)));
    }

    #[test]
    fn resident_memory_is_read_from_status() {
        assert_eq!(parse_resident(STATUS), Some(85_160 * 1024));
    }

    #[test]
    fn a_truncated_or_unexpected_file_is_unknown_rather_than_wrong() {
        assert_eq!(parse_stat("41237 (kapto) S 1 2 3"), None);
        assert_eq!(parse_stat("no parenthesis"), None);
        assert_eq!(parse_resident("Name:\tkapto\n"), None);
        assert_eq!(parse_resident("VmRSS:\t lots kB\n"), None);
    }

    fn state(processes: &Processes, name: &str) -> ProcessStatus {
        processes
            .snapshot()
            .into_iter()
            .find(|process| process.name == name)
            .unwrap_or_else(|| panic!("no process {name}"))
    }

    #[test]
    fn children_follow_the_supervisors_events() {
        let processes = Processes::new("serve", &["capture"]);
        assert_eq!(state(&processes, "serve").state, ProcessState::Running);
        assert_eq!(state(&processes, "capture").state, ProcessState::Starting);

        processes.record(&Event::Started {
            name: "capture".into(),
            pid: 4242,
        });
        let running = state(&processes, "capture");
        assert_eq!(running.state, ProcessState::Running);
        assert_eq!(running.pid, Some(4242));
        assert!(running.uptime_s.is_some());

        processes.record(&Event::Restarting {
            name: "capture".into(),
            reason: "signal: 9 (SIGKILL)".into(),
            delay: Duration::from_secs(30),
        });
        let restarting = state(&processes, "capture");
        assert_eq!(restarting.state, ProcessState::Restarting);
        assert_eq!(restarting.pid, None);
        assert_eq!(restarting.restarts, 1);
        assert_eq!(restarting.last_exit.as_deref(), Some("signal: 9 (SIGKILL)"));
        assert!(restarting.restart_in_s.is_some_and(|left| left > 29.0));

        processes.record(&Event::Stopped {
            name: "capture".into(),
            killed: false,
        });
        assert_eq!(state(&processes, "capture").state, ProcessState::Stopped);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_server_itself_is_measured_from_proc() {
        let processes = Processes::new("serve", &[]);
        processes.sample();
        let first = state(&processes, "serve");
        assert!(first.memory_bytes.is_some_and(|bytes| bytes > 0));
        assert!(first.threads.is_some_and(|threads| threads >= 1));
        // The first sample has nothing to compare with.
        assert_eq!(first.cpu_cores, None);
        let spin = Instant::now();
        while spin.elapsed() < Duration::from_millis(50) {
            std::hint::black_box(0);
        }
        processes.sample();
        assert!(
            state(&processes, "serve")
                .cpu_cores
                .is_some_and(|cores| cores >= 0.0)
        );
    }
}
