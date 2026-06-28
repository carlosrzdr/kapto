// SPDX-License-Identifier: Apache-2.0

//! Controlling a running capture process: start, stop and abort episodes,
//! ask what it is doing.
//!
//! The capture process listens on a Unix stream socket. Each request and
//! each response is one line of JSON. A connection begins with `hello`,
//! carrying the protocol version; a different version is refused, so a
//! server and a capture process from different releases never misread each
//! other. Then any number of `status`, `start`, `stop` and `abort` requests
//! follow, each answered in turn.
//!
//! A connection that sends `watch` is told, as it happens, each time an
//! episode starts and ends ([`Announcement`]), so a watcher never misses an
//! episode shorter than it would poll. It receives only that from then on.
//! The capture process hands each announcement to a thread of its own,
//! which writes it to every watcher: recording never waits on a slow one.
//! Each episode recovered when the capture process started is also told to
//! every watcher that connects later, first, so one that connects after it
//! was recovered still hears of it.
//!
//! `kapto serve` keeps one connection open on which it sends `supervise`
//! now and then. If every such connection has closed, because serve
//! stopped or crashed, the capture process knows for how long
//! ([`ControlServer::unsupervised_for`]) and finalises an episode that has
//! gone unsupervised too long.
//!
//! ```text
//! → {"command":"hello","version":1}
//! ← {"hello":{"version":1,"kapto":"0.1.0"}}
//! → {"command":"start"}
//! ← {"started":{"episode":"20260310T120000.000Z-1a2b3c4d"}}
//! ```

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{Event, Phase, Preflight, SessionProfile, SessionTask};
use serde::{Deserialize, Serialize};

use crate::tap::MAX_PATH_BYTES;

/// The protocol version this code speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest request line accepted, so a stuck or hostile client can't
/// make the capture process buffer without end.
const MAX_LINE_BYTES: usize = 4096;

/// What a client asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    /// The first request on every connection.
    Hello {
        /// The client's protocol version.
        version: u32,
    },
    /// What the capture process is doing.
    Status,
    /// Start recording an episode.
    Start {
        /// Stop by itself after this many seconds; without it, records until
        /// asked to stop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_s: Option<u32>,
        /// The session profile to record it with, as read when asked, and
        /// the robot package it records: it is refused unless that is the
        /// package capture records.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<Box<ChosenProfile>>,
        /// The task to record it for, as read when asked.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<Box<SessionTask>>,
        /// The cause codes of failed pre-flight checks to record despite:
        /// without them, a failed check refuses the start. The episode says
        /// which checks were waived.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        waive: Vec<String>,
    },
    /// Stop recording and finalise the episode.
    Stop,
    /// Stop recording and discard the episode: nothing of it is kept.
    Abort,
    /// The supervisor watches over the capture process for as long as this
    /// connection stays open. Sent again as a sign of life.
    Supervise,
    /// Tell this connection each time an episode starts and ends, as it
    /// happens. It receives only announcements from then on.
    Watch,
    /// Check again whether this machine can record what the robot sends:
    /// watch the streams for `capture.preflight_window` and test the disk.
    /// Answered once done; refused while an episode is recorded.
    Preflight,
    /// Something happened: during an episode it is written into the
    /// recording, and every watcher is told either way.
    Event(Box<NewEvent>),
}

/// An event as a client sends it: everything but its moment, which is now
/// unless given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEvent {
    /// What happened: dotted lowercase words, not beginning `kapto.`.
    pub kind: String,
    /// When, in UTC nanoseconds; when the capture process takes it if not
    /// given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_utc_ns: Option<i64>,
    /// When a whole stretch ended; only for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_utc_ns: Option<i64>,
    /// A moment, the beginning or end of a stretch, or a whole stretch.
    #[serde(default)]
    pub phase: Phase,
    /// Pairs a beginning with its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Where it comes from: `page`, `api`, `cli` or `robot:<stream>`.
    pub source: String,
    /// What someone said about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// More about it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl NewEvent {
    /// The event, at its moment or at `now_utc_ns`.
    #[must_use]
    pub fn at(self, now_utc_ns: i64) -> Event {
        Event {
            kind: self.kind,
            at_utc_ns: self.at_utc_ns.unwrap_or(now_utc_ns),
            until_utc_ns: self.until_utc_ns,
            phase: self.phase,
            id: self.id,
            source: self.source,
            text: self.text,
            attributes: self.attributes,
        }
    }
}

