// SPDX-License-Identifier: Apache-2.0

//! The check framework, with fake checks: how results are made, and that
//! only a failed gate rejects an episode.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use kapto_core::{ErrorCode, KaptoError};
use kapto_qa::conformance::check_meets_the_contract;
use kapto_qa::{Check, CheckKind, EpisodeView, Finding, TimeRange, Verdict, run_checks};

const ID: &str = "20260305T192345.123Z-1a2b3c4d";

fn episode() -> EpisodeView {
    let summary = kapto_testkit::episode(
        ID,
        (1_772_700_400_000_000_000, 1_772_700_460_000_000_000),
        Vec::new(),
    );
    EpisodeView::new(Path::new("/data").join(ID).as_path(), summary)
}

/// A check that always finds what it was made to find.
struct Fake {
    id: &'static str,
    kind: CheckKind,
    finding: Result<Finding, ErrorCode>,
}

impl Check for Fake {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> u32 {
        2
    }

    fn kind(&self) -> CheckKind {
        self.kind
    }

    fn run(&self, _: &EpisodeView) -> Result<Finding, KaptoError> {
        self.finding.clone().map_err(KaptoError::new)
    }
}

fn fake(id: &'static str, kind: CheckKind, verdict: Verdict) -> Fake {
    let finding = match verdict {
        Verdict::Passed => Finding::passed("fine"),
        Verdict::Failed => Finding::failed("not fine"),
        Verdict::NotApplicable => Finding::not_applicable("nothing to look at"),
    };
    Fake {
        id,
        kind,
        finding: Ok(finding),
    }
}

#[test]
fn results_name_each_check_with_its_version_kind_and_finding_in_order() {
    let found = Finding::failed("12 messages dropped")
        .with_value("dropped", 12.0)
        .with_range(TimeRange {
            start_utc_ns: 1,
            end_utc_ns: 2,
        });
    let gate = Fake {
        id: "streams.complete",
        kind: CheckKind::Gate,
        finding: Ok(found.clone()),
    };
    let advisory = fake("episode.duration", CheckKind::Advisory, Verdict::Passed);
    let run = run_checks(&[&gate, &advisory], &episode(), Some("3".into()));
    assert_eq!(run.episode, ID);
    assert_eq!(run.params_version.as_deref(), Some("3"));
    let first = &run.results[0];
    assert_eq!(
        (
            first.check.as_str(),
            first.version,
            first.kind,
            first.verdict
        ),
        ("streams.complete", 2, CheckKind::Gate, Verdict::Failed)
    );
    assert_eq!(
        (&first.message, &first.values, &first.ranges),
        (&found.message, &found.values, &found.ranges)
    );
    assert_eq!(run.results[1].check, "episode.duration");
}

#[test]
fn a_failed_gate_rejects_the_episode_and_a_failed_advisory_never_does() {
    let episode = episode();
    let passing_gate = fake("streams.complete", CheckKind::Gate, Verdict::Passed);
    let failing_advisory = fake("motion.smooth", CheckKind::Advisory, Verdict::Failed);
    let failing_gate = fake("time.monotonic", CheckKind::Gate, Verdict::Failed);
    let absent_gate = fake("camera.frames", CheckKind::Gate, Verdict::NotApplicable);
    assert!(
        run_checks(
            &[&passing_gate, &failing_advisory, &absent_gate],
            &episode,
            None
        )
        .accepted()
    );
    assert!(!run_checks(&[&passing_gate, &failing_gate], &episode, None).accepted());
}

#[test]
fn a_check_that_cannot_run_is_recorded_as_failed_saying_why() {
    let broken = Fake {
        id: "recording.readable",
        kind: CheckKind::Gate,
        finding: Err(ErrorCode::RecordingUnreadable),
    };
    let run = run_checks(&[&broken], &episode(), None);
    assert_eq!(run.results[0].verdict, Verdict::Failed);
    assert!(
        run.results[0].message.starts_with("couldn't check: "),
        "{}",
        run.results[0].message
    );
    assert!(!run.accepted());
}

#[test]
fn a_check_meeting_the_contract_passes_the_conformance_suite() {
    check_meets_the_contract(
        &fake("streams.complete", CheckKind::Gate, Verdict::Passed),
        &[episode()],
    );
}

/// A check whose finding changes from one run to the next.
struct Flaky(AtomicU32);

impl Check for Flaky {
    fn id(&self) -> &'static str {
        "flaky.check"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Advisory
    }

    fn run(&self, _: &EpisodeView) -> Result<Finding, KaptoError> {
        let runs = self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Finding::passed("fine").with_value("runs", f64::from(runs)))
    }
}

#[test]
#[should_panic(expected = "found something else the second time")]
fn a_check_whose_finding_changes_between_runs_fails_the_conformance_suite() {
    check_meets_the_contract(&Flaky(AtomicU32::new(0)), &[episode()]);
}

#[test]
#[should_panic(expected = "isn't two or more dot-separated names")]
fn a_check_with_a_badly_formed_id_fails_the_conformance_suite() {
    check_meets_the_contract(&fake("Streams", CheckKind::Gate, Verdict::Passed), &[]);
}

#[test]
fn a_measured_value_that_isnt_a_finite_number_is_left_out() {
    let finding = Finding::passed("measured")
        .with_value("finite", 1.5)
        .with_value("infinite", f64::INFINITY)
        .with_value("not_a_number", f64::NAN);
    assert_eq!(finding.values.keys().collect::<Vec<_>>(), ["finite"]);
}
