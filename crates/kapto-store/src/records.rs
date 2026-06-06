// SPDX-License-Identifier: Apache-2.0

//! The metadata records a recording carries about itself, so the MCAP file
//! alone says what it is: which episode, robot package, profile and task,
//! recorded how. They say what `episode.json` says, under the same names.
//!
//! An MCAP metadata record maps names to texts, so a record is flattened:
//! nested fields get dotted names (`package.id`), numbers and switches are
//! written as text, lists as JSON, and absent fields are left out.

use std::collections::BTreeMap;

use kapto_core::{ClockMap, EpisodeId};
use serde::Serialize;
use serde_json::Value;

use crate::episode::{
    BufferSummary, ClockSummary, OperationMode, PackageIdentity, Preflight, SessionProfile,
    SessionTask, StreamSummary,
};

/// The name of the record saying what the episode is, written when
/// recording starts.
pub const EPISODE_RECORD: &str = "kapto.episode";

/// The name of the record of the capture settings that shaped the
/// recording, written when recording starts.
pub const CAPTURE_RECORD: &str = "kapto.capture";

/// The name of the record of pre-flight's run the episode started after,
/// written when recording starts, if pre-flight had run.
pub const PREFLIGHT_RECORD: &str = "kapto.preflight";

/// The name of the record of how recording went, written when it ends,
/// before the file is closed. A recording without it wasn't finished.
pub const SUMMARY_RECORD: &str = "kapto.summary";

/// What is known about an episode when its recording starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EpisodeStart {
    /// The episode's ID.
    pub id: String,
    /// The version of Kapto recording it.
    pub kapto_version: String,
    /// When recording started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// The robot package its streams were declared in, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageIdentity>,
    /// The session profile it is recorded with, if one was chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<SessionProfile>,
    /// The task it is recorded for, if one was chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<SessionTask>,
}

impl EpisodeStart {
    /// Episode `id`, started at `started_utc_ns` by this Kapto, with the
    /// package, profile and task it is recorded with.
    #[must_use]
    pub fn new(
        id: &EpisodeId,
        started_utc_ns: i64,
        package: Option<PackageIdentity>,
        (profile, task): (Option<SessionProfile>, Option<SessionTask>),
    ) -> Self {
        Self {
            id: id.to_string(),
            kapto_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_utc_ns,
            package,
            profile,
            task,
        }
    }

    /// The `kapto.episode` record's entries.
    #[must_use]
    pub fn record(&self) -> BTreeMap<String, String> {
        flatten(self)
    }

    /// What a `kapto.episode` record says, read back field by field as
    /// [`EpisodeStart::record`] writes them, so a recording cut off before
    /// its `episode.json` was written still says what it is. `None` without
    /// its ID or start time; a package, profile or task without its ID is
    /// left out.
    #[must_use]
    pub fn from_record(entries: &BTreeMap<String, String>) -> Option<Self> {
        let text = |name: &str| entries.get(name).cloned();
        let list = |name: &str| {
            entries
                .get(name)
                .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                .unwrap_or_default()
        };
        let package = match (
            text("package.id"),
            text("package.version"),
            text("package.hash"),
        ) {
            (Some(id), Some(version), Some(hash)) => Some(PackageIdentity { id, version, hash }),
            _ => None,
        };
        let profile = text("profile.id").map(|id| SessionProfile {
            id,
            hash: text("profile.hash"),
            operator: text("profile.operator"),
            environment: text("profile.environment"),
            mode: entries
                .get("profile.mode")
                .and_then(|mode| OperationMode::parse(mode)),
            tags: list("profile.tags"),
            notes: text("profile.notes"),
        });
        let task = text("task.id").map(|id| SessionTask {
            id,
            hash: text("task.hash"),
            name: text("task.name"),
            description: text("task.description"),
            prompt: text("task.prompt"),
            objects: list("task.objects"),
            subtasks: list("task.subtasks"),
        });
        Some(Self {
            id: text("id")?,
            kapto_version: text("kapto_version").unwrap_or_default(),
            started_utc_ns: entries.get("started_utc_ns")?.parse().ok()?,
            package,
            profile,
            task,
        })
    }
}

