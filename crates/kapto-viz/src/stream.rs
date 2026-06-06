// SPDX-License-Identifier: Apache-2.0

//! The live view as a stream of frames: the bridge sends each connected
//! page, over a WebSocket, a frame with the robot's link poses and each
//! stream's plotted extremes as soon as they arrive from the tap. Specified
//! in `docs/reference/viewer-stream.md`.
//!
//! The stream is lossy, like the tap it comes from: each page has a few
//! frames' room, and a frame that doesn't fit is dropped and counted.
//! Nothing here ever waits on a page, so a slow or stalled one costs the
//! bridge only the frames it misses.
//!
//! What was streamed is also kept for the last minutes ([`History`]), and
//! a page can ask for any stretch of it, merged to the points it can show.
//! So are the events and episodes of that time ([`Marks`]), which every
//! page is told as they come.

use std::io::{self, ErrorKind};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use kapto_capture::tap::wire::TapSample;
use kapto_robot::Transform;
use serde::{Deserialize, Serialize};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::{StatusCode, header};
use tungstenite::{Bytes, Message, WebSocket};

use crate::history::{History, MAX_POINTS, Record, SampleRecord, pose};
use crate::marks::{Mark, Marks, message};
use crate::plots::Plots;

/// What a frame starts with.
pub const FRAME_MAGIC: &[u8; 4] = b"KVFR";
/// The version of the stream: its opening message and its frames.
pub const STREAM_VERSION: u16 = 1;
/// Frames a page may have waiting before the next is dropped.
const PAGE_QUEUE: usize = 4;
/// Marks a page may have waiting before the next is dropped: they come a
/// few a second at most, so only a page that stopped reading fills it.
const MARKS_QUEUE: usize = 256;
/// Pages connected at once; more are refused.
const MAX_PAGES: usize = 16;
/// How long writing to a page may take before the page is let go.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the listener checks whether the bridge is stopping.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// How long a page's thread waits for a frame before looking for a
/// question from the page anyway.
const QUESTION_POLL: Duration = Duration::from_millis(20);
/// How long a question may take to arrive whole once it has begun.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// What a page is told first, as text: the stream's version, the order of
/// the poses in each frame and the names of each stream's values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Opening {
    /// The stream's version.
    pub version: u16,
    /// Each link's name, in the order its pose comes in a frame: the
    /// robot description's, as `/api/robot/scene` lists them.
    pub links: Vec<String>,
    /// Each stream, by its number in a frame.
    pub streams: Vec<OpeningStream>,
    /// How far back the bridge keeps what it streamed, in seconds, at most.
    pub history_s: u64,
}

/// One stream as the opening message names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningStream {
    /// Its ID.
    pub name: String,
    /// Each decoded value, in the order a frame carries them.
    pub values: Vec<OpeningValue>,
}

/// One decoded value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningValue {
    /// Its name: the field's, or `field[index]` for an array's.
    pub name: String,
    /// Whether it is plotted; counters and clocks aren't.
    pub plotted: bool,
}

impl OpeningStream {
    /// Each stream `plots` shows, with its values, as an opening names them.
    #[must_use]
    pub fn of(plots: &Plots) -> Vec<Self> {
        plots
            .values()
            .into_iter()
            .map(|(name, values)| Self {
                name: name.to_owned(),
                values: values
                    .into_iter()
                    .map(|(name, plotted)| OpeningValue {
                        name: name.to_owned(),
                        plotted,
                    })
                    .collect(),
            })
            .collect()
    }
}

/// One frame being put together.
#[derive(Debug, Default)]
pub struct Frame {
    record: Record,
}

impl Frame {
    /// Adds every link's pose, in link order.
    pub fn poses(&mut self, poses: &[Transform]) {
        self.record.poses = Some(poses.iter().map(pose).collect());
    }

    /// Adds `sample`'s extremes; `offset_ns` maps its receive times to UTC.
    pub fn sample(&mut self, sample: &TapSample, offset_ns: i64) {
        let utc = |mono: u64| {
            i64::try_from(mono)
                .unwrap_or(i64::MAX)
                .saturating_add(offset_ns)
        };
        let newest_utc_ns = utc(sample.recv_time_ns);
        self.record.samples.push(SampleRecord {
            stream: sample.stream,
            messages: sample.messages,
            first_utc_ns: utc(sample.first_recv_time_ns),
            newest_utc_ns,
            extremes: sample
                .minimums
                .iter()
                .copied()
                .zip(sample.maximums.iter().copied())
                .collect(),
        });
        self.record.received_utc_ns = self.record.received_utc_ns.max(newest_utc_ns);
    }

