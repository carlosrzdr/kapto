// SPDX-License-Identifier: Apache-2.0

//! What is unusual about an episode for its robot package and task, learned
//! from the episodes recorded before it: an autoencoder of their
//! statistics, which learns what usually goes together, so an episode
//! whose statistics are each ordinary but whose mix isn't still stands out.
//!
//! The first is linear: principal components of the episodes' standardised
//! statistics, learned in one step (a singular value decomposition), so it
//! needs few episodes and gives the same model from the same episodes every
//! time. An episode is reconstructed from its components; how far the
//! reconstruction is from the episode says how unusual it is. What counts
//! as far is learned too: each training episode is reconstructed by a
//! model learned without it, and an episode is flagged beyond the median of
//! those errors by `threshold` robust standard deviations of their spread.

use std::collections::BTreeMap;

use kapto_core::KaptoError;
use nalgebra::{DMatrix, DVector};
use serde::{Deserialize, Serialize};

use crate::anomalies::{AnomalyDetector, MAD_TO_SD, Session, measures, median, readable};
use crate::episode_statistics::Statistics;
use crate::statistics::RELATIVE_ERROR;
use crate::{EpisodeView, Finding};

pub mod neural;

use neural::NeuralAutoencoder;

/// The most statistics furthest from their reconstruction a message names.
const NAMED: usize = 5;

/// A linear autoencoder of episodes' statistics: their principal
/// components.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearAutoencoder {
    /// The statistics it reconstructs, in order: those every episode it
    /// learned from had.
    pub names: Vec<String>,
    /// Each statistic's mean over the episodes it learned from.
    pub means: Vec<f64>,
    /// Each statistic's scale: its standard deviation, at least the
    /// statistics' own error, 1 % of its mean.
    pub scales: Vec<f64>,
    /// The principal components kept, one row of `names.len()` each.
    pub components: Vec<Vec<f64>>,
    /// How many episodes it learned from.
    pub episodes: usize,
    /// The median of the errors of reconstructing each of them with a
    /// model that didn't learn from it.
    pub error_median: f64,
    /// The spread of those errors: 1.4826 times their median absolute
    /// deviation, at least the error of every statistic being off by its
    /// own error, 1 %.
    pub error_spread: f64,
}

/// How an episode compared with its reconstruction.
#[derive(Clone, Debug, PartialEq)]
pub struct Reconstruction {
    /// The mean squared difference, in scales, over the statistics.
    pub error: f64,
    /// For each statistic: its name, value, reconstruction and squared
    /// difference in scales, the furthest first.
    pub statistics: Vec<(String, f64, f64, f64)>,
    /// The model's statistics the episode lacked, reconstructed from the
    /// others and counted as their mean.
    pub missing: usize,
}

impl LinearAutoencoder {
    /// Learns from `episodes`, keeping the fewest components that explain
    /// `variance` of their spread (from 0 to 1). `None` with fewer than 3
    /// episodes or no statistic they all have.
    #[must_use]
    pub fn train(episodes: &[Statistics], variance: f64) -> Option<Self> {
        if episodes.len() < 3 {
            return None;
        }
        let (names, values) = table(episodes)?;
        let mut model = fit(names, &values, variance);
        let errors: Vec<f64> = (0..values.len())
            .map(|left_out| {
                let others: Vec<Vec<f64>> = values
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != left_out)
                    .map(|(_, row)| row.clone())
                    .collect();
                let without = fit(model.names.clone(), &others, variance);
                without.error_of(&values[left_out]).0
            })
            .collect();
        (model.error_median, model.error_spread) = usual(errors, &model.means, &model.scales);
        Some(model)
    }

    /// How `statistics` compare with their reconstruction.
    #[must_use]
    pub fn reconstruct(&self, statistics: &Statistics) -> Reconstruction {
        compare(
            (&self.names, &self.means, &self.scales),
            statistics,
            |values| self.error_of(values),
        )
    }

    /// The reconstruction error of `values`, in the order of `names`, and
    /// the reconstruction.
    fn error_of(&self, values: &[f64]) -> (f64, Vec<f64>) {
        let standard = DVector::from_vec(standardised(values, &self.means, &self.scales));
        let mut rebuilt = DVector::zeros(values.len());
        for component in &self.components {
            let component = DVector::from_column_slice(component);
            rebuilt += &component * component.dot(&standard);
        }
        error_and_values(&standard, &rebuilt, &self.means, &self.scales)
    }
}

