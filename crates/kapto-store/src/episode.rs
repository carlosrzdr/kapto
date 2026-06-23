// SPDX-License-Identifier: Apache-2.0

//! `episode.json`: what an episode is and what happened while it was
//! recorded, in a small file people and tools can read without opening the
//! recording.
//!
//! Fields are only ever added, never renamed, removed or retyped. Version 1
//! holds what Kapto measures: identity, timing, the clock's state, every
//! stream's message counts, the robot package the streams were declared in,
//! if they were, and the session profile and task the episode was recorded
//! with, as they were when recording started.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use kapto_core::{ClockMap, EpisodeId, ErrorCode, KaptoError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sidecar::write_json;

/// The file's name in an episode folder.
pub const EPISODE_FILE: &str = "episode.json";

/// The value of the `format` field.
pub const FORMAT: &str = "kapto.episode";

/// The version this code writes.
pub const VERSION: u32 = 1;

/// What an episode is and what happened while it was recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Episode {
    /// Always `"kapto.episode"`.
    pub format: String,
    /// The format version; versions only ever add fields.
    pub version: u32,
    /// The episode's ID, the same as its folder name.
    pub id: String,
    /// The version of Kapto that recorded it.
    pub kapto_version: String,
    /// When its first message was received, its pre-roll's if it has one,
    /// or when recording began if that was earlier: the receive clock
    /// mapped to UTC, in nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// When recording ended, after any post-roll, on the same clock.
    pub ended_utc_ns: i64,
    /// When Start was asked, or `kapto record` began, on the same clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commanded_start_utc_ns: Option<i64>,
    /// When Stop was asked, or the episode's duration was up, on the same
    /// clock. Absent if it ended otherwise: aborted, the disk full, the
    /// process stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commanded_stop_utc_ns: Option<i64>,
    /// The recording's file name in the episode folder.
    pub recording: String,
    /// How the host's clock behaved during the episode.
    pub clock: ClockSummary,
    /// How close the buffers between receiving and writing came to full.
    pub buffers: BufferSummary,
    /// How long received messages waited before they were on disk, which
    /// bounds what a power cut could have lost.
    pub durability: DurabilitySummary,
    /// Every stream, in the order they were declared.
    pub streams: Vec<StreamSummary>,
    /// The robot package the streams were declared in, as it was when
    /// recording started. Absent when they weren't declared in one: the
    /// episode is `unchecked`, recorded exactly as received with no robot
    /// to check it against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageIdentity>,
    /// The session profile it was recorded with: with which robot, by
    /// whom, where and how. A copy as it was when recording started, so the
    /// episode keeps it however the profile changes later. Absent when none
    /// was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<SessionProfile>,
    /// The task it was recorded for: what the robot was asked to do. A copy
    /// as it was when recording started. Absent when none was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<SessionTask>,
    /// Present only for an episode recovered after recording stopped
    /// without finishing it, by a crash, a kill or a power cut: its
    /// recording was salvaged to its last intact record. Its counts are
    /// what the recording holds; how many messages were lost, by Kapto or
    /// before it, isn't known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered: Option<Recovered>,
    /// Pre-flight's latest run when the episode started: whether this
    /// machine could record what the robot sends, each check with whether
    /// it passed and, if it failed, whether it was waived to record anyway.
    /// Absent if pre-flight never ran, as for `kapto record`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight: Option<Preflight>,
}

/// How an episode was recovered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recovered {
    /// When it was recovered, in UTC nanoseconds since the Unix epoch.
    pub at_utc_ns: i64,
    /// Bytes cut off the end of its recording: records written only in
    /// part, or not put on disk before recording stopped.
    pub bytes_cut: u64,
    /// The newest message kept, in UTC nanoseconds; absent if none was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_utc_ns: Option<i64>,
    /// Why it had to be recovered, in words.
    pub reason: String,
}

