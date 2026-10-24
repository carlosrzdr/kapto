// SPDX-License-Identifier: Apache-2.0

//! Serve's job socket: how the worker gets work from the job queue in
//! `kapto.db`, which only serve writes. The worker connects to a Unix socket
//! in the data directory, says `hello`, then asks for the `next` job and
//! reports each one `finished`, with what it found if it checks quality, or
//! `released` if it stopped it part-way, as one line of JSON each way.
//!
//! The worker has a connection per lane, each served on a thread of its
//! own (`srv-jobs-*`) as `srv-jobs` accepts it: a lane taking only uploads
//! never waits behind a lane checking an episode, nor the other way round.
//! A lane that is restarted connects again.
//!
//! An upload lane also reports each step of an upload, which serve writes
//! in `kapto.db`, so an upload carries on after a restart from where it was.
//! Serve queues an episode's [`UPLOAD`] only once it qualifies as
//! `upload.when` says: its files checked, and, for `accepted`, its quality
//! checks passed. An upload that failed is queued again by itself once its
//! destination's `upload.retry_failed_after` has passed since it failed,
//! when a lane next asks for an upload.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kapto_config::UploadWhen;
use kapto_core::clock::utc_now_ns;
use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use kapto_store::{
    Episode, Event, JobFailure, JobId, JobState, Kinds, QaRun, Reader, Upload, UploadCounts,
    UploadState, UploadedObject, Writer, episode_folder,
};

use serde::{Deserialize, Serialize};

use crate::episode_bin;
use crate::retention::{self, Retention};

/// The socket's file name in the data directory.
pub const JOB_SOCKET: &str = "worker.sock";

/// The version of the protocol below; both ends must speak the same.
pub const PROTOCOL_VERSION: u32 = 1;

/// The job kind that checks an episode's files against its manifest,
/// queued for every episode once it is finalised.
pub const VERIFY: &str = "verify";

/// The job kind that runs the quality checks on an episode, queued for
/// every episode once it is finalised, after [`VERIFY`].
pub const QA: &str = "qa";

/// The job kind that reads an episode's events from its recording into
/// the episode index, so episodes can be found by what happened in them,
/// queued for every episode once it is finalised, after [`VERIFY`].
pub const EVENTS: &str = "events";

/// The job kind that makes an episode's replay, its replay view to watch
/// it again, queued the first time someone asks for it.
pub const REPLAY: &str = "replay";

/// The job kind that uploads an episode to every destination, queued when
/// it qualifies as `upload.when` says.
pub const UPLOAD: &str = "upload";

/// The job kind that deletes an episode from this machine, queued as
/// `upload.delete_local` says once it is verified on enough destinations,
/// and checked again when handed out ([`crate::retention`]).
pub const DELETE: &str = kapto_store::jobs::DELETE;

/// How often, at most, serve looks for episodes to delete, as a lane asks
/// for work.
const RETENTION_EVERY: Duration = Duration::from_secs(10);

/// The longest request line read: room for an episode's quality results
/// with a model learned during the run, a few MB at most (a few hundred
/// statistics by as many components, as JSON text).
const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;

/// How often a waiting connection or listener looks whether to stop.
const POLL: Duration = Duration::from_millis(100);

/// What the worker asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum JobRequest {
    /// The first request on a connection.
    Hello {
        /// The worker's [`PROTOCOL_VERSION`].
        version: u32,
    },
    /// Start the oldest queued job and hand it over: of one of `kinds`,
    /// for a lane doing only those; of any but `except`, for a lane
    /// leaving those to another; or of any kind.
    Next {
        /// The kinds wanted; every kind if left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kinds: Option<Vec<String>>,
        /// The kinds not wanted, if `kinds` is left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        except: Option<Vec<String>>,
    },
    /// An episode's upload to a destination starts, under `key` unless it
    /// was started before: answered with the key and progress it has.
    UploadStart {
        /// The episode.
        episode: String,
        /// The destination's name.
        destination: String,
        /// The key worked out now, used if it is new.
        key: String,
    },
    /// How far an upload got, as the upload engine wrote it.
    UploadProgress {
        /// The episode.
        episode: String,
        /// The destination's name.
        destination: String,
        /// The progress.
        progress: String,
        /// How far it got, in numbers.
        #[serde(default)]
        counts: UploadCounts,
    },
    /// An upload is done, its marker written, with every object of it.
    Uploaded {
        /// The episode.
        episode: String,
        /// The destination's name.
        destination: String,
        /// Every object of it there.
        objects: Vec<UploadedObject>,
    },
    /// An episode being deleted, its folder set aside, with its
    /// `episode.json`: serve records the deletion, or refuses it, before
    /// the folder goes.
    Deleting {
        /// The episode.
        episode: String,
        /// Its `episode.json`, as it was.
        summary: String,
    },
    /// Try an episode's failed uploads again now, as asked by hand.
    RetryUploads {
        /// The episode.
        episode: String,
    },
    /// Delete an episode by hand from the command line: into the bin.
    PutInBin {
        /// The episode.
        episode: String,
    },
    /// Put an episode back from the bin, as asked from the command line.
    PutBack {
        /// The episode.
        episode: String,
    },
    /// An upload stopped with a failure; its progress is kept.
    UploadFailed {
        /// The episode.
        episode: String,
        /// The destination's name.
        destination: String,
        /// Why.
        failure: JobFailure,
    },
    /// A job handed over was stopped part-way, to be done again later: back
    /// in the queue, not counted as an attempt.
    Released {
        /// Its number.
        job: JobId,
    },
    /// A job handed over has ended: done, or failed with `failure`.
    Finished {
        /// Its number.
        job: JobId,
        /// Why it failed, if it did.
        failure: Option<JobFailure>,
        /// What the quality checks found, for a [`QA`] job that ran them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        qa: Option<QaRun>,
        /// The events read, for an [`EVENTS`] job.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        events: Option<Box<EpisodeEvents>>,
        /// What it couldn't do in full though it was done, e.g. a
        /// [`REPLAY`] without the robot.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<JobFailure>,
    },
}

