// SPDX-License-Identifier: Apache-2.0

//! What happened, for the timeline of the live view: every event the
//! capture process takes, recorded or not, and each episode's start and
//! end. The bridge watches capture's control socket for them, keeps them as
//! long as its history, and tells each page all it keeps when it connects,
//! then each new one as it comes. Specified in
//! `docs/reference/viewer-stream.md`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use kapto_capture::control::{Announcement, ControlClient};
use kapto_core::clock::utc_now_ns;
use kapto_store::Event;
use serde::Serialize;

/// The most marks kept, whatever their age.
pub const MAX_MARKS: usize = 10_000;
/// How long to wait before watching capture again once it stopped
/// answering.
const RECONNECT: Duration = Duration::from_secs(1);
/// How long capture may take to answer when watching starts.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// One thing on the timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    /// An event capture took.
    Noted {
        /// The event as kept.
        event: Event,
        /// The episode it was recorded in; `None` between episodes.
        episode: Option<String>,
    },
    /// An episode started being recorded.
    EpisodeStarted {
        /// Its ID.
        episode: String,
        /// When the bridge heard of it, UTC nanoseconds.
        at_utc_ns: i64,
    },
    /// The episode being recorded ended.
    EpisodeEnded {
        /// Its ID.
        episode: String,
        /// When the bridge heard of it, UTC nanoseconds.
        at_utc_ns: i64,
        /// Whether it was finalised; `false` if aborted or failed.
        finalised: bool,
    },
}

impl Mark {
    /// What capture announced, as a mark heard of at `now_utc_ns`; `None`
    /// for what the timeline doesn't show.
    #[must_use]
    pub fn of(announcement: Announcement, now_utc_ns: i64) -> Option<Self> {
        match announcement {
            Announcement::EventNoted { happened, episode } => Some(Self::Noted {
                event: *happened,
                episode,
            }),
            Announcement::EpisodeStarted { episode } => Some(Self::EpisodeStarted {
                episode,
                at_utc_ns: now_utc_ns,
            }),
            Announcement::EpisodeEnded { episode, finalised } => Some(Self::EpisodeEnded {
                episode,
                at_utc_ns: now_utc_ns,
                finalised,
            }),
            Announcement::EpisodeRecovered { .. } => None,
        }
    }

    /// When it ended: an event's end if it is a whole stretch, else its
    /// moment.
    fn latest_utc_ns(&self) -> i64 {
        match self {
            Self::Noted { event, .. } => event.until_utc_ns.unwrap_or(event.at_utc_ns),
            Self::EpisodeStarted { at_utc_ns, .. } | Self::EpisodeEnded { at_utc_ns, .. } => {
                *at_utc_ns
            }
        }
    }
}

/// What a page is told of marks, as text: all those kept when it connects,
/// then each new one.
#[derive(Debug, Serialize)]
pub struct MarksMessage<'a> {
    /// The marks, oldest heard of first.
    pub marks: &'a [Mark],
}

/// The marks kept, oldest heard of first.
#[derive(Debug)]
pub struct Marks {
    kept: VecDeque<Mark>,
    age_ns: i64,
}

impl Marks {
    /// Keeps marks for `age_ns` nanoseconds behind the newest, at most
    /// [`MAX_MARKS`].
    #[must_use]
    pub fn new(age_ns: i64) -> Self {
        Self {
            kept: VecDeque::new(),
            age_ns,
        }
    }

    /// Adds `mark`, then lets go of what is too old or too many.
    pub fn push(&mut self, mark: Mark) {
        let newest = mark.latest_utc_ns();
        self.kept.push_back(mark);
        while self.kept.len() > MAX_MARKS
            || self
                .kept
                .front()
                .is_some_and(|oldest| newest.saturating_sub(oldest.latest_utc_ns()) > self.age_ns)
        {
            self.kept.pop_front();
        }
    }

    /// Every mark kept, oldest heard of first.
    #[must_use]
    pub fn all(&self) -> Vec<Mark> {
        self.kept.iter().cloned().collect()
    }
}

/// The text telling a page `marks`.
#[must_use]
pub fn message(marks: &[Mark]) -> String {
    serde_json::to_string(&MarksMessage { marks }).unwrap_or_default()
}

/// Watches the capture process on `socket` until `stop` is set, on a
/// thread of its own, handing `mark` each mark as capture announces it.
/// While capture doesn't answer it tries again every second, so it picks up
/// a capture process started, or restarted, later.
///
/// # Errors
///
/// The thread couldn't be started.
pub fn watch_capture(
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    mark: impl Fn(Mark) + Send + 'static,
) -> std::io::Result<()> {
    thread::Builder::new()
        .name("viz-marks".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(announcements) =
                    ControlClient::connect(&socket, ANSWER_TIMEOUT).and_then(ControlClient::watch)
                {
                    // Ends when capture does.
                    for announcement in announcements {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        if let Some(heard) = Mark::of(announcement, utc_now_ns()) {
                            mark(heard);
                        }
                    }
                }
                thread::sleep(RECONNECT);
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kapto_store::Phase;

    use super::*;

    fn note(at_utc_ns: i64) -> Mark {
        Mark::Noted {
            event: Event {
                kind: "note".to_owned(),
                at_utc_ns,
                until_utc_ns: None,
                phase: Phase::Moment,
                id: None,
                source: "api".to_owned(),
                text: None,
                attributes: BTreeMap::new(),
            },
            episode: None,
        }
    }

    #[test]
    fn marks_older_than_the_age_behind_the_newest_go_and_never_more_than_the_most() {
        let mut marks = Marks::new(10);
        for at in 0..30 {
            marks.push(note(at));
        }
        assert_eq!(marks.all().first(), Some(&note(19)));
        let mut many = Marks::new(i64::MAX);
        for at in 0..i64::try_from(MAX_MARKS).unwrap() + 5 {
            many.push(note(at));
        }
        assert_eq!(many.all().len(), MAX_MARKS);
        assert_eq!(many.all().first(), Some(&note(5)));
    }

    #[test]
    fn a_page_is_told_events_and_episodes_as_json_with_what_happened_named() {
        let started = Mark::of(
            Announcement::EpisodeStarted {
                episode: "e1".to_owned(),
            },
            7,
        )
        .unwrap();
        assert_eq!(
            message(&[started, note(9)]),
            r#"{"marks":[{"episode_started":{"episode":"e1","at_utc_ns":7}},{"noted":{"event":{"kind":"note","at_utc_ns":9,"phase":"moment","source":"api"},"episode":null}}]}"#
        );
        assert_eq!(
            Mark::of(
                Announcement::EpisodeRecovered {
                    episode: "e0".to_owned()
                },
                1
            ),
            None,
            "recovered before anyone watched: not on the timeline"
        );
    }
}
