// SPDX-License-Identifier: Apache-2.0

//! Checks of an episode's segments: the subtasks marked in it, as
//! `subtask.<name>` stretches, against those its task declares.

use std::collections::BTreeMap;

use kapto_core::KaptoError;
use kapto_store::{Phase, TimeRange};

use crate::{Check, CheckKind, EpisodeView, Finding, seconds};

/// The beginning of the kinds that mark a subtask.
const SUBTASK: &str = "subtask.";

/// An episode of a task that declares subtasks marks at least one: a gate,
/// as such an episode is to be cut into them, and one without any can't
/// be.
#[derive(Clone, Copy, Debug, Default)]
pub struct SegmentsPresent;

/// The subtasks marked cover the episode: no more than `max_gap_s` between
/// two, at least `min_coverage` of the time from Start to Stop, none
/// shorter than `min_seconds`: advisory, as each says the marking was
/// loose rather than that the data is wrong.
#[derive(Clone, Copy, Debug)]
pub struct SegmentsLayout {
    /// The longest time between two subtasks not flagged, in seconds.
    pub max_gap_s: f64,
    /// The least share of the time from Start to Stop covered.
    pub min_coverage: f64,
    /// The shortest subtask not flagged, in seconds.
    pub min_seconds: f64,
}

/// The episode's span to judge segments in: from Start being asked to
/// Stop, or its own start and end without them.
fn window(episode: &EpisodeView) -> (i64, i64) {
    let summary = episode.summary();
    (
        summary
            .commanded_start_utc_ns
            .unwrap_or(summary.started_utc_ns),
        summary
            .commanded_stop_utc_ns
            .unwrap_or(summary.ended_utc_ns),
    )
}

/// Every subtask marked, by when it started: a stretch begun and never
/// ended lasts to `end`.
fn subtasks(episode: &EpisodeView, end: i64) -> Vec<(String, TimeRange)> {
    let mut found = Vec::new();
    let mut open: BTreeMap<(String, Option<String>), i64> = BTreeMap::new();
    for event in episode.events() {
        let Some(name) = event.kind.strip_prefix(SUBTASK) else {
            continue;
        };
        let range = |start, end| TimeRange {
            start_utc_ns: start,
            end_utc_ns: end,
        };
        let key = (event.kind.clone(), event.id.clone());
        match event.phase {
            Phase::Stretch => found.push((
                name.to_owned(),
                range(
                    event.at_utc_ns,
                    event.until_utc_ns.unwrap_or(event.at_utc_ns),
                ),
            )),
            Phase::Begin => {
                open.insert(key, event.at_utc_ns);
            }
            Phase::End => {
                if let Some(start) = open.remove(&key) {
                    found.push((name.to_owned(), range(start, event.at_utc_ns)));
                }
            }
            Phase::Moment => {}
        }
    }
    for ((kind, _), start) in open {
        let name = kind.trim_start_matches(SUBTASK).to_owned();
        found.push((
            name,
            TimeRange {
                start_utc_ns: start,
                end_utc_ns: end.max(start),
            },
        ));
    }
    found.sort_by_key(|(_, range)| (range.start_utc_ns, range.end_utc_ns));
    found
}

impl Check for SegmentsPresent {
    fn id(&self) -> &'static str {
        "segments.present"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Gate
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let Some(task) = episode
            .summary()
            .task
            .as_ref()
            .filter(|task| !task.subtasks.is_empty())
        else {
            return Ok(Finding::not_applicable("its task declares no subtasks"));
        };
        let marked = subtasks(episode, window(episode).1);
        #[allow(clippy::cast_precision_loss, reason = "a count of subtasks")]
        let finding = if marked.is_empty() {
            Finding::failed(format!(
                "its task {} declares {} subtasks, and none was marked",
                task.name.as_deref().unwrap_or(&task.id),
                task.subtasks.len()
            ))
        } else {
            Finding::passed(format!("{} subtasks marked", marked.len()))
        }
        .with_value("marked", marked.len() as f64);
        Ok(finding)
    }
}

impl Check for SegmentsLayout {
    fn id(&self) -> &'static str {
        "segments.layout"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let (from, to) = window(episode);
        let marked = subtasks(episode, to);
        if marked.is_empty() {
            return Ok(Finding::not_applicable("no subtask was marked"));
        }
        let mut problems = Vec::new();
        let mut ranges = Vec::new();
        // What they cover together, overlaps counted once, and the gaps
        // between.
        let mut covered = 0;
        let mut gaps = Vec::new();
        let mut reach: Option<i64> = None;
        for (_, range) in &marked {
            let start = range.start_utc_ns.clamp(from, to);
            let end = range.end_utc_ns.clamp(from, to);
            match reach {
                Some(last) if start > last => {
                    if seconds(start - last) > self.max_gap_s {
                        gaps.push(TimeRange {
                            start_utc_ns: last,
                            end_utc_ns: start,
                        });
                    }
                    covered += end - start;
                    reach = Some(end);
                }
                Some(last) => {
                    covered += (end - last).max(0);
                    reach = Some(last.max(end));
                }
                None => {
                    covered += end - start;
                    reach = Some(end);
                }
            }
        }
        if !gaps.is_empty() {
            let longest = gaps
                .iter()
                .map(|gap| seconds(gap.end_utc_ns - gap.start_utc_ns))
                .fold(0.0, f64::max);
            problems.push(format!(
                "{} gaps between subtasks, up to {longest:.1} s",
                gaps.len()
            ));
            ranges.extend(gaps);
        }
        let coverage = seconds(covered) / seconds(to - from).max(f64::EPSILON);
        if coverage < self.min_coverage {
            problems.push(format!(
                "subtasks cover {:.0} % of the episode",
                coverage * 100.0
            ));
        }
        let short: Vec<&(String, TimeRange)> = marked
            .iter()
            .filter(|(_, range)| seconds(range.end_utc_ns - range.start_utc_ns) < self.min_seconds)
            .collect();
        if !short.is_empty() {
            let names: Vec<&str> = short.iter().map(|(name, _)| name.as_str()).collect();
            problems.push(format!(
                "shorter than {} s: {}",
                self.min_seconds,
                names.join(", ")
            ));
            ranges.extend(short.iter().map(|(_, range)| *range));
        }
        let finding = if problems.is_empty() {
            Finding::passed(format!("{} subtasks cover the episode", marked.len()))
        } else {
            Finding::failed(problems.join("; "))
        }
        .with_value("coverage", coverage);
        ranges.sort_by_key(|range| (range.start_utc_ns, range.end_utc_ns));
        Ok(ranges.into_iter().fold(finding, Finding::with_range))
    }
}