/// An episode's events, as its recording holds them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeEvents {
    /// The episode's ID.
    pub episode: String,
    /// Its events, in the order the recording holds them.
    pub events: Vec<Event>,
}

/// What a job found once done, reported with it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// What the quality checks found, for a [`QA`] job.
    pub qa: Option<QaRun>,
    /// The events read, for an [`EVENTS`] job.
    pub events: Option<EpisodeEvents>,
    /// What it couldn't do in full though it was done.
    pub warnings: Vec<JobFailure>,
}

/// What serve answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum JobResponse {
    /// Serve's [`PROTOCOL_VERSION`].
    Hello {
        /// The version.
        version: u32,
    },
    /// The job to do, or none if none is queued.
    Job {
        /// The job.
        job: Option<JobOrder>,
    },
    /// The job's end was recorded, if it was still running.
    Finished {
        /// Whether it was running.
        was_running: bool,
    },
    /// The job was given back, if it was still running.
    Released {
        /// Whether it was running.
        was_running: bool,
    },
    /// The upload to carry on: its key and how far it got.
    Upload {
        /// The episode's key at the destination.
        key: String,
        /// How far it got, as the upload engine wrote it.
        progress: String,
    },
    /// An episode put in the bin, and when.
    Binned {
        /// When it went in, in UTC nanoseconds.
        binned_utc_ns: i64,
    },
    /// What was reported is written.
    Saved,
    /// An episode's failed uploads are queued again.
    Retried {
        /// The destinations they are to.
        destinations: Vec<String>,
    },
    /// The request was refused or failed.
    Error {
        /// The cause code.
        code: String,
        /// The message.
        message: String,
    },
}

/// A job handed to the worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOrder {
    /// Its number, to report it finished.
    pub id: JobId,
    /// What to do, e.g. [`VERIFY`].
    pub kind: String,
    /// The episode to do it on.
    pub episode: String,
    /// For an [`UPLOAD`]: the destinations the episode goes to now, those
    /// whose `when` it qualifies for and that don't have it yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<String>,
}

/// Where episodes are uploaded, and when each destination takes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uploads {
    /// Each destination, as serve schedules uploads to it.
    pub destinations: Vec<Destination>,
    /// The data directory the episodes are in.
    pub data_dir: PathBuf,
    /// How episodes verified on enough of them are deleted here.
    pub retention: Retention,
}

/// How the bin of episodes deleted by hand is emptied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bin {
    /// The data directory, whose bin it is.
    pub data_dir: PathBuf,
    /// How long an episode stays in it (`bin.keep_days`).
    pub keep: Duration,
    /// Below how much free space the disk is low, and the oldest in the bin
    /// goes sooner (`capture.disk_warn_below`).
    pub free_below_bytes: u64,
}

/// One destination, as serve schedules uploads to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// Its name.
    pub name: String,
    /// Which episodes it takes by itself (`upload.when`).
    pub when: UploadWhen,
    /// How long after failing an upload to it is tried again by itself
    /// (`upload.retry_failed_after`).
    pub retry_failed_after: Duration,
}

impl Destination {
    /// Whether `upload` to it is one to do now: not started, carrying on,
    /// or failed long enough ago to be tried again at `now`.
    fn wants(&self, upload: Option<&Upload>, now: i64) -> bool {
        upload.is_none_or(|upload| match upload.state {
            UploadState::Uploading => true,
            UploadState::Uploaded => false,
            UploadState::Failed => self.may_retry(upload, now),
        })
    }

