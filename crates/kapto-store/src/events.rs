// SPDX-License-Identifier: Apache-2.0

//! Events: what happened at a moment, or over a stretch, while the robot
//! was recorded. An episode started or ended, a stream lost messages, an
//! action began or ended, an operator noted something. Each one is kept in
//! the recording, on a channel of Kapto's own, at its moment on the same
//! clock as the messages, so selection, labelling and exports can find
//! episodes and stretches by what happened in them.
//!
//! An event's shape is an output format: fields are only ever added.

use std::collections::BTreeMap;

use kapto_core::name::is_dotted_name;
use kapto_core::{ErrorCode, KaptoError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The recording's channel of events. Stream IDs never hold a dot, so no
/// stream can be named like it.
pub const EVENTS_CHANNEL: &str = "kapto.events";

/// The prefix of the kinds Kapto gives its own events; no one else may.
pub const KAPTO_KINDS: &str = "kapto.";

/// The source of Kapto's own events; no one else may give it.
pub const KAPTO_SOURCE: &str = "kapto";

/// The longest kind, source or ID.
const MAX_NAME: usize = 64;
/// The longest text.
const MAX_TEXT: usize = 1000;
/// The most attributes, and the longest name and value of one.
const MAX_ATTRIBUTES: usize = 32;
const MAX_VALUE: usize = 256;

/// Something that happened at a moment, or the beginning or end of a
/// stretch in which something happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// What happened: dotted lowercase words, e.g. `grasp` or
    /// `kapto.stream_lost`. Kinds beginning `kapto.` are Kapto's own.
    pub kind: String,
    /// When, in UTC nanoseconds, on the same clock as the messages'
    /// receive times; for a whole stretch, when it began.
    pub at_utc_ns: i64,
    /// When a whole stretch ended, on the same clock; only for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_utc_ns: Option<i64>,
    /// Whether it is a moment, the beginning or end of a stretch, or a
    /// whole stretch.
    #[serde(default)]
    pub phase: Phase,
    /// Pairs a beginning with its end: the same kind and ID. Required for
    /// both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Where it came from: `kapto`, `page`, `api`, `cli`, or `robot:<stream>`.
    pub source: String,
    /// What someone said about it, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// More about it, as names and texts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

/// Whether an event is a moment or one end of a stretch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// It happened at that moment.
    #[default]
    Moment,
    /// A stretch began.
    Begin,
    /// A stretch ended.
    End,
    /// A whole stretch, from `at_utc_ns` to `until_utc_ns`, told at once,
    /// e.g. when it is marked afterwards.
    Stretch,
}

impl Event {
    /// Checks that it can be kept: a well-formed kind, an ID for a
    /// beginning or an end, and texts within their lengths. `own` allows the
    /// kinds Kapto keeps for itself.
    ///
    /// # Errors
    ///
    /// `capture.event_invalid`, saying what is wrong.
    pub fn check(&self, own: bool) -> Result<(), KaptoError> {
        let invalid = |detail: String| {
            KaptoError::new(ErrorCode::CaptureEventInvalid)
                .detail(detail)
                .context("kind", self.kind.clone())
        };
        if !is_dotted_name(&self.kind) {
            return Err(invalid(format!(
                "the kind `{}` isn't lowercase words joined by dots, at most {MAX_NAME} characters",
                self.kind
            )));
        }
        if !own && self.kind.starts_with(KAPTO_KINDS) {
            return Err(invalid(format!(
                "kinds beginning `{KAPTO_KINDS}` are Kapto's own"
            )));
        }
        if !own && self.source == KAPTO_SOURCE {
            return Err(invalid(format!(
                "the source `{KAPTO_SOURCE}` is Kapto's own"
            )));
        }
        match (self.phase, self.until_utc_ns) {
            (Phase::Stretch, None) => {
                return Err(invalid(
                    "a whole stretch needs `until_utc_ns`, when it ended".to_owned(),
                ));
            }
            (Phase::Stretch, Some(until)) if until < self.at_utc_ns => {
                return Err(invalid(
                    "a whole stretch can't end before it began".to_owned(),
                ));
            }
            (Phase::Moment | Phase::Begin | Phase::End, Some(_)) => {
                return Err(invalid(
                    "only a whole stretch has `until_utc_ns`".to_owned(),
                ));
            }
            _ => {}
        }
        match (&self.phase, &self.id) {
            (Phase::Begin | Phase::End, None) => {
                return Err(invalid(
                    "a beginning or an end needs an ID pairing it with the other".to_owned(),
                ));
            }
            (_, Some(id)) if id.is_empty() || id.chars().count() > MAX_NAME => {
                return Err(invalid(format!("an ID is 1 to {MAX_NAME} characters")));
            }
            _ => {}
        }
        if self.source.is_empty() || self.source.chars().count() > MAX_NAME {
            return Err(invalid(format!("a source is 1 to {MAX_NAME} characters")));
        }
        if self
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_TEXT)
        {
            return Err(invalid(format!("a text is at most {MAX_TEXT} characters")));
        }
        if self.attributes.len() > MAX_ATTRIBUTES
            || self.attributes.iter().any(|(name, value)| {
                name.is_empty()
                    || name.chars().count() > MAX_NAME
                    || value.chars().count() > MAX_VALUE
            })
        {
            return Err(invalid(format!(
                "at most {MAX_ATTRIBUTES} attributes, each named in 1 to {MAX_NAME} characters with a value of at most {MAX_VALUE}"
            )));
        }
        Ok(())
    }

    /// The event as its channel's messages hold it: JSON.
    #[must_use]
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The event a message of its channel holds; `None` if it isn't one.
    #[must_use]
    pub fn from_json(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }
}

