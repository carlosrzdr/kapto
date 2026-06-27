// SPDX-License-Identifier: Apache-2.0

//! Checks of when each stream's messages arrived, from the timing read
//! with the episode's roles: when it started and ended, its rate, and its
//! silences. A stream kept for late subscribers, which may rightly send
//! nothing for long, and one that sent nothing at all (`streams.present`'s)
//! are left out.

use kapto_core::KaptoError;
use kapto_store::TimeRange;

use crate::timing::{BINS, LONGEST, StreamTiming};
use crate::{Check, CheckKind, EpisodeView, Finding};

const SECOND: f64 = 1e9;

/// Each stream sent its first message within `max_start_delay_s` of Start
/// being asked and its last within `max_end_gap_s` of Stop: advisory, as a
/// stream late to start or stopping early (a node slow to come up, a
/// driver that crashed) leaves part of the episode without it.
#[derive(Clone, Copy, Debug)]
pub struct StreamsTiming {
    /// The longest a stream may take to send its first message, in seconds.
    pub max_start_delay_s: f64,
    /// The longest a stream may be silent before the end, in seconds.
    pub max_end_gap_s: f64,
}

/// Each stream with a declared rate kept to it: measured from its first
/// message to its last, no more than `max_error` off, and in the last fifth
/// of the episode no slower than `min_end_ratio` of the first: advisory, as
/// a source that is slower than declared, or slows down as it goes (a CPU
/// throttling, a network filling), records less than asked.
#[derive(Clone, Copy, Debug)]
pub struct StreamsRate {
    /// The largest share the measured rate may be off the declared one.
    pub max_error: f64,
    /// The least share of its starting rate a stream keeps at the end.
    pub min_end_ratio: f64,
}

/// No stream was silent longer than `max_seconds`, or three of its periods
/// if that is longer: advisory, as a silence is a stretch of the episode
/// without it. Each one is named with where it is.
#[derive(Clone, Copy, Debug)]
pub struct StreamsGap {
    /// The longest silence not flagged, in seconds.
    pub max_seconds: f64,
}

/// The parts the rate at the start and at the end are measured over: a
/// fifth of the episode each.
const FIFTH: usize = BINS / 5;

/// The fewest messages in the first fifth for a drop to be judged.
const MIN_TO_JUDGE: u64 = 10;

/// The streams the timing checks look at: those with messages, but the
/// kept ones.
fn timed(episode: &EpisodeView) -> Vec<(&str, &StreamTiming)> {
    let kept = |name: &str| {
        episode
            .summary()
            .streams
            .iter()
            .any(|stream| stream.id == name && stream.carried.is_some())
    };
    episode
        .timing()
        .iter()
        .filter(|(name, timing)| timing.messages > 0 && !kept(name))
        .map(|(name, timing)| (name.as_str(), timing))
        .collect()
}

/// Nanoseconds as seconds.
#[allow(
    clippy::cast_precision_loss,
    reason = "spans of an episode, in seconds"
)]
fn seconds(nanos: i64) -> f64 {
    nanos as f64 / SECOND
}

impl Check for StreamsTiming {
    fn id(&self) -> &'static str {
        "streams.timing"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let streams = timed(episode);
        if streams.is_empty() {
            return Ok(Finding::not_applicable("no stream sent a message"));
        }
        let summary = episode.summary();
        let from = summary
            .commanded_start_utc_ns
            .unwrap_or(summary.started_utc_ns);
        let to = summary
            .commanded_stop_utc_ns
            .unwrap_or(summary.ended_utc_ns);
        let mut problems = Vec::new();
        let mut ranges = Vec::new();
        for (name, timing) in &streams {
            let late = seconds(timing.first_utc_ns - from);
            if late > self.max_start_delay_s {
                problems.push(format!("{name} started {late:.1} s after Start"));
                ranges.push(TimeRange {
                    start_utc_ns: from,
                    end_utc_ns: timing.first_utc_ns,
                });
            }
            let early = seconds(to - timing.last_utc_ns);
            if early > self.max_end_gap_s {
                problems.push(format!("{name} ended {early:.1} s before Stop"));
                ranges.push(TimeRange {
                    start_utc_ns: timing.last_utc_ns,
                    end_utc_ns: to,
                });
            }
        }
        #[allow(clippy::cast_precision_loss, reason = "a count of streams")]
        let finding = if problems.is_empty() {
            Finding::passed("every stream sent from Start to Stop")
        } else {
            Finding::failed(problems.join("; "))
        }
        .with_value("late_or_early", problems.len() as f64);
        Ok(ranges.into_iter().fold(finding, Finding::with_range))
    }
}

