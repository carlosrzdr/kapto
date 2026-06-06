// SPDX-License-Identifier: Apache-2.0

//! What is unusual about an episode for its session: the episodes recorded
//! with the same robot package, profile and task on the same day, which
//! should be alike. An [`AnomalyDetector`] only flags: its results are
//! advisory, and never reject an episode.
//!
//! The first detector, [`StatisticalAnomalies`], compares an episode's
//! statistics with the session's by their median and median absolute
//! deviation, which a few unusual episodes in the session don't move: no
//! model to train, and useful from the session's first few episodes.

use std::collections::BTreeMap;

use kapto_config::QaSettings;
use kapto_core::KaptoError;

use crate::episode_statistics::{Statistic, Statistics};
use crate::statistics::RELATIVE_ERROR;
use crate::{CheckKind, EpisodeView, Finding, QaResult};

/// What an episode is compared with.
#[derive(Clone, Copy, Debug)]
pub struct Session<'a> {
    /// The episode's own statistics, by name.
    pub statistics: &'a Statistics,
    /// Those of the other episodes of its session that were checked, the
    /// newest first.
    pub others: &'a [Statistics],
}

/// One way of finding what is unusual about an episode, made with its
/// parameters. Its findings are advisory: a failed one flags the episode
/// and never rejects it.
pub trait AnomalyDetector: Send + Sync {
    /// Its ID: two or more dot-separated names, e.g. `anomalies.statistics`.
    fn id(&self) -> &'static str;

    /// Its version: raised whenever what it computes changes. From 1.
    fn version(&self) -> u32;

    /// How many of the session's other episodes it wants, the newest; 0 if
    /// it doesn't compare with them.
    fn session_episodes(&self) -> usize;

    /// What it finds on `episode`, given its `session`. The same inputs
    /// always give the same finding.
    ///
    /// # Errors
    ///
    /// What kept it from looking.
    fn detect(&self, episode: &EpisodeView, session: Session<'_>) -> Result<Finding, KaptoError>;
}

/// The detectors `settings` turn on, each made with its settings.
#[must_use]
pub fn from_settings(settings: &QaSettings) -> Vec<Box<dyn AnomalyDetector>> {
    let mut detectors: Vec<Box<dyn AnomalyDetector>> = Vec::new();
    let statistical = &settings.statistical_anomalies;
    if statistical.enabled {
        detectors.push(Box::new(StatisticalAnomalies {
            threshold: statistical.threshold,
            min_episodes: statistical.min_episodes,
            max_episodes: statistical.max_episodes,
        }));
    }
    detectors
}

/// Runs `detectors` on `episode` in order, and gives their results, each
/// advisory whatever it found, so none can reject the episode. A detector
/// that can't run is recorded as failed, saying why.
#[must_use]
pub fn run_detectors(
    detectors: &[&dyn AnomalyDetector],
    episode: &EpisodeView,
    session: Session<'_>,
) -> Vec<QaResult> {
    detectors
        .iter()
        .map(|detector| {
            detector
                .detect(episode, session)
                .unwrap_or_else(|error| Finding::failed(format!("couldn't look: {error}")))
                .into_result(detector.id(), detector.version(), CheckKind::Advisory)
        })
        .collect()
}

/// The MAD of normally distributed values times this is their standard
/// deviation, so the threshold reads as standard deviations.
pub(crate) const MAD_TO_SD: f64 = 1.4826;

/// `x` as a message shows it: three decimals, or, very large or small,
/// three significant figures with its power of ten (`4.43e13`), so no
/// figure runs to a hundred digits.
pub(crate) fn readable(x: f64) -> String {
    if x != 0.0 && !(1e-3..1e6).contains(&x.abs()) {
        format!("{x:.2e}")
    } else {
        format!("{x:.3}")
    }
}

/// How many flagged statistics a message names; the rest are counted.
const NAMED: usize = 5;

/// Flags each statistic of an episode that is more than `threshold` robust
/// standard deviations from the median of the same statistic across the
/// other episodes of its session: a single value as it is, a distribution
/// by its 1st, 50th, 90th and 99th percentiles. A spread smaller than the
/// statistics' own error, 1 % of the median, counts as that much, so
/// episodes that are nearly the same aren't flagged for a rounding.
#[derive(Clone, Copy, Debug)]
pub struct StatisticalAnomalies {
    /// How far from the median is flagged, in robust standard deviations.
    pub threshold: f64,
    /// The fewest other episodes to compare with.
    pub min_episodes: usize,
    /// The most other episodes to compare with, the newest.
    pub max_episodes: usize,
}

impl AnomalyDetector for StatisticalAnomalies {
    fn id(&self) -> &'static str {
        "anomalies.statistics"
    }

