// SPDX-License-Identifier: Apache-2.0

//! The kinds of events that may be sent, as the API shows them, and the
//! check every event sent through the API or the CLI passes before capture
//! is asked. Read again on every request, so a vocabulary or task saved
//! meanwhile is seen without starting serve again.
//!
//! The API's own types rather than the configuration's: the API version and
//! the file formats change on their own schedules.

use std::collections::BTreeMap;
use std::path::Path;

use kapto_capture::control::NewEvent;
use kapto_config::{EVENTS_FILE, KindSource, Vocabulary, load_sessions, load_vocabulary};
use kapto_core::clock::utc_now_ns;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{Event, Phase};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::ApiError;

/// Every kind of event that may be sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EventVocabulary {
    /// The kinds: the vocabulary file's (or the defaults without it), then
    /// each task's subtasks.
    pub kinds: Vec<VocabularyKind>,
    /// What is wrong with the vocabulary file; the defaults stand in for
    /// its kinds while it has problems.
    pub problems: Vec<ApiError>,
}

/// One kind of event that may be sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VocabularyKind {
    /// The kind, as events carry it, e.g. `grasp` or `subtask.reach`.
    pub kind: String,
    /// What people call it.
    pub name: String,
    /// Its colour on the timeline, `#rrggbb`, if one is given.
    pub color: Option<String>,
    /// Whether it is usually a stretch, begun and ended, rather than a
    /// moment.
    pub stretch: bool,
    /// When to send it, if said.
    pub description: Option<String>,
    /// Where it is defined: `file` (the vocabulary file), `default` (there
    /// without one) or `task` (a task's subtask).
    pub from: String,
    /// For a subtask, the task's ID.
    pub task: Option<String>,
}

/// Send an event: something that happened now, or at a given moment, or
/// the beginning, end or whole of a stretch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendEvent {
    /// Its kind, one of `/api/event-kinds`.
    pub kind: String,
    /// A moment if not given.
    #[serde(default)]
    pub phase: EventPhase,
    /// Pairs a beginning with its end: required for both.
    #[serde(default)]
    pub id: Option<String>,
    /// When, in UTC nanoseconds; when capture takes it if not given. For a
    /// whole stretch, when it began.
    #[serde(default)]
    pub at_utc_ns: Option<i64>,
    /// When a whole stretch ended; only for one.
    #[serde(default)]
    pub until_utc_ns: Option<i64>,
    /// What someone said about it.
    #[serde(default)]
    pub text: Option<String>,
    /// More about it, as names and texts.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Who sends it: `api`, or `page` from the web UI; `api` if not given.
    #[serde(default)]
    pub source: Option<String>,
}

/// Whether an event is a moment, one end of a stretch, or a whole one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventPhase {
    /// It happened at that moment.
    #[default]
    Moment,
    /// A stretch began.
    Begin,
    /// A stretch ended.
    End,
    /// A whole stretch, from `at_utc_ns` to `until_utc_ns`.
    Stretch,
}

/// An event has been taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EventSent {
    /// The event as it was kept.
    pub event: SentEvent,
    /// The episode it was recorded in; `null` if none was being recorded,
    /// so it wasn't kept.
    pub episode: Option<String>,
}

/// An event as it was kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SentEvent {
    /// Its kind.
    pub kind: String,
    /// When, in UTC nanoseconds.
    pub at_utc_ns: i64,
    /// When a whole stretch ended.
    pub until_utc_ns: Option<i64>,
    /// A moment, the beginning or end of a stretch, or a whole one.
    pub phase: EventPhase,
    /// Pairs a beginning with its end.
    pub id: Option<String>,
    /// Where it came from.
    pub source: String,
    /// What someone said about it.
    pub text: Option<String>,
    /// More about it.
    pub attributes: BTreeMap<String, String>,
}

impl From<EventPhase> for Phase {
    fn from(phase: EventPhase) -> Self {
        match phase {
            EventPhase::Moment => Self::Moment,
            EventPhase::Begin => Self::Begin,
            EventPhase::End => Self::End,
            EventPhase::Stretch => Self::Stretch,
        }
    }
}

impl From<Event> for SentEvent {
    fn from(event: Event) -> Self {
        Self {
            kind: event.kind,
            at_utc_ns: event.at_utc_ns,
            until_utc_ns: event.until_utc_ns,
            phase: match event.phase {
                Phase::Moment => EventPhase::Moment,
                Phase::Begin => EventPhase::Begin,
                Phase::End => EventPhase::End,
                Phase::Stretch => EventPhase::Stretch,
            },
            id: event.id,
            source: event.source,
            text: event.text,
            attributes: event.attributes,
        }
    }
}

/// The vocabulary of `config_folder` and its tasks.
fn read(config_folder: &Path) -> Vocabulary {
    load_vocabulary(config_folder, &load_sessions(config_folder).tasks)
}

/// The kinds of events that may be sent with the configuration in
/// `config_folder`.
#[must_use]
pub fn vocabulary(config_folder: &Path) -> EventVocabulary {
    let vocabulary = read(config_folder);
    EventVocabulary {
        kinds: vocabulary
            .kinds
            .into_iter()
            .map(|kind| {
                let (from, task) = match kind.from {
                    KindSource::File => ("file", None),
                    KindSource::Default => ("default", None),
                    KindSource::Task(task) => ("task", Some(task)),
                };
                VocabularyKind {
                    kind: kind.kind,
                    name: kind.name,
                    color: kind.color,
                    stretch: kind.stretch,
                    description: kind.description,
                    from: from.to_owned(),
                    task,
                }
            })
            .collect(),
        problems: vocabulary.problems.iter().map(ApiError::from).collect(),
    }
}

/// Checks `event` before capture is asked to take it: that it is well
/// formed, as capture would, and that its kind is in the vocabulary of
/// `config_folder`.
///
/// # Errors
///
/// `capture.event_invalid` if it isn't well formed; `events.unknown_kind`
/// if its kind isn't one that may be sent.
pub fn check(config_folder: &Path, event: &NewEvent) -> Result<(), KaptoError> {
    event.clone().at(utc_now_ns()).check(false)?;
    let vocabulary = read(config_folder);
    if vocabulary.kind(&event.kind).is_some() {
        return Ok(());
    }
    let known: Vec<&str> = vocabulary
        .kinds
        .iter()
        .map(|kind| kind.kind.as_str())
        .collect();
    let defaults = vocabulary
        .problems
        .first()
        .map_or_else(String::new, |problem| {
            format!(
                " (the defaults, while {EVENTS_FILE} has problems: {})",
                problem.message()
            )
        });
    let detail = format!(
        "`{}`; the kinds are {}{defaults}",
        event.kind,
        known.join(", ")
    );
    Err(KaptoError::new(ErrorCode::EventsUnknownKind)
        .detail(detail)
        .context("folder", config_folder.display().to_string()))
}
