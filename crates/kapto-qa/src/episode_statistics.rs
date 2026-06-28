// SPDX-License-Identifier: Apache-2.0

//! The statistics of one episode, by role: what anomalies and the
//! autoencoder compare episodes by.
//!
//! - `episode.duration_s`: how long it was recorded.
//! - `streams.<stream>.rate_hz`, `.lost_upstream_ratio` and
//!   `.dropped_ratio`: how each stream arrived.
//! - `joints.<joint>.position`: the distribution of each joint's position:
//!   where it went, its range of motion.
//! - `joints.<joint>.speed`, `.acceleration` and `.jerk`: the distributions
//!   of how fast it moved, how fast that changed and how fast that did,
//!   each as a magnitude: from its velocity role if mapped, otherwise from
//!   its positions' changes over receive time, and each from the changes of
//!   the one before. Shaking and chattering show in the last two before
//!   they show in the speed.
//! - `joints.<joint>.effort`: the distribution of each joint's effort.
//! - `<role>.cycles` for each role ending in `opening`, e.g.
//!   `gripper.opening.cycles`: how many times it closed after being open.

use std::collections::BTreeMap;

use kapto_core::Role;
use serde::{Deserialize, Serialize};

use crate::statistics::Distribution;
use crate::{EpisodeView, seconds};

/// One statistic: a single value, or a distribution of values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Statistic {
    /// A single value.
    Value(f64),
    /// The distribution of many.
    Distribution(Distribution),
}

/// An episode's statistics, by name.
pub type Statistics = BTreeMap<String, Statistic>;

/// `statistics` as they are stored beside the quality results: each as
/// JSON, read back with [`from_stored`].
#[must_use]
pub fn to_stored(statistics: Statistics) -> BTreeMap<String, serde_json::Value> {
    statistics
        .into_iter()
        .filter_map(|(name, statistic)| Some((name, serde_json::to_value(statistic).ok()?)))
        .collect()
}

/// Statistics stored with [`to_stored`]; one stored in a form this
/// version doesn't read is left out.
#[must_use]
pub fn from_stored(stored: BTreeMap<String, serde_json::Value>) -> Statistics {
    stored
        .into_iter()
        .filter_map(|(name, value)| Some((name, serde_json::from_value(value).ok()?)))
        .collect()
}

/// Above this share of its range an opening counts as open, below the
/// other as closed; between them it keeps what it was, so jitter around
/// one threshold isn't counted as cycles.
const OPEN: f64 = 0.7;
const CLOSED: f64 = 0.3;

/// The statistics of `episode`, by name.
#[must_use]
pub fn of_episode(episode: &EpisodeView) -> Statistics {
    let summary = episode.summary();
    let mut statistics = BTreeMap::new();
    // Never negative, even for an episode.json whose end is before its
    // start: no rate is worked out from it then.
    let duration_s = seconds(
        summary
            .ended_utc_ns
            .saturating_sub(summary.started_utc_ns)
            .max(0),
    );
    statistics.insert(
        "episode.duration_s".to_owned(),
        Statistic::Value(duration_s),
    );
    for stream in &summary.streams {
        #[allow(clippy::cast_precision_loss, reason = "counts stay far below 2^53")]
        let (received, lost, dropped) = (
            stream.received as f64,
            stream.lost_upstream as f64,
            stream.dropped_by_us as f64,
        );
        let ratio = |part: f64, whole: f64| if whole > 0.0 { part / whole } else { 0.0 };
        let mut add = |name: &str, value| {
            statistics.insert(
                format!("streams.{}.{name}", stream.id),
                Statistic::Value(value),
            );
        };
        add("rate_hz", ratio(received, duration_s));
        add("lost_upstream_ratio", ratio(lost, received + lost));
        add("dropped_ratio", ratio(dropped, received));
    }
    for role in episode.roles() {
        let Some(column) = episode.role(role).filter(|column| column.width == 1) else {
            continue;
        };
        let segments: Vec<&str> = role.segments().collect();
        match segments.as_slice() {
            ["joints", joint, "position"] => {
                let velocity: Role = match format!("joints.{joint}.velocity").parse() {
                    Ok(velocity) => velocity,
                    Err(_) => continue,
                };
                let derived;
                let (times, velocities) = if let Some(measured) =
                    episode.role(&velocity).filter(|column| column.width == 1)
                {
                    (&measured.times_utc_ns[..], &measured.values[..])
                } else {
                    derived = rates(&column.times_utc_ns, &column.values);
                    (&derived.0[..], &derived.1[..])
                };
                let accelerations = rates(times, velocities);
                let jerks = rates(&accelerations.0, &accelerations.1);
                let magnitudes = |values: &[f64]| {
                    Statistic::Distribution(Distribution::of(values.iter().map(|v| v.abs())))
                };
                for (name, statistic) in [
                    (
                        "position",
                        Statistic::Distribution(Distribution::of(column.values.iter().copied())),
                    ),
                    ("speed", magnitudes(velocities)),
                    ("acceleration", magnitudes(&accelerations.1)),
                    ("jerk", magnitudes(&jerks.1)),
                ] {
                    statistics.insert(format!("joints.{joint}.{name}"), statistic);
                }
            }
            ["joints", joint, "effort"] => {
                let efforts = Distribution::of(column.values.iter().copied());
                statistics.insert(
                    format!("joints.{joint}.effort"),
                    Statistic::Distribution(efforts),
                );
            }
            [.., "opening"] => {
                #[allow(clippy::cast_precision_loss, reason = "cycle counts are small")]
                let count = closings(&column.values).len() as f64;
                statistics.insert(format!("{role}.cycles"), Statistic::Value(count));
            }
            _ => {}
        }
    }
    statistics
}

/// How fast `values` changed between consecutive rows, per second of
/// receive time, each at the later row's time; rows received at the same
/// moment are skipped.
fn rates(times: &[i64], values: &[f64]) -> (Vec<i64>, Vec<f64>) {
    times
        .windows(2)
        .zip(values.windows(2))
        .filter_map(|(times, values)| {
            let step = seconds(times[1] - times[0]);
            (step > 0.0).then(|| (times[1], (values[1] - values[0]) / step))
        })
        .unzip()
}

/// The rows at which an opening closed after being open, with hysteresis
/// between [`CLOSED`] and [`OPEN`] of its range.
pub(crate) fn closings(openings: &[f64]) -> Vec<usize> {
    let finite = openings.iter().copied().filter(|value| value.is_finite());
    let (min, max) = finite.fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
        (min.min(value), max.max(value))
    });
    let range = max - min;
    if !range.is_finite() || range <= f64::EPSILON {
        return Vec::new();
    }
    let mut open: Option<bool> = None;
    let mut closes = Vec::new();
    for (row, value) in openings.iter().enumerate() {
        if !value.is_finite() {
            continue;
        }
        let share = (value - min) / range;
        if share >= OPEN {
            open = Some(true);
        } else if share <= CLOSED {
            if open == Some(true) {
                closes.push(row);
            }
            open = Some(false);
        }
    }
    closes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_after_being_open_counts_once_however_it_jitters() {
        // Open, jitter near the middle, closed, open, closed.
        let openings = [
            0.08, 0.079, 0.05, 0.041, 0.039, 0.01, 0.0, 0.011, 0.08, 0.002,
        ];
        assert_eq!(closings(&openings), [5, 9]);
        assert_eq!(closings(&[0.04; 5]), [] as [usize; 0]);
        assert_eq!(closings(&[]), [] as [usize; 0]);
    }
}
