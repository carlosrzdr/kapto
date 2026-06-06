// SPDX-License-Identifier: Apache-2.0

//! What every [`Check`] and [`AnomalyDetector`] must do, for each one's own
//! tests to call with the episodes it is tested on.

use std::str::FromStr;

use kapto_core::Role;

use crate::anomalies::{AnomalyDetector, Session};
use crate::episode_statistics::Statistics;
use crate::{Check, EpisodeView};

/// Checks that `check` meets the contract on each of `episodes`: its ID has
/// the form of a role (dot-separated names), its version is at least 1,
/// and running it twice on the same episode gives the same finding.
///
/// # Panics
///
/// When it doesn't, naming what is wrong: it is meant for tests.
pub fn check_meets_the_contract(check: &dyn Check, episodes: &[EpisodeView]) {
    let id = check.id();
    assert!(
        Role::from_str(id).is_ok(),
        "`{id}` isn't two or more dot-separated names"
    );
    assert!(check.version() >= 1, "`{id}` has version 0");
    for episode in episodes {
        let first = check.run(episode).map_err(|error| error.report());
        let again = check.run(episode).map_err(|error| error.report());
        assert_eq!(
            first,
            again,
            "`{id}` found something else the second time on {}",
            episode.summary().id
        );
    }
}

/// Checks that `detector` meets the contract on `episode` with each of
/// `cases`, its statistics and its session's: its ID has the form of a
/// role, its version is at least 1, and detecting twice gives the same
/// finding. Its findings are always advisory: [`run_detectors`](crate::anomalies::run_detectors)
/// records them so.
///
/// # Panics
///
/// When it doesn't, naming what is wrong: it is meant for tests.
pub fn detector_meets_the_contract(
    detector: &dyn AnomalyDetector,
    episode: &EpisodeView,
    cases: &[(Statistics, Vec<Statistics>)],
) {
    let id = detector.id();
    assert!(
        Role::from_str(id).is_ok(),
        "`{id}` isn't two or more dot-separated names"
    );
    assert!(detector.version() >= 1, "`{id}` has version 0");
    for (statistics, others) in cases {
        let session = Session { statistics, others };
        let first = detector
            .detect(episode, session)
            .map_err(|error| error.report());
        let again = detector
            .detect(episode, session)
            .map_err(|error| error.report());
        assert_eq!(first, again, "`{id}` found something else the second time");
    }
}