    /// Whether failed `upload` may be tried again at `now`.
    fn may_retry(&self, upload: &Upload, now: i64) -> bool {
        let wait = i64::try_from(self.retry_failed_after.as_nanos()).unwrap_or(i64::MAX);
        upload.state == UploadState::Failed
            && upload
                .failed_utc_ns
                .is_some_and(|failed| now.saturating_sub(failed) >= wait)
    }
}

/// Serve's end of the job socket. Dropping it stops listening and removes
/// the socket.
#[derive(Debug)]
pub struct JobServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    queue: Arc<Queue>,
}

impl JobServer {
    /// Listens on `path`, [`JOB_SOCKET`] in the data directory, replacing a
    /// socket left there by an earlier run, and hands out jobs from
    /// `store`'s queue.
    ///
    /// # Errors
    ///
    /// `server.job_socket_failed` if the path is taken by a file that isn't
    /// a socket, or the socket can't be bound or served.
    pub fn listen(path: &Path, store: Writer) -> Result<Self, KaptoError> {
        Self::listen_with_uploads(path, store, None)
    }

    /// The same, also queueing an episode's [`UPLOAD`] once it qualifies
    /// for a destination of `uploads`, as that destination's `upload.when`
    /// says. `None`, or every destination's `when` being `never`, queues
    /// none.
    ///
    /// # Errors
    ///
    /// As [`JobServer::listen`].
    pub fn listen_with_uploads(
        path: &Path,
        store: Writer,
        uploads: Option<Uploads>,
    ) -> Result<Self, KaptoError> {
        let failed = |detail: String| {
            KaptoError::new(ErrorCode::ServerJobSocketFailed)
                .detail(detail)
                .context("socket", path.display().to_string())
        };
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                std::fs::remove_file(path).map_err(|error| failed(error.to_string()))?;
            }
            Ok(_) => return Err(failed("a file that isn't a socket is in the way".into())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed(error.to_string())),
        }
        let listener = UnixListener::bind(path)
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
            .map_err(|error| failed(error.to_string()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let retention = uploads
            .as_ref()
            .map(|uploads| (uploads.retention.clone(), uploads.data_dir.clone()));
        let queue = Arc::new(Queue {
            store,
            retention,
            retained: Mutex::new(None),
            bin: Mutex::new(None),
            uploads: uploads
                .map(|mut uploads| {
                    uploads
                        .destinations
                        .retain(|destination| destination.when != UploadWhen::Never);
                    uploads
                })
                .filter(|uploads| !uploads.destinations.is_empty()),
        });
        let thread = thread::Builder::new()
            .name("srv-jobs".into())
            .spawn({
                let (stop, queue) = (Arc::clone(&stop), Arc::clone(&queue));
                move || accept(&listener, &queue, &stop)
            })
            .map_err(|error| failed(error.to_string()))?;
        Ok(Self {
            path: path.to_owned(),
            stop,
            thread: Some(thread),
            queue,
        })
    }

    /// Empties the bin of episodes deleted by hand as `bin` says, through
    /// the retention executor's [`DELETE`] jobs, whether or not anything is
    /// uploaded.
    pub fn empty_bin_by(&self, bin: Bin) {
        *self
            .queue
            .bin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bin);
    }
}

impl Drop for JobServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The job queue, and when episodes are queued for upload.
#[derive(Debug)]
struct Queue {
    store: Writer,
    uploads: Option<Uploads>,
    /// How episodes are deleted here, and where they are; kept even with
    /// no destination uploading by itself.
    retention: Option<(Retention, PathBuf)>,
    /// When serve last looked for episodes to delete.
    retained: Mutex<Option<Instant>>,
    /// How the bin is emptied, once set.
    bin: Mutex<Option<Bin>>,
}

fn accept(listener: &UnixListener, queue: &Arc<Queue>, stop: &Arc<AtomicBool>) {
    let mut lanes: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((connection, _)) => {
                lanes.retain(|lane| !lane.is_finished());
                let (queue, stop) = (Arc::clone(queue), Arc::clone(stop));
                let spawned = thread::Builder::new()
                    .name(format!("srv-jobs-{}", lanes.len()))
                    // A connection that fails ends; the lane connects again.
                    .spawn(move || drop(converse(connection, &queue, &stop)));
                if let Ok(lane) = spawned {
                    lanes.push(lane);
                }
            }
            Err(_) => thread::sleep(POLL),
        }
    }
    for lane in lanes {
        let _ = lane.join();
    }
}

