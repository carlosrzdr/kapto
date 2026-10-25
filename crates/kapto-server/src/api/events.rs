// SPDX-License-Identifier: Apache-2.0

//! `/api/events`: a WebSocket on which the API tells what changes, so a
//! client doesn't have to keep asking.
//!
//! The capture process announces each episode starting and ending as it
//! happens; serve keeps one connection listening ([`CaptureAnnouncements`])
//! and every stream passes them on at once, however short the episode and
//! whoever started it (`kapto episode`, another client). Each stream also
//! asks the capture process for its status four times a second and sends
//! it at least once a second, which is also how an episode is seen if an
//! announcement was missed, e.g. while capture restarted. Browsers don't
//! hold `WebSocket`s to the same-origin rule, so a page from another site
//! is refused by its `Origin` header.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use kapto_capture::control::{Announcement, ControlClient};
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::manifest::FINALISED_FILE;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::extensions::Extensions;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};

use super::status::current_status;
use super::{ApiError, ApiState, Failure, Status};

/// How often the capture process is asked.
const POLL: Duration = Duration::from_millis(250);

/// Status events are sent at least this often, every one if they change.
const STATUS_EVERY: u32 = 4;

/// How long to wait before listening to the capture process again, after
/// it couldn't be reached or the connection ended.
const LISTEN_AGAIN: Duration = Duration::from_secs(1);

/// What the capture process announces, passed on to every event stream.
/// Lossy: a stream that falls behind misses announcements, and its status
/// polling still sees the episodes.
#[derive(Clone, Debug)]
pub struct CaptureAnnouncements(broadcast::Sender<Announcement>);

impl Default for CaptureAnnouncements {
    fn default() -> Self {
        Self(broadcast::channel(64).0)
    }
}

impl CaptureAnnouncements {
    /// Listens to the capture process on `socket` while `running` is set,
    /// passing on what it announces, and listens again whenever it was
    /// restarted or couldn't be reached. Blocks: run it on a thread.
    pub fn relay_while(&self, socket: &Path, running: &AtomicBool) {
        while running.load(Ordering::Relaxed) {
            let watching =
                ControlClient::connect(socket, LISTEN_AGAIN).and_then(ControlClient::watch);
            if let Ok(announcements) = watching {
                for announcement in announcements {
                    // Nobody listening is fine: nothing to tell.
                    let _ = self.0.send(announcement);
                }
            }
            let again = Instant::now() + LISTEN_AGAIN;
            while running.load(Ordering::Relaxed) && Instant::now() < again {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// Passes on `announcement` as if the capture process had made it.
    pub fn announce(&self, announcement: Announcement) {
        let _ = self.0.send(announcement);
    }

    /// What is announced from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Announcement> {
        self.0.subscribe()
    }
}

/// Something that happened, sent as one JSON text message.
#[derive(Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Event {
    /// What happened.
    pub event: EventKind,
    /// For `status`: what Kapto is doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// For `episode_started`, `episode_ended` and `episode_recovered`: the
    /// episode's ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<String>,
    /// For `episode_ended`: whether it was finalised; `false` if it was
    /// aborted or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalised: Option<bool>,
    /// For `capture_unreachable`: why, and what to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// The kinds of event. More may be added within an API version, so the
/// document lists them as `x-extensible-enum`: a client ignores an event of
/// a kind it doesn't know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// What Kapto is doing, as `/api/status` answers.
    Status,
    /// An episode started being recorded.
    EpisodeStarted,
    /// The episode being recorded ended.
    EpisodeEnded,
    /// An episode that recording left unfinished, by a crash, a kill or a
    /// power cut, was recovered when capture started, and is listed.
    EpisodeRecovered,
    /// The capture process stopped answering; sent once, until it answers
    /// again.
    CaptureUnreachable,
}

impl EventKind {
    /// Every kind, as the API document lists them.
    const ALL: [Self; 5] = [
        Self::Status,
        Self::EpisodeStarted,
        Self::EpisodeEnded,
        Self::EpisodeRecovered,
        Self::CaptureUnreachable,
    ];
}

impl utoipa::PartialSchema for EventKind {
    fn schema() -> RefOr<Schema> {
        let kinds: Vec<serde_json::Value> = Self::ALL
            .iter()
            .map(|kind| serde_json::to_value(kind).unwrap_or_default())
            .collect();
        ObjectBuilder::new()
            .schema_type(Type::String)
            .description(Some(
                "The kinds of event. More may be added: ignore an event of a kind you don't know.",
            ))
            .extensions(Some(
                Extensions::builder()
                    .add("x-extensible-enum", kinds)
                    .build(),
            ))
            .into()
    }
}

impl ToSchema for EventKind {}

impl Event {
    fn new(event: EventKind) -> Self {
        Self {
            event,
            status: None,
            episode: None,
            finalised: None,
            error: None,
        }
    }
}

/// Opens the stream, unless the request comes from another site's page.
pub(super) async fn stream(State(state): State<Arc<ApiState>>, request: Request) -> Response {
    let (mut parts, _) = request.into_parts();
    if let Some(origin) = parts.headers.get(header::ORIGIN) {
        let origin = String::from_utf8_lossy(origin.as_bytes()).into_owned();
        if !same_site(&origin, &parts.headers) {
            return Failure::from(KaptoError::new(ErrorCode::ApiOriginNotAllowed).detail(origin))
                .into_response();
        }
    }
    match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(upgrade) => upgrade.on_upgrade(move |socket| send(socket, state)),
        Err(rejection) => rejection.into_response(),
    }
}

