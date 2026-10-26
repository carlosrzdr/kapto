// SPDX-License-Identifier: Apache-2.0

//! What the quality checks found on an episode, its statistics, and how a
//! simulation it recorded ran, as the API shows them.
//!
//! The API's own types rather than the store's: the API version and the
//! database change on their own schedules.

use std::collections::BTreeMap;
use std::path::Path;

use kapto_core::KaptoError;
use kapto_qa::episode_statistics::{self, Statistic};
use kapto_store::{CheckKind, QaRun, Reader, Simulated, Verdict};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::episode::stored_here;

/// What the quality checks found on an episode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeQuality {
    /// Whether it was accepted: no gate failed. `null` until it is checked,
    /// which the worker does after it is recorded.
    pub accepted: Option<bool>,
    /// The version of the quality settings it was checked with (the
    /// `qa_params_version` of the `qa.toml` its recording carries); `null`
    /// if it was checked with the defaults, or not yet.
    pub params_version: Option<String>,
    /// When it was checked, in UTC nanoseconds since the Unix epoch; `null`
    /// if not yet.
    pub computed_utc_ns: Option<i64>,
    /// Each check's result, in the order they ran.
    pub checks: Vec<CheckResult>,
}

/// One check's result on an episode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CheckResult {
    /// The check's ID, e.g. `streams.complete`.
    pub check: String,
    /// The check's version: results of different versions aren't alike.
    pub version: u32,
    /// `gate` (deterministic; a failed one rejects the episode) or
    /// `advisory` (only flags).
    pub kind: String,
    /// `passed`, `failed` or `not_applicable`.
    pub verdict: String,
    /// What it found, in words.
    pub message: String,
    /// What it measured, by name.
    pub values: BTreeMap<String, f64>,
    /// Where in the episode what it found is.
    pub ranges: Vec<CheckRange>,
}

/// A span of an episode a check found something in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CheckRange {
    /// Where it starts, in UTC nanoseconds since the Unix epoch.
    pub start_utc_ns: i64,
    /// Where it ends, the same way.
    pub end_utc_ns: i64,
}

/// An episode's statistics, worked out with its quality checks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeStatistics {
    /// When they were worked out, in UTC nanoseconds since the Unix epoch;
    /// `null` until the worker has checked the episode.
    pub computed_utc_ns: Option<i64>,
    /// Each statistic, by name.
    pub statistics: Vec<StatisticSummary>,
}

/// How a simulation an episode recorded ran, worked out with its quality
/// checks from the messages of the field its package gives the role
/// `time.simulated`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeSimulated {
    /// When it was worked out, in UTC nanoseconds since the Unix epoch;
    /// `null` until the worker has checked the episode.
    pub computed_utc_ns: Option<i64>,
    /// How it ran; `null` until then, and for an episode without a
    /// simulation's clock, or whose clock sent nothing.
    pub simulated: Option<SimulatedRun>,
}

/// How a simulation ran over an episode. Its own times are in simulated
/// nanoseconds; when something happened, in UTC nanoseconds since the Unix
/// epoch, as Kapto received the clock's messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SimulatedRun {
    /// The simulated time of the clock's first message.
    pub start_ns: i64,
    /// The simulated time of its last.
    pub end_ns: i64,
    /// How much simulated time passed, each segment between resets added up.
    pub simulated_ns: i64,
    /// How much time passed from the clock's first message to its last, as
    /// Kapto received them.
    pub real_ns: i64,
    /// How much of that the simulation was paused.
    pub paused_ns: i64,
    /// Simulated time over real time while it wasn't paused: 0.5 at half
    /// real time, 2 at twice; `null` if it was paused throughout.
    pub ratio: Option<f64>,
    /// Each pause, in order.
    pub pauses: Vec<SimulatedPauseSpan>,
    /// Each reset, simulated time going back, in order.
    pub resets: Vec<SimulatedResetAt>,
    /// How simulated time moved: each segment between resets as a few
    /// points of receive time and simulated time, within 10 ms of the
    /// clock's own, simulated time moving straight from one to the next.
    /// A replay plays in simulated time by these. Empty for a run worked
    /// out before they were kept; checking the episode again fills it.
    pub segments: Vec<Vec<SimulatedPoint>>,
}

/// A point of a simulation's time against receive time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SimulatedPoint {
    /// When, in UTC nanoseconds.
    pub receive_utc_ns: i64,
    /// The simulated time then, in nanoseconds.
    pub simulated_ns: i64,
}

/// A span in which a simulation was paused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SimulatedPauseSpan {
    /// When it was paused, in UTC nanoseconds.
    pub start_utc_ns: i64,
    /// When it played again, in UTC nanoseconds.
    pub end_utc_ns: i64,
    /// The simulated time it was paused at.
    pub simulated_ns: i64,
}

/// A simulation reset: its time going back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SimulatedResetAt {
    /// When the clock's first message after it was received, in UTC
    /// nanoseconds.
    pub at_utc_ns: i64,
    /// The simulated time before it.
    pub from_ns: i64,
    /// The simulated time it went back to.
    pub to_ns: i64,
}

