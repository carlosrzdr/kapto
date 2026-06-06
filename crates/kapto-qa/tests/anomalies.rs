// SPDX-License-Identifier: Apache-2.0

//! Finding what is unusual about an episode for its session, on sessions
//! made to be alike and episodes made to stand out.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::path::Path;

use kapto_config::QaSettings;
use kapto_qa::anomalies::{
    AnomalyDetector, Session, StatisticalAnomalies, from_settings, run_detectors,
};
use kapto_qa::conformance::detector_meets_the_contract;
use kapto_qa::episode_statistics::Statistic;
use kapto_qa::statistics::Distribution;
use kapto_qa::{CheckKind, EpisodeView, Finding, QaRun, Verdict};

const ID: &str = "20260305T192345.123Z-1a2b3c4d";

/// An episode with no streams: the statistical detector looks only at
/// statistics.
fn episode() -> EpisodeView {
    let summary = kapto_testkit::episode(
        ID,
        (1_772_700_400_000_000_000, 1_772_700_430_000_000_000),
        Vec::new(),
    );
    EpisodeView::new(Path::new("/data").join(ID).as_path(), summary)
}

const DETECTOR: StatisticalAnomalies = StatisticalAnomalies {
    threshold: 3.5,
    min_episodes: 5,
    max_episodes: 200,
};

/// Statistics of an episode that lasted `duration_s` and whose joint
/// moved at speeds around `speed`.
fn statistics(duration_s: f64, speed: f64) -> BTreeMap<String, Statistic> {
    BTreeMap::from([
        (
            "episode.duration_s".to_owned(),
            Statistic::Value(duration_s),
        ),
        (
            "joints.joint_1.speed".to_owned(),
            Statistic::Distribution(Distribution::of(
                (0..100).map(|step| speed * (0.5 + f64::from(step) / 100.0)),
            )),
        ),
    ])
}

/// Ten episodes that vary a little, as a session does.
fn session() -> Vec<BTreeMap<String, Statistic>> {
    (0..10)
        .map(|index| {
            let wobble = f64::from(index % 5) * 0.02;
            statistics(30.0 * (1.0 + wobble), 0.4 * (1.0 - wobble))
        })
        .collect()
}

fn detect(
    statistics: &BTreeMap<String, Statistic>,
    others: &[BTreeMap<String, Statistic>],
) -> Finding {
    DETECTOR
        .detect(&episode(), Session { statistics, others })
        .unwrap()
}

#[test]
fn an_episode_like_its_session_passes_and_a_planted_outlier_is_flagged_by_name() {
    let session = session();
    let usual = detect(&statistics(31.0, 0.39), &session);
    assert_eq!(usual.verdict, Verdict::Passed, "{}", usual.message);
    assert_eq!(usual.values["session.episodes"], 10.0);

    // Its joint ran three times as fast as usual.
    let unusual = detect(&statistics(31.0, 1.2), &session);
    assert_eq!(unusual.verdict, Verdict::Failed);
    assert!(
        unusual
            .message
            .starts_with("unusual for its session of 10 other episodes: joints.joint_1.speed."),
        "{}",
        unusual.message
    );
    assert!(unusual.values["joints.joint_1.speed.p50"] > 3.5);
    assert!(!unusual.values.contains_key("episode.duration_s"));
    // Shorter than all of them, the other way.
    let short = detect(&statistics(5.0, 0.4), &session);
    assert!(
        short.values["episode.duration_s"] < -3.5,
        "{}",
        short.message
    );
    assert!(
        short.message.contains("episode.duration_s"),
        "{}",
        short.message
    );
    assert!(short.message.contains("below"), "{}", short.message);
}

#[test]
fn a_session_too_small_is_compared_with_nothing_and_says_so() {
    let alone = detect(&statistics(30.0, 0.4), &[]);
    assert_eq!(alone.verdict, Verdict::NotApplicable);
    assert_eq!(
        alone.message,
        "compared with nothing: 0 other checked episodes in its session, fewer than the 5 needed"
    );
    let few = detect(&statistics(90.0, 0.4), &session()[..4]);
    assert_eq!(few.verdict, Verdict::NotApplicable);
}

#[test]
fn a_session_whose_episodes_are_all_the_same_doesnt_flag_a_difference_within_the_statistics_error()
{
    let same = vec![statistics(30.0, 0.4); 6];
    // Half a percent off: within what the statistics can tell apart.
    assert_eq!(
        detect(&statistics(30.15, 0.4), &same).verdict,
        Verdict::Passed
    );
    // Ten percent off is beyond it.
    assert_eq!(
        detect(&statistics(33.0, 0.4), &same).verdict,
        Verdict::Failed
    );
}

#[test]
fn a_statistic_the_session_always_had_at_zero_is_flagged_saying_so_not_in_numberless_deviations() {
    // No message was ever lost in the session; this episode lost some.
    let lost = |ratio: f64| {
        let mut statistics = statistics(30.0, 0.4);
        statistics.insert(
            "streams.arm.lost_upstream_ratio".to_owned(),
            Statistic::Value(ratio),
        );
        statistics
    };
    let session = vec![lost(0.0); 6];
    let found = detect(&lost(0.077), &session);
    assert_eq!(found.verdict, Verdict::Failed);
    assert!(
        found
            .message
            .contains("streams.arm.lost_upstream_ratio 0.077 where the session's were all 0.000"),
        "{}",
        found.message
    );
    assert!(!found.message.contains("deviations"), "{}", found.message);
}

#[test]
fn a_statistic_the_session_doesnt_have_is_left_out_rather_than_flagged() {
    let mut gripped = statistics(30.0, 0.4);
    gripped.insert("gripper.opening.cycles".to_owned(), Statistic::Value(40.0));
    assert_eq!(detect(&gripped, &session()).verdict, Verdict::Passed);
}

#[test]
fn the_statistical_detector_meets_the_contract() {
    let session = session();
    detector_meets_the_contract(
        &DETECTOR,
        &episode(),
        &[
            (statistics(30.0, 0.4), session.clone()),
            (statistics(30.0, 2.0), session),
        ],
    );
}

#[test]
fn what_a_detector_flags_is_advisory_and_never_rejects_the_episode() {
    let session = session();
    let statistics = statistics(31.0, 1.2);
    let results = run_detectors(
        &[&DETECTOR],
        &episode(),
        Session {
            statistics: &statistics,
            others: &session,
        },
    );
    assert_eq!(
        (results[0].kind, results[0].verdict),
        (CheckKind::Advisory, Verdict::Failed)
    );
    let run = QaRun {
        episode: ID.to_owned(),
        params_version: None,
        computed_utc_ns: 1,
        results,
        statistics: BTreeMap::new(),
        model: None,
    };
    assert!(run.accepted());
}

#[test]
fn the_settings_turn_the_statistical_detector_on_with_its_parameters_or_off() {
    let mut settings = QaSettings::default();
    settings.statistical_anomalies.max_episodes = 50;
    let detectors = from_settings(&settings);
    assert_eq!(detectors.len(), 1);
    assert_eq!(detectors[0].id(), "anomalies.statistics");
    assert_eq!(detectors[0].session_episodes(), 50);
    settings.statistical_anomalies.enabled = false;
    assert!(from_settings(&settings).is_empty());
}