    fn version(&self) -> u32 {
        1
    }

    fn session_episodes(&self) -> usize {
        self.max_episodes
    }

    fn detect(&self, _: &EpisodeView, session: Session<'_>) -> Result<Finding, KaptoError> {
        let others = &session.others[..session.others.len().min(self.max_episodes)];
        if others.len() < self.min_episodes {
            return Ok(Finding::not_applicable(format!(
                "compared with nothing: {} other checked episodes in its session, fewer than the {} needed",
                others.len(),
                self.min_episodes
            )));
        }
        let others: Vec<BTreeMap<String, f64>> = others.iter().map(measures).collect();
        // Each flagged measure: its name, value, score, the session's median
        // and whether the session's were all exactly that.
        let mut flagged: Vec<(String, f64, f64, f64, bool)> = Vec::new();
        for (name, value) in measures(session.statistics) {
            let mut alike: Vec<f64> = others
                .iter()
                .filter_map(|other| other.get(&name).copied())
                .collect();
            if alike.len() < self.min_episodes {
                continue;
            }
            let median = median(&mut alike);
            let mut deviations: Vec<f64> = alike.iter().map(|x| (x - median).abs()).collect();
            let usual =
                (MAD_TO_SD * self::median(&mut deviations)).max(RELATIVE_ERROR * median.abs());
            let spread = usual.max(f64::MIN_POSITIVE);
            let score = (value - median) / spread;
            if score.abs() > self.threshold {
                flagged.push((name, value, score, median, usual == 0.0));
            }
        }
        let compared = others.len();
        #[allow(clippy::cast_precision_loss, reason = "episode counts are small")]
        let counted = |finding: Finding| finding.with_value("session.episodes", compared as f64);
        if flagged.is_empty() {
            return Ok(counted(Finding::passed(format!(
                "like the other {compared} episodes of its session"
            ))));
        }
        // The furthest out first.
        flagged.sort_by(|a, b| b.2.abs().total_cmp(&a.2.abs()).then(a.0.cmp(&b.0)));
        let mut named: Vec<String> = flagged
            .iter()
            .take(NAMED)
            .map(|(name, value, score, median, same)| {
                if *same {
                    // No spread to count deviations in: any difference is off the scale.
                    return format!(
                        "{name} {} where the session's were all {}",
                        readable(*value),
                        readable(*median)
                    );
                }
                let side = if *score > 0.0 { "above" } else { "below" };
                format!(
                    "{name} {}, {} deviations {side} the session's median {}",
                    readable(*value),
                    readable(score.abs()),
                    readable(*median)
                )
            })
            .collect();
        if flagged.len() > NAMED {
            named.push(format!("and {} more", flagged.len() - NAMED));
        }
        let finding = counted(Finding::failed(format!(
            "unusual for its session of {compared} other episodes: {}",
            named.join("; ")
        )));
        // Each flagged measure's score, kept finite so it is stored.
        Ok(flagged
            .into_iter()
            .fold(finding, |finding, (name, _, score, ..)| {
                finding.with_value(&name, score.clamp(-1e12, 1e12))
            }))
    }
}

/// What is compared of each statistic, by name: a single value, and a
/// distribution's 1st, 50th, 90th and 99th percentiles as `<name>.p1`,
/// `.p50`, `.p90` and `.p99`: both ends, so a joint that went further
/// either way than usual shows. Values that aren't finite are left out.
pub(crate) fn measures(statistics: &Statistics) -> BTreeMap<String, f64> {
    let mut measures = BTreeMap::new();
    for (name, statistic) in statistics {
        match statistic {
            Statistic::Value(value) => {
                measures.insert(name.clone(), *value);
            }
            Statistic::Distribution(distribution) => {
                for (suffix, fraction) in [("p1", 0.01), ("p50", 0.5), ("p90", 0.9), ("p99", 0.99)]
                {
                    if let Some(value) = distribution.quantile(fraction) {
                        measures.insert(format!("{name}.{suffix}"), value);
                    }
                }
            }
        }
    }
    measures.retain(|_, value| value.is_finite());
    measures
}

/// The median of `values`, which aren't empty; sorts them.
pub(crate) fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    }
}