fn converse(connection: UnixStream, queue: &Queue, stop: &AtomicBool) -> io::Result<()> {
    connection.set_nonblocking(false)?;
    connection.set_read_timeout(Some(POLL))?;
    let mut reader = BufReader::new(connection.try_clone()?);
    let mut writer = connection;
    let mut greeted = false;
    let mut line = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match Read::take(&mut reader, MAX_LINE_BYTES + 1).read_until(b'\n', &mut line) {
            Ok(0) if line.is_empty() => return Ok(()),
            Ok(_) if line.ends_with(b"\n") || line.len() as u64 > MAX_LINE_BYTES => {}
            Ok(_) => return Ok(()),
            // Nothing yet: look whether to stop, then keep what was read.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
        let request = if line.len() as u64 > MAX_LINE_BYTES {
            Err(invalid(&format!(
                "the request is longer than {MAX_LINE_BYTES} bytes"
            )))
        } else {
            serde_json::from_slice(&line).map_err(|error| invalid(&error.to_string()))
        };
        line.clear();
        let (response, keep_going) = match (request, greeted) {
            (Ok(JobRequest::Hello { version }), _) if version == PROTOCOL_VERSION => {
                greeted = true;
                (
                    JobResponse::Hello {
                        version: PROTOCOL_VERSION,
                    },
                    true,
                )
            }
            (Ok(JobRequest::Hello { version }), _) => (
                refusal(
                    &KaptoError::new(ErrorCode::WorkerVersionMismatch).detail(format!(
                        "the worker speaks version {version}, serve {PROTOCOL_VERSION}"
                    )),
                ),
                false,
            ),
            (Ok(_), false) => (
                refusal(&invalid("the first request must be `hello`")),
                false,
            ),
            (Ok(request), true) => (answer(queue, request), true),
            (Err(error), _) => (refusal(&error), false),
        };
        send(&mut writer, &response)?;
        if !keep_going {
            return Ok(());
        }
    }
    Ok(())
}

fn answer(queue: &Queue, request: JobRequest) -> JobResponse {
    let store = &queue.store;
    let answered = match request {
        // Answered in `converse`; a repeated one gets the same answer.
        JobRequest::Hello { .. } => Ok(JobResponse::Hello {
            version: PROTOCOL_VERSION,
        }),
        JobRequest::Next { kinds, except } => {
            let kinds = match (kinds, except) {
                (Some(kinds), _) => Kinds::Only(kinds),
                (None, Some(except)) => Kinds::Except(except),
                (None, None) => Kinds::Any,
            };
            next(queue, &kinds).map(|job| JobResponse::Job { job })
        }
        JobRequest::RetryUploads { episode } => {
            retry_uploads(store, &episode).map(|destinations| JobResponse::Retried { destinations })
        }
        JobRequest::PutInBin { episode } => put_in_bin(queue, &episode, true),
        JobRequest::PutBack { episode } => put_in_bin(queue, &episode, false),
        JobRequest::Deleting { episode, summary } => {
            match (&queue.retention, in_bin(store, &episode)) {
                (_, Ok(true)) => episode_bin::record_emptied(store, &episode, summary),
                (_, Err(error)) => Err(error),
                (Some((retention, _)), Ok(false)) => {
                    retention::record(store, retention, &episode, summary)
                }
                (None, Ok(false)) => {
                    retention::record(store, &Retention::default(), &episode, summary)
                }
            }
            .map(|()| JobResponse::Saved)
        }
        JobRequest::UploadStart {
            episode,
            destination,
            key,
        } => store
            .start_upload(&episode, &destination, &key)
            .map(|upload| JobResponse::Upload {
                key: upload.key,
                progress: upload.progress,
            }),
        JobRequest::UploadProgress {
            episode,
            destination,
            progress,
            counts,
        } => {
            // Deleted by hand meanwhile: stopped at its next step, given
            // back as far as it got, to carry on if it is put back.
            if matches!(in_bin(store, &episode), Ok(true)) {
                Err(KaptoError::new(ErrorCode::ApiEpisodeInBin).context("episode", episode))
            } else {
                store
                    .upload_progress(&episode, &destination, &progress, counts)
                    .map(|()| JobResponse::Saved)
            }
        }
        JobRequest::Uploaded {
            episode,
            destination,
            objects,
        } => store
            .uploaded(&episode, &destination, objects)
            .map(|()| JobResponse::Saved),
        JobRequest::UploadFailed {
            episode,
            destination,
            failure,
        } => match in_bin(store, &episode) {
            // Its folder moved into the bin under it: not a failure.
            Ok(true) => Ok(JobResponse::Saved),
            Ok(false) => store
                .upload_failed(&episode, &destination, failure)
                .map(|()| JobResponse::Saved),
            Err(error) => Err(error),
        },
        JobRequest::Released { job } => store
            .release(job)
            .map(|was_running| JobResponse::Released { was_running }),
        // An upload of an episode deleted by hand meanwhile, failed as its
        // folder moved: given back, to carry on if it is put back.
        JobRequest::Finished {
            job,
            failure: Some(_),
            ..
        } if upload_of_binned(store, job) => store
            .release(job)
            .map(|was_running| JobResponse::Finished { was_running }),
        JobRequest::Finished {
            job,
            failure,
            qa,
            events,
            warnings,
        } => qa
            .map_or(Ok(()), |run| record_qa(queue, run))
            .and_then(|()| events.map_or(Ok(()), |events| record_events(queue, *events)))
            .and_then(|()| store.finish(job, failure, warnings))
            .and_then(|was_running| {
                queue_upload(queue, job)?;
                queue_deletions_after(queue, job)?;
                Ok(JobResponse::Finished { was_running })
            }),
    };
    answered.unwrap_or_else(|error| refusal(&error))
}