/// How recording went, known when it ends: what `episode.json` says then,
/// but for how long messages waited to be on disk, which includes putting
/// the closed file on disk and so is known only after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EpisodeEnd {
    /// When recording ended, in UTC nanoseconds since the Unix epoch.
    pub ended_utc_ns: i64,
    /// How the host's clock behaved.
    pub clock: ClockSummary,
    /// How close the buffers came to full.
    pub buffers: BufferSummary,
    /// Every stream's counts, under its name.
    pub streams: BTreeMap<String, Value>,
}

impl EpisodeEnd {
    /// Recording ended at `ended_utc_ns`, with `clock`, `buffers` and
    /// `streams` as they were then.
    #[must_use]
    pub fn new(
        ended_utc_ns: i64,
        clock: &ClockMap,
        buffers: &BufferSummary,
        streams: &[StreamSummary],
    ) -> Self {
        let streams = streams
            .iter()
            .filter_map(|stream| {
                let mut counts = serde_json::to_value(stream).ok()?;
                // The name is the key already.
                counts.as_object_mut()?.remove("id");
                Some((stream.id.clone(), counts))
            })
            .collect();
        Self {
            ended_utc_ns,
            clock: ClockSummary::of(clock),
            buffers: buffers.clone(),
            streams,
        }
    }

    /// The `kapto.summary` record's entries.
    #[must_use]
    pub fn record(&self) -> BTreeMap<String, String> {
        flatten(self)
    }
}

/// `value` as a record's entries: nested fields under dotted names,
/// numbers and switches as text, lists as JSON, absent fields left out.
#[must_use]
pub fn flatten(value: &impl Serialize) -> BTreeMap<String, String> {
    let mut entries = BTreeMap::new();
    if let Ok(value) = serde_json::to_value(value) {
        add(&mut entries, "", &value);
    }
    entries
}

fn add(entries: &mut BTreeMap<String, String>, name: &str, value: &Value) {
    let text = match value {
        Value::Null => return,
        Value::Object(fields) => {
            for (field, value) in fields {
                let name = if name.is_empty() {
                    field.clone()
                } else {
                    format!("{name}.{field}")
                };
                add(entries, &name, value);
            }
            return;
        }
        Value::String(text) => text.clone(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) => value.to_string(),
    };
    entries.insert(name.to_owned(), text);
}

