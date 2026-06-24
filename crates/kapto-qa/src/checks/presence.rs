// SPDX-License-Identifier: Apache-2.0

//! Checks of an episode's shape, from what `episode.json` says: how long it
//! lasted and which streams have messages in it.

use kapto_core::KaptoError;
use kapto_store::StreamSummary;

use crate::{Check, CheckKind, EpisodeView, Finding};

/// The episode lasted at least `min_seconds`, from its first message to its
/// end: a gate, as one shorter can hardly hold a task (a start pressed by
/// mistake, a recording cut short).
#[derive(Clone, Copy, Debug)]
pub struct EpisodeShort {
    /// The shortest episode accepted, in seconds.
    pub min_seconds: f64,
}

/// The episode lasted no longer than `max_seconds`: advisory, as one far
/// longer than its task was likely left recording.
#[derive(Clone, Copy, Debug)]
pub struct EpisodeLong {
    /// The longest episode not flagged, in seconds.
    pub max_seconds: f64,
}

/// How long `episode` lasted, in seconds.
fn seconds(episode: &EpisodeView) -> f64 {
    let summary = episode.summary();
    // Nanosecond spans stay far below 2^53 s, so the loss is below a
    // nanosecond.
    #[allow(clippy::cast_precision_loss, reason = "a span in seconds")]
    let seconds = summary.ended_utc_ns.saturating_sub(summary.started_utc_ns) as f64 / 1e9;
    seconds.max(0.0)
}

impl Check for EpisodeShort {
    fn id(&self) -> &'static str {
        "episode.short"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Gate
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let seconds = seconds(episode);
        let finding = if seconds < self.min_seconds {
            Finding::failed(format!(
                "the episode lasted {seconds:.1} s, shorter than the {} s accepted",
                self.min_seconds
            ))
        } else {
            Finding::passed(format!("the episode lasted {seconds:.1} s"))
        };
        Ok(finding.with_value("duration_s", seconds))
    }
}

impl Check for EpisodeLong {
    fn id(&self) -> &'static str {
        "episode.long"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let seconds = seconds(episode);
        let finding = if seconds > self.max_seconds {
            Finding::failed(format!(
                "the episode lasted {seconds:.0} s, longer than {} s: was it left recording?",
                self.max_seconds
            ))
        } else {
            Finding::passed(format!("the episode lasted {seconds:.1} s"))
        };
        Ok(finding.with_value("duration_s", seconds))
    }
}

/// Every critical stream has messages in the episode: a gate, as an episode
/// without one (a camera unplugged, a topic never published) lacks what it
/// was recorded for. With no stream named critical, every stream is, but
/// those kept for late subscribers, which may rightly send nothing while
/// an episode is recorded.
#[derive(Clone, Debug, Default)]
pub struct StreamsPresent {
    /// The critical streams, by name; empty for every stream but the kept.
    pub critical: Vec<String>,
}

/// Each stream [`StreamsPresent`] doesn't hold to has messages in the
/// episode: advisory, to say which are empty without rejecting it. A kept
/// stream counts the messages it started with.
#[derive(Clone, Debug, Default)]
pub struct StreamsEmpty {
    /// The streams [`StreamsPresent`] holds to; empty for every stream but
    /// the kept.
    pub critical: Vec<String>,
}

/// Whether `stream` is one `critical` names, or with none named, any but
/// a kept one.
fn is_critical(critical: &[String], stream: &StreamSummary) -> bool {
    if critical.is_empty() {
        stream.carried.is_none()
    } else {
        critical.contains(&stream.id)
    }
}

/// Whether `stream` holds any message, the kept ones it started with too.
fn has_messages(stream: &StreamSummary) -> bool {
    stream.written + stream.carried.unwrap_or(0) > 0
}

impl Check for StreamsPresent {
    fn id(&self) -> &'static str {
        "streams.present"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Gate
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let streams = &episode.summary().streams;
        let missing: Vec<&str> = self
            .critical
            .iter()
            .filter(|name| !streams.iter().any(|stream| &stream.id == *name))
            .map(String::as_str)
            .collect();
        let held: Vec<&StreamSummary> = streams
            .iter()
            .filter(|stream| is_critical(&self.critical, stream))
            .collect();
        if held.is_empty() && missing.is_empty() {
            return Ok(Finding::not_applicable(
                "the episode has no critical stream",
            ));
        }
        let empty: Vec<&str> = held
            .iter()
            .filter(|stream| !has_messages(stream))
            .map(|stream| stream.id.as_str())
            .collect();
        let mut problems = Vec::new();
        if !empty.is_empty() {
            problems.push(format!("no messages from {}", empty.join(", ")));
        }
        if !missing.is_empty() {
            problems.push(format!("no stream {} in the episode", missing.join(", ")));
        }
        // Counts of streams, far below 2^53.
        #[allow(clippy::cast_precision_loss, reason = "a count of streams")]
        let finding = if problems.is_empty() {
            Finding::passed(format!("all {} critical streams have messages", held.len()))
        } else {
            Finding::failed(problems.join("; "))
        }
        .with_value("empty", (empty.len() + missing.len()) as f64);
        Ok(finding)
    }
}

