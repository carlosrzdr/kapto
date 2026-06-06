// SPDX-License-Identifier: Apache-2.0

//! The events Kapto records of its own during an episode: a stream losing
//! messages, falling silent and coming back, at the moment it happened,
//! which the episode's totals don't say. They are worked out from each
//! stream's counts once a second, on the thread that watches the episode,
//! never per message.

use std::time::Duration;

use kapto_store::events::{KAPTO_KINDS, KAPTO_SOURCE};
use kapto_store::{Event, Phase};

/// How long a stream may send nothing before it counts as silent.
pub(crate) const SILENT_AFTER: Duration = Duration::from_secs(2);

/// One stream's counts as the episode goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) received: u64,
    pub(crate) dropped_by_us: u64,
    pub(crate) lost_upstream: u64,
}

/// What a stream was last seen doing.
#[derive(Clone, Debug, Default)]
struct Seen {
    counts: Counts,
    /// When it last received anything, in the episode's time.
    last_received: Duration,
    silent: bool,
}

/// Works out Kapto's own events for one episode.
#[derive(Debug, Default)]
pub(crate) struct OwnEvents {
    streams: Vec<(String, Seen)>,
    /// The next whole second at which counts are compared.
    next_check: Duration,
}

impl OwnEvents {
    /// For an episode recording `streams`, none of them received from yet.
    pub(crate) fn new(streams: impl IntoIterator<Item = String>) -> Self {
        Self {
            streams: streams
                .into_iter()
                .map(|stream| (stream, Seen::default()))
                .collect(),
            next_check: Duration::from_secs(1),
        }
    }

    /// The events each stream's `counts` at `elapsed` into the episode call
    /// for, at `now_utc_ns`: compared once a second, so at most one of each
    /// kind per stream per second. A stream silent for [`SILENT_AFTER`] is
    /// said to be once, and back once it sends again.
    pub(crate) fn tick(
        &mut self,
        counts: &[Counts],
        elapsed: Duration,
        now_utc_ns: i64,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        for ((stream, seen), now) in self.streams.iter_mut().zip(counts) {
            if now.received > seen.counts.received {
                seen.last_received = elapsed;
            }
            if elapsed < self.next_check {
                continue;
            }
            let lost = now.lost_upstream.saturating_sub(seen.counts.lost_upstream);
            let dropped = now.dropped_by_us.saturating_sub(seen.counts.dropped_by_us);
            if lost > 0 || dropped > 0 {
                let mut event = own("stream_lost", now_utc_ns);
                event.attributes.insert("stream".to_owned(), stream.clone());
                event
                    .attributes
                    .insert("lost_upstream".to_owned(), lost.to_string());
                event
                    .attributes
                    .insert("dropped_by_us".to_owned(), dropped.to_string());
                events.push(event);
            }
            let quiet = elapsed.saturating_sub(seen.last_received) >= SILENT_AFTER;
            if quiet != seen.silent {
                seen.silent = quiet;
                let kind = if quiet {
                    "stream_silent"
                } else {
                    "stream_back"
                };
                let mut event = own(kind, now_utc_ns);
                event.attributes.insert("stream".to_owned(), stream.clone());
                events.push(event);
            }
            seen.counts = *now;
        }
        if elapsed >= self.next_check {
            self.next_check = Duration::from_secs(elapsed.as_secs() + 1);
        }
        events
    }
}

/// One of Kapto's own events, `kapto.<kind>`, at `at_utc_ns`.
fn own(kind: &str, at_utc_ns: i64) -> Event {
    Event {
        kind: format!("{KAPTO_KINDS}{kind}"),
        at_utc_ns,
        until_utc_ns: None,
        phase: Phase::Moment,
        id: None,
        source: KAPTO_SOURCE.to_owned(),
        text: None,
        attributes: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(received: u64, lost_upstream: u64) -> [Counts; 1] {
        [Counts {
            received,
            dropped_by_us: 0,
            lost_upstream,
        }]
    }

    fn kinds(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .map(|event| {
                let detail = event
                    .attributes
                    .get("lost_upstream")
                    .map_or(String::new(), |lost| format!(" lost {lost}"));
                format!("{}{detail}", event.kind)
            })
            .collect()
    }

    #[test]
    fn losses_are_told_at_most_once_a_second_with_how_many() {
        let mut own = OwnEvents::new(["joints".to_owned()]);
        let at = |ms: u64| Duration::from_millis(ms);
        // Three losses within the first second: told once, with all three.
        assert_eq!(own.tick(&counts(10, 1), at(300), 0), []);
        assert_eq!(own.tick(&counts(20, 3), at(600), 0), []);
        assert_eq!(
            kinds(&own.tick(&counts(30, 3), at(1_000), 0)),
            ["kapto.stream_lost lost 3"]
        );
        // Nothing more lost: nothing told.
        assert_eq!(own.tick(&counts(40, 3), at(2_000), 0), []);
        let event = &own.tick(&counts(50, 5), at(3_000), 7)[0];
        assert_eq!(event.attributes["stream"], "joints");
        assert_eq!((event.source.as_str(), event.at_utc_ns), ("kapto", 7));
        assert!(event.check(true).is_ok(), "Kapto's own, well formed");
    }

    #[test]
    fn a_stream_sending_nothing_is_silent_once_and_back_once_it_sends() {
        let mut own = OwnEvents::new(["joints".to_owned()]);
        let mut told = Vec::new();
        let mut received = 0;
        for second in 1..=8_u64 {
            // Silent from 2 s to 6 s.
            if !(2..6).contains(&second) {
                received += 100;
            }
            told.extend(kinds(&own.tick(
                &counts(received, 0),
                Duration::from_secs(second),
                0,
            )));
        }
        assert_eq!(told, ["kapto.stream_silent", "kapto.stream_back"]);
    }
}