/// The session profile an episode was recorded with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionProfile {
    /// Its ID: the profile file's name without `.toml`.
    pub id: String,
    /// What it says, as a SHA-256 in lowercase hex ([`content_hash`]): the
    /// same for every episode recorded with exactly this profile, whatever
    /// its file is called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Who operated the robot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// Where it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// How the robot was driven.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<OperationMode>,
    /// Words to find the episode by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Anything else worth knowing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// How the robot was driven while an episode was recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationMode {
    /// By a person.
    Teleoperated,
    /// By a policy: a rollout, kept and findable whether it went well or
    /// not.
    Autonomous,
    /// By a policy that a person took over at times.
    Mixed,
    /// By a fixed program.
    Scripted,
}

/// The content hash of a profile's or task's copy: SHA-256, in lowercase
/// hex, of its fields as `episode.json` writes them, without `id` and
/// `hash`, as compact JSON with the keys in alphabetical order. Two copies
/// that say the same have the same hash, whatever their files are called;
/// any change to what they say changes it. The order is set here rather
/// than left to the JSON library, whose order depends on its features.
#[must_use]
pub fn content_hash<T: Serialize>(copy: &T) -> String {
    let value = serde_json::to_value(copy).unwrap_or_default();
    let fields: BTreeMap<&String, &serde_json::Value> = value
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.as_str() != "id" && key.as_str() != "hash")
        .collect();
    let text = serde_json::to_string(&fields).unwrap_or_default();
    Sha256::digest(text.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

impl OperationMode {
    const ALL: [Self; 4] = [
        Self::Teleoperated,
        Self::Autonomous,
        Self::Mixed,
        Self::Scripted,
    ];

    /// The name `episode.json` gives it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Teleoperated => "teleoperated",
            Self::Autonomous => "autonomous",
            Self::Mixed => "mixed",
            Self::Scripted => "scripted",
        }
    }

    /// The mode named `name`, if it is one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == name)
    }
}

/// The task an episode was recorded for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionTask {
    /// Its ID: the task file's name without `.toml`.
    pub id: String,
    /// What it says, as a SHA-256 in lowercase hex ([`content_hash`]): the
    /// same for every episode recorded for exactly this task, whatever its
    /// file is called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// What people call it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A sentence or two about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The instruction given to the operator or the policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The objects it involves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<String>,
    /// Its steps in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<String>,
}

/// Which robot package, and exactly which files of it, an episode was
/// recorded with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    /// Its ID, as its `package.toml` says.
    pub id: String,
    /// Its version, as its `package.toml` says.
    pub version: String,
    /// Its content hash when recording started, as lowercase hex: changes
    /// whenever any of its files does, so two episodes with the same hash
    /// were recorded with exactly the same package.
    pub hash: String,
}

/// A pre-flight run: whether this machine could record what the robot
/// sends, checked before recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Preflight {
    /// When it finished, in UTC, ISO 8601.
    pub ran_at: String,
    /// How long the streams were watched, in milliseconds.
    pub window_ms: u64,
    /// Every check: the disk's room, its speed, then each stream in
    /// declaration order.
    pub checks: Vec<PreflightCheck>,
}

/// One pre-flight check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreflightCheck {
    /// The cause code the check fails with, which names it too, e.g.
    /// `preflight.stream_silent`.
    pub code: String,
    /// The stream it is about, for a stream's check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    /// Whether what was measured is enough.
    pub passed: bool,
    /// What was measured, in words.
    pub measured: String,
    /// What recording needs, in words.
    pub needed: String,
    /// Whether the operator chose to record despite its failure.
    #[serde(default)]
    pub waived: bool,
}

/// How the host's clock behaved during an episode. The full clock map is in
/// the recording's `kapto.clock_map` metadata record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClockSummary {
    /// How many times both clocks were read together.
    pub samples: u64,
    /// The largest gap between the two readings of one sample, in
    /// nanoseconds: how precisely receive times map to UTC.
    pub max_uncertainty_ns: u64,
    /// Every time the wall clock was stepped (e.g. by time sync) during the
    /// episode. Receive times aren't affected; times mapped to UTC are
    /// offset by the steps after the first sample.
    pub steps: Vec<ClockStepRecord>,
}

/// How full the buffers between receiving and writing got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BufferSummary {
    /// Batches of messages the buffers hold.
    pub batches: u64,
    /// The most batches ever waiting for the writer at once. Messages are
    /// dropped only when every batch is waiting.
    pub peak_batches: u64,
    /// Memory the buffers take, in bytes, set when recording starts.
    pub memory_bytes: u64,
}

