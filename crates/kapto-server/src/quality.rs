// SPDX-License-Identifier: Apache-2.0

//! What the quality checks found on an episode, and its statistics, as the
//! API shows them.
//!
//! The API's own types rather than the store's: the API version and the
//! database change on their own schedules.

use std::collections::BTreeMap;
use std::path::Path;

use kapto_core::KaptoError;
use kapto_qa::episode_statistics::{self, Statistic};
use kapto_store::{CheckKind, QaRun, Reader, Verdict};
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