/// The model of `values` (one row per episode, one column per name),
/// before its errors are learned.
fn fit(names: Vec<String>, values: &[Vec<f64>], variance: f64) -> LinearAutoencoder {
    let (rows, columns) = (values.len(), names.len());
    let (means, scales) = standardisation(values, columns);
    let standard = DMatrix::from_fn(rows, columns, |row, column| {
        (values[row][column] - means[column]) / scales[column]
    });
    let svd = standard.svd(false, true);
    let mut components = Vec::new();
    if let Some(v_t) = svd.v_t {
        // Largest first, as the decomposition gives them.
        let mut order: Vec<usize> = (0..svd.singular_values.len()).collect();
        order.sort_by(|a, b| svd.singular_values[*b].total_cmp(&svd.singular_values[*a]));
        let total: f64 = svd.singular_values.iter().map(|value| value * value).sum();
        let mut explained = 0.0;
        for index in order {
            if total <= 0.0 || explained >= variance * total {
                break;
            }
            explained += svd.singular_values[index].powi(2);
            components.push(v_t.row(index).iter().copied().collect());
        }
    }
    LinearAutoencoder {
        names,
        means,
        scales,
        components,
        episodes: rows,
        error_median: 0.0,
        error_spread: 0.0,
    }
}

/// The statistics every one of `episodes` has, in order, and each
/// episode's value of each, one row per episode; `None` if they share none.
pub(crate) fn table(episodes: &[Statistics]) -> Option<(Vec<String>, Vec<Vec<f64>>)> {
    let rows: Vec<BTreeMap<String, f64>> = episodes.iter().map(measures).collect();
    let names: Vec<String> = rows
        .first()?
        .keys()
        .filter(|name| rows.iter().all(|row| row.contains_key(*name)))
        .cloned()
        .collect();
    if names.is_empty() {
        return None;
    }
    let values = rows
        .iter()
        .map(|row| names.iter().map(|name| row[name]).collect())
        .collect();
    Some((names, values))
}

/// Each of `columns` statistics' mean over `values` (a row per episode),
/// and its scale: its standard deviation, at least 1 % of its mean, the
/// statistics' own error, and never closer to zero than noise.
pub(crate) fn standardisation(values: &[Vec<f64>], columns: usize) -> (Vec<f64>, Vec<f64>) {
    #[allow(clippy::cast_precision_loss, reason = "episode counts are small")]
    let count = values.len() as f64;
    let means: Vec<f64> = (0..columns)
        .map(|column| values.iter().map(|row| row[column]).sum::<f64>() / count)
        .collect();
    let scales = (0..columns)
        .map(|column| {
            let mean = means[column];
            let spread = values
                .iter()
                .map(|row| (row[column] - mean).powi(2))
                .sum::<f64>()
                / count;
            spread
                .sqrt()
                .max(RELATIVE_ERROR * mean.abs())
                // Values this close to zero are noise, not a scale to
                // measure others by.
                .max(1e-9)
        })
        .collect();
    (means, scales)
}

/// `values` in scales from their means.
pub(crate) fn standardised(values: &[f64], means: &[f64], scales: &[f64]) -> Vec<f64> {
    values
        .iter()
        .zip(means.iter().zip(scales))
        .map(|(value, (mean, scale))| (value - mean) / scale)
        .collect()
}

/// The mean squared difference between `standard` and its reconstruction
/// `rebuilt`, both in scales, and the reconstruction in the statistics'
/// own units.
pub(crate) fn error_and_values(
    standard: &DVector<f64>,
    rebuilt: &DVector<f64>,
    means: &[f64],
    scales: &[f64],
) -> (f64, Vec<f64>) {
    #[allow(clippy::cast_precision_loss, reason = "statistic counts are small")]
    let error = (standard - rebuilt).norm_squared() / standard.len() as f64;
    let values = rebuilt
        .iter()
        .zip(means.iter().zip(scales))
        .map(|(standard, (mean, scale))| mean + standard * scale)
        .collect();
    (error, values)
}

/// The median and spread of reconstruction `errors` of episodes a model
/// didn't learn from: what is usual. The spread is 1.4826 times their
/// median absolute deviation, and at least the error every statistic being
/// off by its own error, 1 %, would make: smaller can't tell episodes apart.
pub(crate) fn usual(mut errors: Vec<f64>, means: &[f64], scales: &[f64]) -> (f64, f64) {
    let error_median = median(&mut errors);
    let mut deviations: Vec<f64> = errors
        .iter()
        .map(|error| (error - error_median).abs())
        .collect();
    #[allow(clippy::cast_precision_loss, reason = "statistic counts are small")]
    let measurement = means
        .iter()
        .zip(scales)
        .map(|(mean, scale)| (RELATIVE_ERROR * mean / scale).powi(2))
        .sum::<f64>()
        / means.len() as f64;
    (
        error_median,
        (MAD_TO_SD * median(&mut deviations)).max(measurement),
    )
}

/// How `statistics` compare with their reconstruction by a model of the
/// statistics `names`, with their `means` and `scales`, which
/// `reconstructed_by` reconstructs (giving the error and the values). A statistic the episode
/// lacks is taken as its mean.
pub(crate) fn compare(
    (names, means, scales): (&[String], &[f64], &[f64]),
    statistics: &Statistics,
    reconstructed_by: impl FnOnce(&[f64]) -> (f64, Vec<f64>),
) -> Reconstruction {
    let measured = measures(statistics);
    let missing = names
        .iter()
        .filter(|name| !measured.contains_key(*name))
        .count();
    let values: Vec<f64> = names
        .iter()
        .zip(means)
        .map(|(name, mean)| measured.get(name).copied().unwrap_or(*mean))
        .collect();
    let (error, rebuilt) = reconstructed_by(&values);
    let mut compared: Vec<(String, f64, f64, f64)> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let off = (values[index] - rebuilt[index]) / scales[index];
            (name.clone(), values[index], rebuilt[index], off * off)
        })
        .collect();
    compared.sort_by(|a, b| b.3.total_cmp(&a.3).then(a.0.cmp(&b.0)));
    Reconstruction {
        error,
        statistics: compared,
        missing,
    }
}

