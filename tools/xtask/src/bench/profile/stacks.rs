// SPDX-License-Identifier: Apache-2.0

//! A profile from `perf script`'s text: which threads are sampled and
//! which functions their samples land in. Parsing is kept here, free of
//! I/O, so it is tested on captured text.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// How many functions are listed for each thread.
const TOP_FUNCTIONS: usize = 8;

/// What `perf` sampled.
#[derive(Debug, Default)]
pub(super) struct Profile {
    /// Samples per stack, by thread name then frames from the outermost.
    stacks: BTreeMap<(String, Vec<String>), u64>,
}

impl Profile {
    /// From `perf script -F comm,tid,ip,sym,dso`.
    pub(super) fn new(script: &str) -> Self {
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

    /// The report: each thread's samples, then the functions they land in.
    pub(super) fn markdown(&self, scenario: &str, seconds: u32) -> String {
        let mut text = format!(
            "# Profile of `kapto record`: the {scenario} load, {seconds} s\n\n\
             | Thread | Samples | Of all |\n|---|---:|---:|\n"
        );
        let samples = self.samples_by_thread();
        let all = samples.values().sum::<u64>() as f64;
        let mut threads: Vec<_> = samples.iter().collect();
        threads.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let share = |part: f64, of: f64| if of > 0.0 { 100.0 * part / of } else { 0.0 };
        for (name, count) in &threads {
            let _ = writeln!(
                text,
                "| `{name}` | {count} | {:.1} % |",
                share(**count as f64, all)
            );
        }
        for (name, total) in threads {
            let functions = self.functions(name);
            if functions.is_empty() {
                continue;
            }
            let total = *total as f64;
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

    #[test]
    fn stacks_are_folded_from_the_outermost_frame_with_rust_symbols_demangled() {
        let profile = Profile::new(SCRIPT);
        assert_eq!(
            profile.folded(),
            "cap-rx udp;[libc.so.6];kapto_transport::udp::sys::receive;[kernel] 1\n\
             cap-writer;start_thread;kapto_capture::writer::run;kapto_capture::writer::write_batch 1\n\
             cap-writer;start_thread;kapto_capture::writer::run;kapto_capture::writer::write_batch;[kernel] 1\n"
        );
    }

    #[test]
    fn the_report_lists_each_threads_samples_and_the_functions_they_land_in() {
        let report = Profile::new(SCRIPT).markdown("reference", 10);
        assert!(report.contains("| `cap-writer` | 2 | 66.7 % |"), "{report}");
        assert!(report.contains("| `cap-rx udp` | 1 | 33.3 % |"), "{report}");
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
