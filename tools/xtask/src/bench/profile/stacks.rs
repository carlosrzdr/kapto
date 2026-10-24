// SPDX-License-Identifier: Apache-2.0

//! A profile from `perf script`'s text and each thread's times: which
//! threads run, wait for a core or are blocked, and which functions their
//! samples land in. Parsing is kept here, free of I/O, so it is tested on
//! captured text.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

/// How many functions are listed for each thread.
const TOP_FUNCTIONS: usize = 8;

/// One reading of a thread's `/proc/<pid>/task/<tid>/schedstat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Reading {
    /// When it was read, since watching began.
    pub(super) at: Duration,
    /// Nanoseconds spent on a core so far.
    pub(super) running_ns: u64,
    /// Nanoseconds spent ready, waiting for a core.
    pub(super) waiting_ns: u64,
}

impl Reading {
    /// Reads `schedstat`'s first two fields, running and waiting.
    pub(super) fn parse(at: Duration, schedstat: &str) -> Option<Self> {
        let mut fields = schedstat.split_whitespace().map(str::parse::<u64>);
        Some(Self {
            at,
            running_ns: fields.next()?.ok()?,
            waiting_ns: fields.next()?.ok()?,
        })
    }
}

/// One thread's first and last readings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ThreadTimes {
    pub(super) tid: String,
    pub(super) name: String,
    pub(super) first: Reading,
    pub(super) last: Reading,
}

/// The threads sharing a name, over the run.
#[derive(Clone, Debug, Default, PartialEq)]
struct Thread {
    count: usize,
    /// Seconds the threads were watched, added up.
    seconds: f64,
    running: f64,
    waiting: f64,
}

/// What `perf` sampled and what each thread did.
#[derive(Debug, Default)]
pub(super) struct Profile {
    /// Samples per stack, by thread name then frames from the outermost.
    stacks: BTreeMap<(String, Vec<String>), u64>,
    threads: BTreeMap<String, Thread>,
    /// Seconds from the first reading to the last.
    wall: f64,
}

impl Profile {
    /// From `perf script -F comm,tid,ip,sym,dso` and each thread's times.
    pub(super) fn new(script: &str, times: &[ThreadTimes]) -> Self {
        let mut profile = Self::default();
        let mut sample: Option<(String, Vec<String>)> = None;
        for line in script.lines().chain([""]) {
            if line.trim().is_empty() {
                if let Some((thread, mut frames)) = sample.take() {
                    frames.reverse();
                    *profile.stacks.entry((thread, frames)).or_insert(0) += 1;
                }
            } else if line.starts_with(char::is_whitespace) {
                if let Some((_, frames)) = sample.as_mut() {
                    frames.push(function(line));
                }
            } else {
                // The thread's name may hold spaces; its ID comes last.
                let name = line.trim().rsplit_once(' ').map_or(line, |(name, _)| name);
                sample = Some((name.trim().to_owned(), Vec::new()));
            }
        }
        let start = times
            .iter()
            .map(|thread| thread.first.at)
            .min()
            .unwrap_or_default();
        let end = times
            .iter()
            .map(|thread| thread.last.at)
            .max()
            .unwrap_or_default();
        profile.wall = end.saturating_sub(start).as_secs_f64();
        for times in times {
            let thread = profile.threads.entry(times.name.clone()).or_default();
            let nanos = |ns: u64| ns as f64 / 1e9;
            thread.count += 1;
            thread.seconds += times.last.at.saturating_sub(times.first.at).as_secs_f64();
            thread.running += nanos(times.last.running_ns.saturating_sub(times.first.running_ns));
            thread.waiting += nanos(times.last.waiting_ns.saturating_sub(times.first.waiting_ns));
        }
        profile
    }

    /// Each stack as `thread;outer;…;inner samples`, a flamegraph's input.
    pub(super) fn folded(&self) -> String {
        let mut text = String::new();
        for ((thread, frames), samples) in &self.stacks {
            let _ = writeln!(text, "{thread};{} {samples}", frames.join(";"));
        }
        text
    }