/// A learned autoencoder: linear or neural.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "model", rename_all = "snake_case")]
pub enum AutoencoderModel {
    /// Principal components: straight-line relations, learned in one step.
    Linear(LinearAutoencoder),
    /// A neural network: curved relations too.
    Neural(NeuralAutoencoder),
}

impl AutoencoderModel {
    /// Which it is, as `qa.toml` names it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Linear(_) => "linear",
            Self::Neural(_) => "neural",
        }
    }

    /// How `statistics` compare with their reconstruction.
    #[must_use]
    pub fn reconstruct(&self, statistics: &Statistics) -> Reconstruction {
        match self {
            Self::Linear(model) => model.reconstruct(statistics),
            Self::Neural(model) => model.reconstruct(statistics),
        }
    }

    /// How many episodes it learned from, and the median and spread of its
    /// usual errors.
    fn learned(&self) -> (usize, f64, f64) {
        match self {
            Self::Linear(model) => (model.episodes, model.error_median, model.error_spread),
            Self::Neural(model) => (model.episodes, model.error_median, model.error_spread),
        }
    }
}

/// Flags an episode whose statistics its package and task's autoencoder
/// reconstructs worse than it does the episodes it learned from. Until a
/// model is learned it says so, with how many episodes are still needed.
#[derive(Clone, Debug)]
pub struct AutoencoderAnomalies {
    /// The model, or how many more episodes it needs before one is learned.
    pub model: Result<AutoencoderModel, usize>,
    /// The model's version among its package and task's, which each result
    /// names, so a flag can be explained after the next is learned.
    pub version: u32,
    /// How far beyond the median error is flagged, in robust standard
    /// deviations of the errors.
    pub threshold: f64,
    /// Why a newer model isn't used, e.g. learning it stopped at its time
    /// limit; said with each result.
    pub note: Option<String>,
}

impl AutoencoderAnomalies {
    /// Its ID, which its models are kept under too.
    pub const ID: &'static str = "anomalies.autoencoder";
}

impl AnomalyDetector for AutoencoderAnomalies {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn version(&self) -> u32 {
        1
    }

    fn session_episodes(&self) -> usize {
        0
    }

    fn detect(&self, _: &EpisodeView, session: Session<'_>) -> Result<Finding, KaptoError> {
        let model = match (&self.model, &self.note) {
            (Ok(model), _) => model,
            (Err(_), Some(note)) => {
                return Ok(Finding::not_applicable(format!("not trained yet: {note}")));
            }
            (Err(needed), None) => {
                return Ok(Finding::not_applicable(format!(
                    "not trained yet: {needed} more checked episodes of this robot package and task needed"
                )));
            }
        };
        let compared = model.reconstruct(session.statistics);
        let (episodes, error_median, error_spread) = model.learned();
        let limit = error_median + self.threshold * error_spread;
        let noted = |message: String| match &self.note {
            Some(note) => format!("{message} ({note})"),
            None => message,
        };
        #[allow(clippy::cast_precision_loss, reason = "counts are small")]
        let counts = |finding: Finding| {
            finding
                .with_value("reconstruction.error", compared.error)
                .with_value("reconstruction.limit", limit)
                .with_value("model.episodes", episodes as f64)
                .with_value("model.version", f64::from(self.version))
                .with_value("model.missing", compared.missing as f64)
        };
        if compared.error <= limit {
            return Ok(counts(Finding::passed(noted(format!(
                "reconstructed as well as the {episodes} episodes its {} model learned from: error {:.3}, flagged above {limit:.3}",
                model.name(),
                compared.error
            )))));
        }
        // The statistics that account for the error, at least a tenth of it
        // each, the furthest first.
        let total: f64 = compared.statistics.iter().map(|(.., off)| off).sum();
        let furthest: Vec<&(String, f64, f64, f64)> = compared
            .statistics
            .iter()
            .take(NAMED)
            .filter(|(.., off)| *off >= 0.1 * total)
            .collect();
        let named: Vec<String> = furthest
            .iter()
            .map(|(name, value, rebuilt, _)| {
                format!(
                    "{name} {} where {} was expected",
                    readable(*value),
                    readable(*rebuilt)
                )
            })
            .collect();
        let finding = counts(Finding::failed(noted(format!(
            "unlike the {episodes} episodes its {} model learned from: error {}, above {}; furthest: {}",
            model.name(),
            readable(compared.error),
            readable(limit),
            named.join("; ")
        ))));
        Ok(furthest
            .into_iter()
            .fold(finding, |finding, (name, _, _, off)| {
                finding.with_value(name, *off)
            }))
    }
}
