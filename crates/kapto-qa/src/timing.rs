// SPDX-License-Identifier: Apache-2.0

//! When each stream's messages arrived through an episode, kept in a few
//! numbers rather than message by message, so an hour at 2 kHz costs no
//! more than a second: its first and last message, how many it sent in
//! each hundredth of the episode, and its longest silences. Read in the
//! same pass as the roles.

use std::collections::BTreeMap;

use kapto_config::StreamDeclaration;
use kapto_store::TimeRange;

/// The parts an episode is cut into to count each stream's messages.
pub const BINS: usize = 100;

/// The most silences kept per stream, the longest.
pub const LONGEST: usize = 20;

/// When one stream's messages arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamTiming {
    /// Its declared rate, if it has one.
    pub rate_hz: Option<u32>,
    /// How many messages it sent.
    pub messages: u64,
    /// When its first and last were received, in UTC nanoseconds; both 0
    /// without any.
    pub first_utc_ns: i64,
    /// See `first_utc_ns`.
    pub last_utc_ns: i64,
    /// How many it sent in each of [`BINS`] equal parts of the episode,
    /// from its start to its end.
    pub bins: Vec<u64>,
    /// Its longest silences between two messages, longest first, at most
    /// [`LONGEST`].
    pub silences: Vec<TimeRange>,
}

impl StreamTiming {
    fn new(rate_hz: Option<u32>) -> Self {
        Self {
            rate_hz,
            messages: 0,
            first_utc_ns: 0,
            last_utc_ns: 0,
            bins: vec![0; BINS],
            silences: Vec::new(),
        }
    }

    /// How many it sent in the parts `from` to `to` of [`BINS`].
    #[must_use]
    pub fn sent_in(&self, from: usize, to: usize) -> u64 {
        self.bins
            .get(from..to.min(BINS))
            .map_or(0, |bins| bins.iter().sum())
    }
}

/// Every stream's timing so far, by its name.
#[derive(Debug)]
pub(crate) struct TimingReader {
    /// The episode's span, in UTC nanoseconds.
    span: (i64, i64),
    streams: BTreeMap<String, StreamTiming>,
}

impl TimingReader {
    /// Timing of the streams through an episode spanning `span`, in UTC
    /// nanoseconds, each with the rate `declared` gives it.
    pub(crate) fn new(span: (i64, i64), declared: &[StreamDeclaration]) -> Self {
        let streams = declared
            .iter()
            .map(|stream| (stream.id.to_string(), StreamTiming::new(stream.rate_hz)))
            .collect();
        Self { span, streams }
    }

    /// Takes a message of the stream `topic` received at `log_time`.
    pub(crate) fn take(&mut self, topic: &str, log_time: u64) {
        let time = i64::try_from(log_time).unwrap_or(i64::MAX);
        let stream = match self.streams.get_mut(topic) {
            Some(stream) => stream,
            None => self
                .streams
                .entry(topic.to_owned())
                .or_insert_with(|| StreamTiming::new(None)),
        };
        if stream.messages == 0 {
            stream.first_utc_ns = time;
        } else if time > stream.last_utc_ns {
            keep_if_longest(
                &mut stream.silences,
                TimeRange {
                    start_utc_ns: stream.last_utc_ns,
                    end_utc_ns: time,
                },
            );
        }
        stream.messages += 1;
        stream.last_utc_ns = stream.last_utc_ns.max(time);
        let (start, end) = self.span;
        let length = i128::from(end.saturating_sub(start)).max(1);
        let bin = i128::from(time.saturating_sub(start)) * BINS as i128 / length;
        let bin = usize::try_from(bin.clamp(0, BINS as i128 - 1)).unwrap_or(0);
        stream.bins[bin] += 1;
    }

    /// Every stream's timing, those that sent nothing too.
    pub(crate) fn finish(self) -> BTreeMap<String, StreamTiming> {
        self.streams
    }
}

/// Keeps `silence` among `longest`, longest first, if it is one of the
/// [`LONGEST`].
fn keep_if_longest(longest: &mut Vec<TimeRange>, silence: TimeRange) {
    let length = |range: &TimeRange| range.end_utc_ns - range.start_utc_ns;
    if longest.len() == LONGEST
        && longest
            .last()
            .is_some_and(|shortest| length(shortest) >= length(&silence))
    {
        return;
    }
    let at = longest.partition_point(|kept| length(kept) >= length(&silence));
    longest.insert(at, silence);
    longest.truncate(LONGEST);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;
    const START: i64 = 1_772_700_400 * SECOND;

    fn declared(text: &str) -> Vec<StreamDeclaration> {
        kapto_config::parse_streams(&kapto_config::ConfigFile {
            path: "streams.toml".into(),
            contents: text.to_owned(),
        })
        .unwrap()
    }

    #[test]
    fn each_stream_keeps_its_first_last_count_per_part_and_longest_silences() {
        let streams = declared(
            "[streams.arm]\nrate_hz = 10\nudp = { bind = \"127.0.0.1:9000\" }\n\
             [streams.quiet]\nrate_hz = 5\nudp = { bind = \"127.0.0.1:9001\" }\n",
        );
        let mut reader = TimingReader::new((START, START + 10 * SECOND), &streams);
        // Every tenth of a second, but for a silence from 4 s to 7 s.
        for tenth in (0..100).filter(|tenth| !(41..70).contains(tenth)) {
            reader.take("arm", u64::try_from(START + tenth * SECOND / 10).unwrap());
        }
        reader.take("undeclared", u64::try_from(START).unwrap());
        let timing = reader.finish();
        let arm = &timing["arm"];
        assert_eq!(arm.rate_hz, Some(10));
        assert_eq!(arm.messages, 71);
        assert_eq!(
            (arm.first_utc_ns, arm.last_utc_ns),
            (START, START + 99 * SECOND / 10)
        );
        assert_eq!(arm.sent_in(0, 10), 10);
        assert_eq!(arm.sent_in(50, 60), 0);
        assert_eq!(arm.sent_in(0, BINS), 71);
        assert_eq!(arm.silences.len(), LONGEST);
        assert_eq!(
            arm.silences[0],
            TimeRange {
                start_utc_ns: START + 4 * SECOND,
                end_utc_ns: START + 7 * SECOND
            }
        );
        assert_eq!(timing["quiet"].messages, 0);
        assert_eq!(timing["undeclared"].rate_hz, None);
    }

    #[test]
    fn only_the_longest_silences_are_kept_longest_first() {
        let mut longest = Vec::new();
        for length in [5, 1, 9, 3].into_iter().chain(0..30) {
            keep_if_longest(
                &mut longest,
                TimeRange {
                    start_utc_ns: 0,
                    end_utc_ns: length,
                },
            );
        }
        let lengths: Vec<i64> = longest.iter().map(|range| range.end_utc_ns).collect();
        assert_eq!(lengths[..4], [29, 28, 27, 26]);
        assert_eq!(lengths.len(), LONGEST);
        assert!(lengths.windows(2).all(|pair| pair[0] >= pair[1]));
    }
}