    /// Whether it holds nothing to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.record.poses.is_none() && self.record.samples.is_empty()
    }

    /// The frame's bytes, and what it showed, to keep. `received_utc_ns` is
    /// when Kapto received the newest message it shows, if later than any
    /// sample's; `sent_utc_ns` is now.
    #[must_use]
    pub fn finish(mut self, received_utc_ns: i64, sent_utc_ns: i64) -> (Bytes, Record) {
        let record = &mut self.record;
        if record.samples.is_empty() {
            record.received_utc_ns = received_utc_ns;
        } else {
            record.received_utc_ns = record.received_utc_ns.max(received_utc_ns);
        }
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes();
        let poses = record.poses.as_deref().unwrap_or_default();
        let mut bytes = Vec::with_capacity(32 + poses.len() * 28 + record.samples.len() * 64);
        bytes.extend(FRAME_MAGIC);
        bytes.extend(STREAM_VERSION.to_le_bytes());
        bytes.extend(u16::from(!poses.is_empty()).to_le_bytes());
        bytes.extend(record.received_utc_ns.to_le_bytes());
        bytes.extend(sent_utc_ns.to_le_bytes());
        bytes.extend(count(poses.len()));
        bytes.extend(count(record.samples.len()));
        for value in poses.iter().flatten() {
            bytes.extend(value.to_le_bytes());
        }
        for sample in &record.samples {
            bytes.extend(sample.stream.to_le_bytes());
            bytes.extend(0_u16.to_le_bytes());
            bytes.extend(sample.messages.to_le_bytes());
            bytes.extend(count(sample.extremes.len()));
            bytes.extend(sample.first_utc_ns.to_le_bytes());
            bytes.extend(sample.newest_utc_ns.to_le_bytes());
            for (minimum, maximum) in &sample.extremes {
                bytes.extend(minimum.to_le_bytes());
                bytes.extend(maximum.to_le_bytes());
            }
        }
        (Bytes::from(bytes), self.record)
    }
}

/// How the stream has fared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// Pages connected now.
    pub pages: usize,
    /// Frames handed to a page.
    pub sent: u64,
    /// Frames a page had no room for, and missed.
    pub dropped: u64,
    /// Connections refused: from another site's page, or one too many.
    pub refused: u64,
    /// Marks a page had no room for, and missed.
    pub marks_dropped: u64,
    /// What the history holds, roughly, in bytes.
    pub history_bytes: usize,
}

/// A connected page's queues: frames, and marks as text.
#[derive(Debug)]
struct Page {
    frames: SyncSender<Bytes>,
    marks: SyncSender<String>,
}

/// What the listener's threads share with the stream.
#[derive(Clone, Debug)]
struct Shared {
    pages: Arc<Mutex<Vec<Page>>>,
    /// Locked before `pages` whenever both are, so a page connecting is
    /// told every mark exactly once: those kept, or later as they come.
    marks: Arc<Mutex<Marks>>,
    marks_dropped: Arc<AtomicU64>,
    /// The newest frame with the robot's poses, sent to each page first so
    /// it shows the robot before it next moves.
    latest: Arc<Mutex<Option<Bytes>>>,
    history: Arc<Mutex<History>>,
    refused: Arc<AtomicU64>,
}

/// The stream's listener and the pages connected to it.
#[derive(Debug)]
pub struct FrameStream {
    address: SocketAddr,
    shared: Shared,
    sent: AtomicU64,
    dropped: AtomicU64,
    stop: Arc<AtomicBool>,
}