/// The beginnings in `events`, in order, that no later end of the same
/// kind and ID closes: stretches still open when the recording ended,
/// e.g. an intervention whose end was never marked.
#[must_use]
pub fn unended(events: &[Event]) -> Vec<&Event> {
    let mut open: Vec<&Event> = Vec::new();
    for event in events {
        match event.phase {
            Phase::Begin => open.push(event),
            Phase::End => {
                if let Some(at) = open
                    .iter()
                    .rposition(|begun| begun.kind == event.kind && begun.id == event.id)
                {
                    open.remove(at);
                }
            }
            Phase::Moment | Phase::Stretch => {}
        }
    }
    open
}

/// The JSON Schema of an event, as its channel's schema carries it.
#[must_use]
pub fn event_schema() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(Event)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str) -> Event {
        Event {
            kind: kind.to_owned(),
            at_utc_ns: 1_772_700_400_000_000_000,
            until_utc_ns: None,
            phase: Phase::Moment,
            id: None,
            source: "api".to_owned(),
            text: None,
            attributes: BTreeMap::new(),
        }
    }

    #[test]
    fn a_beginning_without_a_later_end_of_its_kind_and_id_is_unended() {
        let at = |kind: &str, phase, id: &str| Event {
            phase,
            id: Some(id.to_owned()),
            ..event(kind)
        };
        let events = [
            at("intervention", Phase::Begin, "i1"),
            at("intervention", Phase::Begin, "i2"),
            at("note", Phase::Moment, "n"),
            // Another kind with the same ID ends nothing of this one.
            at("grasp", Phase::End, "i1"),
            at("intervention", Phase::End, "i1"),
            at("grasp", Phase::Begin, "g1"),
            // An end before its beginning closes nothing.
            at("place", Phase::End, "p1"),
            at("place", Phase::Begin, "p1"),
        ];
        let open: Vec<(&str, Option<&str>)> = unended(&events)
            .into_iter()
            .map(|event| (event.kind.as_str(), event.id.as_deref()))
            .collect();
        assert_eq!(
            open,
            [
                ("intervention", Some("i2")),
                ("grasp", Some("g1")),
                ("place", Some("p1"))
            ]
        );
    }

    #[test]
    fn a_well_formed_event_is_kept_as_json_and_read_back_the_same() {
        let mut grasp = event("subtask.grasp");
        grasp.phase = Phase::Begin;
        grasp.id = Some("g1".to_owned());
        grasp.text = Some("second try".to_owned());
        grasp
            .attributes
            .insert("hand".to_owned(), "left".to_owned());
        assert!(grasp.check(false).is_ok());
        let json = grasp.to_json();
        assert_eq!(
            String::from_utf8(json.clone()).unwrap(),
            r#"{"kind":"subtask.grasp","at_utc_ns":1772700400000000000,"phase":"begin","id":"g1","source":"api","text":"second try","attributes":{"hand":"left"}}"#
        );
        assert_eq!(Event::from_json(&json), Some(grasp));
        // Only what is given is written; a moment is the phase left out.
        let note = Event::from_json(br#"{"kind":"note","at_utc_ns":5,"source":"page"}"#).unwrap();
        assert_eq!(note.phase, Phase::Moment);
        assert_eq!(Event::from_json(b"not an event"), None);
    }

    #[test]
    fn a_malformed_event_is_refused_saying_what_is_wrong() {
        let refused = |event: &Event, own: bool| {
            let error = event.check(own).unwrap_err();
            assert_eq!(error.code(), ErrorCode::CaptureEventInvalid);
            error.message()
        };
        for kind in ["", "Grasp", "grasp-left", "1st", "grasp.", "a.b..c"] {
            assert!(
                refused(&event(kind), false).contains("lowercase words"),
                "{kind}"
            );
        }
        assert!(refused(&event(&"a".repeat(65)), false).contains("at most 64"));
        assert!(refused(&event("kapto.episode_started"), false).contains("Kapto's own"));
        assert!(event("kapto.episode_started").check(true).is_ok());
        let mut posing = event("note");
        posing.source = "kapto".to_owned();
        assert!(refused(&posing, false).contains("Kapto's own"));
        let mut begin = event("grasp");
        begin.phase = Phase::Begin;
        assert!(refused(&begin, false).contains("needs an ID"));
        let mut long = event("note");
        long.text = Some("x".repeat(1001));
        assert!(refused(&long, false).contains("at most 1000"));
        let mut whole = event("intervention");
        whole.phase = Phase::Stretch;
        assert!(refused(&whole, false).contains("needs `until_utc_ns`"));
        whole.until_utc_ns = Some(whole.at_utc_ns - 1);
        assert!(refused(&whole, false).contains("end before it began"));
        whole.until_utc_ns = Some(whole.at_utc_ns + 5_000_000_000);
        assert!(whole.check(false).is_ok(), "no ID needed");
        let mut moment = event("note");
        moment.until_utc_ns = Some(0);
        assert!(refused(&moment, false).contains("only a whole stretch"));
        let mut many = event("note");
        many.attributes = (0..33).map(|n| (format!("a{n}"), String::new())).collect();
        assert!(refused(&many, false).contains("at most 32 attributes"));
    }
}