/// Queues `episode`'s failed uploads to be tried again now, however
/// recently they failed: asked for by hand, with `kapto upload retry` or the
/// API. Returns their destinations.
///
/// # Errors
///
/// `upload.nothing_to_retry` if none of its uploads failed;
/// `store.query_failed` if writing failed.
pub fn retry_uploads(store: &Writer, episode: &str) -> Result<Vec<String>, KaptoError> {
    let destinations = store.retry_uploads(episode)?;
    if destinations.is_empty() {
        return Err(KaptoError::new(ErrorCode::UploadNothingToRetry)
            .detail(format!("no upload of `{episode}` failed"))
            .context("episode", episode.to_owned()));
    }
    store.enqueue_again(UPLOAD, episode)?;
    Ok(destinations)
}

/// The next job of `kinds` to hand out, if any: failed uploads due again
/// and deletions due queued first, and a deletion no longer allowed ended
/// as done instead of handed out.
fn next(queue: &Queue, kinds: &Kinds) -> Result<Option<JobOrder>, KaptoError> {
    let now = utc_now_ns();
    if kinds.includes(UPLOAD) {
        retry_failed(queue, now)?;
    }
    if kinds.includes(DELETE) {
        let due = {
            let mut retained = queue
                .retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let due = retained.is_none_or(|at| at.elapsed() >= RETENTION_EVERY);
            if due {
                *retained = Some(Instant::now());
            }
            due
        };
        if due {
            if let Some((retention, data_dir)) = &queue.retention {
                retention::queue_deletions(&queue.store, retention, data_dir)?;
            }
            let bin = queue
                .bin
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(bin) = bin {
                episode_bin::queue_emptying(&queue.store, &bin, now)?;
            }
        }
    }
    loop {
        let Some(job) = queue.store.claim_of(kinds.clone())? else {
            return Ok(None);
        };
        if job.kind == DELETE {
            let retention = queue.retention.as_ref().map(|(retention, _)| retention);
            let allowed = if in_bin(&queue.store, &job.episode)? {
                episode_bin::may_empty(&queue.store, &job.episode, job.id)?
            } else {
                match retention {
                    Some(retention) => {
                        retention::may_delete(&queue.store, retention, &job.episode, job.id)?
                    }
                    None => false,
                }
            };
            if !allowed {
                // Nothing to do now; it is queued again once it may go.
                queue.store.finish(job.id, None, Vec::new())?;
                continue;
            }
        }
        let destinations = if job.kind == UPLOAD {
            due(queue, &job.episode, |destination, upload| {
                destination.wants(upload, now)
            })?
        } else {
            Vec::new()
        };
        return Ok(Some(JobOrder {
            id: job.id,
            kind: job.kind,
            episode: job.episode,
            destinations,
        }));
    }
}

/// Deletes `episode` into the bin from the command line, or, if not `into`,
/// puts it back.
fn put_in_bin(queue: &Queue, episode: &str, into: bool) -> Result<JobResponse, KaptoError> {
    let data_dir = bin_dir(queue)?;
    if into {
        let now = ("command line", utc_now_ns());
        episode_bin::put_in_bin(&data_dir, &queue.store, episode, now).map(|binned| {
            JobResponse::Binned {
                binned_utc_ns: binned.binned_utc_ns,
            }
        })
    } else {
        episode_bin::put_back(&data_dir, &queue.store, episode).map(|()| JobResponse::Saved)
    }
}

/// The data directory whose bin serve keeps, once it is set.
fn bin_dir(queue: &Queue) -> Result<PathBuf, KaptoError> {
    queue
        .bin
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|bin| bin.data_dir.clone())
        .ok_or_else(|| {
            KaptoError::new(ErrorCode::StoreBinFailed).detail("serve keeps no bin".to_owned())
        })
}

/// Whether `episode` is in the bin, deleted by hand.
fn in_bin(store: &Writer, episode: &str) -> Result<bool, KaptoError> {
    Ok(store.reader()?.binned(episode)?.is_some())
}

