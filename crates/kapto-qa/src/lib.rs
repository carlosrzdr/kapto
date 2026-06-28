// SPDX-License-Identifier: Apache-2.0

//! Quality checks on recorded episodes, per-stream statistics and automatic labelling.
//!
//! A [`Check`] looks at one recorded episode through an [`EpisodeView`] and
//! says what it found ([`Finding`]). Each is either a **gate**,
//! deterministic and structural, which decides whether the episode is
//! accepted, or an **advisory**, which only flags: nothing advisory can
//! reject data. [`run_checks`] runs a set of checks on an episode and gives
//! their results ([`QaRun`]), which the worker records.
//!
//! A check is made with its own parameters, read from the robot package's
//! `qa.toml`, so running it needs only the episode.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kapto_core::clock::utc_now_ns;
use kapto_core::{KaptoError, Role};
use kapto_store::Episode;
pub use kapto_store::{CheckKind, QaResult, QaRun, TimeRange, Verdict};
pub use roles::RoleColumn;

pub mod anomalies;
pub mod autoencoder;
pub mod cameras;
pub mod carried;
pub mod checks;
pub mod conformance;
pub mod episode_statistics;
pub mod roles;
pub mod statistics;
pub mod timing;

/// `nanos` nanoseconds as seconds. Spans in an episode stay far below
/// 2^53 ns, so the result is exact to the nanosecond.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "spans of an episode, in seconds"
)]
pub fn seconds(nanos: i64) -> f64 {
    nanos as f64 / 1e9
}

/// A recorded episode as checks see it: what `episode.json` says about it,
/// its folder, and its values by role once read ([`roles::read_roles`]).
/// Checks never read files themselves, nor see a stream's name.
#[derive(Clone, Debug)]
pub struct EpisodeView {
    folder: PathBuf,
    episode: Episode,
    roles: BTreeMap<Role, RoleColumn>,
    cameras: BTreeMap<String, cameras::CameraTimeline>,
    timing: BTreeMap<String, timing::StreamTiming>,
    events: Vec<kapto_store::Event>,
}

impl EpisodeView {
    /// The finalised episode in `folder`.
    ///
    /// # Errors
    ///
    /// `episode.unreadable` if its `episode.json` can't be read.
    pub fn open(folder: &Path) -> Result<Self, KaptoError> {
        Episode::read(folder).map(|episode| Self::new(folder, episode))
    }

    /// The episode `episode`, kept in `folder`.
    #[must_use]
    pub fn new(folder: &Path, episode: Episode) -> Self {
        Self {
            folder: folder.to_owned(),
            episode,
            roles: BTreeMap::new(),
            cameras: BTreeMap::new(),
            timing: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    /// The same, with the events it holds.
    #[must_use]
    pub fn with_events(mut self, events: Vec<kapto_store::Event>) -> Self {
        self.events = events;
        self
    }

    /// The events it holds, in the order written, once read
    /// ([`roles::read_episode`]).
    #[must_use]
    pub fn events(&self) -> &[kapto_store::Event] {
        &self.events
    }

    /// The same, with when each stream's messages arrived, by stream name.
    #[must_use]
    pub fn with_timing(mut self, timing: BTreeMap<String, timing::StreamTiming>) -> Self {
        self.timing = timing;
        self
    }

    /// When each stream's messages arrived, by stream name, once read
    /// ([`roles::read_episode`]).
    #[must_use]
    pub fn timing(&self) -> &BTreeMap<String, timing::StreamTiming> {
        &self.timing
    }

    /// The same, with its cameras' frames, by stream name.
    #[must_use]
    pub fn with_cameras(mut self, cameras: BTreeMap<String, cameras::CameraTimeline>) -> Self {
        self.cameras = cameras;
        self
    }

    /// Each camera's frames, by stream name, once read
    /// ([`roles::read_episode`]).
    #[must_use]
    pub fn cameras(&self) -> &BTreeMap<String, cameras::CameraTimeline> {
        &self.cameras
    }

    /// The same, with its values by role.
    #[must_use]
    pub fn with_roles(mut self, roles: BTreeMap<Role, RoleColumn>) -> Self {
        self.roles = roles;
        self
    }

    /// The values of `role`, if the episode has it.
    #[must_use]
    pub fn role(&self, role: &Role) -> Option<&RoleColumn> {
        self.roles.get(role)
    }

    /// Every role the episode has, in order.
    pub fn roles(&self) -> impl Iterator<Item = &Role> {
        self.roles.keys()
    }

    /// What `episode.json` says: its times, every stream's counts, the
    /// robot package, profile and task.
    #[must_use]
    pub fn summary(&self) -> &Episode {
        &self.episode
    }

    /// Its folder.
    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.folder
    }
}

/// What a check found on an episode.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// Passed, failed, or not applicable.
    pub verdict: Verdict,
    /// What it found, in words.
    pub message: String,
    /// What it measured, by name.
    pub values: BTreeMap<String, f64>,
    /// Where in the episode what it found is.
    pub ranges: Vec<TimeRange>,
}