    /// The report: each thread's time, then the functions its samples land in.
    pub(super) fn markdown(&self, scenario: &str, seconds: u32) -> String {
        let mut text = format!(
            "# Profile of `kapto record`: the {scenario} load, {seconds} s\n\n\
             Watched for {:.1} s. On a core, waiting for one and blocked are shares of each \
             thread's time; cores are its time on a core over the run.\n\n\
             | Thread | Threads | Cores | On a core | Waiting for a core | Blocked | Samples |\n\
             |---|---:|---:|---:|---:|---:|---:|\n",
            self.wall
        );
        let samples = self.samples_by_thread();
        let mut threads: Vec<_> = self.threads.iter().collect();
        threads.sort_by(|a, b| b.1.running.total_cmp(&a.1.running).then(a.0.cmp(b.0)));
        let share = |part: f64, of: f64| if of > 0.0 { 100.0 * part / of } else { 0.0 };
        for (name, thread) in &threads {
            let blocked = (thread.seconds - thread.running - thread.waiting).max(0.0);
            let _ = writeln!(
                text,
                "| `{name}` | {} | {:.3} | {:.0} % | {:.0} % | {:.0} % | {} |",
                thread.count,
                share(thread.running, self.wall) / 100.0,
                share(thread.running, thread.seconds),
                share(thread.waiting, thread.seconds),
                share(blocked, thread.seconds),
                samples.get(name.as_str()).copied().unwrap_or(0),
            );
        }
        for (name, _) in threads {
            let functions = self.functions(name);
            if functions.is_empty() {
                continue;
            }
            let total = samples.get(name.as_str()).copied().unwrap_or(0) as f64;
            let _ = write!(
                text,
                "\n## `{name}`\n\n| Function | Self | With what it calls |\n|---|---:|---:|\n"
            );
            for (function, own, within) in functions.into_iter().take(TOP_FUNCTIONS) {
                let _ = writeln!(
                    text,
                    "| `{function}` | {:.1} % | {:.1} % |",
                    share(own as f64, total),
                    share(within as f64, total)
                );
            }
        }
        text
    }

    fn samples_by_thread(&self) -> BTreeMap<&str, u64> {
        let mut samples = BTreeMap::new();
        for ((thread, _), count) in &self.stacks {
            *samples.entry(thread.as_str()).or_insert(0) += count;
        }
        samples
    }

    /// The functions `thread`'s samples land in, with the samples in each
    /// itself and in it or what it calls, the most itself first; those with
    /// none of their own left out.
    fn functions(&self, thread: &str) -> Vec<(&str, u64, u64)> {
        let mut functions: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
        for ((name, frames), count) in &self.stacks {
            if name != thread {
                continue;
            }
            let mut seen: Vec<&str> = Vec::new();
            for frame in frames {
                if !seen.contains(&frame.as_str()) {
                    seen.push(frame);
                    functions.entry(frame).or_default().1 += count;
                }
            }
            if let Some(inner) = frames.last() {
                functions.entry(inner).or_default().0 += count;
            }
        }
        let mut listed: Vec<_> = functions
            .into_iter()
            .filter(|(_, (own, _))| *own > 0)
            .map(|(function, (own, within))| (function, own, within))
            .collect();
        listed.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(b.0)));
        listed
    }
}