/// Whether a page at `origin` was served by this API: same host and port.
fn same_site(origin: &str, headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
    else {
        return false;
    };
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|origin| origin.eq_ignore_ascii_case(host))
}

/// Sends events until the client goes away.
async fn send(mut socket: WebSocket, state: Arc<ApiState>) {
    let mut seen = Seen::default();
    let mut poll = tokio::time::interval(POLL);
    let mut announced = state.announcements.subscribe();
    let finalised = |episode: &str| state.data_dir.join(episode).join(FINALISED_FILE).exists();
    loop {
        let events = tokio::select! {
            _ = poll.tick() => {
                let status = current_status(&state).await.map_err(|Failure(_, error)| error);
                seen.update(status, &finalised)
            }
            announcement = announced.recv() => match announcement {
                Ok(announcement) => seen.announce(announcement),
                // Missed some while behind, or serve is ending: the status
                // polling still sees the episodes.
                Err(_) => Vec::new(),
            },
            message = socket.recv() => match message {
                // Pings are answered by the socket itself; nothing else
                // from the client means anything.
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => Vec::new(),
            },
        };
        for event in events {
            let Ok(text) = serde_json::to_string(&event) else {
                continue;
            };
            if socket.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }
    }
}

/// What a connection has told its client so far.
#[derive(Debug, Default)]
struct Seen {
    /// The episode it last said started and hasn't said ended.
    episode: Option<String>,
    /// An episode it said started that capture is closing: its end is told
    /// once capture announces it, or polling sees it closed.
    closing: Option<String>,
    /// The episode it last said ended, so a status answered just before
    /// the end, or an announcement of what polling already saw, doesn't
    /// tell it again.
    ended: Option<String>,
    /// Whether the last poll had no answer.
    unreachable: bool,
    /// Polls since the last status event.
    since_status: Option<u32>,
}

