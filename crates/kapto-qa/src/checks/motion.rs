// SPDX-License-Identifier: Apache-2.0

//! Checks of how the joints moved, each naming where in the episode what it
//! found is. Both are advisory: a robot can do anything once, and these
//! only point at moments worth a look.

use kapto_core::KaptoError;

use crate::episode_statistics::closings;
use crate::{Check, CheckKind, EpisodeView, Finding, RoleColumn, TimeRange};

/// How many joints a message names; the rest are counted.
const NAMED: usize = 5;

/// A joint that moved faster than `max_speed` between two messages: no
/// motor turns that fast, so its reading jumped, e.g. a glitch, a wrapped
/// angle or a sensor catching up after it froze.
#[derive(Clone, Copy, Debug)]
pub struct JointJumps {
    /// The fastest a joint can move, in rad/s or m/s.
    pub max_speed: f64,
}

/// A joint whose reading stayed exactly the same for at least `max_seconds`
/// right after it moved at `moving_speed` or more, while another joint
/// moved: it stopped dead mid-motion, as a reading that stopped updating
/// does. A joint that slows down to rest isn't flagged, nor one alone.
#[derive(Clone, Copy, Debug)]
pub struct FrozenJoints {
    /// The shortest stillness flagged, in seconds.
    pub max_seconds: f64,
    /// The speed, in rad/s or m/s, a joint must have had just before.
    pub moving_speed: f64,
}

impl Check for JointJumps {
    fn id(&self) -> &'static str {
        "motion.jump"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let joints = positions(episode);
        if joints.is_empty() {
            return Ok(Finding::not_applicable(
                "the episode has no joint positions",
            ));
        }
        let mut found = Vec::new();
        for (joint, column) in &joints {
            let mut ranges = Vec::new();
            let mut fastest: f64 = 0.0;
            for (times, values) in column.times_utc_ns.windows(2).zip(column.values.windows(2)) {
                let Some(speed) = speed(times, values) else {
                    continue;
                };
                if speed > self.max_speed {
                    fastest = fastest.max(speed);
                    push(&mut ranges, times[0], times[1]);
                }
            }
            if !ranges.is_empty() {
                found.push((joint.clone(), ranges, fastest));
            }
        }
        if found.is_empty() {
            return Ok(Finding::passed(format!(
                "no joint moved faster than {} between two messages",
                self.max_speed
            )));
        }
        let named: Vec<String> = found
            .iter()
            .map(|(joint, ranges, fastest)| {
                format!("{joint} {} times, at up to {fastest:.3}", ranges.len())
            })
            .collect();
        let finding = Finding::failed(format!(
            "jumped faster than {} between two messages: {}",
            self.max_speed,
            listed(&named)
        ));
        #[allow(clippy::cast_precision_loss, reason = "counts are small")]
        Ok(found
            .into_iter()
            .fold(finding, |finding, (joint, ranges, fastest)| {
                let finding = finding
                    .with_value(&format!("{joint}.jumps"), ranges.len() as f64)
                    .with_value(&format!("{joint}.fastest"), fastest);
                ranges.into_iter().fold(finding, Finding::with_range)
            }))
    }
}

impl Check for FrozenJoints {
    fn id(&self) -> &'static str {
        "motion.frozen"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let joints = positions(episode);
        if joints.len() < 2 {
            return Ok(Finding::not_applicable(
                "fewer than two joints: a joint at rest can't be told from one that froze",
            ));
        }
        let mut found = Vec::new();
        for (index, (joint, column)) in joints.iter().enumerate() {
            let others: Vec<&RoleColumn> = joints
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, (_, column))| *column)
                .collect();
            let ranges: Vec<TimeRange> = still(column, self.moving_speed)
                .filter(|(start, end)| {
                    #[allow(clippy::cast_precision_loss, reason = "durations are small")]
                    let seconds = (end - start) as f64 / 1e9;
                    seconds >= self.max_seconds
                        && others.iter().any(|other| moved(other, *start, *end))
                })
                .map(|(start_utc_ns, end_utc_ns)| TimeRange {
                    start_utc_ns,
                    end_utc_ns,
                })
                .collect();
            if !ranges.is_empty() {
                found.push((joint.clone(), ranges));
            }
        }
        if found.is_empty() {
            return Ok(Finding::passed(format!(
                "no joint stopped dead mid-motion for {} s or more while the others moved",
                self.max_seconds
            )));
        }
        #[allow(clippy::cast_precision_loss, reason = "durations are small")]
        let longest = |ranges: &[TimeRange]| {
            ranges
                .iter()
                .map(|range| (range.end_utc_ns - range.start_utc_ns) as f64 / 1e9)
                .fold(0.0, f64::max)
        };
        let named: Vec<String> = found
            .iter()
            .map(|(joint, ranges)| {
                format!(
                    "{joint} {} times, for up to {:.1} s",
                    ranges.len(),
                    longest(ranges)
                )
            })
            .collect();
        let finding = Finding::failed(format!(
            "stopped dead mid-motion while the others moved, as a reading that stopped updating does: {}",
            listed(&named)
        ));
        #[allow(clippy::cast_precision_loss, reason = "counts are small")]
        Ok(found.into_iter().fold(finding, |finding, (joint, ranges)| {
            let finding = finding
                .with_value(&format!("{joint}.frozen"), ranges.len() as f64)
                .with_value(&format!("{joint}.longest_s"), longest(&ranges));
            ranges.into_iter().fold(finding, Finding::with_range)
        }))
    }
}

