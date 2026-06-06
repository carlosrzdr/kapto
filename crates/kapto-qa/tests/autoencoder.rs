// SPDX-License-Identifier: Apache-2.0

//! The linear autoencoder of a package and task's episodes, on episodes
//! made to go together and episodes made not to.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;

use kapto_qa::anomalies::{AnomalyDetector, Session, StatisticalAnomalies};
use kapto_qa::autoencoder::{AutoencoderAnomalies, AutoencoderModel, LinearAutoencoder};
use kapto_qa::conformance::detector_meets_the_contract;
use kapto_qa::episode_statistics::{Statistic, Statistics};
use kapto_qa::statistics::Distribution;
use kapto_qa::{EpisodeView, Finding, Verdict};

const ID: &str = "20260305T192345.123Z-1a2b3c4d";

/// An episode that lasted `duration_s`, closed the gripper `cycles` times
/// and moved its joint at speeds around `speed`.
fn statistics(duration_s: f64, cycles: f64, speed: f64) -> Statistics {
    Statistics::from([
        (
            "episode.duration_s".to_owned(),
            Statistic::Value(duration_s),
        ),
        (
            "gripper.opening.cycles".to_owned(),
            Statistic::Value(cycles),
        ),
        (
            "joints.joint_1.speed".to_owned(),
            Statistic::Distribution(Distribution::of(
                (0..50).map(|step| speed * (0.5 + f64::from(step) / 50.0)),
            )),
        ),
    ])
}

/// Thirty episodes of a task in which the gripper closes once every five
/// seconds, so longer episodes have more cycles, at varied speeds.
fn learned_from() -> Vec<Statistics> {
    (0..30)
        .map(|index| {
            let wobble = f64::from(index).sin();
            let duration = 30.0 + 10.0 * wobble;
            statistics(
                duration,
                duration / 5.0 + 0.1 * f64::from(index).cos(),
                0.4 + 0.05 * (f64::from(index) * 0.7).cos(),
            )
        })
        .collect()
}

fn detector() -> AutoencoderAnomalies {
    AutoencoderAnomalies {
        model: Ok(AutoencoderModel::Linear(
            LinearAutoencoder::train(&learned_from(), 0.9).unwrap(),
        )),
        version: 1,
        threshold: 3.5,
        note: None,
    }
}

fn detect(detector: &dyn AnomalyDetector, statistics: &Statistics) -> Finding {
    let episode = EpisodeView::new(
        &Path::new("/data").join(ID),
        kapto_testkit::episode(ID, (0, 30_000_000_000), Vec::new()),
    );
    detector
        .detect(
            &episode,
            Session {
                statistics,
                others: &learned_from(),
            },
        )
        .unwrap()
}

#[test]
fn an_episode_like_those_it_learned_from_is_reconstructed_well_and_passes() {
    let usual = detect(&detector(), &statistics(35.0, 7.0, 0.42));
    assert_eq!(usual.verdict, Verdict::Passed, "{}", usual.message);
    assert_eq!(usual.values["model.episodes"], 30.0);
    assert_eq!(usual.values["model.version"], 1.0);
    assert!(usual.values["reconstruction.error"] <= usual.values["reconstruction.limit"]);
}

#[test]
fn a_mix_unlike_any_it_learned_from_is_flagged_though_each_statistic_is_ordinary() {
    // A short episode with as many cycles as the long ones: each within
    // what was seen, together not.
    let odd = statistics(22.0, 7.8, 0.42);
    let flagged = detect(&detector(), &odd);
    assert_eq!(flagged.verdict, Verdict::Failed, "{}", flagged.message);
    assert!(
        flagged
            .message
            .starts_with("unlike the 30 episodes its linear model learned from"),
        "{}",
        flagged.message
    );
    assert!(flagged.values.contains_key("gripper.opening.cycles"));
    // Only what accounts for the error is named: not the joint's speed.
    assert!(
        !flagged.message.contains("joints.joint_1"),
        "{}",
        flagged.message
    );
    // Compared one statistic at a time, nothing stands out.
    let one_at_a_time = StatisticalAnomalies {
        threshold: 3.5,
        min_episodes: 5,
        max_episodes: 200,
    };
    assert_eq!(detect(&one_at_a_time, &odd).verdict, Verdict::Passed);
}