/// The function of a frame line, `address symbol (object)`: its symbol,
/// demangled and without Rust's hash, else what it is in.
fn function(line: &str) -> String {
    let (address, line) = line.trim().split_once(' ').unwrap_or_default();
    let (symbol, object) = line
        .rsplit_once(" (")
        .map_or((line, ""), |(symbol, object)| {
            (symbol, object.trim_end_matches(')'))
        });
    if symbol != "[unknown]" && !symbol.is_empty() {
        if let Ok(demangled) = rustc_demangle::try_demangle(symbol) {
            return format!("{demangled:#}");
        }
        // What `perf` demangles itself keeps the legacy mangling's hash,
        // `::h` and 16 hex digits.
        return match symbol.rsplit_once("::h") {
            Some((path, hash))
                if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                path.to_owned()
            }
            _ => symbol.to_owned(),
        };
    }
    // The kernel's symbols are hidden from the profiler; its addresses are
    // the top half of the address space.
    if object.starts_with("[kernel") || (object == "[unknown]" && address.starts_with("ffff")) {
        return "[kernel]".to_owned();
    }
    let object = object.rsplit('/').next().unwrap_or(object);
    if object.starts_with('[') {
        object.to_owned()
    } else {
        format!("[{object}]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "\
cap-writer 4242
\t    5c262fd850ad kapto_capture::writer::write_batch::h0123456789abcdef (/repo/target/profiling/release/kapto)
\t    5c262fd7c1b0 kapto_capture::writer::run::hfedcba9876543210 (/repo/target/profiling/release/kapto)
\t    7a17b262a1ca start_thread (/usr/lib/x86_64-linux-gnu/libc.so.6)

cap-writer 4242
\t    ffffffff8a2b [unknown] ([kernel.kallsyms])
\t    5c262fd850ad kapto_capture::writer::write_batch::h0123456789abcdef (/repo/target/profiling/release/kapto)
\t    5c262fd7c1b0 kapto_capture::writer::run::hfedcba9876543210 (/repo/target/profiling/release/kapto)
\t    7a17b262a1ca start_thread (/usr/lib/x86_64-linux-gnu/libc.so.6)

cap-rx udp 4243
\t    ffffffffa060012b [unknown] ([unknown])
\t    5c262fd850ad _RNvNtNtCs1qY1sNuKwjA_15kapto_transport3udp3sys7receive (/repo/target/profiling/release/kapto)
\t    7a17b27aaf5e [unknown] (/usr/lib/x86_64-linux-gnu/libc.so.6)
";

    fn times(name: &str, tid: &str, running_s: u64, waiting_s: u64) -> ThreadTimes {
        let at = |seconds, running, waiting| Reading {
            at: Duration::from_secs(seconds),
            running_ns: running * 1_000_000_000,
            waiting_ns: waiting * 1_000_000_000,
        };
        ThreadTimes {
            tid: tid.to_owned(),
            name: name.to_owned(),
            first: at(0, 0, 0),
            last: at(10, running_s, waiting_s),
        }
    }

    #[test]
    fn a_schedstat_line_gives_the_time_running_and_waiting() {
        let reading = Reading::parse(Duration::from_secs(1), "123 45 6\n").unwrap();
        assert_eq!((reading.running_ns, reading.waiting_ns), (123, 45));
        assert_eq!(Reading::parse(Duration::ZERO, "garbage"), None);
    }

    #[test]
    fn stacks_are_folded_from_the_outermost_frame_with_rust_symbols_demangled() {
        let profile = Profile::new(SCRIPT, &[]);
        assert_eq!(
            profile.folded(),
            "cap-rx udp;[libc.so.6];kapto_transport::udp::sys::receive;[kernel] 1\n\
             cap-writer;start_thread;kapto_capture::writer::run;kapto_capture::writer::write_batch 1\n\
             cap-writer;start_thread;kapto_capture::writer::run;kapto_capture::writer::write_batch;[kernel] 1\n"
        );
    }

    #[test]
    fn the_report_shows_each_threads_time_on_a_core_waiting_and_blocked() {
        let profile = Profile::new(
            SCRIPT,
            &[
                times("cap-writer", "4242", 5, 1),
                times("cap-rx udp", "4243", 1, 0),
                times("cap-idle", "4244", 0, 0),
            ],
        );
        let report = profile.markdown("reference", 10);
        assert!(
            report.contains("| `cap-writer` | 1 | 0.500 | 50 % | 10 % | 40 % | 2 |"),
            "{report}"
        );
        // A thread that only waits is listed, with no functions.
        assert!(
            report.contains("| `cap-idle` | 1 | 0.000 | 0 % | 0 % | 100 % | 0 |"),
            "{report}"
        );
        assert!(!report.contains("## `cap-idle`"), "{report}");
        // Half the writer's samples are in the kernel under `write_batch`.
        assert!(
            report.contains("| `[kernel]` | 50.0 % | 50.0 % |"),
            "{report}"
        );
        assert!(
            report.contains("| `kapto_capture::writer::write_batch` | 50.0 % | 100.0 % |"),
            "{report}"
        );
        // Functions only calling others aren't listed.
        assert!(!report.contains("| `start_thread`"), "{report}");
    }
}