/// What the capture process answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// The answer to `hello`.
    Hello {
        /// The capture process's protocol version.
        version: u32,
        /// Its Kapto version.
        kapto: String,
    },
    /// The answer to `status`.
    Status(Status),
    /// An episode has started.
    Started {
        /// Its ID.
        episode: String,
    },
    /// The episode has been finalised.
    Stopped {
        /// Its ID.
        episode: String,
        /// Its folder.
        folder: String,
    },
    /// The episode has been discarded.
    Aborted {
        /// Its ID.
        episode: String,
    },
    /// The answer to `supervise`.
    Supervised,
    /// The answer to `watch`; announcements follow.
    Watching,
    /// Something that happened, sent to every connection that asked to
    /// `watch`.
    Announced(Announcement),
    /// The answer to `preflight`: each check's result.
    Preflight(Preflight),
    /// The answer to `event`: the event as kept, and whether it was written
    /// into an episode's recording (only while one is recorded).
    Noted {
        /// The event, with its moment.
        event: Box<Event>,
        /// The episode it was recorded in, if one was being recorded.
        episode: Option<String>,
    },
    /// The request was refused or failed.
    Error {
        /// The cause code.
        code: String,
        /// What happened: the code's summary and the specifics.
        message: String,
    },
}

impl Response {
    /// The response reporting `error`.
    #[must_use]
    pub fn error(error: &KaptoError) -> Self {
        Self::Error {
            code: error.code().id().to_owned(),
            message: error.message(),
        }
    }
}

/// What a watching connection is told.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Announcement {
    /// An episode started being recorded.
    EpisodeStarted {
        /// Its ID.
        episode: String,
    },
    /// The episode being recorded ended.
    EpisodeEnded {
        /// Its ID.
        episode: String,
        /// Whether it was finalised; `false` if it was aborted or failed.
        finalised: bool,
    },
    /// An episode that recording left unfinished, by a crash, a kill or a
    /// power cut, was finalised when the capture process started. Its
    /// `episode.json` says whether any of it was lost. Told to every
    /// watcher, those connecting later included.
    EpisodeRecovered {
        /// Its ID.
        episode: String,
    },
    /// Something happened: an event, as kept. Told as it happens, recorded
    /// or not.
    EventNoted {
        /// The event.
        happened: Box<Event>,
        /// The episode it was recorded in, if one was being recorded.
        episode: Option<String>,
    },
}

impl Announcement {
    /// Whether every watcher connecting later is told it too: what was
    /// recovered happened before anyone could be watching.
    fn lasts(&self) -> bool {
        matches!(self, Self::EpisodeRecovered { .. })
    }
}

/// A session profile chosen for an episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenProfile {
    /// The ID of the robot package it records.
    pub package: String,
    /// What it says, kept in the episode.
    pub profile: SessionProfile,
}

/// What the capture process is doing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// The episode being recorded, if one is.
    pub episode: Option<String>,
    /// Seconds since the episode started, or since receiving started while
    /// none is recorded.
    pub seconds: u64,
    /// Each stream's counts over the same time.
    pub streams: Vec<StreamStatus>,
    /// Pre-flight's latest run, once one has ended; while an episode is
    /// recorded, the run it keeps, with the checks it waived. Older capture
    /// processes leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preflight: Option<Preflight>,
}

/// One stream's counts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamStatus {
    /// The stream's ID.
    pub stream: String,
    /// Messages received.
    pub received: u64,
    /// Messages written to the episode; zero while none is recorded.
    pub written: u64,
    /// Messages Kapto discarded.
    pub dropped_by_us: u64,
    /// Messages the source sent that never arrived.
    pub lost_upstream: u64,
    /// The newest decoded values, field after field, array elements in
    /// order, if the live view decoded any. Older capture processes leave
    /// it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<f64>>,
    /// How long ago those values were decoded, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values_age_ms: Option<u64>,
    /// For a camera, what became of its frames on their way to the live
    /// view. Older capture processes leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_view: Option<LiveViewCounts>,
}

