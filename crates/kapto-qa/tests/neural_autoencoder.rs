// SPDX-License-Identifier: Apache-2.0

//! The neural autoencoder of a package and task's episodes, on episodes
//! whose statistics lie on a curve, which the linear one can't learn.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::time::{Duration, Instant};

use kapto_qa::autoencoder::LinearAutoencoder;
use kapto_qa::autoencoder::neural::{NeuralAutoencoder, TimeLimit};
use kapto_qa::episode_statistics::{Statistic, Statistics};

/// An episode whose joint moved at `speed` and needed `effort`.
fn statistics(speed: f64, effort: f64) -> Statistics {
    Statistics::from([
        (
            "joints.elbow.speed_mean".to_owned(),
            Statistic::Value(speed),
        ),
        (
            "joints.elbow.effort_mean".to_owned(),
            Statistic::Value(effort),
        ),
        (
            "episode.duration_s".to_owned(),
            Statistic::Value(10.0 + speed),
        ),
    ])
}

/// Eighty episodes whose effort grows with the square of how far the speed
/// is from 2: on a curve, with a little noise.
fn learned_from() -> Vec<Statistics> {
    (0..80)
        .map(|index| {
            let speed = 1.0 + 2.0 * f64::from(index) / 79.0;
            statistics(
                speed,
                (speed - 2.0).powi(2) + 0.01 * f64::from(index * 13).sin(),
            )
        })
        .collect()
}

fn later() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

#[test]
fn a_mix_off_the_curve_is_flagged_by_the_neural_autoencoder_and_missed_by_the_linear_one() {
    let episodes = learned_from();
    let neural = NeuralAutoencoder::train(&episodes, 16, later())
        .unwrap()
        .unwrap();
    let linear = LinearAutoencoder::train(&episodes, 0.9).unwrap();
    let neural_limit = neural.error_median + 3.5 * neural.error_spread;
    let linear_limit = linear.error_median + 3.5 * linear.error_spread;
    for (speed, effort) in [(1.5, 0.25), (2.8, 0.64)] {
        let on = statistics(speed, effort);
        assert!(
            neural.reconstruct(&on).error <= neural_limit,
            "{speed} {effort}"
        );
    }
    // Each value within what was seen, together not.
    let off = statistics(2.0, 0.8);
    let flagged = neural.reconstruct(&off);
    assert!(
        flagged.error > neural_limit,
        "{} vs {neural_limit}",
        flagged.error
    );
    assert_eq!(flagged.statistics[0].0, "joints.elbow.effort_mean");
    assert!(linear.reconstruct(&off).error <= linear_limit);
    assert!(neural.epochs > 0 && neural.epochs <= kapto_qa::autoencoder::neural::MAX_EPOCHS);
}

#[test]
fn the_same_episodes_give_the_same_model_and_it_reads_back_the_same() {
    let episodes = learned_from();
    let model = NeuralAutoencoder::train(&episodes, 8, later())
        .unwrap()
        .unwrap();
    assert_eq!(
        NeuralAutoencoder::train(&episodes, 8, later())
            .unwrap()
            .unwrap(),
        model
    );
    let kept: NeuralAutoencoder =
        serde_json::from_str(&serde_json::to_string(&model).unwrap()).unwrap();
    let episode = statistics(2.0, 0.8);
    assert_eq!(kept.reconstruct(&episode), model.reconstruct(&episode));
}

#[test]
fn too_few_episodes_give_no_model_and_a_passed_time_limit_stops_learning() {
    let episodes = learned_from();
    assert_eq!(
        NeuralAutoencoder::train(&episodes[..4], 16, later()),
        Ok(None)
    );
    assert_eq!(
        NeuralAutoencoder::train(&episodes, 16, Instant::now()),
        Err(TimeLimit)
    );
}