impl Check for StreamsRate {
    fn id(&self) -> &'static str {
        "streams.rate"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let streams: Vec<(&str, &StreamTiming, u32)> = timed(episode)
            .into_iter()
            .filter_map(|(name, timing)| Some((name, timing, timing.rate_hz?)))
            .filter(|(_, timing, _)| timing.messages >= 2)
            .collect();
        if streams.is_empty() {
            return Ok(Finding::not_applicable(
                "no stream with a declared rate sent two messages",
            ));
        }
        let summary = episode.summary();
        let span = summary.ended_utc_ns - summary.started_utc_ns;
        let mut problems = Vec::new();
        let mut finding_values = Vec::new();
        let mut ranges = Vec::new();
        for (name, timing, declared) in streams {
            let lasted = seconds(timing.last_utc_ns - timing.first_utc_ns).max(f64::EPSILON);
            #[allow(clippy::cast_precision_loss, reason = "a count of messages")]
            let measured = (timing.messages - 1) as f64 / lasted;
            let error = (measured / f64::from(declared) - 1.0).abs();
            if error > self.max_error {
                problems.push(format!("{name} at {measured:.0} Hz, declared {declared}"));
            }
            finding_values.push((format!("{name}.rate_hz"), measured));
            let (start, end) = (timing.sent_in(0, FIFTH), timing.sent_in(BINS - FIFTH, BINS));
            if start >= MIN_TO_JUDGE {
                #[allow(clippy::cast_precision_loss, reason = "counts of messages")]
                let kept = end as f64 / start as f64;
                if kept < self.min_end_ratio {
                    problems.push(format!(
                        "{name} sent {:.0} % as fast in the last fifth as in the first",
                        kept * 100.0
                    ));
                    ranges.push(TimeRange {
                        start_utc_ns: summary.ended_utc_ns - span / 5,
                        end_utc_ns: summary.ended_utc_ns,
                    });
                }
                finding_values.push((format!("{name}.end_ratio"), kept));
            }
        }
        let finding = if problems.is_empty() {
            Finding::passed("every stream kept to its declared rate")
        } else {
            Finding::failed(problems.join("; "))
        };
        let finding = finding_values
            .into_iter()
            .fold(finding, |finding, (name, value)| {
                finding.with_value(&name, value)
            });
        Ok(ranges.into_iter().fold(finding, Finding::with_range))
    }
}

impl Check for StreamsGap {
    fn id(&self) -> &'static str {
        "streams.gap"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let streams = timed(episode);
        if streams.is_empty() {
            return Ok(Finding::not_applicable("no stream sent a message"));
        }
        let mut problems = Vec::new();
        let mut finding_values = Vec::new();
        let mut ranges = Vec::new();
        for (name, timing) in streams {
            let periods = timing
                .rate_hz
                .map_or(0.0, |rate| 3.0 / f64::from(rate.max(1)));
            let longest_allowed = self.max_seconds.max(periods);
            let silent: Vec<&TimeRange> = timing
                .silences
                .iter()
                .filter(|range| seconds(range.end_utc_ns - range.start_utc_ns) > longest_allowed)
                .collect();
            let longest = timing
                .silences
                .first()
                .map_or(0.0, |range| seconds(range.end_utc_ns - range.start_utc_ns));
            finding_values.push((format!("{name}.longest_silence_s"), longest));
            if silent.is_empty() {
                continue;
            }
            // Only the longest silences are kept: all of them flagged may be
            // fewer than there were.
            let times = match silent.len() {
                1 => "once".to_owned(),
                count if count == LONGEST => format!("{count} times or more"),
                count => format!("{count} times"),
            };
            problems.push(format!("{name} silent {times}, for up to {longest:.1} s"));
            let mut found: Vec<TimeRange> = silent.into_iter().copied().collect();
            found.sort_by_key(|range| range.start_utc_ns);
            ranges.extend(found);
        }
        let finding = if problems.is_empty() {
            Finding::passed("no stream was silent for long")
        } else {
            Finding::failed(problems.join("; "))
        };
        let finding = finding_values
            .into_iter()
            .fold(finding, |finding, (name, value)| {
                finding.with_value(&name, value)
            });
        Ok(ranges.into_iter().fold(finding, Finding::with_range))
    }
}

#[cfg(test)]
mod tests {
    use kapto_store::{StreamSummary, Verdict};

    use super::*;
    use crate::timing::TimingReader;

    const NS: i64 = 1_000_000_000;
    const START: i64 = 1_772_700_400 * NS;