impl From<Simulated> for SimulatedRun {
    fn from(ran: Simulated) -> Self {
        Self {
            start_ns: ran.start_ns,
            end_ns: ran.end_ns,
            simulated_ns: ran.simulated_ns,
            real_ns: ran.real_ns,
            paused_ns: ran.paused_ns,
            ratio: ran.ratio,
            pauses: ran
                .pauses
                .into_iter()
                .map(|pause| SimulatedPauseSpan {
                    start_utc_ns: pause.start_utc_ns,
                    end_utc_ns: pause.end_utc_ns,
                    simulated_ns: pause.simulated_ns,
                })
                .collect(),
            resets: ran
                .resets
                .into_iter()
                .map(|reset| SimulatedResetAt {
                    at_utc_ns: reset.at_utc_ns,
                    from_ns: reset.from_ns,
                    to_ns: reset.to_ns,
                })
                .collect(),
            segments: ran
                .segments
                .into_iter()
                .map(|knots| {
                    knots
                        .into_iter()
                        .map(|knot| SimulatedPoint {
                            receive_utc_ns: knot.receive_utc_ns,
                            simulated_ns: knot.simulated_ns,
                        })
                        .collect()
                })
                .collect(),
        }
    }
}

/// How the simulation the finalised episode `id` in `data_dir`, or deleted
/// here, recorded ran, as `reader` has it.
///
/// # Errors
///
/// As for [`read`].
pub fn simulated(
    data_dir: &Path,
    reader: &Reader,
    id: &str,
) -> Result<EpisodeSimulated, KaptoError> {
    stored_here(data_dir, reader, id)?;
    let run = reader.qa(id)?;
    Ok(EpisodeSimulated {
        computed_utc_ns: run.as_ref().map(|run| run.computed_utc_ns),
        simulated: run
            .and_then(|run| run.simulated)
            .map(|ran| SimulatedRun::from(*ran)),
    })
}

/// One statistic, summarised: a single value, or a distribution's count,
/// extremes, mean and percentiles.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StatisticSummary {
    /// Its name, e.g. `joints.joint_1.speed` or `episode.duration_s`.
    pub name: String,
    /// The value, for a single value.
    pub value: Option<f64>,
    /// How many values, for a distribution.
    pub count: Option<u64>,
    /// The smallest.
    pub min: Option<f64>,
    /// The largest.
    pub max: Option<f64>,
    /// The mean.
    pub mean: Option<f64>,
    /// The median, within 1 %.
    pub p50: Option<f64>,
    /// The 90th percentile, within 1 %.
    pub p90: Option<f64>,
    /// The 99th percentile, within 1 %.
    pub p99: Option<f64>,
}

/// The statistics of the finalised episode `id` in `data_dir`, or deleted
/// here, as `reader` has them; a statistic stored in a form this version doesn't read is
/// left out.
///
/// # Errors
///
/// As for [`read`].
pub fn statistics(
    data_dir: &Path,
    reader: &Reader,
    id: &str,
) -> Result<EpisodeStatistics, KaptoError> {
    stored_here(data_dir, reader, id)?;
    let Some(run) = reader.qa(id)? else {
        return Ok(EpisodeStatistics {
            computed_utc_ns: None,
            statistics: Vec::new(),
        });
    };
    let statistics = episode_statistics::from_stored(run.statistics)
        .into_iter()
        .map(|(name, statistic)| match statistic {
            Statistic::Value(value) => StatisticSummary {
                name,
                value: Some(value),
                ..StatisticSummary::default()
            },
            Statistic::Distribution(distribution) => StatisticSummary {
                name,
                value: None,
                count: Some(distribution.count),
                min: distribution.min,
                max: distribution.max,
                mean: distribution.mean(),
                p50: distribution.quantile(0.5),
                p90: distribution.quantile(0.9),
                p99: distribution.quantile(0.99),
            },
        })
        .collect();
    Ok(EpisodeStatistics {
        computed_utc_ns: Some(run.computed_utc_ns),
        statistics,
    })
}

/// What the quality checks found on the finalised episode `id` in
/// `data_dir`, or deleted here, as `reader` has it.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID; `api.episode_not_found`
/// if no finalised episode, here or deleted here, has it;
/// `store.query_failed` if the results can't be read.
pub fn read(data_dir: &Path, reader: &Reader, id: &str) -> Result<EpisodeQuality, KaptoError> {
    stored_here(data_dir, reader, id)?;
    Ok(reader.qa(id)?.map_or(
        EpisodeQuality {
            accepted: None,
            params_version: None,
            computed_utc_ns: None,
            checks: Vec::new(),
        },
        shown,
    ))
}

fn shown(run: QaRun) -> EpisodeQuality {
    EpisodeQuality {
        accepted: Some(run.accepted()),
        params_version: run.params_version,
        computed_utc_ns: Some(run.computed_utc_ns),
        checks: run
            .results
            .into_iter()
            .map(|result| CheckResult {
                check: result.check,
                version: result.version,
                kind: match result.kind {
                    CheckKind::Gate => "gate",
                    CheckKind::Advisory => "advisory",
                }
                .to_owned(),
                verdict: match result.verdict {
                    Verdict::Passed => "passed",
                    Verdict::Failed => "failed",
                    Verdict::NotApplicable => "not_applicable",
                }
                .to_owned(),
                message: result.message,
                values: result.values,
                ranges: result
                    .ranges
                    .into_iter()
                    .map(|range| CheckRange {
                        start_utc_ns: range.start_utc_ns,
                        end_utc_ns: range.end_utc_ns,
                    })
                    .collect(),
            })
            .collect(),
    }
}
