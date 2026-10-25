// SPDX-License-Identifier: Apache-2.0

//! `cap-finalise`: closes each episode capture hands it, in turn, while
//! capture receives again and feeds the live view. Closing an episode
//! (the file's index and sync, `episode.json`, the rename, the manifest,
//! which reads every byte back, and `_FINALISED`) grows with what was
//! recorded, seconds for a minute of cameras; on capture's own thread
//! nothing was received meanwhile and the live view stopped.
//!
//! Stop is still answered, and the episode's end announced, only once the
//! episode is finalised, so whoever acts on it finds it complete. Episodes
//! are closed one at a time and their ends told in the order they stopped,
//! an aborted one's too.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use kapto_capture::control::{Announcement, ControlServer, Pending, Response};
use kapto_core::{EpisodeId, ErrorCode, KaptoError};

use crate::station::{Closing, Outcome, thread_failed};

/// What became of an episode once it is no longer recorded.
enum Ended {
    /// Closed: its folder, or why it couldn't be.
    Closed(Outcome<PathBuf>),
    /// Discarded, as asked.
    Aborted,
}

/// An episode stopped, with the Stop or Abort waiting for its end and,
/// once known, its end.
struct Stopped {
    episode: EpisodeId,
    asked: Option<Pending>,
    ended: Option<Ended>,
}

/// The thread closing episodes and the episodes waiting on it.
pub(crate) struct Closer {
    closings: Option<mpsc::Sender<Closing>>,
    closed: mpsc::Receiver<(EpisodeId, Outcome<PathBuf>)>,
    thread: Option<JoinHandle<()>>,
    /// Oldest first, as they stopped.
    stopped: VecDeque<Stopped>,
}

impl Closer {
    /// Starts `cap-finalise`, at the worker's lower priority, so reading an
    /// episode back to hash it never takes a core from receiving.
    ///
    /// # Errors
    ///
    /// The thread couldn't be started.
    pub(crate) fn start() -> Outcome<Self> {
        let (closings, to_close) = mpsc::channel::<Closing>();
        let (done, closed) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("cap-finalise".to_owned())
            .spawn(move || {
                if let Err(error) = crate::worker::lower_priority() {
                    eprintln!("Episodes are closed at capture's priority: {error}");
                }
                for closing in to_close {
                    let episode = closing.episode;
                    if done.send((episode, closing.close())).is_err() {
                        return;
                    }
                }
            })
            .map_err(|error| vec![thread_failed("cap-finalise", &error.to_string())])?;
        Ok(Self {
            closings: Some(closings),
            closed,
            thread: Some(thread),
            stopped: VecDeque::new(),
        })
    }

    /// Hands `closing` over to be closed, `asked` (its Stop, if one was
    /// asked) answered once it is.
    pub(crate) fn close(&mut self, closing: Closing, asked: Option<Pending>) {
        let episode = closing.episode;
        let handed = self
            .closings
            .as_ref()
            .is_some_and(|closings| closings.send(closing).is_ok());
        let ended = (!handed).then(|| {
            Ended::Closed(Err(vec![thread_failed(
                "cap-finalise",
                "it stopped unexpectedly",
            )]))
        });
        self.stopped.push_back(Stopped {
            episode,
            asked,
            ended,
        });
    }

    /// Notes `episode` discarded, its Abort answered in its turn.
    pub(crate) fn aborted(&mut self, episode: EpisodeId, asked: Option<Pending>) {
        self.stopped.push_back(Stopped {
            episode,
            asked,
            ended: Some(Ended::Aborted),
        });
    }

    /// The episode being closed, if one is.
    pub(crate) fn closing(&self) -> Option<String> {
        self.stopped
            .iter()
            .find(|stopped| stopped.ended.is_none())
            .map(|stopped| stopped.episode.to_string())
    }

    /// Tells `control` of each episode whose end is known, in the order
    /// they stopped, and answers what waited for it. Never waits.
    pub(crate) fn answer(&mut self, control: &ControlServer) {
        while let Ok((episode, outcome)) = self.closed.try_recv() {
            if let Some(stopped) = self
                .stopped
                .iter_mut()
                .find(|stopped| stopped.episode == episode && stopped.ended.is_none())
            {
                stopped.ended = Some(Ended::Closed(outcome));
            }
        }
        while self
            .stopped
            .front()
            .is_some_and(|stopped| stopped.ended.is_some())
        {
            if let Some(stopped) = self.stopped.pop_front() {
                tell(control, stopped);
            }
        }
    }

    /// Waits for every episode handed over to be closed, then tells and
    /// answers each, as capture shuts down.
    pub(crate) fn finish(mut self, control: &ControlServer) {
        self.closings = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            eprintln!("cap-finalise stopped unexpectedly");
        }
        self.answer(control);
        // Any left never came back: the thread is gone.
        for mut stopped in self.stopped.drain(..) {
            stopped.ended.get_or_insert_with(|| {
                Ended::Closed(Err(vec![thread_failed(
                    "cap-finalise",
                    "it stopped before closing the episode",
                )]))
            });
            tell(control, stopped);
        }
    }
}

/// Announces `stopped`'s end and answers what asked for it.
fn tell(control: &ControlServer, stopped: Stopped) {
    let episode = stopped.episode.to_string();
    let (finalised, answer) = match stopped.ended {
        Some(Ended::Closed(Ok(folder))) => (
            true,
            Response::Stopped {
                episode: episode.clone(),
                folder: folder.display().to_string(),
            },
        ),
        Some(Ended::Closed(Err(errors))) => {
            for error in &errors {
                eprintln!("{}", error.report());
            }
            let error = errors
                .into_iter()
                .next()
                .unwrap_or_else(|| KaptoError::new(ErrorCode::CaptureWriteFailed));
            (false, Response::error(&error))
        }
        Some(Ended::Aborted) | None => (
            false,
            Response::Aborted {
                episode: episode.clone(),
            },
        ),
    };
    control.announce(Announcement::EpisodeEnded { episode, finalised });
    if let Some(asked) = stopped.asked {
        asked.answer(answer);
    }
}