/// How long received messages waited before they were on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DurabilitySummary {
    /// How often the recording was put on disk, in nanoseconds.
    pub sync_interval_ns: u64,
    /// How many times it was.
    pub syncs: u64,
    /// The longest a single sync took, in nanoseconds.
    pub max_sync_ns: u64,
    /// Half of the messages were on disk within this many nanoseconds.
    pub p50_unsynced_ns: u64,
    /// 99 % of the messages were on disk within this many nanoseconds.
    pub p99_unsynced_ns: u64,
    /// The longest any message waited, in nanoseconds.
    pub max_unsynced_ns: u64,
}

/// One step of the wall clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClockStepRecord {
    /// The receive clock, in nanoseconds, of the first sample after the step.
    pub at_mono_ns: u64,
    /// How far the wall clock moved, in nanoseconds; negative if back.
    pub change_ns: i64,
}

/// One stream's message counts. Every message received is either written or
/// dropped by Kapto; `lost_upstream` never reached Kapto at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamSummary {
    /// The stream's name.
    pub id: String,
    /// How loss before Kapto was detected, which says what `lost_upstream`
    /// means.
    pub loss_detection: LossDetectionKind,
    /// Messages that reached Kapto.
    pub received: u64,
    /// Received messages whose live fields could be read.
    pub decoded: u64,
    /// Received messages stored in the recording.
    pub written: u64,
    /// Received messages Kapto discarded.
    pub dropped_by_us: u64,
    /// Messages the source sent that never reached Kapto.
    pub lost_upstream: u64,
    /// For a stream that arrives over a connection (TCP): how many times
    /// the connection was lost. Whatever the robot sent while it was down
    /// never arrived. Absent for streams without a connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disconnects: Option<u64>,
    /// For a stream whose messages are kept for late subscribers: how many
    /// kept from before the episode it starts with, written at its start
    /// and not counted in `received` or `written`. Absent for other streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<u64>,
    /// For a stream with a pre-roll: how many of `written` were received
    /// before Start was asked. Absent for other streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_roll: Option<u64>,
    /// For a stream with a post-roll: how many of `written` were received
    /// after Stop was asked. Absent for other streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_roll: Option<u64>,
}

/// How a stream's upstream loss was detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LossDetectionKind {
    /// From gaps in the sequence numbers in its messages: exact.
    Sequence,
    /// From silences longer than its declared rate allows: an estimate.
    RateGap,
    /// Not at all: `lost_upstream` is always 0 and says nothing.
    Undetectable,
    /// From gaps in its publishers' own counts of their messages, which a
    /// ROS 2 middleware gives with each (Fast DDS and Zenoh do): exact for
    /// every message after the first received.
    PublisherCount,
}

impl ClockSummary {
    /// Summarises `clock`.
    #[must_use]
    pub fn of(clock: &ClockMap) -> Self {
        let samples = clock.samples();
        Self {
            samples: samples.len() as u64,
            max_uncertainty_ns: samples
                .iter()
                .map(|sample| u64::try_from(sample.uncertainty.as_nanos()).unwrap_or(u64::MAX))
                .max()
                .unwrap_or(0),
            steps: clock
                .steps()
                .into_iter()
                .map(|step| ClockStepRecord {
                    at_mono_ns: step.at.as_nanos(),
                    change_ns: step.change_ns,
                })
                .collect(),
        }
    }
}

