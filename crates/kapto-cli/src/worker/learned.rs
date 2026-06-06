// SPDX-License-Identifier: Apache-2.0

//! The autoencoder detector for an episode: with the newest model of its
//! robot package and task, learned again first when enough new accepted
//! episodes came since the last, when `qa.toml` asks for another kind, or
//! when the one kept can't be read; or not trained yet. Learning that
//! passes its time limit stops, and the previous model stays in use.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use kapto_config::{AutoencoderAnomaliesSettings, AutoencoderChoice};
use kapto_core::clock::utc_now_ns;
use kapto_core::{ErrorCode, KaptoError};
use kapto_qa::EpisodeView;
use kapto_qa::autoencoder::neural::{NeuralAutoencoder, TimeLimit};
use kapto_qa::autoencoder::{AutoencoderAnomalies, AutoencoderModel, LinearAutoencoder};
use kapto_qa::episode_statistics::{self, Statistics};
use kapto_store::db::DATABASE_FILE;
use kapto_store::{AnomalyModel, Reader};

/// The autoencoder for `episode`, from the database in the data directory
/// holding `folder`, and the model learned for it now, to be kept with its
/// results, if one was.
///
/// # Errors
///
/// `store.open_failed` or `store.query_failed` if the database can't be
/// read; `store.query_failed` if a learned model can't be written.
pub(super) fn autoencoder(
    settings: &AutoencoderAnomaliesSettings,
    episode: &EpisodeView,
    folder: &Path,
) -> Result<(AutoencoderAnomalies, Option<AnomalyModel>), KaptoError> {
    let kind = AutoencoderAnomalies::ID;
    let id = &episode.summary().id;
    let data_dir = folder.parent().unwrap_or(folder);
    let reader = Reader::open(&data_dir.join(DATABASE_FILE))?;
    let latest = reader.latest_model(id, kind)?;
    let learnable = reader.training_statistics(id, settings.max_episodes)?;
    let known: BTreeSet<&String> = latest.iter().flat_map(|model| &model.episodes).collect();
    let (learnable_count, new) = (
        learnable.len(),
        learnable.keys().filter(|id| !known.contains(id)).count(),
    );
    // A kept model this version can't read is learned again, as is one of
    // another kind than `qa.toml` now asks for.
    let previous: Option<(AutoencoderModel, u32)> = latest.as_ref().and_then(|latest| {
        serde_json::from_str(&latest.model)
            .ok()
            .map(|model| (model, latest.version))
    });
    let due = previous.as_ref().is_none_or(|(model, _)| {
        new >= settings.retrain_after || model.name() != settings.model.name()
    });
    let mut note = None;
    if learnable_count >= settings.min_episodes && due {
        let episodes: Vec<String> = learnable.keys().cloned().collect();
        let statistics: Vec<Statistics> = learnable
            .into_values()
            .map(episode_statistics::from_stored)
            .collect();
        match learn(settings, &statistics) {
            Ok(Some(model)) => {
                let version = latest.as_ref().map_or(1, |latest| latest.version + 1);
                let kept = AnomalyModel {
                    package_id: episode
                        .summary()
                        .package
                        .as_ref()
                        .map(|package| package.id.clone()),
                    task: episode.summary().task.as_ref().map(|task| task.id.clone()),
                    kind: kind.to_owned(),
                    version,
                    trained_utc_ns: utc_now_ns(),
                    episodes,
                    settings: serde_json::json!({
                        "model": settings.model.name(),
                        "neurons": settings.neurons,
                        "variance": settings.variance,
                    }),
                    model: serde_json::to_string(&model).map_err(|error| unreadable(&error))?,
                };
                let detector = detector(settings, Ok(model), version, None);
                return Ok((detector, Some(kept)));
            }
            Ok(None) => {}
            Err(TimeLimit) => {
                let limit = format!("{} s time limit", settings.train_seconds);
                note = Some(if previous.is_some() {
                    format!("learning a newer model stopped at its {limit}")
                } else {
                    format!("learning stopped at its {limit}")
                });
            }
        }
    }
    let detector = match previous {
        Some((model, version)) => detector(settings, Ok(model), version, note),
        None => detector(
            settings,
            Err(settings.min_episodes.saturating_sub(learnable_count)),
            0,
            note,
        ),
    };
    Ok((detector, None))
}

/// The model of `episodes` `settings` ask for, unless its time limit passes
/// first; `None` if there are too few or they share no statistic.
fn learn(
    settings: &AutoencoderAnomaliesSettings,
    episodes: &[Statistics],
) -> Result<Option<AutoencoderModel>, TimeLimit> {
    match settings.model {
        AutoencoderChoice::Linear => {
            Ok(LinearAutoencoder::train(episodes, settings.variance).map(AutoencoderModel::Linear))
        }
        AutoencoderChoice::Neural => {
            let deadline = Instant::now() + Duration::from_secs_f64(settings.train_seconds);
            NeuralAutoencoder::train(episodes, settings.neurons, deadline)
                .map(|model| model.map(AutoencoderModel::Neural))
        }
    }
}

fn detector(
    settings: &AutoencoderAnomaliesSettings,
    model: Result<AutoencoderModel, usize>,
    version: u32,
    note: Option<String>,
) -> AutoencoderAnomalies {
    AutoencoderAnomalies {
        model,
        version,
        threshold: settings.threshold,
        note,
    }
}

fn unreadable(error: &serde_json::Error) -> KaptoError {
    KaptoError::new(ErrorCode::StoreQueryFailed).detail(format!(
        "the anomaly model can't be read or written: {error}"
    ))
}