impl Seen {
    /// The events that tell what changed since the last poll.
    /// `finalised` tells whether an episode has been finalised.
    fn update(
        &mut self,
        status: Result<Status, KaptoError>,
        finalised: &dyn Fn(&str) -> bool,
    ) -> Vec<Event> {
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                if self.unreachable {
                    return Vec::new();
                }
                self.unreachable = true;
                self.since_status = None;
                return vec![Event {
                    error: Some(ApiError::from(&error)),
                    ..Event::new(EventKind::CaptureUnreachable)
                }];
            }
        };
        self.unreachable = false;
        let mut events = Vec::new();
        // A status asked for just before an announced end still names it.
        let running = status
            .capture
            .episode
            .clone()
            .filter(|episode| self.ended.as_ref() != Some(episode));
        let closing = status.capture.closing.clone();
        // Closed since: its end announced, or missed.
        if let Some(closed) = self.closing.clone()
            && closing.as_ref() != Some(&closed)
        {
            events.push(self.end(closed, finalised));
        }
        if running != self.episode {
            if let Some(ended) = self.episode.clone() {
                if closing.as_ref() == Some(&ended) {
                    // Not ended until it is finalised.
                    self.closing = self.episode.take();
                } else {
                    events.push(self.end(ended, finalised));
                }
            }
            if let Some(started) = running {
                events.push(self.start(started));
            }
        }
        let due = self
            .since_status
            .is_none_or(|polls| polls + 1 >= STATUS_EVERY);
        if due || !events.is_empty() {
            self.since_status = Some(0);
            events.push(Event {
                status: Some(status),
                ..Event::new(EventKind::Status)
            });
        } else {
            self.since_status = self.since_status.map(|polls| polls + 1);
        }
        events
    }

    /// The events that tell what the capture process announced, unless
    /// polling already told it.
    fn announce(&mut self, announcement: Announcement) -> Vec<Event> {
        let mut events = Vec::new();
        match announcement {
            Announcement::EpisodeStarted { episode } => {
                if self.episode.as_ref() != Some(&episode) && self.ended.as_ref() != Some(&episode)
                {
                    // The one before is still being closed, or its end was
                    // missed: told once announced, or once polling sees it
                    // closed.
                    if let Some(earlier) = self.episode.take() {
                        self.closing = Some(earlier);
                    }
                    events.push(self.start(episode));
                }
            }
            Announcement::EpisodeEnded { episode, finalised } => {
                if self.ended.as_ref() != Some(&episode) {
                    // Too short for polling to see it start.
                    if self.episode.as_ref() != Some(&episode)
                        && self.closing.as_ref() != Some(&episode)
                    {
                        events.push(self.start(episode.clone()));
                    }
                    events.push(self.end(episode, &|_| finalised));
                }
            }
            // Recovered when capture started: none started or ended now.
            Announcement::EpisodeRecovered { episode } => events.push(Event {
                episode: Some(episode),
                ..Event::new(EventKind::EpisodeRecovered)
            }),
            // What happened reaches the page on the viewer bridge's
            // timeline, with the stretch it belongs to, not as an API event.
            Announcement::EventNoted { .. } => {}
        }
        events
    }

    fn start(&mut self, episode: String) -> Event {
        self.episode = Some(episode.clone());
        Event {
            episode: Some(episode),
            ..Event::new(EventKind::EpisodeStarted)
        }
    }

    fn end(&mut self, episode: String, finalised: &dyn Fn(&str) -> bool) -> Event {
        if self.episode.as_ref() == Some(&episode) {
            self.episode = None;
        }
        if self.closing.as_ref() == Some(&episode) {
            self.closing = None;
        }
        self.ended = Some(episode.clone());
        Event {
            finalised: Some(finalised(&episode)),
            episode: Some(episode),
            ..Event::new(EventKind::EpisodeEnded)
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::super::{CaptureStatus, Viewer};
    use super::*;

    fn status(episode: Option<&str>) -> Status {
        Status {
            capture: CaptureStatus {
                episode: episode.map(str::to_owned),
                seconds: 0,
                streams: Vec::new(),
                values: Vec::new(),
                preflight: None,
                closing: None,
            },
            viewer: Viewer { stream_port: 9877 },
        }
    }

    fn kinds(events: &[Event]) -> Vec<EventKind> {
        events.iter().map(|event| event.event).collect()
    }

    #[test]
    fn the_status_is_sent_at_once_then_once_a_second() {
        let mut seen = Seen::default();
        let never = |_: &str| false;
        assert_eq!(
            kinds(&seen.update(Ok(status(None)), &never)),
            [EventKind::Status]
        );
        for _ in 1..STATUS_EVERY {
            assert_eq!(seen.update(Ok(status(None)), &never), []);
        }
        assert_eq!(
            kinds(&seen.update(Ok(status(None)), &never)),
            [EventKind::Status]
        );
    }

    #[test]
    fn an_episode_starting_and_ending_is_told_at_once_with_whether_it_was_finalised() {
        let mut seen = Seen::default();
        let finalised = |episode: &str| episode == "kept";
        seen.update(Ok(status(None)), &finalised);

        let started = seen.update(Ok(status(Some("kept"))), &finalised);
        assert_eq!(
            kinds(&started),
            [EventKind::EpisodeStarted, EventKind::Status]
        );
        assert_eq!(started[0].episode.as_deref(), Some("kept"));

        let ended = seen.update(Ok(status(None)), &finalised);
        assert_eq!(kinds(&ended), [EventKind::EpisodeEnded, EventKind::Status]);
        assert_eq!(ended[0].finalised, Some(true));

        seen.update(Ok(status(Some("aborted"))), &finalised);
        // Stopped and the next one started between two polls.
        let next = seen.update(Ok(status(Some("next"))), &finalised);
        assert_eq!(
            kinds(&next),
            [
                EventKind::EpisodeEnded,
                EventKind::EpisodeStarted,
                EventKind::Status
            ]
        );
        assert_eq!(next[0].finalised, Some(false));
    }

    #[test]
    fn capture_not_answering_is_told_once_with_its_cause_until_it_answers_again() {
        let mut seen = Seen::default();
        let never = |_: &str| false;
        let gone = || Err(KaptoError::new(ErrorCode::CaptureControlUnreachable));
        let events = seen.update(gone(), &never);
        assert_eq!(kinds(&events), [EventKind::CaptureUnreachable]);
        assert_eq!(
            events[0].error.as_ref().map(|error| error.code.as_str()),
            Some("capture.control_unreachable")
        );
        assert_eq!(seen.update(gone(), &never), []);
        assert_eq!(
            kinds(&seen.update(Ok(status(None)), &never)),
            [EventKind::Status]
        );
    }

    fn started(episode: &str) -> Announcement {
        Announcement::EpisodeStarted {
            episode: episode.into(),
        }
    }

    fn ended(episode: &str, finalised: bool) -> Announcement {
        Announcement::EpisodeEnded {
            episode: episode.into(),
            finalised,
        }
    }

    #[test]
    fn an_episode_too_short_for_polling_is_told_from_what_capture_announces() {
        let mut seen = Seen::default();
        let never = |_: &str| false;
        seen.update(Ok(status(None)), &never);

        // Started and ended between two polls.
        assert_eq!(
            kinds(&seen.announce(started("short"))),
            [EventKind::EpisodeStarted]
        );
        let end = seen.announce(ended("short", true));
        assert_eq!(kinds(&end), [EventKind::EpisodeEnded]);
        assert_eq!(end[0].finalised, Some(true));
        // The next poll sees nothing new.
        for _ in 1..STATUS_EVERY {
            assert_eq!(seen.update(Ok(status(None)), &never), []);
        }

        // An end whose start was missed is told with its start.
        assert_eq!(
            kinds(&seen.announce(ended("missed", false))),
            [EventKind::EpisodeStarted, EventKind::EpisodeEnded]
        );
    }

    /// `episode` recorded and `closing` being closed.
    fn closing(episode: Option<&str>, closing: &str) -> Status {
        let mut status = status(episode);
        status.capture.closing = Some(closing.to_owned());
        status
    }

    #[test]
    fn an_episode_being_closed_is_told_ended_once_finalised_not_when_recording_stops() {
        let mut seen = Seen::default();
        let finalised = |episode: &str| episode == "e1";
        seen.update(Ok(status(Some("e1"))), &finalised);
        // Recorded, and being closed: not ended yet.
        let stopped = seen.update(Ok(closing(None, "e1")), &finalised);
        assert!(
            !kinds(&stopped).contains(&EventKind::EpisodeEnded),
            "{stopped:?}"
        );
        // Finalised, as capture announces.
        let end = seen.announce(ended("e1", true));
        assert_eq!(kinds(&end), [EventKind::EpisodeEnded]);
        assert_eq!(end[0].finalised, Some(true));
        // Not told again once polling sees it closed.
        for _ in 0..STATUS_EVERY {
            let polled = seen.update(Ok(status(None)), &finalised);
            assert!(
                !kinds(&polled).contains(&EventKind::EpisodeEnded),
                "{polled:?}"
            );
        }
    }

    #[test]
    fn the_next_episode_starting_while_one_is_closed_ends_neither_early() {
        let mut seen = Seen::default();
        let never = |_: &str| false;
        seen.update(Ok(status(Some("e1"))), &never);
        // Stopped, and the next started before a poll saw it closing.
        let next = seen.announce(started("e2"));
        assert_eq!(kinds(&next), [EventKind::EpisodeStarted]);
        assert_eq!(next[0].episode.as_deref(), Some("e2"));
        let polled = seen.update(Ok(closing(Some("e2"), "e1")), &never);
        assert!(
            !kinds(&polled).contains(&EventKind::EpisodeEnded),
            "{polled:?}"
        );
        // The first's end, once finalised: not started again.
        let end = seen.announce(ended("e1", true));
        assert_eq!(kinds(&end), [EventKind::EpisodeEnded]);
        assert_eq!(end[0].episode.as_deref(), Some("e1"));
        let polled = seen.update(Ok(status(Some("e2"))), &never);
        assert!(
            !kinds(&polled).contains(&EventKind::EpisodeEnded),
            "{polled:?}"
        );
    }

    #[test]
    fn a_closed_episode_whose_end_was_missed_is_told_from_polling() {
        let mut seen = Seen::default();
        let finalised = |episode: &str| episode == "e1";
        seen.update(Ok(status(Some("e1"))), &finalised);
        seen.update(Ok(closing(None, "e1")), &finalised);
        // Its announcement missed: polling sees it closed.
        let end = seen.update(Ok(status(None)), &finalised);
        assert_eq!(kinds(&end), [EventKind::EpisodeEnded, EventKind::Status]);
        assert_eq!(end[0].finalised, Some(true));
    }

    #[test]
    fn an_episode_recovered_is_told_as_such_never_as_one_starting_or_ending() {
        let mut seen = Seen::default();
        let never = |_: &str| false;
        seen.update(Ok(status(Some("e1"))), &never);
        let recovered = seen.announce(Announcement::EpisodeRecovered {
            episode: "r1".into(),
        });
        assert_eq!(kinds(&recovered), [EventKind::EpisodeRecovered]);
        assert_eq!(recovered[0].episode.as_deref(), Some("r1"));
        // The episode being recorded is still the one being recorded.
        assert_eq!(seen.announce(started("e1")), []);
    }

    #[test]
    fn what_polling_and_announcements_both_see_is_told_once() {
        let mut seen = Seen::default();
        let finalised = |_: &str| true;
        seen.update(Ok(status(None)), &finalised);

        // Polling first, then the announcement of the same start.
        assert_eq!(
            kinds(&seen.update(Ok(status(Some("e1"))), &finalised)),
            [EventKind::EpisodeStarted, EventKind::Status]
        );
        assert_eq!(seen.announce(started("e1")), []);
        // Announced end first; a status asked just before the end still
        // names the episode, and must not start it again.
        assert_eq!(
            kinds(&seen.announce(ended("e1", true))),
            [EventKind::EpisodeEnded]
        );
        let stale = seen.update(Ok(status(Some("e1"))), &finalised);
        assert!(
            !kinds(&stale).contains(&EventKind::EpisodeStarted),
            "{stale:?}"
        );
        let after = seen.update(Ok(status(None)), &finalised);
        assert!(
            !kinds(&after).contains(&EventKind::EpisodeEnded),
            "{after:?}"
        );
        // Polling sees an end first; its announcement says nothing more.
        seen.update(Ok(status(Some("e2"))), &finalised);
        assert_eq!(
            kinds(&seen.update(Ok(status(None)), &finalised)),
            [EventKind::EpisodeEnded, EventKind::Status]
        );
        assert_eq!(seen.announce(ended("e2", true)), []);
    }

    #[test]
    fn only_pages_served_by_this_api_may_open_the_stream() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:7700"));
        assert!(same_site("http://localhost:7700", &headers));
        assert!(same_site("https://LOCALHOST:7700", &headers));
        assert!(!same_site("http://localhost:5173", &headers));
        assert!(!same_site("http://attacker.example", &headers));
        assert!(!same_site("null", &headers));
        assert!(!same_site("http://localhost:7700", &HeaderMap::new()));
    }

    #[test]
    fn an_event_carries_only_the_fields_of_its_kind() {
        let event = Event {
            episode: Some("e".into()),
            ..Event::new(EventKind::EpisodeStarted)
        };
        assert_eq!(
            serde_json::to_string(&event).ok().as_deref(),
            Some(r#"{"event":"episode_started","episode":"e"}"#)
        );
    }
}

/// Events as they happen, over a WebSocket. Each message is one `Event` as
/// JSON. Connections from another site's web page are refused.
#[utoipa::path(
    get,
    path = "/api/events",
    responses(
        (status = 101, description = "A WebSocket, on which each text message is one Event as JSON", body = Event),
        (status = 403, description = "The request came from another site's page", body = ApiError)
    )
)]
pub(super) async fn events(state: State<Arc<ApiState>>, request: Request) -> Response {
    stream(state, request).await
}