impl Episode {
    /// An episode summary of this format version, recorded by this Kapto.
    #[must_use]
    pub fn new(
        id: &EpisodeId,
        (started_utc_ns, ended_utc_ns): (i64, i64),
        recording: &str,
        clock: &ClockMap,
        (buffers, durability): (BufferSummary, DurabilitySummary),
        streams: Vec<StreamSummary>,
    ) -> Self {
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            id: id.to_string(),
            kapto_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_utc_ns,
            ended_utc_ns,
            commanded_start_utc_ns: None,
            commanded_stop_utc_ns: None,
            recording: recording.to_owned(),
            clock: ClockSummary::of(clock),
            buffers,
            durability,
            streams,
            package: None,
            profile: None,
            task: None,
            recovered: None,
            preflight: None,
        }
    }

    /// The same, started when asked at `start` and stopped when asked at
    /// `stop`, if it was, in UTC nanoseconds.
    #[must_use]
    pub fn with_commands(mut self, (start, stop): (i64, Option<i64>)) -> Self {
        self.commanded_start_utc_ns = Some(start);
        self.commanded_stop_utc_ns = stop;
        self
    }

    /// The same, recorded with `package`; `None` for an `unchecked` one.
    #[must_use]
    pub fn with_package(mut self, package: Option<PackageIdentity>) -> Self {
        self.package = package;
        self
    }

    /// The same, recorded with `profile` and for `task`, either of which
    /// may be absent.
    #[must_use]
    pub fn with_session(
        mut self,
        profile: Option<SessionProfile>,
        task: Option<SessionTask>,
    ) -> Self {
        self.profile = profile;
        self.task = task;
        self
    }

    /// The same, started after pre-flight's run `preflight`, if any.
    #[must_use]
    pub fn with_preflight(mut self, preflight: Option<Preflight>) -> Self {
        self.preflight = preflight;
        self
    }

    /// The same, recovered as `recovered` says.
    #[must_use]
    pub fn with_recovered(mut self, recovered: Recovered) -> Self {
        self.recovered = Some(recovered);
        self
    }

    /// Writes `episode.json` in `dir`, complete or not at all.
    ///
    /// # Errors
    ///
    /// `episode.write_failed`.
    pub fn write(&self, dir: &Path) -> Result<(), KaptoError> {
        write_json(dir, EPISODE_FILE, self).map_err(|error| {
            KaptoError::new(ErrorCode::EpisodeWriteFailed)
                .detail(error.to_string())
                .context("folder", dir.display().to_string())
                .source(error)
        })
    }

    /// Reads `episode.json` from `dir`.
    ///
    /// # Errors
    ///
    /// `episode.unreadable` if it is missing, isn't valid JSON, or isn't of a
    /// format version this code knows.
    pub fn read(dir: &Path) -> Result<Self, KaptoError> {
        let path = dir.join(EPISODE_FILE);
        let unreadable = |detail: String| {
            KaptoError::new(ErrorCode::EpisodeUnreadable)
                .detail(detail)
                .context("file", path.display().to_string())
        };
        let text = std::fs::read(&path).map_err(|error| unreadable(error.to_string()))?;
        Self::parse(&text).map_err(unreadable)
    }

    /// The summary in `text`, as [`Episode::read`] reads it from a file.
    ///
    /// # Errors
    ///
    /// Why, if it isn't valid JSON or isn't of a format version this code
    /// knows.
    pub fn parse(text: &[u8]) -> Result<Self, String> {
        let episode: Self = serde_json::from_slice(text).map_err(|error| error.to_string())?;
        if episode.format != FORMAT || episode.version == 0 || episode.version > VERSION {
            return Err(format!(
                "format `{}` version {}; this Kapto reads `{FORMAT}` up to version {VERSION}",
                episode.format, episode.version
            ));
        }
        Ok(episode)
    }
}