/// A gripper that closed more than `max_closings` times within
/// `window_seconds`: chattering, from a loose grasp, a controller hunting,
/// or a reading flickering between open and closed. Counted on each role
/// ending in `opening`, with the hysteresis gripper cycles use.
#[derive(Clone, Copy, Debug)]
pub struct GripperChatter {
    /// The most closings allowed within the window.
    pub max_closings: usize,
    /// The window, in seconds.
    pub window_seconds: f64,
}

/// A robot that stood still, every joint slower than `moving_speed`, for
/// more than `max_ratio` of the episode (from its first joint position to
/// its last), counting spans of `min_seconds` or more: an episode that may
/// hold little of the task.
#[derive(Clone, Copy, Debug)]
pub struct Idle {
    /// The largest share of the episode still, from 0 to 1.
    pub max_ratio: f64,
    /// The shortest span counted, in seconds.
    pub min_seconds: f64,
    /// Slower than this, in rad/s or m/s, a joint is still.
    pub moving_speed: f64,
}

impl Check for GripperChatter {
    fn id(&self) -> &'static str {
        "motion.chatter"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let openings: Vec<(String, &RoleColumn)> = episode
            .roles()
            .filter(|role| role.segments().last() == Some("opening"))
            .filter_map(|role| {
                episode
                    .role(role)
                    .filter(|column| column.width == 1)
                    .map(|column| (role.to_string(), column))
            })
            .collect();
        if openings.is_empty() {
            return Ok(Finding::not_applicable(
                "the episode has no gripper opening",
            ));
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a window of seconds in nanoseconds fits an i64"
        )]
        let window = (self.window_seconds * 1e9) as i64;
        let mut found = Vec::new();
        for (role, column) in &openings {
            let times: Vec<i64> = closings(&column.values)
                .into_iter()
                .map(|row| column.times_utc_ns[row])
                .collect();
            let (mut ranges, mut most) = (Vec::new(), 0);
            for (first, start) in times.iter().enumerate() {
                let last =
                    first + times[first..].partition_point(|time| time - start <= window) - 1;
                if last - first + 1 > self.max_closings {
                    most = most.max(last - first + 1);
                    push(&mut ranges, *start, times[last]);
                }
            }
            if !ranges.is_empty() {
                found.push((role.clone(), ranges, most));
            }
        }
        if found.is_empty() {
            return Ok(Finding::passed(format!(
                "no gripper closed more than {} times within {} s",
                self.max_closings, self.window_seconds
            )));
        }
        let named: Vec<String> = found
            .iter()
            .map(|(role, _, most)| format!("{role} {most} times"))
            .collect();
        let finding = Finding::failed(format!(
            "closed more than {} times within {} s: {}",
            self.max_closings,
            self.window_seconds,
            listed(&named)
        ));
        #[allow(clippy::cast_precision_loss, reason = "counts are small")]
        Ok(found
            .into_iter()
            .fold(finding, |finding, (role, ranges, most)| {
                let finding = finding.with_value(&format!("{role}.most_closings"), most as f64);
                ranges.into_iter().fold(finding, Finding::with_range)
            }))
    }
}