    /// A 10 s episode, Start asked 1 s in and Stop 1 s before its end, of
    /// `arm` at 100 Hz (declared) sending at each of `times` (seconds from
    /// its start), and of a kept stream that sent once.
    fn episode(times: impl Iterator<Item = f64>) -> EpisodeView {
        let declared = kapto_config::parse_streams(&kapto_config::ConfigFile {
            path: "streams.toml".into(),
            contents: "[streams.arm]\nrate_hz = 100\nudp = { bind = \"127.0.0.1:9000\" }\n"
                .to_owned(),
        })
        .unwrap();
        let mut reader = TimingReader::new((START, START + 10 * NS), &declared);
        for second in times {
            #[allow(clippy::cast_possible_truncation, reason = "a test's times")]
            reader.take("arm", u64::try_from(START + (second * 1e9) as i64).unwrap());
        }
        reader.take("tf_static", u64::try_from(START).unwrap());
        let summary = kapto_testkit::episode(
            "20260305T192345.123Z-1a2b3c4d",
            (START, START + 10 * NS),
            vec![
                kapto_testkit::stream("arm", 1000),
                StreamSummary {
                    carried: Some(1),
                    ..kapto_testkit::stream("tf_static", 1)
                },
            ],
        )
        .with_commands((START + NS, Some(START + 9 * NS)));
        EpisodeView::new(std::path::Path::new("."), summary).with_timing(reader.finish())
    }

    /// Every hundredth of a second from `from` to `to`.
    fn steady(from: f64, to: f64) -> impl Iterator<Item = f64> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a test's times"
        )]
        let count = ((to - from) * 100.0).round() as u32;
        (0..count).map(move |n| from + f64::from(n) / 100.0)
    }

    const TIMING: StreamsTiming = StreamsTiming {
        max_start_delay_s: 2.0,
        max_end_gap_s: 5.0,
    };
    const RATE: StreamsRate = StreamsRate {
        max_error: 0.15,
        min_end_ratio: 0.5,
    };
    const GAP: StreamsGap = StreamsGap { max_seconds: 2.0 };

    fn verdict(check: &dyn Check, episode: &EpisodeView) -> (Verdict, String) {
        let finding = check.run(episode).unwrap();
        (finding.verdict, finding.message)
    }

    #[test]
    fn a_stream_sending_at_its_rate_from_start_to_end_passes_every_timing_check() {
        let fine = episode(steady(0.0, 10.0));
        for check in [&TIMING as &dyn Check, &RATE, &GAP] {
            assert_eq!(verdict(check, &fine).0, Verdict::Passed, "{}", check.id());
        }
        let rate = RATE.run(&fine).unwrap();
        assert!((rate.values["arm.rate_hz"] - 100.0).abs() < 0.5);
    }

    #[test]
    fn a_stream_starting_late_or_ending_early_is_flagged_from_start_and_stop() {
        // 4 s after Start was asked, and 6 s before Stop.
        let late = episode(steady(5.0, 9.0));
        assert_eq!(
            verdict(&TIMING, &late),
            (Verdict::Failed, "arm started 4.0 s after Start".to_owned())
        );
        let early = TIMING.run(&episode(steady(0.0, 3.0))).unwrap();
        assert_eq!(early.message, "arm ended 6.0 s before Stop");
        assert_eq!(
            early.ranges,
            [TimeRange {
                start_utc_ns: START + 299 * NS / 100,
                end_utc_ns: START + 9 * NS
            }]
        );
    }

    #[test]
    fn a_stream_off_its_rate_or_slowing_down_is_flagged() {
        // Every 13 ms: 77 Hz where 100 are declared.
        let slow = episode((0..770).map(|n| f64::from(n) * 0.013));
        assert_eq!(
            verdict(&RATE, &slow),
            (Verdict::Failed, "arm at 77 Hz, declared 100".to_owned())
        );
        // At its rate for 8 s, then one message in five.
        let slowing = episode(steady(0.0, 8.0).chain(steady(8.0, 10.0).step_by(5)));
        let finding = RATE.run(&slowing).unwrap();
        // Which slows it on average too.
        assert_eq!(
            finding.message,
            "arm at 84 Hz, declared 100; arm sent 20 % as fast in the last fifth as in the first"
        );
        assert_eq!(finding.ranges[0].start_utc_ns, START + 8 * NS);
    }

    #[test]
    fn each_silence_longer_than_allowed_is_named_where_it_is() {
        let silent = episode(steady(0.0, 3.0).chain(steady(6.0, 10.0)));
        let finding = GAP.run(&silent).unwrap();
        assert_eq!(finding.message, "arm silent once, for up to 3.0 s");
        assert_eq!(
            finding.ranges,
            [TimeRange {
                start_utc_ns: START + 299 * NS / 100,
                end_utc_ns: START + 6 * NS
            }]
        );
        // The kept stream, silent the whole episode, isn't flagged.
        assert!(!finding.message.contains("tf_static"));
    }
}