impl FrameStream {
    /// Listens on `bind`:`port`, telling each page `opening` first, and
    /// keeps what it streams in `history` and what happened in `marks`.
    /// Only pages of Kapto's web UI,
    /// served on `ui_port` of the same host, may connect, and clients that
    /// aren't browsers.
    ///
    /// # Errors
    ///
    /// The port can't be listened on.
    pub fn start(
        (bind, port, ui_port): (IpAddr, u16, u16),
        opening: &Opening,
        history: History,
        marks: Marks,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::new(bind, port))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let opening = serde_json::to_string(opening).map_err(io::Error::other)?;
        let shared = Shared {
            pages: Arc::default(),
            latest: Arc::default(),
            history: Arc::new(Mutex::new(history)),
            marks: Arc::new(Mutex::new(marks)),
            marks_dropped: Arc::default(),
            refused: Arc::default(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        thread::Builder::new().name("viz-stream".into()).spawn({
            let (shared, stop) = (shared.clone(), Arc::clone(&stop));
            move || accept(&listener, ui_port, &opening, &shared, &stop)
        })?;
        Ok(Self {
            address,
            shared,
            sent: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            stop,
        })
    }

    /// Where the stream listens.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Hands `frame` to every page with room for it, and counts each that
    /// had none. Never waits. What it showed, `record`, is kept in the
    /// history, and a frame with poses also for pages that connect later.
    pub fn send(&self, frame: &Bytes, record: Record) {
        if record.poses.is_some() {
            *lock(&self.shared.latest) = Some(frame.clone());
        }
        lock(&self.shared.history).push(record);
        lock(&self.shared.pages).retain(|page| match page.frames.try_send(frame.clone()) {
            Ok(()) => {
                self.sent.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// How the stream has fared so far.
    #[must_use]
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            pages: lock(&self.shared.pages).len(),
            sent: self.sent.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            refused: self.shared.refused.load(Ordering::Relaxed),
            marks_dropped: self.shared.marks_dropped.load(Ordering::Relaxed),
            history_bytes: lock(&self.shared.history).bytes(),
        }
    }
}

/// Hands what happened to the stream, from any thread.
#[derive(Clone, Debug)]
pub struct Marker {
    shared: Shared,
}

impl Marker {
    /// Keeps `mark` and tells every page with room for it. Never waits on
    /// a page.
    pub fn mark(&self, mark: Mark) {
        let text = message(std::slice::from_ref(&mark));
        let mut marks = lock(&self.shared.marks);
        marks.push(mark);
        lock(&self.shared.pages).retain(|page| match page.marks.try_send(text.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.shared.marks_dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

impl FrameStream {
    /// A handle telling the stream what happened.
    #[must_use]
    pub fn marker(&self) -> Marker {
        Marker {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for FrameStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Every lock here guards data a panicking holder can't leave half-made.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Takes connections until the bridge stops, each on a thread of its own,
/// so a slow handshake never holds up the others.
fn accept(listener: &TcpListener, ui_port: u16, opening: &str, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((connection, _)) => {
                if lock(&shared.pages).len() >= MAX_PAGES {
                    shared.refused.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let shared = shared.clone();
                let opening = opening.to_owned();
                let _ = thread::Builder::new()
                    .name("viz-stream-page".into())
                    .spawn(move || {
                        if serve_page(connection, ui_port, &opening, &shared).is_err() {
                            shared.refused.fetch_add(1, Ordering::Relaxed);
                        }
                    });
            }
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
}

/// Opens the WebSocket, tells the page the opening message, then writes it
/// every frame it is handed and answers what it asks until it goes away.
/// Errors only if the connection was refused.
fn serve_page(
    connection: TcpStream,
    ui_port: u16,
    opening: &str,
    shared: &Shared,
) -> Result<(), ()> {
    let _ = connection.set_nonblocking(false);
    let _ = connection.set_nodelay(true);
    let _ = connection.set_write_timeout(Some(WRITE_TIMEOUT));
    let _ = connection.set_read_timeout(Some(READ_TIMEOUT));
    #[allow(
        clippy::result_large_err,
        reason = "the handshake's callback returns tungstenite's own types"
    )]
    let checked = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let text = |name| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
        };
        if allowed(text(header::ORIGIN), text(header::HOST), ui_port) {
            Ok(response)
        } else {
            let mut refusal = ErrorResponse::new(Some(
                "only Kapto's own web UI may watch the robot".to_owned(),
            ));
            *refusal.status_mut() = StatusCode::FORBIDDEN;
            Err(refusal)
        }
    };
    let mut socket = tungstenite::accept_hdr(connection, checked).map_err(|_| ())?;
    if socket.send(Message::text(opening)).is_err() {
        return Ok(());
    }
    let (queue, frames) = mpsc::sync_channel(PAGE_QUEUE);
    if let Some(latest) = lock(&shared.latest).clone() {
        let _ = queue.try_send(latest);
    }
    let (marks_queue, marks) = mpsc::sync_channel(MARKS_QUEUE);
    let kept = {
        let kept = lock(&shared.marks);
        lock(&shared.pages).push(Page {
            frames: queue,
            marks: marks_queue,
        });
        kept.all()
    };
    if socket.send(Message::text(message(&kept))).is_err() {
        return Ok(());
    }
    converse(&mut socket, (&frames, &marks), &shared.history);
    let _ = socket.close(None);
    Ok(())
}

/// What a page may ask, as text.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Question {
    /// A stretch of the history, from and to in UTC nanoseconds, merged
    /// to `points` parts.
    History {
        id: u32,
        from: i64,
        to: i64,
        points: u32,
    },
}

/// Writes the page every frame and mark it is handed and answers what it
/// asks, until it goes away. After each frame, or a moment without one, the
/// page is looked at, without waiting, for a question: a page streamed to
/// without pause still has its questions answered, and a frame never waits
/// for one.
fn converse(
    socket: &mut WebSocket<TcpStream>,
    (frames, marks): (&Receiver<Bytes>, &Receiver<String>),
    history: &Mutex<History>,
) {
    loop {
        match frames.recv_timeout(QUESTION_POLL) {
            Ok(frame) => {
                if socket.send(Message::Binary(frame)).is_err() {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(mark) = marks.try_recv() {
            if socket.send(Message::text(mark)).is_err() {
                return;
            }
        }
        match asked(socket.get_ref()) {
            Some(false) => continue,
            Some(true) => {}
            None => return,
        }
        let answer = match socket.read() {
            Ok(Message::Text(text)) => answer(&text, history),
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(_) => continue,
        };
        if socket.send(answer).is_err() {
            return;
        }
    }
}

/// Whether the page has sent something, looked at without waiting;
/// `None` if it has gone.
fn asked(connection: &TcpStream) -> Option<bool> {
    connection.set_nonblocking(true).ok()?;
    let peeked = connection.peek(&mut [0_u8]);
    connection.set_nonblocking(false).ok()?;
    match peeked {
        Ok(0) => None,
        Ok(_) => Some(true),
        Err(error) if error.kind() == ErrorKind::WouldBlock => Some(false),
        Err(_) => None,
    }
}

/// The answer to the question in `text`: a stretch of the history as
/// binary, or what is wrong with the question as text.
fn answer(text: &str, history: &Mutex<History>) -> Message {
    match serde_json::from_str::<Question>(text) {
        Ok(Question::History {
            id,
            from,
            to,
            points,
        }) => {
            if from > to || points == 0 || points > MAX_POINTS {
                return refusal(
                    Some(id),
                    &format!("ask for `from` at or before `to`, and 1 to {MAX_POINTS} points"),
                );
            }
            // Taken in a moment, so the bridge is never held up while the
            // stretch is merged.
            let snapshot = lock(history).snapshot();
            Message::Binary(snapshot.stretch(from, to, points).reply(id))
        }
        Err(error) => refusal(None, &error.to_string()),
    }
}

fn refusal(id: Option<u32>, message: &str) -> Message {
    let refusal = serde_json::json!({"error": {"id": id, "message": message}});
    Message::text(refusal.to_string())
}

/// Whether a connection from a page at `origin`, sent to `host`, may watch:
/// one from Kapto's web UI on the same host, at `ui_port`, or one that
/// isn't from a page at all (no `Origin`). Any other site's page is
/// refused, so it can't watch the robot through the browser of someone who
/// visits it.
#[must_use]
pub fn allowed(origin: Option<&str>, host: Option<&str>, ui_port: u16) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(host) = host else {
        return false;
    };
    let host = host.rsplit_once(':').map_or(host, |(name, port)| {
        if port.bytes().all(|byte| byte.is_ascii_digit()) && !name.is_empty() {
            name
        } else {
            host
        }
    });
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .and_then(|origin| origin.rsplit_once(':'))
        .is_some_and(|(name, port)| port == ui_port.to_string() && name.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_web_ui_on_the_same_host_or_a_client_that_isnt_a_page_may_watch() {
        assert!(allowed(None, Some("127.0.0.1:9877"), 7700));
        assert!(allowed(
            Some("http://127.0.0.1:7700"),
            Some("127.0.0.1:9877"),
            7700
        ));
        assert!(allowed(
            Some("http://Robot.local:7700"),
            Some("robot.local:9877"),
            7700
        ));
        assert!(allowed(Some("http://[::1]:7700"), Some("[::1]:9877"), 7700));
        for (origin, host) in [
            ("http://evil.example:7700", "127.0.0.1:9877"),
            ("http://127.0.0.1:8000", "127.0.0.1:9877"),
            ("http://127.0.0.1", "127.0.0.1:9877"),
            ("null", "127.0.0.1:9877"),
        ] {
            assert!(!allowed(Some(origin), Some(host), 7700), "{origin}");
        }
        assert!(!allowed(Some("http://127.0.0.1:7700"), None, 7700));
    }
}