/// What became of a camera's frames on their way to the live view, since
/// the capture process started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveViewCounts {
    /// Frames offered: each camera's newest, at most `viewer.camera_max_fps`
    /// a second.
    pub offered: u64,
    /// Offered frames skipped because the live view held every slot.
    pub skipped: u64,
    /// Frames replaced by newer ones before they could be sent.
    pub superseded: u64,
    /// Frames not sent because no viewer bridge was listening.
    pub not_sent: u64,
    /// Frames the viewer bridge took.
    pub sent: u64,
}

/// A request waiting for the capture process's answer.
pub struct Pending {
    /// The request.
    pub request: Request,
    reply: Sender<Response>,
}

impl Pending {
    /// Answers the request. A client that has gone away misses the answer;
    /// that changes nothing here.
    pub fn answer(self, response: Response) {
        let _ = self.reply.send(response);
    }
}

/// The capture process's end: listens on the control socket and hands each
/// request after `hello` to whoever polls [`ControlServer::pending`]. The
/// socket file is removed when it is dropped.
pub struct ControlServer {
    requests: Receiver<Pending>,
    path: PathBuf,
    supervision: Arc<Supervision>,
    announcer: Announcer,
}

/// Tells every watching connection what happened, from any thread.
#[derive(Clone, Debug)]
pub struct Announcer {
    announcements: Sender<(u64, Announcement)>,
    made: Arc<AtomicU64>,
}

impl Announcer {
    /// Tells every watching connection what happened. Never waits: a
    /// thread of its own does the writing.
    pub fn announce(&self, announcement: Announcement) {
        // Numbered as it is made, so the announcing thread, which may still
        // be telling earlier ones, knows which watchers it came after.
        let number = self.made.fetch_add(1, Ordering::Relaxed);
        // Gone only with the process's end, when nobody is left to tell.
        let _ = self.announcements.send((number, announcement));
    }
}