impl Check for StreamsEmpty {
    fn id(&self) -> &'static str {
        "streams.empty"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let others: Vec<&StreamSummary> = episode
            .summary()
            .streams
            .iter()
            .filter(|stream| !is_critical(&self.critical, stream))
            .collect();
        if others.is_empty() {
            return Ok(Finding::not_applicable(
                "every stream is critical, for streams.present",
            ));
        }
        let empty: Vec<&str> = others
            .iter()
            .filter(|stream| !has_messages(stream))
            .map(|stream| stream.id.as_str())
            .collect();
        #[allow(clippy::cast_precision_loss, reason = "a count of streams")]
        let finding = if empty.is_empty() {
            Finding::passed("every stream has messages")
        } else {
            Finding::failed(format!("no messages from {}", empty.join(", ")))
        }
        .with_value("empty", empty.len() as f64);
        Ok(finding)
    }
}

#[cfg(test)]
mod tests {
    use kapto_store::Verdict;
    use kapto_testkit::stream;

    use super::*;

    fn episode(seconds: i64, streams: Vec<StreamSummary>) -> EpisodeView {
        let start = 1_772_700_400_000_000_000;
        let summary = kapto_testkit::episode(
            "20260305T192345.123Z-1a2b3c4d",
            (start, start + seconds * 1_000_000_000),
            streams,
        );
        EpisodeView::new(std::path::Path::new("."), summary)
    }

    fn kept(id: &str, carried: u64) -> StreamSummary {
        StreamSummary {
            carried: Some(carried),
            ..stream(id, 0)
        }
    }

    fn verdict(check: &dyn Check, episode: &EpisodeView) -> (Verdict, String) {
        let finding = check.run(episode).unwrap();
        (finding.verdict, finding.message)
    }

    #[test]
    fn an_episode_shorter_than_the_least_accepted_fails_and_a_long_one_is_flagged() {
        let short = EpisodeShort { min_seconds: 2.0 };
        let long = EpisodeLong { max_seconds: 600.0 };
        let streams = || vec![stream("joints", 10)];
        assert_eq!(
            verdict(&short, &episode(1, streams())),
            (
                Verdict::Failed,
                "the episode lasted 1.0 s, shorter than the 2 s accepted".to_owned()
            )
        );
        assert_eq!(verdict(&short, &episode(2, streams())).0, Verdict::Passed);
        assert_eq!(verdict(&long, &episode(600, streams())).0, Verdict::Passed);
        assert_eq!(
            verdict(&long, &episode(601, streams())),
            (
                Verdict::Failed,
                "the episode lasted 601 s, longer than 600 s: was it left recording?".to_owned()
            )
        );
        let finding = short.run(&episode(30, streams())).unwrap();
        assert_eq!(finding.values["duration_s"], 30.0);
    }

    #[test]
    fn with_none_named_every_stream_but_the_kept_must_have_messages() {
        let present = StreamsPresent::default();
        let empty = StreamsEmpty::default();
        let fine = episode(10, vec![stream("joints", 10), kept("tf_static", 0)]);
        assert_eq!(verdict(&present, &fine).0, Verdict::Passed);
        // The kept stream is only the advisory's: it kept nothing.
        assert_eq!(
            verdict(&empty, &fine),
            (Verdict::Failed, "no messages from tf_static".to_owned())
        );
        let silent = episode(10, vec![stream("joints", 10), stream("camera", 0)]);
        assert_eq!(
            verdict(&present, &silent),
            (Verdict::Failed, "no messages from camera".to_owned())
        );
        assert_eq!(verdict(&empty, &silent).0, Verdict::NotApplicable);
    }

    #[test]
    fn named_critical_streams_must_be_there_with_messages_and_the_rest_are_flagged() {
        let critical = vec!["joints".to_owned(), "wrist".to_owned()];
        let present = StreamsPresent {
            critical: critical.clone(),
        };
        let empty = StreamsEmpty { critical };
        let episode = episode(
            10,
            vec![stream("joints", 0), stream("camera", 0), kept("tf", 1)],
        );
        assert_eq!(
            verdict(&present, &episode),
            (
                Verdict::Failed,
                "no messages from joints; no stream wrist in the episode".to_owned()
            )
        );
        assert_eq!(
            verdict(&empty, &episode),
            (Verdict::Failed, "no messages from camera".to_owned())
        );
    }
}