impl Finding {
    /// Nothing wrong.
    #[must_use]
    pub fn passed(message: impl Into<String>) -> Self {
        Self::of(Verdict::Passed, message)
    }

    /// Something wrong, said in `message`.
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self::of(Verdict::Failed, message)
    }

    /// The episode has nothing the check looks at, for `reason`.
    #[must_use]
    pub fn not_applicable(reason: impl Into<String>) -> Self {
        Self::of(Verdict::NotApplicable, reason)
    }

    /// The same, with `value` measured as `name`; a value that isn't a
    /// finite number is left out, as results keep only those.
    #[must_use]
    pub fn with_value(mut self, name: &str, value: f64) -> Self {
        if value.is_finite() {
            self.values.insert(name.to_owned(), value);
        }
        self
    }

    /// The same, with what was found in `range`.
    #[must_use]
    pub fn with_range(mut self, range: TimeRange) -> Self {
        self.ranges.push(range);
        self
    }

    /// The result it makes for check or detector `check` of `version` and
    /// `kind`.
    #[must_use]
    pub fn into_result(self, check: &str, version: u32, kind: CheckKind) -> QaResult {
        QaResult {
            check: check.to_owned(),
            version,
            kind,
            verdict: self.verdict,
            message: self.message,
            values: self.values,
            ranges: self.ranges,
        }
    }

    fn of(verdict: Verdict, message: impl Into<String>) -> Self {
        Self {
            verdict,
            message: message.into(),
            values: BTreeMap::new(),
            ranges: Vec::new(),
        }
    }
}

/// One quality check, made with its parameters.
pub trait Check: Send + Sync {
    /// Its ID: two or more dot-separated names, e.g. `streams.complete`.
    fn id(&self) -> &'static str;

    /// Its version: raised whenever what it computes changes, so results of
    /// different versions aren't compared as alike. From 1.
    fn version(&self) -> u32;

    /// Whether it can reject an episode.
    fn kind(&self) -> CheckKind;

    /// What it finds on `episode`. The same episode and parameters always
    /// give the same finding.
    ///
    /// # Errors
    ///
    /// What kept it from looking, e.g. the recording can't be read.
    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError>;
}

/// Runs `checks` on `episode` in order, and gives their results.
/// `params_version` is the version of the parameters they were made with.
///
/// A check that can't run is recorded as failed, saying why: a gate that
/// can't confirm what it checks doesn't let the episode pass, and nothing
/// is left out silently.
#[must_use]
pub fn run_checks(
    checks: &[&dyn Check],
    episode: &EpisodeView,
    params_version: Option<String>,
) -> QaRun {
    let results = checks
        .iter()
        .map(|check| {
            check
                .run(episode)
                .unwrap_or_else(|error| Finding::failed(format!("couldn't check: {error}")))
                .into_result(check.id(), check.version(), check.kind())
        })
        .collect();
    QaRun {
        statistics: BTreeMap::new(),
        episode: episode.summary().id.clone(),
        params_version,
        computed_utc_ns: utc_now_ns(),
        results,
        model: None,
    }
}