/// The connections that asked to `watch`, and what each one connecting
/// later is told first, with how many announcements have been made.
#[derive(Debug, Default)]
struct Watchers {
    watching: Mutex<Watching>,
    made: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct Watching {
    connections: Vec<Watcher>,
    lasting: Vec<Announcement>,
}

/// A watching connection, and the number of the first announcement made
/// after it asked to watch: it is told that one on.
#[derive(Debug)]
struct Watcher {
    connection: UnixStream,
    from: u64,
}

/// How long the announcing thread waits on one watcher before giving it up.
const WATCHER_TIMEOUT: Duration = Duration::from_secs(1);

/// The connections a supervisor holds open.
#[derive(Debug, Default)]
struct Supervision {
    state: Mutex<Supervisors>,
}

#[derive(Debug, Default)]
struct Supervisors {
    /// Open connections that sent `supervise`.
    open: usize,
    /// When the last one closed, if one ever did and none is open.
    left: Option<Instant>,
}

impl Supervision {
    fn update(&self, change: impl FnOnce(&mut Supervisors)) {
        change(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// Holds a connection's place as a supervisor; the last one to go marks
/// the time.
struct Supervising(Arc<Supervision>);

impl Supervising {
    fn new(supervision: Arc<Supervision>) -> Self {
        supervision.update(|supervisors| {
            supervisors.open += 1;
            supervisors.left = None;
        });
        Self(supervision)
    }
}

impl Drop for Supervising {
    fn drop(&mut self) {
        self.0.update(|supervisors| {
            supervisors.open -= 1;
            if supervisors.open == 0 {
                supervisors.left = Some(Instant::now());
            }
        });
    }
}

impl ControlServer {
    /// Listens on `path`, replacing a socket left there by an earlier run.
    ///
    /// # Errors
    ///
    /// `capture.control_socket_failed` if the path is too long for a Unix
    /// socket, is taken by a file that isn't a socket, or can't be bound.
    pub fn listen(path: &Path) -> Result<Self, KaptoError> {
        let failed = |detail: String| {
            KaptoError::new(ErrorCode::CaptureControlSocketFailed)
                .detail(detail)
                .context("socket", path.display().to_string())
        };
        let length = path.as_os_str().len();
        if length > MAX_PATH_BYTES {
            return Err(failed(format!(
                "the path is {length} bytes long, but a Unix socket path can have at most {MAX_PATH_BYTES}"
            )));
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                std::fs::remove_file(path).map_err(|error| failed(error.to_string()))?;
            }
            Ok(_) => return Err(failed("a file that isn't a socket is in the way".into())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed(error.to_string())),
        }
        if let Some(folder) = path.parent().filter(|f| !f.as_os_str().is_empty()) {
            std::fs::create_dir_all(folder).map_err(|error| failed(error.to_string()))?;
        }
        let listener = UnixListener::bind(path).map_err(|error| failed(error.to_string()))?;
        let (sender, requests) = mpsc::channel();
        let supervision = Arc::new(Supervision::default());
        let watchers = Arc::new(Watchers::default());
        let made = Arc::clone(&watchers.made);
        let (announcements, announced) = mpsc::channel();
        thread::Builder::new()
            .name("cap-control-tell".into())
            .spawn({
                let watchers = Arc::clone(&watchers);
                move || tell(&announced, &watchers)
            })
            .map_err(|error| failed(error.to_string()))?;
        thread::Builder::new()
            .name("cap-control".into())
            .spawn({
                let supervision = Arc::clone(&supervision);
                move || accept(&listener, &sender, &supervision, &watchers)
            })
            .map_err(|error| failed(error.to_string()))?;
        Ok(Self {
            requests,
            path: path.to_path_buf(),
            supervision,
            announcer: Announcer {
                announcements,
                made,
            },
        })
    }

    /// Tells every watching connection what happened. Never waits: a
    /// thread of its own does the writing.
    pub fn announce(&self, announcement: Announcement) {
        self.announcer.announce(announcement);
    }

    /// Tells the watchers from another thread.
    #[must_use]
    pub fn announcer(&self) -> Announcer {
        self.announcer.clone()
    }

    /// How long ago the last supervisor's connection closed. `None` while
    /// one is open, or if none ever was: a capture process run by hand has
    /// nobody to miss.
    pub fn unsupervised_for(&self) -> Option<Duration> {
        let supervisors = self
            .supervision
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        supervisors.left.map(|left| left.elapsed())
    }

    /// The requests received since the last call, oldest first. Never
    /// waits.
    pub fn pending(&self) -> impl Iterator<Item = Pending> + '_ {
        self.requests.try_iter()
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        // Nothing to do about a failure: the next run replaces the file.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn accept(
    listener: &UnixListener,
    requests: &Sender<Pending>,
    supervision: &Arc<Supervision>,
    watchers: &Arc<Watchers>,
) {
    for connection in listener.incoming() {
        let Ok(connection) = connection else {
            continue;
        };
        let requests = requests.clone();
        let supervision = Arc::clone(supervision);
        let watchers = Arc::clone(watchers);
        // A thread per connection, so a slow client never holds up another.
        let _ = thread::Builder::new()
            .name("cap-control-conn".into())
            .spawn(move || {
                let _ = converse(connection, &requests, &supervision, &watchers);
            });
    }
}

/// Writes each announcement to every watcher that asked to watch before it
/// was made, dropping one that has gone or doesn't take it within
/// [`WATCHER_TIMEOUT`]. What lasts is told to every watcher, even one that
/// came while it was still waiting here: it wasn't yet among the lasting
/// ones that watcher was told first.
fn tell(announced: &Receiver<(u64, Announcement)>, watchers: &Watchers) {
    for (number, announcement) in announced {
        let mut watching = watchers
            .watching
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        watching.connections.retain_mut(|watcher| {
            (number < watcher.from && !announcement.lasts())
                || send(
                    &mut watcher.connection,
                    &Response::Announced(announcement.clone()),
                )
                .is_ok()
        });
        if announcement.lasts() {
            watching.lasting.push(announcement);
        }
    }
}

/// Answers one connection's requests until it closes. Ends on the first
/// error, after answering it.
fn converse(
    connection: UnixStream,
    requests: &Sender<Pending>,
    supervision: &Arc<Supervision>,
    watchers: &Watchers,
) -> io::Result<()> {
    let mut reader = BufReader::new(connection.try_clone()?);
    let mut writer = connection;
    let mut greeted = false;
    // Dropped when the connection ends, however it ends.
    let mut supervising = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = Read::take(&mut reader, MAX_LINE_BYTES as u64 + 1).read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        let request = if line.len() > MAX_LINE_BYTES {
            Err(invalid("the request is longer than 4096 bytes"))
        } else {
            serde_json::from_str::<Request>(&line).map_err(|error| invalid(&error.to_string()))
        };
        let (response, keep_going) = match request {
            Err(error) => (Response::error(&error), false),
            Ok(Request::Hello { version }) if version == PROTOCOL_VERSION => {
                greeted = true;
                (hello(), true)
            }
            Ok(Request::Hello { version }) => (Response::error(&version_mismatch(version)), false),
            Ok(_) if !greeted => (
                Response::error(&invalid("the first request must be `hello`")),
                false,
            ),
            // Answered here, so a sign of life never waits for the capture
            // process's own loop.
            Ok(Request::Supervise) => {
                supervising.get_or_insert_with(|| Supervising::new(Arc::clone(supervision)));
                (Response::Supervised, true)
            }
            Ok(Request::Watch) => {
                writer.set_write_timeout(Some(WATCHER_TIMEOUT))?;
                // Answered, told what lasts and added under the lock the
                // announcing thread takes, so the answer comes first and
                // nothing is missed or told twice.
                let mut watching = watchers
                    .watching
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                // Before the answer, under the lock: whatever is announced
                // once the client has its answer is told after this
                // connection is added, and nothing made before is.
                let from = watchers.made.load(Ordering::Relaxed);
                send(&mut writer, &Response::Watching)?;
                for announcement in &watching.lasting {
                    send(&mut writer, &Response::Announced(announcement.clone()))?;
                }
                watching.connections.push(Watcher {
                    connection: writer,
                    from,
                });
                drop(watching);
                // Only announcements from now on, written by their own
                // thread; whatever the client sends is ignored until it
                // closes, which drops it at the next announcement.
                return io::copy(&mut reader, &mut io::sink()).map(drop);
            }
            Ok(request) => {
                let (reply, answer) = mpsc::channel();
                if requests.send(Pending { request, reply }).is_err() {
                    return Ok(());
                }
                match answer.recv() {
                    Ok(response) => (response, true),
                    Err(_) => return Ok(()),
                }
            }
        };
        send(&mut writer, &response)?;
        if !keep_going {
            close_after_refusal(&mut reader, &writer);
            return Ok(());
        }
    }
}

/// Ends a connection after its refusal so the client reads the refusal and
/// then the end. Closing a Unix socket with unread data in it makes the
/// client's next read fail with a reset instead, which a client that sent
/// more than one request could take for a lost connection. Whatever it
/// already sent is read and discarded, up to a limit, so a client that
/// keeps sending can't hold the connection open.
fn close_after_refusal(reader: &mut BufReader<UnixStream>, writer: &UnixStream) {
    const LIMIT: u64 = 1024 * 1024;
    let _ = writer.shutdown(std::net::Shutdown::Write);
    let _ = writer.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = io::copy(&mut Read::take(reader, LIMIT), &mut io::sink());
}

fn hello() -> Response {
    Response::Hello {
        version: PROTOCOL_VERSION,
        kapto: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn send(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)
}

fn invalid(detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureControlInvalidRequest).detail(detail.to_owned())
}

fn version_mismatch(theirs: u32) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureControlVersionMismatch).detail(format!(
        "the client speaks version {theirs}, this capture process version {PROTOCOL_VERSION}"
    ))
}