#[test]
fn a_detector_not_trained_yet_says_how_many_episodes_it_still_needs() {
    let untrained = AutoencoderAnomalies {
        model: Err(12),
        version: 1,
        threshold: 3.5,
        note: None,
    };
    let finding = detect(&untrained, &statistics(30.0, 6.0, 0.4));
    assert_eq!(finding.verdict, Verdict::NotApplicable);
    assert_eq!(
        finding.message,
        "not trained yet: 12 more checked episodes of this robot package and task needed"
    );
}

#[test]
fn the_same_episodes_give_the_same_model_and_too_few_give_none() {
    let episodes = learned_from();
    assert_eq!(
        LinearAutoencoder::train(&episodes, 0.9),
        LinearAutoencoder::train(&episodes, 0.9)
    );
    assert_eq!(LinearAutoencoder::train(&episodes[..2], 0.9), None);
    assert_eq!(
        LinearAutoencoder::train(&vec![Statistics::new(); 5], 0.9),
        None
    );
}

#[test]
fn a_statistic_the_episode_lacks_is_counted_and_taken_as_usual() {
    let model = LinearAutoencoder::train(&learned_from(), 0.9).unwrap();
    let mut lacking = statistics(30.0, 6.0, 0.4);
    lacking.remove("gripper.opening.cycles");
    let compared = model.reconstruct(&lacking);
    assert_eq!(compared.missing, 1);
    assert_eq!(
        detect(&detector(), &lacking).verdict,
        Verdict::Passed,
        "{}",
        compared.error
    );
}

#[test]
fn the_autoencoder_detector_meets_the_contract() {
    let episode = EpisodeView::new(
        &Path::new("/data").join(ID),
        kapto_testkit::episode(ID, (0, 30_000_000_000), Vec::new()),
    );
    detector_meets_the_contract(
        &detector(),
        &episode,
        &[
            (statistics(30.0, 6.0, 0.4), Vec::new()),
            (statistics(22.0, 7.8, 0.4), Vec::new()),
        ],
    );
}

#[test]
fn episodes_all_the_same_dont_flag_a_difference_within_the_statistics_error() {
    let same = vec![statistics(30.0, 6.0, 0.4); 10];
    let detector = AutoencoderAnomalies {
        model: Ok(AutoencoderModel::Linear(
            LinearAutoencoder::train(&same, 0.9).unwrap(),
        )),
        version: 1,
        threshold: 3.5,
        note: None,
    };
    // Half a percent off: within what the statistics can tell apart.
    let close = detect(&detector, &statistics(30.15, 6.03, 0.4));
    assert_eq!(close.verdict, Verdict::Passed, "{}", close.message);
    // A fifth off is beyond it.
    let far = detect(&detector, &statistics(36.0, 6.0, 0.4));
    assert_eq!(far.verdict, Verdict::Failed, "{}", far.message);
}

#[test]
fn why_a_newer_model_isnt_used_is_said_with_each_result() {
    let noted = AutoencoderAnomalies {
        note: Some("learning a newer model stopped at its 60 s time limit".to_owned()),
        ..detector()
    };
    let finding = detect(&noted, &statistics(35.0, 7.0, 0.42));
    assert!(
        finding
            .message
            .ends_with("(learning a newer model stopped at its 60 s time limit)"),
        "{}",
        finding.message
    );
    let first = AutoencoderAnomalies {
        model: Err(0),
        note: Some("learning stopped at its 60 s time limit".to_owned()),
        ..detector()
    };
    assert_eq!(
        detect(&first, &statistics(35.0, 7.0, 0.42)).message,
        "not trained yet: learning stopped at its 60 s time limit"
    );
}