/// Whether job `id` is an upload of an episode in the bin.
fn upload_of_binned(store: &Writer, id: JobId) -> bool {
    store
        .reader()
        .and_then(|reader| reader.job_with_id(id))
        .ok()
        .flatten()
        .is_some_and(|job| job.kind == UPLOAD && matches!(in_bin(store, &job.episode), Ok(true)))
}

/// Looks for episodes to delete at once when job `id` was an upload, which
/// may have made one verified on enough destinations: `after_upload` means
/// right after it.
fn queue_deletions_after(queue: &Queue, id: JobId) -> Result<(), KaptoError> {
    let Some((retention, data_dir)) = &queue.retention else {
        return Ok(());
    };
    let uploaded = queue
        .store
        .reader()?
        .job_with_id(id)?
        .is_some_and(|job| job.kind == UPLOAD);
    if uploaded {
        retention::queue_deletions(&queue.store, retention, data_dir)?;
    }
    Ok(())
}

/// Queues the upload of the episode job `id` was done on if a destination
/// it now qualifies for, as that destination's `upload.when` says, hasn't
/// started its upload: the job's end may be what made it qualify. An
/// upload job ending is checked too, so a destination the episode came to
/// qualify for while it ran is uploaded to next; one it started, done or
/// failed, isn't queued again here, and nothing is after an upload job
/// that failed, which would only fail again at once.
fn queue_upload(queue: &Queue, id: JobId) -> Result<(), KaptoError> {
    if queue.uploads.is_none() {
        return Ok(());
    }
    let Some(job) = queue.store.reader()?.job_with_id(id)? else {
        return Ok(());
    };
    let failed_upload = job.kind == UPLOAD && job.failure.is_some();
    if ![VERIFY, QA, UPLOAD].contains(&job.kind.as_str()) || failed_upload {
        return Ok(());
    }
    if !due(queue, &job.episode, |_, upload| upload.is_none())?.is_empty() {
        queue.store.enqueue_again(UPLOAD, &job.episode)?;
    }
    Ok(())
}

/// Queues again the upload of each episode with an upload that failed
/// longer ago than its destination's `upload.retry_failed_after`, if the
/// episode still qualifies for that destination. An upload job queued or
/// running already is left as it is.
fn retry_failed(queue: &Queue, now: i64) -> Result<(), KaptoError> {
    let Some(uploads) = &queue.uploads else {
        return Ok(());
    };
    let mut episodes: Vec<String> = Vec::new();
    for upload in queue.store.reader()?.failed_uploads()? {
        let waited = uploads.destinations.iter().any(|destination| {
            destination.name == upload.destination && destination.may_retry(&upload, now)
        });
        if waited && episodes.last() != Some(&upload.episode) {
            episodes.push(upload.episode);
        }
    }
    for episode in episodes {
        let retry = due(queue, &episode, |destination, upload| {
            upload.is_some_and(|upload| destination.may_retry(upload, now))
        })?;
        if !retry.is_empty() {
            queue.store.enqueue_again(UPLOAD, &episode)?;
        }
    }
    Ok(())
}

/// The destinations `episode` qualifies for now, as each one's
/// `upload.when` says, whose upload, if it was ever started, `wanted` says
/// to do.
fn due(
    queue: &Queue,
    episode: &str,
    wanted: impl Fn(&Destination, Option<&Upload>) -> bool,
) -> Result<Vec<String>, KaptoError> {
    let Some(uploads) = &queue.uploads else {
        return Ok(Vec::new());
    };
    let reader = queue.store.reader()?;
    let mut due = Vec::new();
    for destination in &uploads.destinations {
        let upload = reader.upload(episode, &destination.name)?;
        if wanted(destination, upload.as_ref())
            && qualifies(destination.when, &reader, &uploads.data_dir, episode)?
        {
            due.push(destination.name.clone());
        }
    }
    Ok(due)
}

/// Whether `episode` qualifies as `when` says: its files checked against
/// its manifest, and, for `finalised_clean`, recorded without trouble, or,
/// for `accepted`, accepted by its quality checks.
fn qualifies(
    when: UploadWhen,
    reader: &Reader,
    data_dir: &Path,
    episode: &str,
) -> Result<bool, KaptoError> {
    let verified = reader
        .job(VERIFY, episode)?
        .is_some_and(|job| job.state == JobState::Done && job.failure.is_none());
    if !verified {
        return Ok(false);
    }
    Ok(match when {
        UploadWhen::Never => false,
        UploadWhen::Finalised => true,
        UploadWhen::FinalisedClean => episode_folder(data_dir, episode)
            .and_then(|folder| Episode::read(&folder).ok())
            .is_some_and(|episode| clean(&episode)),
        UploadWhen::Accepted => reader.qa(episode)?.is_some_and(|run| run.accepted()),
    })
}