impl Check for Idle {
    fn id(&self) -> &'static str {
        "motion.idle"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let joints = positions(episode);
        if joints.is_empty() {
            return Ok(Finding::not_applicable(
                "the episode has no joint positions",
            ));
        }
        let mut moving = Vec::new();
        for (_, column) in &joints {
            for (times, values) in column.times_utc_ns.windows(2).zip(column.values.windows(2)) {
                if speed(times, values).is_some_and(|speed| speed >= self.moving_speed) {
                    moving.push((times[0], times[1]));
                }
            }
        }
        moving.sort_unstable();
        // From the first message to the last: stillness is about the
        // motion reported.
        let start = joints
            .iter()
            .filter_map(|(_, column)| column.times_utc_ns.first())
            .min()
            .copied()
            .unwrap_or_default();
        let end = joints
            .iter()
            .filter_map(|(_, column)| column.times_utc_ns.last())
            .max()
            .copied()
            .unwrap_or_default();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "seconds in nanoseconds fit an i64"
        )]
        let shortest = (self.min_seconds * 1e9) as i64;
        let mut still = Vec::new();
        let mut from = start;
        for (moving_from, moving_to) in moving.into_iter().chain([(end, end)]) {
            if moving_from - from >= shortest {
                still.push(TimeRange {
                    start_utc_ns: from,
                    end_utc_ns: moving_from.min(end),
                });
            }
            from = from.max(moving_to);
        }
        #[allow(clippy::cast_precision_loss, reason = "durations are small")]
        let ratio = still
            .iter()
            .map(|range| (range.end_utc_ns - range.start_utc_ns) as f64)
            // From +0, which an empty sum of floats isn't.
            .fold(0.0, |total, still| total + still)
            / (end - start).max(1) as f64;
        let percent = 100.0 * ratio;
        if ratio <= self.max_ratio {
            return Ok(Finding::passed(format!(
                "still for {percent:.0} % of the episode, counting spans of {} s or more",
                self.min_seconds
            ))
            .with_value("still_ratio", ratio));
        }
        let finding = Finding::failed(format!(
            "still for {percent:.0} % of the episode, more than {:.0} %, in {} spans of {} s or more",
            100.0 * self.max_ratio,
            still.len(),
            self.min_seconds
        ))
        .with_value("still_ratio", ratio);
        Ok(still.into_iter().fold(finding, Finding::with_range))
    }
}

/// Each joint's positions, by joint name, in order: those of a single value.
fn positions(episode: &EpisodeView) -> Vec<(String, &RoleColumn)> {
    episode
        .roles()
        .filter_map(|role| {
            let segments: Vec<&str> = role.segments().collect();
            match segments.as_slice() {
                ["joints", joint, "position"] => episode
                    .role(role)
                    .filter(|column| column.width == 1)
                    .map(|column| ((*joint).to_owned(), column)),
                _ => None,
            }
        })
        .collect()
}

/// How fast a value changed between two rows, per second; `None` for rows
/// received at the same moment.
fn speed(times: &[i64], values: &[f64]) -> Option<f64> {
    #[allow(clippy::cast_precision_loss, reason = "time steps are small")]
    let seconds = (times[1] - times[0]) as f64 / 1e9;
    (seconds > 0.0).then(|| ((values[1] - values[0]) / seconds).abs())
}

/// Each span over which `column`'s value stayed exactly the same, from the
/// row it reached that value, entered at `moving_speed` or more.
fn still(column: &RoleColumn, moving_speed: f64) -> impl Iterator<Item = (i64, i64)> + '_ {
    let (times, values) = (&column.times_utc_ns, &column.values);
    let mut spans = Vec::new();
    let mut start = 0;
    for index in 1..=values.len() {
        let ended = index == values.len() || values[index].to_bits() != values[start].to_bits();
        if ended {
            let entered = start
                .checked_sub(1)
                .and_then(|before| speed(&times[before..=start], &values[before..=start]));
            if index - 1 > start && entered.is_some_and(|speed| speed >= moving_speed) {
                spans.push((times[start], times[index - 1]));
            }
            start = index;
        }
    }
    spans.into_iter()
}

/// Whether `column`'s value changed between `start` and `end`.
fn moved(column: &RoleColumn, start: i64, end: i64) -> bool {
    let from = column.times_utc_ns.partition_point(|time| *time < start);
    let to = column.times_utc_ns.partition_point(|time| *time <= end);
    column.values.get(from..to).is_some_and(|values| {
        values
            .windows(2)
            .any(|pair| pair[0].to_bits() != pair[1].to_bits())
    })
}

/// Adds the span from `start` to `end` to `ranges`, joining it to the last
/// if they touch.
fn push(ranges: &mut Vec<TimeRange>, start: i64, end: i64) {
    match ranges.last_mut() {
        Some(last) if last.end_utc_ns >= start => last.end_utc_ns = end,
        _ => ranges.push(TimeRange {
            start_utc_ns: start,
            end_utc_ns: end,
        }),
    }
}

/// The first few of `named`, and how many more.
fn listed(named: &[String]) -> String {
    let mut shown: Vec<String> = named.iter().take(NAMED).cloned().collect();
    if named.len() > NAMED {
        shown.push(format!("and {} more", named.len() - NAMED));
    }
    shown.join("; ")
}