#[cfg(test)]
mod tests {
    use kapto_store::{Event, SessionTask, Verdict};

    use super::*;

    const NS: i64 = 1_000_000_000;
    const START: i64 = 1_772_700_400 * NS;

    /// A 20 s episode of Pick and place, Start asked 1 s in and Stop 1 s
    /// before its end, with `events`.
    fn episode(events: Vec<Event>, subtasks: &[&str]) -> EpisodeView {
        let mut summary = kapto_testkit::episode(
            "20260305T192345.123Z-1a2b3c4d",
            (START, START + 20 * NS),
            vec![kapto_testkit::stream("arm", 100)],
        )
        .with_commands((START + NS, Some(START + 19 * NS)));
        summary.task = Some(SessionTask {
            id: "pick_and_place".to_owned(),
            hash: None,
            name: Some("Pick and place".to_owned()),
            description: None,
            prompt: Some("Pick up the cube".to_owned()),
            objects: Vec::new(),
            subtasks: subtasks.iter().map(|name| (*name).to_owned()).collect(),
        });
        EpisodeView::new(std::path::Path::new("."), summary).with_events(events)
    }

    fn event(kind: &str, phase: Phase, at_s: i64, until_s: Option<i64>) -> Event {
        Event {
            kind: kind.to_owned(),
            at_utc_ns: START + at_s * NS,
            until_utc_ns: until_s.map(|until| START + until * NS),
            phase,
            id: (phase != Phase::Stretch).then(|| "1".to_owned()),
            source: "page".to_owned(),
            text: None,
            attributes: BTreeMap::new(),
        }
    }

    const LAYOUT: SegmentsLayout = SegmentsLayout {
        max_gap_s: 5.0,
        min_coverage: 0.5,
        min_seconds: 1.0,
    };

    #[test]
    fn a_task_with_subtasks_needs_one_marked_and_one_without_is_not_held_to_it() {
        let none = episode(
            vec![event("grasp", Phase::Moment, 3, None)],
            &["reach", "grasp"],
        );
        let finding = SegmentsPresent.run(&none).unwrap();
        assert_eq!(finding.verdict, Verdict::Failed);
        assert_eq!(
            finding.message,
            "its task Pick and place declares 2 subtasks, and none was marked"
        );
        let marked = episode(
            vec![event("subtask.reach", Phase::Stretch, 1, Some(9))],
            &["reach", "grasp"],
        );
        assert_eq!(
            SegmentsPresent.run(&marked).unwrap().verdict,
            Verdict::Passed
        );
        let free = episode(Vec::new(), &[]);
        assert_eq!(
            SegmentsPresent.run(&free).unwrap().verdict,
            Verdict::NotApplicable
        );
    }

    #[test]
    fn subtasks_covering_start_to_stop_closely_pass() {
        let close = episode(
            vec![
                event("subtask.reach", Phase::Begin, 1, None),
                event("subtask.reach", Phase::End, 9, None),
                event("subtask.grasp", Phase::Stretch, 10, Some(18)),
            ],
            &["reach", "grasp"],
        );
        let finding = LAYOUT.run(&close).unwrap();
        assert_eq!(finding.verdict, Verdict::Passed, "{}", finding.message);
        assert!((finding.values["coverage"] - 16.0 / 18.0).abs() < 1e-9);
    }

    #[test]
    fn a_gap_low_coverage_and_a_short_subtask_are_each_flagged_where_they_are() {
        let loose = episode(
            vec![
                event("subtask.reach", Phase::Stretch, 1, Some(3)),
                event("subtask.grasp", Phase::Stretch, 10, Some(10)),
                // Begun and never ended: it lasts to Stop.
                event("subtask.lift", Phase::Begin, 17, None),
            ],
            &["reach", "grasp", "lift"],
        );
        let finding = LAYOUT.run(&loose).unwrap();
        assert_eq!(finding.verdict, Verdict::Failed);
        assert_eq!(
            finding.message,
            "2 gaps between subtasks, up to 7.0 s; subtasks cover 22 % of the episode; shorter than 1 s: grasp"
        );
        let at = |second: i64| START + second * NS;
        assert_eq!(
            finding
                .ranges
                .iter()
                .map(|range| (range.start_utc_ns, range.end_utc_ns))
                .collect::<Vec<_>>(),
            [(at(3), at(10)), (at(10), at(10)), (at(10), at(17))]
        );
    }
}
