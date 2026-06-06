// SPDX-License-Identifier: Apache-2.0

//! The capacity search: how many streams at a given rate this machine
//! records completely.
//!
//! Each step records a number of streams for a while. A step passes when
//! every message sent was written and none was dropped. The number of
//! streams doubles until a step fails, then the gap between the last pass
//! and the first failure is halved until it is within a tenth of the pass.
//! The search stops early if the load generator falls behind its schedule,
//! because the load measured is then smaller than the load asked for.

use std::fmt::Write as _;

use serde::Serialize;

use super::report::Machine;

/// One step of the search.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Step {
    pub(crate) streams: u16,
    pub(crate) sent: u64,
    pub(crate) written: u64,
    pub(crate) dropped_by_us: u64,
    pub(crate) lost_upstream: u64,
    pub(crate) buffer_peak_percent: f64,
    pub(crate) cpu_cores: f64,
    /// The load generator sent everything on schedule.
    pub(crate) generator_kept_up: bool,
}

impl Step {
    /// Every message sent was written and none was dropped.
    pub(crate) fn passed(&self) -> bool {
        self.generator_kept_up && self.dropped_by_us == 0 && self.written == self.sent
    }
}

/// What ended the search.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Limit {
    /// Kapto recorded the capacity but not the next step tried.
    Kapto,
    /// The load generator couldn't send more; Kapto's real capacity is at
    /// least the result.
    Generator,
    /// The largest number of streams allowed was recorded completely.
    Maximum,
}

/// The number of streams to try next, given the most known to pass (`lo`,
/// 0 if none yet), the fewest known to fail (`hi`), and the most allowed; or
/// `None` when the search is done.
pub(crate) fn next_streams(lo: u16, hi: Option<u16>, max: u16) -> Option<u16> {
    match hi {
        None if lo >= max => None,
        None => Some(lo.saturating_mul(2).min(max)),
        Some(hi) if hi - lo <= (lo / 10).max(1) => None,
        Some(hi) => Some(lo + (hi - lo) / 2),
    }
}

/// A whole capacity search.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CapacityReport {
    pub(crate) machine: Machine,
    pub(crate) rate_hz: u32,
    pub(crate) step_seconds: u32,
    pub(crate) steps: Vec<Step>,
    /// The most streams recorded completely.
    pub(crate) streams: u16,
    pub(crate) limit: Limit,
}

impl CapacityReport {
    pub(crate) fn markdown(&self) -> String {
        let rate = u64::from(self.rate_hz);
        let mut page = String::from("# Capacity\n\n");
        page.push_str(
            "Provisional: measured on this machine, not the reference PC. Compare runs \
             only with runs on the same machine.\n\n",
        );
        let _ = writeln!(
            page,
            "**{} streams at {} Hz ({} messages/s)** recorded completely, each step for {} s. {}\n",
            self.streams,
            self.rate_hz,
            u64::from(self.streams) * rate,
            self.step_seconds,
            match self.limit {
                Limit::Kapto => "The next step tried lost or dropped messages.",
                Limit::Generator =>
                    "The load generator couldn't send more on schedule, so Kapto's capacity may be higher.",
                Limit::Maximum => "That is the most the search was allowed to try.",
            }
        );
        let machine = &self.machine;
        let _ = writeln!(
            page,
            "Machine: {}, {} threads, {:.1} GiB, kernel {}, CPU governor {}. Kapto ran {}.\n",
            machine.cpu,
            machine.threads,
            machine.memory_gib,
            machine.kernel,
            machine.cpu_governor,
            machine.runs_in
        );
        page.push_str(
            "| Streams | Messages/s | Written of sent | Dropped | Lost upstream | Buffer peak | CPU (cores) | Result |\n\
             |---|---|---|---|---|---|---|---|\n",
        );
        for step in &self.steps {
            let result = if !step.generator_kept_up {
                "generator fell behind"
            } else if step.passed() {
                "complete"
            } else {
                "**incomplete**"
            };
            let _ = writeln!(
                page,
                "| {} | {} | {} of {} | {} | {} | {:.1} % | {:.2} | {result} |",
                step.streams,
                u64::from(step.streams) * rate,
                step.written,
                step.sent,
                step.dropped_by_us,
                step.lost_upstream,
                step.buffer_peak_percent,
                step.cpu_cores
            );
        }
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the search against a machine that records up to `capacity`
    /// streams, returning the streams tried.
    fn search(start: u16, max: u16, capacity: u16) -> (Vec<u16>, u16) {
        let mut tried = vec![start];
        let (mut lo, mut hi) = (0, None);
        let mut next = Some(start);
        while let Some(streams) = next {
            if streams <= capacity {
                lo = streams;
            } else {
                hi = Some(streams);
            }
            next = next_streams(lo, hi, max);
            tried.extend(next);
        }
        (tried, lo)
    }

    #[test]
    fn the_search_doubles_then_halves_the_gap_to_within_a_tenth() {
        let (tried, found) = search(16, 1_024, 300);
        assert_eq!(tried, [16, 32, 64, 128, 256, 512, 384, 320, 288, 304]);
        assert_eq!(found, 288);
        assert!(300 - found <= found / 10);
    }

    #[test]
    fn the_search_stops_at_the_maximum_allowed() {
        let (tried, found) = search(16, 100, 5_000);
        assert_eq!(tried, [16, 32, 64, 100]);
        assert_eq!(found, 100);
    }

    #[test]
    fn a_machine_that_cannot_record_the_first_step_is_searched_downwards() {
        let (tried, found) = search(16, 1_024, 3);
        assert_eq!(tried, [16, 8, 4, 2, 3]);
        assert_eq!(found, 3);
        assert_eq!(search(16, 1_024, 0).1, 0);
    }

    fn step(streams: u16, written: u64, kept_up: bool) -> Step {
        Step {
            streams,
            sent: 1_000,
            written,
            dropped_by_us: 0,
            lost_upstream: 1_000 - written,
            buffer_peak_percent: 1.5,
            cpu_cores: 0.25,
            generator_kept_up: kept_up,
        }
    }

    #[test]
    fn a_step_passes_only_if_everything_sent_on_schedule_was_written() {
        assert!(step(16, 1_000, true).passed());
        assert!(!step(16, 999, true).passed());
        assert!(!step(16, 1_000, false).passed());
    }

    #[test]
    fn the_report_states_the_capacity_what_limited_it_and_every_step() {
        let report = CapacityReport {
            machine: Machine {
                cpu: "Test CPU".to_owned(),
                threads: 8,
                memory_gib: 16.0,
                kernel: "6.10".to_owned(),
                cpu_governor: "performance".to_owned(),
                runs_in: "container kapto:dev, host networking".to_owned(),
            },
            rate_hz: 2_000,
            step_seconds: 20,
            steps: vec![step(16, 1_000, true), step(32, 990, true)],
            streams: 16,
            limit: Limit::Kapto,
        };
        let page = report.markdown();
        assert!(page.contains("**16 streams at 2000 Hz (32000 messages/s)** recorded completely"));
        assert!(page.contains("The next step tried lost or dropped messages."));
        assert!(page.contains("| 16 | 32000 | 1000 of 1000 | 0 | 0 | 1.5 % | 0.25 | complete |"));
        assert!(
            page.contains("| 32 | 64000 | 990 of 1000 | 0 | 10 | 1.5 % | 0.25 | **incomplete** |")
        );
    }
}