/// A client's end: a connection to a capture process, greeted.
pub struct ControlClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl ControlClient {
    /// Connects to the capture process listening on `path` and says hello.
    /// Each request is written, and each answer waited for, within
    /// `timeout`.
    ///
    /// # Errors
    ///
    /// `capture.control_unreachable` if nothing listens there or it doesn't
    /// answer; `capture.control_version_mismatch` if it speaks another
    /// protocol version.
    pub fn connect(path: &Path, timeout: Duration) -> Result<Self, KaptoError> {
        let unreachable = |error: io::Error| {
            KaptoError::new(ErrorCode::CaptureControlUnreachable)
                .detail(error.to_string())
                .context("socket", path.display().to_string())
        };
        let writer = UnixStream::connect(path).map_err(unreachable)?;
        writer
            .set_read_timeout(Some(timeout))
            .and_then(|()| writer.set_write_timeout(Some(timeout)))
            .map_err(unreachable)?;
        let reader = BufReader::new(writer.try_clone().map_err(unreachable)?);
        let mut client = Self { reader, writer };
        match client.ask(&Request::Hello {
            version: PROTOCOL_VERSION,
        })? {
            Response::Hello { .. } => Ok(client),
            other => Err(unexpected(&other)),
        }
    }

    /// Asks to be told each time an episode starts and ends, and returns
    /// the announcements as they come, waiting for each without a limit.
    /// They end when the capture process does.
    ///
    /// # Errors
    ///
    /// The capture process's refusal (an older one answers
    /// `capture.control_invalid_request`), or
    /// `capture.control_unreachable` if the connection fails.
    pub fn watch(mut self) -> Result<Announcements, KaptoError> {
        match self.ask(&Request::Watch)? {
            Response::Watching => {}
            other => return Err(unexpected(&other)),
        }
        self.writer.set_read_timeout(None).map_err(|error| {
            KaptoError::new(ErrorCode::CaptureControlUnreachable).detail(error.to_string())
        })?;
        Ok(Announcements {
            reader: self.reader,
        })
    }

