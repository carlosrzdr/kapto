// SPDX-License-Identifier: Apache-2.0

//! Pre-flight as the API shows it: each check of a run with what its
//! failure means and what to do about it, so a page can show the remedy
//! next to the check and offer to record anyway, waiving exactly the
//! checks that failed.

use kapto_core::ErrorCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A pre-flight run: whether this machine can record what the robot sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PreflightRun {
    /// When it finished, in UTC, ISO 8601.
    pub ran_at: String,
    /// How long the streams were watched, in milliseconds.
    pub window_ms: u64,
    /// Whether every check passed. While one failed, starting an episode is
    /// refused (`capture.preflight_failed`) unless the start waives it, or
    /// `capture.preflight` is `warn`.
    pub passed: bool,
    /// Every check: the disk's room, its speed, then each stream in
    /// declaration order.
    pub checks: Vec<PreflightCheck>,
}

/// One pre-flight check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PreflightCheck {
    /// The cause code the check fails with, which names it too, e.g.
    /// `preflight.stream_silent`: what a start waives it by.
    pub code: String,
    /// What its failure means.
    pub summary: String,
    /// What to do if it failed.
    pub remedy: String,
    /// The stream it is about, for a stream's check.
    pub stream: Option<String>,
    /// Whether what was measured is enough.
    pub passed: bool,
    /// What was measured, in words.
    pub measured: String,
    /// What recording needs, in words.
    pub needed: String,
    /// Whether the episode being recorded was started despite its failure.
    pub waived: bool,
}

/// Run pre-flight again; an empty object.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {}

impl From<kapto_store::Preflight> for PreflightRun {
    fn from(run: kapto_store::Preflight) -> Self {
        Self {
            ran_at: run.ran_at,
            window_ms: run.window_ms,
            passed: run.checks.iter().all(|check| check.passed),
            checks: run.checks.into_iter().map(PreflightCheck::from).collect(),
        }
    }
}

impl From<kapto_store::PreflightCheck> for PreflightCheck {
    fn from(check: kapto_store::PreflightCheck) -> Self {
        // A code this server doesn't know, from a newer capture process,
        // still shows; only its texts are missing.
        let known = ErrorCode::ALL.iter().find(|code| code.id() == check.code);
        Self {
            summary: known.map_or_else(String::new, |code| code.summary().to_owned()),
            remedy: known.map_or_else(String::new, |code| code.remedy().to_owned()),
            code: check.code,
            stream: check.stream,
            passed: check.passed,
            measured: check.measured,
            needed: check.needed,
            waived: check.waived,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(code: &str, passed: bool) -> kapto_store::PreflightCheck {
        kapto_store::PreflightCheck {
            code: code.to_owned(),
            stream: Some("arm_joints".to_owned()),
            passed,
            measured: "0 messages in 5 s".to_owned(),
            needed: "about 1000 Hz".to_owned(),
            waived: false,
        }
    }

    #[test]
    fn a_run_says_whether_every_check_passed_and_each_check_its_remedy() {
        let run = PreflightRun::from(kapto_store::Preflight {
            ran_at: "2026-03-15T12:00:00Z".to_owned(),
            window_ms: 5000,
            checks: vec![
                check("preflight.disk_too_small", true),
                check("preflight.stream_silent", false),
            ],
        });
        assert!(!run.passed);
        let silent = &run.checks[1];
        assert_eq!(
            silent.summary,
            ErrorCode::PreflightStreamSilent.summary(),
            "its failure in words"
        );
        assert_eq!(silent.remedy, ErrorCode::PreflightStreamSilent.remedy());
        assert_eq!(silent.stream.as_deref(), Some("arm_joints"));
    }

    #[test]
    fn a_check_this_server_doesnt_know_shows_without_its_texts() {
        let unknown = PreflightCheck::from(check("preflight.added_later", false));
        assert_eq!(unknown.code, "preflight.added_later");
        assert!(unknown.summary.is_empty() && unknown.remedy.is_empty());
    }
}