impl Preflight {
    /// The run as its record holds it: when it ran, how long it watched,
    /// and its checks as JSON.
    #[must_use]
    pub fn record(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ran_at".to_owned(), self.ran_at.clone()),
            ("window_ms".to_owned(), self.window_ms.to_string()),
            (
                "checks".to_owned(),
                serde_json::to_string(&self.checks).unwrap_or_default(),
            ),
        ])
    }

    /// The run its record holds, as [`Preflight::record`] writes it; `None`
    /// if a field is missing or unreadable.
    #[must_use]
    pub fn from_record(entries: &BTreeMap<String, String>) -> Option<Self> {
        Some(Self {
            ran_at: entries.get("ran_at")?.clone(),
            window_ms: entries.get("window_ms")?.parse().ok()?,
            checks: serde_json::from_str(entries.get("checks")?).ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::episode::LossDetectionKind;

    #[test]
    fn a_record_names_nested_fields_with_dots_writes_lists_as_json_and_leaves_out_what_is_absent() {
        let start = EpisodeStart::new(
            &"20260305T192345.123Z-1a2b3c4d".parse().unwrap(),
            1_772_700_400_000_000_000,
            Some(PackageIdentity {
                id: "arm7".into(),
                version: "0.1.0".into(),
                hash: "ab12".into(),
            }),
            (
                Some(SessionProfile {
                    id: "lab_ana".into(),
                    hash: Some("cd34".into()),
                    operator: Some("Ana".into()),
                    environment: None,
                    mode: Some(OperationMode::Teleoperated),
                    tags: vec!["shift-a".into(), "new \"gripper\"".into()],
                    notes: None,
                }),
                None,
            ),
        );
        let expected: BTreeMap<String, String> = [
            ("id", "20260305T192345.123Z-1a2b3c4d"),
            ("kapto_version", env!("CARGO_PKG_VERSION")),
            ("started_utc_ns", "1772700400000000000"),
            ("package.id", "arm7"),
            ("package.version", "0.1.0"),
            ("package.hash", "ab12"),
            ("profile.id", "lab_ana"),
            ("profile.hash", "cd34"),
            ("profile.operator", "Ana"),
            ("profile.mode", "teleoperated"),
            ("profile.tags", r#"["shift-a","new \"gripper\""]"#),
        ]
        .into_iter()
        .map(|(name, text)| (name.to_owned(), text.to_owned()))
        .collect();
        assert_eq!(start.record(), expected);
    }

    #[test]
    fn a_preflight_record_reads_back_as_what_was_written_and_without_a_field_as_nothing() {
        let run = Preflight {
            ran_at: "2026-03-15T20:00:00.000Z".into(),
            window_ms: 5000,
            checks: vec![crate::episode::PreflightCheck {
                code: "preflight.stream_silent".into(),
                stream: Some("arm_joints".into()),
                passed: false,
                measured: "0 messages in 5 s".into(),
                needed: "at least 90 % of 1000 Hz".into(),
                waived: true,
            }],
        };
        assert_eq!(Preflight::from_record(&run.record()), Some(run.clone()));
        let mut without = run.record();
        without.remove("window_ms");
        assert_eq!(Preflight::from_record(&without), None);
    }

    #[test]
    fn a_start_record_reads_back_as_what_was_written_and_without_its_id_or_time_as_nothing() {
        let start = EpisodeStart::new(
            &"20260305T192345.123Z-1a2b3c4d".parse().unwrap(),
            1_772_700_400_000_000_000,
            Some(PackageIdentity {
                id: "arm7".into(),
                version: "0.1.0".into(),
                // All digits, which must stay text.
                hash: "1234".into(),
            }),
            (
                Some(SessionProfile {
                    id: "lab_ana".into(),
                    hash: None,
                    operator: Some("Ana".into()),
                    environment: Some("lab 2".into()),
                    mode: Some(OperationMode::Mixed),
                    tags: vec!["shift-a".into(), "new \"gripper\"".into()],
                    notes: Some("7".into()),
                }),
                Some(SessionTask {
                    id: "pick_and_place".into(),
                    hash: Some("ef56".into()),
                    name: Some("Pick and place".into()),
                    description: None,
                    prompt: Some("Pick up the red cube".into()),
                    objects: vec!["cube".into()],
                    subtasks: vec!["reach".into(), "grasp".into()],
                }),
            ),
        );
        assert_eq!(
            EpisodeStart::from_record(&start.record()),
            Some(start.clone())
        );
        let unchecked = EpisodeStart {
            package: None,
            profile: None,
            task: None,
            ..start.clone()
        };
        assert_eq!(
            EpisodeStart::from_record(&unchecked.record()),
            Some(unchecked)
        );
        for missing in ["id", "started_utc_ns"] {
            let mut record = start.record();
            record.remove(missing);
            assert_eq!(EpisodeStart::from_record(&record), None, "{missing}");
        }
    }

    #[test]
    fn the_summary_names_each_streams_counts_under_its_name() {
        let clock = ClockMap::new(kapto_core::ClockSample {
            mono: kapto_core::RecvTime::from_nanos(1),
            utc_ns: 1_772_700_400_000_000_000,
            uncertainty: std::time::Duration::from_nanos(300),
        });
        let stream = |id: &str, dropped| StreamSummary {
            id: id.into(),
            loss_detection: LossDetectionKind::Sequence,
            received: 2000,
            decoded: 2000,
            written: 2000 - dropped,
            dropped_by_us: dropped,
            lost_upstream: 3,
            disconnects: None,
            carried: None,
        };
        let end = EpisodeEnd::new(
            1_772_700_460_000_000_000,
            &clock,
            &BufferSummary {
                batches: 64,
                peak_batches: 5,
                memory_bytes: 1024,
            },
            &[stream("arm_joints", 0), stream("gripper", 12)],
        );
        let record = end.record();
        let entry = |name: &str| record.get(name).map(String::as_str);
        assert_eq!(entry("ended_utc_ns"), Some("1772700460000000000"));
        assert_eq!(entry("buffers.peak_batches"), Some("5"));
        assert_eq!(entry("clock.samples"), Some("1"));
        assert_eq!(entry("clock.max_uncertainty_ns"), Some("300"));
        assert_eq!(entry("streams.gripper.dropped_by_us"), Some("12"));
        assert_eq!(entry("streams.gripper.loss_detection"), Some("sequence"));
        assert_eq!(entry("streams.arm_joints.lost_upstream"), Some("3"));
        assert_eq!(entry("streams.arm_joints.id"), None);
        assert_eq!(entry("streams.arm_joints.disconnects"), None);
    }
}