    /// Sends `request` and waits for its answer. An error answer comes back
    /// as an error with its cause code.
    ///
    /// # Errors
    ///
    /// The capture process's refusal, or `capture.control_unreachable` if
    /// the connection fails.
    pub fn ask(&mut self, request: &Request) -> Result<Response, KaptoError> {
        let lost = |error: io::Error| {
            KaptoError::new(ErrorCode::CaptureControlUnreachable).detail(error.to_string())
        };
        send(&mut self.writer, request).map_err(lost)?;
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).map_err(lost)?;
        if read == 0 {
            return Err(lost(io::ErrorKind::UnexpectedEof.into()));
        }
        match serde_json::from_str(&line) {
            Ok(Response::Error { code, message }) => Err(refused(&code, &message)),
            Ok(response) => Ok(response),
            Err(error) => Err(lost(io::Error::other(error))),
        }
    }
}

/// What a watching client is told, one announcement at a time.
pub struct Announcements {
    reader: BufReader<UnixStream>,
}

impl Iterator for Announcements {
    type Item = Announcement;

    /// The next announcement, once it comes; `None` once the connection
    /// has ended. A line that isn't an announcement is skipped.
    fn next(&mut self) -> Option<Announcement> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            if let Ok(Response::Announced(announcement)) = serde_json::from_str(&line) {
                return Some(announcement);
            }
        }
    }
}

/// The capture process's error, under its own cause code when this side
/// knows it.
fn refused(code: &str, message: &str) -> KaptoError {
    match ErrorCode::ALL.iter().find(|known| known.id() == code) {
        Some(&code) => {
            let detail = message
                .strip_prefix(code.summary())
                .map_or(message, |rest| rest.trim_start_matches(": "));
            if detail.is_empty() {
                KaptoError::new(code)
            } else {
                KaptoError::new(code).detail(detail.to_owned())
            }
        }
        None => KaptoError::new(ErrorCode::CaptureControlUnexpectedAnswer)
            .detail(format!("{code}: {message}")),
    }
}

fn unexpected(response: &Response) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureControlUnexpectedAnswer)
        .detail(format!("unexpected answer: {response:?}"))
}
