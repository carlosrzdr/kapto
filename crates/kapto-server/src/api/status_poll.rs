// SPDX-License-Identifier: Apache-2.0

//! The capture process's status, asked for by one thread on one
//! connection at a fixed rate and read by every request and event stream
//! that wants it. Capture serves each control connection on a thread of
//! its own, so a connection per request would cost it a thread and a
//! connection for every page open; this way it answers the same few
//! requests a second however many pages are open.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::control::{self, ANSWER_TIMEOUT, ControlClient, Request, Response};
use kapto_core::{ErrorCode, KaptoError};
use tokio::sync::watch;

/// How often capture is asked for its status while it answers: the event
/// streams send what changes this often.
pub(super) const POLL: Duration = Duration::from_millis(250);

/// How long to wait before connecting to capture again, after it couldn't
/// be reached or the connection failed.
const CONNECT_AGAIN: Duration = Duration::from_secs(1);

/// The capture process's latest status, shared by everything in the API
/// that reads it. Asking begins with the first read, on a thread of its
/// own (`srv-status`), and ends once the last copy of this is dropped.
#[derive(Clone, Debug, Default)]
pub struct StatusPoller(Arc<Shared>);

#[derive(Debug, Default)]
struct Shared {
    /// The thread asking, once a read started it.
    polling: Mutex<Option<Polling>>,
    /// An answer must have been asked for after this: when an order last
    /// changed what capture does, so whoever gave it reads its effect.
    after: Mutex<Option<Instant>>,
}

#[derive(Debug)]
struct Polling {
    latest: watch::Receiver<Option<Polled>>,
    /// Asks again at once. Dropped with the poller, which ends the thread.
    wake: mpsc::Sender<()>,
}

/// One answer, or why there was none, and when it was asked for.
#[derive(Debug)]
struct Polled {
    asked: Instant,
    answer: Result<control::Status, KaptoError>,
}

impl Polled {
    /// Whether a read made at `read`, which wants an answer asked for
    /// after `after`, may be given this one: asked for within the last
    /// poll, or the last attempt to connect while capture can't be reached.
    fn fresh(&self, read: Instant, after: Option<Instant>) -> bool {
        let oldest = if self.answer.is_ok() {
            POLL
        } else {
            CONNECT_AGAIN + POLL
        };
        after.is_none_or(|after| self.asked >= after)
            && read.saturating_duration_since(self.asked) <= oldest
    }

    fn answer(&self) -> Result<control::Status, KaptoError> {
        self.answer.as_ref().map(Clone::clone).map_err(copied)
    }
}

impl StatusPoller {
    /// Capture's status, asked for on `socket` within the last poll, or
    /// why it couldn't be: waits for the next answer if the latest is
    /// older, as long as capture may take to answer.
    ///
    /// # Errors
    ///
    /// `capture.control_unreachable` if capture can't be reached or doesn't
    /// answer, or whatever it refused the request with.
    pub async fn latest(&self, socket: &Path) -> Result<control::Status, KaptoError> {
        let read = Instant::now();
        let mut latest = self.start(socket)?;
        let after = *lock(&self.0.after);
        let fresh = |polled: &Option<Polled>| polled.as_ref().is_some_and(|p| p.fresh(read, after));
        match tokio::time::timeout(ANSWER_TIMEOUT, latest.wait_for(fresh)).await {
            Ok(Ok(polled)) => polled
                .as_ref()
                .map_or_else(|| Err(stopped()), Polled::answer),
            Ok(Err(_)) => Err(stopped()),
            Err(_) => Err(KaptoError::new(ErrorCode::CaptureControlUnreachable)
                .detail(format!("no answer in {} s", ANSWER_TIMEOUT.as_secs()))),
        }
    }

    /// Makes the next reads wait for an answer asked for from now on, and
    /// asks at once: an order may just have changed what capture does.
    pub fn refresh(&self) {
        *lock(&self.0.after) = Some(Instant::now());
        if let Some(polling) = lock(&self.0.polling).as_ref() {
            // A thread that has ended has nothing left to ask.
            let _ = polling.wake.send(());
        }
    }

    /// The thread's answers, starting it if no read has yet.
    fn start(&self, socket: &Path) -> Result<watch::Receiver<Option<Polled>>, KaptoError> {
        let mut polling = lock(&self.0.polling);
        if let Some(polling) = polling.as_ref() {
            return Ok(polling.latest.clone());
        }
        let (publish, latest) = watch::channel(None);
        let (wake, woken) = mpsc::channel();
        let socket = socket.to_path_buf();
        thread::Builder::new()
            .name("srv-status".into())
            .spawn(move || poll(&socket, &publish, &woken))
            .map_err(|error| {
                KaptoError::new(ErrorCode::CaptureControlUnreachable).detail(format!(
                    "can't start asking capture for its status: {error}"
                ))
            })?;
        *polling = Some(Polling {
            latest: latest.clone(),
            wake,
        });
        Ok(latest)
    }
}

/// Asks capture on `socket` for its status every [`POLL`], on one
/// connection while it lasts, connecting again every [`CONNECT_AGAIN`]
/// while it can't be reached, until the poller is dropped.
fn poll(socket: &Path, publish: &watch::Sender<Option<Polled>>, woken: &mpsc::Receiver<()>) {
    let mut client = None;
    loop {
        let asked = Instant::now();
        let answer = ask(socket, &mut client);
        let again = asked + if answer.is_ok() { POLL } else { CONNECT_AGAIN };
        publish.send_replace(Some(Polled { asked, answer }));
        match woken.recv_timeout(again.saturating_duration_since(Instant::now())) {
            // Several orders since the last answer need only one more.
            Ok(()) => while woken.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Asks on `client`, connecting it first if it isn't; a connection that
/// failed is dropped, to be made again.
fn ask(socket: &Path, client: &mut Option<ControlClient>) -> Result<control::Status, KaptoError> {
    let connected = match client {
        Some(connected) => connected,
        None => client.insert(ControlClient::connect(socket, ANSWER_TIMEOUT)?),
    };
    match connected.ask(&Request::Status) {
        Ok(Response::Status(status)) => Ok(status),
        other => {
            *client = None;
            Err(other.err().unwrap_or_else(|| {
                KaptoError::new(ErrorCode::CaptureControlUnreachable)
                    .detail("capture answered something other than its status")
            }))
        }
    }
}

/// The same failure again, for another read: the code, message and
/// context it is shown with.
fn copied(error: &KaptoError) -> KaptoError {
    let message = error.message();
    let detail = message
        .strip_prefix(error.code().summary())
        .map_or(message.as_str(), |rest| rest.trim_start_matches(": "));
    let mut copy = KaptoError::new(error.code());
    if !detail.is_empty() {
        copy = copy.detail(detail.to_owned());
    }
    for (key, value) in error.context_entries() {
        copy = copy.context(key, value.clone());
    }
    copy
}

fn stopped() -> KaptoError {
    KaptoError::new(ErrorCode::CaptureControlUnreachable)
        .detail("serve stopped asking capture for its status")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