/// Whether an episode was recorded without trouble: nothing lost, dropped
/// or recovered, and pre-flight, if it ran, passed every check.
fn clean(episode: &Episode) -> bool {
    episode.recovered.is_none()
        && episode
            .streams
            .iter()
            .all(|stream| stream.dropped_by_us == 0 && stream.lost_upstream == 0)
        && episode
            .preflight
            .as_ref()
            .is_none_or(|preflight| preflight.checks.iter().all(|check| check.passed))
}

/// Records an episode's events, before the job is finished: a job whose
/// events couldn't be recorded isn't done.
fn record_events(queue: &Queue, read: EpisodeEvents) -> Result<(), KaptoError> {
    if EpisodeId::from_str(&read.episode).is_err() {
        return Err(invalid(&format!(
            "events of `{}`, which isn't an episode ID",
            read.episode
        )));
    }
    queue.store.record_events(&read.episode, read.events)
}

/// Records what the quality checks found, before the job is finished: a
/// job whose results couldn't be recorded isn't done.
fn record_qa(queue: &Queue, run: QaRun) -> Result<(), KaptoError> {
    if EpisodeId::from_str(&run.episode).is_err() {
        return Err(invalid(&format!(
            "quality results for `{}`, which isn't an episode ID",
            run.episode
        )));
    }
    queue.store.record_qa(run)
}

fn refusal(error: &KaptoError) -> JobResponse {
    JobResponse::Error {
        code: error.code().id().to_owned(),
        message: error.message(),
    }
}

fn invalid(detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::WorkerInvalidRequest).detail(detail)
}

fn send(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)
}

/// The worker's end of the job socket.
#[derive(Debug)]
pub struct JobClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl JobClient {
    /// Connects to serve's job socket at `path` and says `hello`.
    ///
    /// # Errors
    ///
    /// `worker.serve_unreachable` if nothing listens there or the
    /// connection fails; serve's refusal, e.g. `worker.version_mismatch`.
    pub fn connect(path: &Path) -> Result<Self, KaptoError> {
        let connection = UnixStream::connect(path).map_err(|error| unreachable(path, &error))?;
        let mut client = Self {
            reader: BufReader::new(
                connection
                    .try_clone()
                    .map_err(|error| unreachable(path, &error))?,
            ),
            writer: connection,
        };
        match client.ask(&JobRequest::Hello {
            version: PROTOCOL_VERSION,
        })? {
            JobResponse::Hello { .. } => Ok(client),
            other => Err(unexpected(&other)),
        }
    }

    /// Takes the oldest queued job, or `None` if none is queued.
    ///
    /// # Errors
    ///
    /// `worker.serve_unreachable` if the connection failed; serve's
    /// refusal, e.g. `store.query_failed`.
    pub fn next_job(&mut self) -> Result<Option<JobOrder>, KaptoError> {
        self.next(None, None)
    }

    /// Takes the oldest queued job of any kind but `except`: what a lane
    /// leaving those to another asks.
    ///
    /// # Errors
    ///
    /// As for [`JobClient::next_job`].
    pub fn next_job_except(&mut self, except: &[&str]) -> Result<Option<JobOrder>, KaptoError> {
        self.next(
            None,
            Some(except.iter().map(|kind| (*kind).to_owned()).collect()),
        )
    }

    /// Takes the oldest queued job of one of `kinds`, or `None` if none is
    /// queued: what a lane doing only those asks.
    ///
    /// # Errors
    ///
    /// As for [`JobClient::next_job`].
    pub fn next_job_of(&mut self, kinds: &[&str]) -> Result<Option<JobOrder>, KaptoError> {
        self.next(
            Some(kinds.iter().map(|kind| (*kind).to_owned()).collect()),
            None,
        )
    }

    fn next(
        &mut self,
        kinds: Option<Vec<String>>,
        except: Option<Vec<String>>,
    ) -> Result<Option<JobOrder>, KaptoError> {
        match self.ask(&JobRequest::Next { kinds, except })? {
            JobResponse::Job { job } => Ok(job),
            other => Err(unexpected(&other)),
        }
    }

    /// Starts `episode`'s upload to `destination` under `key`, or carries
    /// on the one started before: the key it has and how far it got.
    ///
    /// # Errors
    ///
    /// As for [`JobClient::next_job`].
    pub fn upload_start(
        &mut self,
        episode: &str,
        destination: &str,
        key: &str,
    ) -> Result<(String, String), KaptoError> {
        let request = JobRequest::UploadStart {
            episode: episode.to_owned(),
            destination: destination.to_owned(),
            key: key.to_owned(),
        };
        match self.ask(&request)? {
            JobResponse::Upload { key, progress } => Ok((key, progress)),
            other => Err(unexpected(&other)),
        }
    }