/// The folder of episode `id` in `data_dir`; `None` if `id` isn't an
/// episode ID, so a name given from outside can never lead to a folder
/// elsewhere (`..`, a path, an absolute one).
#[must_use]
pub fn episode_folder(data_dir: &Path, id: &str) -> Option<PathBuf> {
    let id = EpisodeId::from_str(id).ok()?;
    Some(data_dir.join(id.to_string()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kapto_core::{ClockSample, RecvTime};

    use super::*;

    fn sample() -> Episode {
        let mut clock = ClockMap::new(ClockSample {
            mono: RecvTime::from_nanos(5_000_000_000),
            utc_ns: 1_772_700_400_000_000_000,
            uncertainty: Duration::from_nanos(300),
        });
        clock.record(ClockSample {
            mono: RecvTime::from_nanos(6_000_000_000),
            utc_ns: 1_772_700_401_250_000_000,
            uncertainty: Duration::from_nanos(900),
        });
        Episode::new(
            &"20260305T192345.123Z-1a2b3c4d".parse().unwrap(),
            (1_772_700_400_000_000_000, 1_772_700_460_000_000_000),
            "recording.mcap",
            &clock,
            (
                BufferSummary {
                    batches: 1_002,
                    peak_batches: 75,
                    memory_bytes: 94_396_416,
                },
                DurabilitySummary {
                    sync_interval_ns: 1_000_000_000,
                    syncs: 61,
                    max_sync_ns: 4_500_000,
                    p50_unsynced_ns: 510_000_000,
                    p99_unsynced_ns: 1_000_000_000,
                    max_unsynced_ns: 1_004_000_000,
                },
            ),
            vec![
                StreamSummary {
                    id: "arm_joints".to_owned(),
                    loss_detection: LossDetectionKind::Sequence,
                    received: 120_000,
                    decoded: 120_000,
                    written: 119_990,
                    dropped_by_us: 10,
                    lost_upstream: 3,
                    disconnects: None,
                    carried: None,
                    pre_roll: None,
                    post_roll: None,
                },
                StreamSummary {
                    id: "base_odometry".to_owned(),
                    loss_detection: LossDetectionKind::RateGap,
                    received: 6_000,
                    decoded: 6_000,
                    written: 6_000,
                    dropped_by_us: 0,
                    lost_upstream: 40,
                    disconnects: Some(1),
                    carried: None,
                    pre_roll: None,
                    post_roll: None,
                },
                StreamSummary {
                    id: "tf_static".to_owned(),
                    loss_detection: LossDetectionKind::Undetectable,
                    received: 0,
                    decoded: 0,
                    written: 0,
                    dropped_by_us: 0,
                    lost_upstream: 0,
                    disconnects: None,
                    carried: Some(2),
                    pre_roll: None,
                    post_roll: None,
                },
            ],
        )
    }

    /// The exact text, so a renamed, removed or reordered field shows up as
    /// a failing test rather than a silent format change.
    #[test]
    fn the_file_has_exactly_this_shape() {
        let json = serde_json::to_string_pretty(&sample()).unwrap();
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            json,
            format!(
                r#"{{
  "format": "kapto.episode",
  "version": 1,
  "id": "20260305T192345.123Z-1a2b3c4d",
  "kapto_version": "{version}",
  "started_utc_ns": 1772700400000000000,
  "ended_utc_ns": 1772700460000000000,
  "recording": "recording.mcap",
  "clock": {{
    "samples": 2,
    "max_uncertainty_ns": 900,
    "steps": [
      {{
        "at_mono_ns": 6000000000,
        "change_ns": 250000000
      }}
    ]
  }},
  "buffers": {{
    "batches": 1002,
    "peak_batches": 75,
    "memory_bytes": 94396416
  }},
  "durability": {{
    "sync_interval_ns": 1000000000,
    "syncs": 61,
    "max_sync_ns": 4500000,
    "p50_unsynced_ns": 510000000,
    "p99_unsynced_ns": 1000000000,
    "max_unsynced_ns": 1004000000
  }},
  "streams": [
    {{
      "id": "arm_joints",
      "loss_detection": "sequence",
      "received": 120000,
      "decoded": 120000,
      "written": 119990,
      "dropped_by_us": 10,
      "lost_upstream": 3
    }},
    {{
      "id": "base_odometry",
      "loss_detection": "rate_gap",
      "received": 6000,
      "decoded": 6000,
      "written": 6000,
      "dropped_by_us": 0,
      "lost_upstream": 40,
      "disconnects": 1
    }},
    {{
      "id": "tf_static",
      "loss_detection": "undetectable",
      "received": 0,
      "decoded": 0,
      "written": 0,
      "dropped_by_us": 0,
      "lost_upstream": 0,
      "carried": 2
    }}
  ]
}}"#
            )
        );
    }

    #[test]
    fn the_package_is_named_last_when_known_and_left_out_when_unchecked() {
        let unchecked = serde_json::to_string(&sample()).unwrap();
        assert!(!unchecked.contains("package"), "{unchecked}");

        let checked = sample().with_package(Some(PackageIdentity {
            id: "arm7".into(),
            version: "1.0.0".into(),
            hash: "9f2c".into(),
        }));
        let json = serde_json::to_string(&checked).unwrap();
        assert!(
            json.ends_with(r#""package":{"id":"arm7","version":"1.0.0","hash":"9f2c"}}"#),
            "{json}"
        );
        let read: Episode = serde_json::from_str(&json).unwrap();
        assert_eq!(read, checked);
    }

    #[test]
    fn the_profile_and_task_are_named_last_with_only_what_they_say() {
        let recorded = sample().with_session(
            Some(SessionProfile {
                id: "lab_ana".into(),
                hash: None,
                operator: Some("Ana".into()),
                environment: None,
                mode: Some(OperationMode::Teleoperated),
                tags: vec!["shift-a".into()],
                notes: None,
            }),
            Some(SessionTask {
                id: "pick_and_place".into(),
                hash: None,
                name: Some("Pick and place".into()),
                description: None,
                prompt: None,
                objects: Vec::new(),
                subtasks: vec!["reach".into(), "grasp".into()],
            }),
        );
        let json = serde_json::to_string(&recorded).unwrap();
        assert!(
            json.ends_with(
                r#""profile":{"id":"lab_ana","operator":"Ana","mode":"teleoperated","tags":["shift-a"]},"task":{"id":"pick_and_place","name":"Pick and place","subtasks":["reach","grasp"]}}"#
            ),
            "{json}"
        );
        let read: Episode = serde_json::from_str(&json).unwrap();
        assert_eq!(read, recorded);
    }

    #[test]
    fn the_content_hash_ignores_the_id_and_changes_with_anything_said() {
        let task = |id: &str, subtasks: &[&str]| SessionTask {
            id: id.into(),
            hash: None,
            name: Some("Pick and place".into()),
            description: None,
            prompt: None,
            objects: Vec::new(),
            subtasks: subtasks.iter().map(|&step| step.to_owned()).collect(),
        };
        let original = content_hash(&task("pick_and_place", &["reach", "grasp"]));
        assert_eq!(original.len(), 64);
        // Renamed: the same.
        assert_eq!(content_hash(&task("pick", &["reach", "grasp"])), original);
        // A hash already set doesn't count.
        let mut hashed = task("pick_and_place", &["reach", "grasp"]);
        hashed.hash = Some("anything".into());
        assert_eq!(content_hash(&hashed), original);
        // Anything it says, even the order of its steps, changes it.
        assert_ne!(
            content_hash(&task("pick_and_place", &["grasp", "reach"])),
            original
        );
        // The hash is of exactly this text, so other tools can check it.
        let digest = Sha256::digest(br#"{"name":"Pick and place","subtasks":["reach","grasp"]}"#);
        let expected = digest.iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        assert_eq!(original, expected);
    }

    #[test]
    fn it_reads_back_as_written_and_refuses_unknown_formats() {
        let dir = std::env::temp_dir().join(format!("kapto-{}-episode", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let episode = sample();
        episode.write(&dir).unwrap();
        assert_eq!(Episode::read(&dir).unwrap(), episode);
        assert!(!dir.join("episode.json.tmp").exists());

        let mut newer = sample();
        newer.version = VERSION + 1;
        newer.write(&dir).unwrap();
        let error = Episode::read(&dir).unwrap_err();
        assert_eq!(error.code(), ErrorCode::EpisodeUnreadable);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            Episode::read(&dir).unwrap_err().code(),
            ErrorCode::EpisodeUnreadable
        );
    }

    #[test]
    fn a_folder_that_cannot_hold_the_file_fails_with_write_failed() {
        let missing = std::env::temp_dir().join("kapto-no-such-episode-folder");
        let error = sample().write(&missing).unwrap_err();
        assert_eq!(error.code(), ErrorCode::EpisodeWriteFailed);
    }

    #[test]
    fn only_an_episode_id_names_an_episode_folder() {
        let data = Path::new("/data");
        assert_eq!(
            episode_folder(data, "20260305T192345.123Z-1a2b3c4d"),
            Some(data.join("20260305T192345.123Z-1a2b3c4d"))
        );
        for outside in [
            "..",
            "../etc",
            "/etc",
            "20260305T192345.123Z-1a2b3c4d/..",
            "",
        ] {
            assert_eq!(episode_folder(data, outside), None, "{outside}");
        }
    }
}