    /// Reports `episode` being deleted, its folder set aside, with its
    /// `episode.json`, so serve records the deletion before the folder goes.
    ///
    /// # Errors
    ///
    /// `store.deletion_refused` if serve won't have it deleted; otherwise as
    /// for [`JobClient::next_job`].
    pub fn deleting(&mut self, episode: &str, summary: String) -> Result<(), KaptoError> {
        let request = JobRequest::Deleting {
            episode: episode.to_owned(),
            summary,
        };
        match self.ask(&request)? {
            JobResponse::Saved => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Asks serve to try `episode`'s failed uploads again now; their
    /// destinations.
    ///
    /// # Errors
    ///
    /// `upload.nothing_to_retry` if none failed; otherwise as for
    /// [`JobClient::next_job`].
    pub fn retry_uploads(&mut self, episode: &str) -> Result<Vec<String>, KaptoError> {
        let request = JobRequest::RetryUploads {
            episode: episode.to_owned(),
        };
        match self.ask(&request)? {
            JobResponse::Retried { destinations } => Ok(destinations),
            other => Err(unexpected(&other)),
        }
    }

    /// Deletes `episode` by hand, into the bin; when it went in.
    ///
    /// # Errors
    ///
    /// What putting it in the bin reports, as for
    /// [`crate::episode_bin::put_in_bin`]; as for [`JobClient::next_job`].
    pub fn put_in_bin(&mut self, episode: &str) -> Result<i64, KaptoError> {
        let request = JobRequest::PutInBin {
            episode: episode.to_owned(),
        };
        match self.ask(&request)? {
            JobResponse::Binned { binned_utc_ns } => Ok(binned_utc_ns),
            other => Err(unexpected(&other)),
        }
    }

    /// Puts `episode` back from the bin.
    ///
    /// # Errors
    ///
    /// What putting it back reports, as for
    /// [`crate::episode_bin::put_back`]; as for [`JobClient::next_job`].
    pub fn put_back(&mut self, episode: &str) -> Result<(), KaptoError> {
        let request = JobRequest::PutBack {
            episode: episode.to_owned(),
        };
        match self.ask(&request)? {
            JobResponse::Saved => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Reports an upload's step: `request`, one of the upload reports.
    ///
    /// # Errors
    ///
    /// As for [`JobClient::next_job`].
    pub fn upload_report(&mut self, request: &JobRequest) -> Result<(), KaptoError> {
        match self.ask(request)? {
            JobResponse::Saved => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Reports job `job` ended: done, or failed with `failure`, with what
    /// it found.
    ///
    /// # Errors
    ///
    /// As for [`JobClient::next_job`].
    pub fn finished(
        &mut self,
        job: JobId,
        failure: Option<JobFailure>,
        found: Found,
    ) -> Result<(), KaptoError> {
        match self.ask(&JobRequest::Finished {
            job,
            failure,
            qa: found.qa,
            events: found.events.map(Box::new),
            warnings: found.warnings,
        })? {
            JobResponse::Finished { .. } => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    /// Gives job `job` back: stopped part-way, it is queued again without
    /// counting as an attempt.
    ///
    /// # Errors
    ///
    /// As for [`JobClient::next_job`].
    pub fn released(&mut self, job: JobId) -> Result<(), KaptoError> {
        match self.ask(&JobRequest::Released { job })? {
            JobResponse::Released { .. } => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    fn ask(&mut self, request: &JobRequest) -> Result<JobResponse, KaptoError> {
        let lost = |error: &dyn std::fmt::Display| {
            KaptoError::new(ErrorCode::WorkerServeUnreachable)
                .detail(format!("the connection to serve failed: {error}"))
        };
        send(&mut self.writer, request).map_err(|error| lost(&error))?;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err(lost(&"serve closed it")),
            Ok(_) => serde_json::from_str(&line).map_err(|error| lost(&error)),
            Err(error) => Err(lost(&error)),
        }
    }
}

fn unreachable(path: &Path, error: &io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::WorkerServeUnreachable)
        .detail(error.to_string())
        .context("socket", path.display().to_string())
}

/// An answer that isn't the one asked for: serve's refusal with its own
/// cause code, or a protocol mistake.
fn unexpected(response: &JobResponse) -> KaptoError {
    if let JobResponse::Error { code, message } = response {
        let known = ErrorCode::ALL.iter().find(|known| known.id() == code);
        return KaptoError::new(known.copied().unwrap_or(ErrorCode::WorkerInvalidRequest))
            .detail(format!("serve answered: {message}"));
    }
    KaptoError::new(ErrorCode::WorkerInvalidRequest)
        .detail(format!("serve answered something else: {response:?}"))
}
