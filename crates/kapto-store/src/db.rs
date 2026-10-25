// SPDX-License-Identifier: Apache-2.0

//! Runtime state in SQLite: `kapto.db` in the data directory. It holds what
//! Kapto works out while it runs, never settings and never the only copy of
//! anything about a recording: deleting it loses runtime state, and the
//! episode index is rebuilt from the episode folders ([`Writer::rebuild`]).
//!
//! One thread, `srv-db`, owns the only connection that writes; everything
//! else sends it commands through a [`Writer`], so there can't be a second
//! writer. Readers open read-only connections of their own ([`Reader`]),
//! which in WAL mode neither wait for the writer nor hold it up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use kapto_core::clock::utc_now_ns;
use kapto_core::{EpisodeId, ErrorCode, KaptoError};
use rusqlite::{Connection, OpenFlags, Transaction, params};

use crate::binned::{self, BIN_DIR, Binned};
use crate::deletions::{self, Deletion};
use crate::episode::{Episode, OperationMode, PackageIdentity, Recovered};
use crate::events::Event;
use crate::indexed_events;
use crate::jobs::{self, Job, JobFailure, JobId, JobState, Kinds, Requeued};
use crate::manifest::FINALISED_FILE;
use crate::models::{self, AnomalyModel};
use crate::qa::{self, QaRun};
use crate::uploads::{self, Upload, UploadCounts, UploadedObject};

/// The database's file name in the data directory.
pub const DATABASE_FILE: &str = "kapto.db";

/// The schema, table by table. Until Kapto's first release it is changed in
/// place, at [`SCHEMA_VERSION`] 1 and without migrations: a database whose
/// tables aren't these, or made for another version, is deleted and made
/// again, which loses only runtime state, and serve rebuilds the episode
/// index from the episode folders when it starts. From the first release
/// on, changes raise the version with a migration that brings a database up
/// to date.
const SCHEMA: &[&str] = &[
    // The episode index: what listing episodes shows, without reading every
    // `episode.json`. IDs begin with the start time, so they sort by it.
    "CREATE TABLE episodes (
        id TEXT PRIMARY KEY NOT NULL,
        started_utc_ns INTEGER NOT NULL,
        ended_utc_ns INTEGER NOT NULL,
        kapto_version TEXT NOT NULL,
        package_id TEXT,
        package_version TEXT,
        package_hash TEXT,
        profile TEXT,
        profile_hash TEXT,
        mode TEXT,
        task TEXT,
        task_hash TEXT,
        task_name TEXT,
        recovered_at_utc_ns INTEGER,
        recovered_bytes_cut INTEGER,
        recovered_last_message_utc_ns INTEGER,
        recovered_reason TEXT
    ) STRICT;
    CREATE TABLE episode_streams (
        episode TEXT NOT NULL REFERENCES episodes (id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        stream TEXT NOT NULL,
        received INTEGER NOT NULL,
        written INTEGER NOT NULL,
        dropped_by_us INTEGER NOT NULL,
        lost_upstream INTEGER NOT NULL,
        PRIMARY KEY (episode, position)
    ) STRICT;",
    // The job queue: work the worker does on episodes after recording.
    jobs::SCHEMA,
    // What the quality checks found on each episode.
    qa::SCHEMA,
    // What anomaly detectors learned from each package and task's episodes.
    models::SCHEMA,
    // What happened in each episode, read from its recording.
    indexed_events::SCHEMA,
    // Each episode's upload to each destination, and where it went.
    uploads::SCHEMA,
    // Episodes deleted here once verified elsewhere, with their summaries.
    deletions::SCHEMA,
    // Episodes deleted by hand, in the bin until it is emptied.
    binned::SCHEMA,
];

/// The schema's version, kept in the database's `user_version`. It stays 1
/// until Kapto's first release.
pub const SCHEMA_VERSION: u32 = 1;

/// A finalised episode as the index holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedEpisode {
    /// Its ID, which is also its folder's name.
    pub id: String,
    /// When recording started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// When it ended, the same way.
    pub ended_utc_ns: i64,
    /// The Kapto version that recorded it.
    pub kapto_version: String,
    /// The robot package it was recorded with, if any.
    pub package: Option<PackageIdentity>,
    /// The ID of the session profile it was recorded with, if any.
    pub profile: Option<String>,
    /// What that profile said, as a content hash.
    pub profile_hash: Option<String>,
    /// How the robot was driven, as its profile said.
    pub mode: Option<OperationMode>,
    /// The ID of the task it was recorded for, if any.
    pub task: Option<String>,
    /// What that task said, as a content hash.
    pub task_hash: Option<String>,
    /// What people call that task.
    pub task_name: Option<String>,
    /// How it was recovered, if recording stopped without finishing it.
    pub recovered: Option<Recovered>,
    /// Each stream's counts, in declaration order.
    pub streams: Vec<IndexedStream>,
}

/// An indexed episode as listed, with what its quality checks decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedEpisode {
    /// The episode.
    pub episode: IndexedEpisode,
    /// Whether it was accepted: no gate failed. `None` until it is checked.
    pub accepted: Option<bool>,
    /// How many of its events are of each kind; `None` until they are read
    /// from its recording.
    pub events: Option<BTreeMap<String, u64>>,
}

/// One stream's counts in an indexed episode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedStream {
    /// The stream's ID.
    pub stream: String,
    /// Messages that reached Kapto.
    pub received: u64,
    /// Received messages stored in the recording.
    pub written: u64,
    /// Received messages Kapto discarded.
    pub dropped_by_us: u64,
    /// Messages the source sent that never reached Kapto.
    pub lost_upstream: u64,
}

impl From<Episode> for IndexedEpisode {
    fn from(episode: Episode) -> Self {
        Self {
            id: episode.id,
            started_utc_ns: episode.started_utc_ns,
            ended_utc_ns: episode.ended_utc_ns,
            kapto_version: episode.kapto_version,
            package: episode.package,
            profile: episode.profile.as_ref().map(|profile| profile.id.clone()),
            profile_hash: episode
                .profile
                .as_ref()
                .and_then(|profile| profile.hash.clone()),
            mode: episode.profile.and_then(|profile| profile.mode),
            task: episode.task.as_ref().map(|task| task.id.clone()),
            task_hash: episode.task.as_ref().and_then(|task| task.hash.clone()),
            task_name: episode.task.and_then(|task| task.name),
            recovered: episode.recovered,
            streams: episode
                .streams
                .into_iter()
                .map(|stream| IndexedStream {
                    stream: stream.id,
                    received: stream.received,
                    written: stream.written,
                    dropped_by_us: stream.dropped_by_us,
                    lost_upstream: stream.lost_upstream,
                })
                .collect(),
        }
    }
}

/// What the writer thread is asked to do, with where to answer.
enum Command {
    /// Add an episode to the index, or replace what it holds for it.
    Index(Box<IndexedEpisode>, Reply<()>),
    /// Make the index hold exactly these episodes, and those deleted here;
    /// answers with each deleted one whose summary couldn't be read.
    Replace(Vec<IndexedEpisode>, Reply<Vec<(String, String)>>),
    /// Queue a job of a kind for an episode.
    Enqueue(String, String, Reply<JobId>),
    /// Queue a job of a kind for an episode again once it ended.
    EnqueueAgain(String, String, Reply<Job>),
    /// Start the oldest queued job.
    Claim(Kinds, Reply<Option<Job>>),
    /// End a running job, failed or not.
    Finish(JobId, Option<JobFailure>, Vec<JobFailure>, Reply<bool>),
    /// Put a running job back in the queue, not counting the attempt.
    Release(JobId, Reply<bool>),
    /// Queue again the jobs left running, up to this many starts each.
    Requeue(u32, Reply<Requeued>),
    /// Replace an episode's quality-check results.
    RecordQa(QaRun, Reply<()>),
    /// Replace an episode's events.
    RecordEvents(String, Vec<Event>, Reply<()>),
    /// Start an episode's upload to a destination under a key, or carry
    /// it on.
    StartUpload(String, String, String, Reply<Upload>),
    /// Save how far an upload got.
    UploadProgress(String, String, String, UploadCounts, Reply<()>),
    /// Record an upload as done, with its objects.
    Uploaded(String, String, Vec<UploadedObject>, Reply<()>),
    /// Record an upload as failed.
    UploadFailed(String, String, JobFailure, Reply<()>),
    /// Record an episode deleted here.
    RecordDeletion(Box<Deletion>, Reply<()>),
    /// Record an episode put in the bin.
    Bin(Box<Binned>, Reply<()>),
    /// Forget an episode was in the bin, answering whether it was.
    Unbin(String, Reply<bool>),
    /// End an episode's queued jobs as done, answering how many.
    EndQueued(String, Reply<usize>),
    /// Make an episode's failed uploads due now.
    RetryUploads(String, Reply<Vec<String>>),
}

type Reply<T> = mpsc::Sender<Result<T, KaptoError>>;

/// Sends changes to the thread that owns the database's only writing
/// connection. Clones send to the same thread, which ends once the last
/// clone is dropped.
#[derive(Clone, Debug)]
pub struct Writer {
    path: PathBuf,
    commands: mpsc::Sender<Command>,
}

impl Writer {
    /// Opens the database at `path`, creating it if needed or if it was
    /// made for another schema version, and starts the `srv-db` thread that
    /// owns it.
    ///
    /// # Errors
    ///
    /// `store.open_failed` if it can't be opened or made, isn't a database,
    /// or the thread can't start.
    pub fn open(path: &Path) -> Result<Self, KaptoError> {
        let connection = open(path)?;
        let (commands, received) = mpsc::channel();
        thread::Builder::new()
            .name("srv-db".into())
            .spawn(move || serve(connection, &received))
            .map_err(|error| {
                KaptoError::new(ErrorCode::StoreOpenFailed)
                    .detail(format!("can't start its writer thread: {error}"))
            })?;
        Ok(Self {
            path: path.to_owned(),
            commands,
        })
    }

    /// Adds a finalised episode to the index, or replaces what it holds for
    /// it.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn index(&self, episode: IndexedEpisode) -> Result<(), KaptoError> {
        self.ask(|reply| Command::Index(Box::new(episode), reply))
    }

    /// Makes the index hold exactly the finalised episodes in `data_dir`,
    /// and those deleted from it once verified elsewhere, from the summaries
    /// kept of them. Returns each episode folder whose `episode.json`
    /// couldn't be read, or each deleted episode's would-be folder whose
    /// summary couldn't, with why; those are left out.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed; the index is then unchanged.
    pub fn rebuild(&self, data_dir: &Path) -> Result<Vec<(PathBuf, KaptoError)>, KaptoError> {
        let mut episodes = Vec::new();
        let mut unreadable = Vec::new();
        for folder in finalised_folders(data_dir) {
            match Episode::read(&folder) {
                Ok(episode) => episodes.push(episode.into()),
                Err(error) => unreadable.push((folder, error)),
            }
        }
        let deleted = self.ask(|reply| Command::Replace(episodes, reply))?;
        unreadable.extend(deleted.into_iter().map(|(id, why)| {
            let error = KaptoError::new(ErrorCode::EpisodeUnreadable)
                .detail(format!("the summary kept of it, deleted here: {why}"));
            (data_dir.join(id), error)
        }));
        Ok(unreadable)
    }

    /// Makes each failed upload of `episode` due to be tried again now,
    /// however recently it failed: what asking for it by hand does. Returns
    /// their destinations, none if none failed.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn retry_uploads(&self, episode: &str) -> Result<Vec<String>, KaptoError> {
        let episode = episode.to_owned();
        self.ask(|reply| Command::RetryUploads(episode, reply))
    }

    /// Records an episode deleted here, as now: what [`Writer::rebuild`]
    /// keeps it in the index by.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn record_deletion(&self, deletion: Deletion) -> Result<(), KaptoError> {
        self.ask(|reply| Command::RecordDeletion(Box::new(deletion), reply))
    }

    /// Records an episode put in the bin, its folder already moved there.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn record_binned(&self, binned: Binned) -> Result<(), KaptoError> {
        self.ask(|reply| Command::Bin(Box::new(binned), reply))
    }

    /// Forgets that `episode` is in the bin, as it is put back or deleted;
    /// whether it was.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn forget_binned(&self, episode: &str) -> Result<bool, KaptoError> {
        let episode = episode.to_owned();
        self.ask(|reply| Command::Unbin(episode, reply))
    }

    /// Ends each queued job of `episode` as done, as it is deleted; how
    /// many there were.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn end_queued_jobs(&self, episode: &str) -> Result<usize, KaptoError> {
        let episode = episode.to_owned();
        self.ask(|reply| Command::EndQueued(episode, reply))
    }

    /// Queues a job of `kind` for `episode`, unless one already is, in any
    /// state. Returns the job's number either way.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn enqueue(&self, kind: &str, episode: &str) -> Result<JobId, KaptoError> {
        let (kind, episode) = (kind.to_owned(), episode.to_owned());
        self.ask(|reply| Command::Enqueue(kind, episode, reply))
    }

    /// Queues `kind` for `episode` unless it is queued or being done: one
    /// that ended, done or failed, is queued again as if new, e.g. a replay
    /// asked for again after it left the cache. Returns the job.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn enqueue_again(&self, kind: &str, episode: &str) -> Result<Job, KaptoError> {
        let (kind, episode) = (kind.to_owned(), episode.to_owned());
        self.ask(|reply| Command::EnqueueAgain(kind, episode, reply))
    }

    /// Starts the oldest queued job and returns it, or `None` if no job is
    /// queued.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn claim(&self) -> Result<Option<Job>, KaptoError> {
        self.claim_of(Kinds::Any)
    }

    /// Starts the oldest queued job of `kinds`, as [`Writer::claim`] does
    /// any: what a lane of the worker asks for.
    ///
    /// # Errors
    ///
    /// As [`Writer::claim`].
    pub fn claim_of(&self, kinds: Kinds) -> Result<Option<Job>, KaptoError> {
        self.ask(|reply| Command::Claim(kinds, reply))
    }

    /// Ends running job `id`: done, or failed with `failure`, with what it
    /// couldn't do in full (`warnings`). Returns whether it was running;
    /// one that wasn't is left as it was.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn finish(
        &self,
        id: JobId,
        failure: Option<JobFailure>,
        warnings: Vec<JobFailure>,
    ) -> Result<bool, KaptoError> {
        self.ask(|reply| Command::Finish(id, failure, warnings, reply))
    }

    /// Puts running job `id` back in the queue, as if it hadn't been
    /// started: the worker stopped it part-way on purpose, so it doesn't
    /// count as an attempt. Returns whether it was running.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn release(&self, id: JobId) -> Result<bool, KaptoError> {
        self.ask(|reply| Command::Release(id, reply))
    }

    /// Queues again every job left running when its worker or serve
    /// stopped, and fails with `worker.job_interrupted` those already
    /// started `max_attempts` times, so a job that stops whatever runs it
    /// can't do so forever.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn requeue_interrupted(&self, max_attempts: u32) -> Result<Requeued, KaptoError> {
        self.ask(|reply| Command::Requeue(max_attempts, reply))
    }

    /// Records `run` as its episode's quality-check results, replacing any
    /// it had.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn record_qa(&self, run: QaRun) -> Result<(), KaptoError> {
        self.ask(|reply| Command::RecordQa(run, reply))
    }

    /// Replaces the events held for `episode` with `events`, read from
    /// its recording, in order.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn record_events(&self, episode: &str, events: Vec<Event>) -> Result<(), KaptoError> {
        let episode = episode.to_owned();
        self.ask(|reply| Command::RecordEvents(episode, events, reply))
    }

    /// Starts `episode`'s upload to `destination` under `key`, or, if it
    /// was started before, carries it on under the key it had then: an
    /// episode never moves because a layout changed.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn start_upload(
        &self,
        episode: &str,
        destination: &str,
        key: &str,
    ) -> Result<Upload, KaptoError> {
        let (episode, destination, key) = (episode.into(), destination.into(), key.into());
        self.ask(|reply| Command::StartUpload(episode, destination, key, reply))
    }

    /// Saves how far `episode`'s upload to `destination` got, as the upload
    /// engine wrote it.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn upload_progress(
        &self,
        episode: &str,
        destination: &str,
        progress: &str,
        counts: UploadCounts,
    ) -> Result<(), KaptoError> {
        let (episode, destination, progress) =
            (episode.into(), destination.into(), progress.into());
        self.ask(|reply| Command::UploadProgress(episode, destination, progress, counts, reply))
    }

    /// Records `episode` as uploaded to `destination`, its marker written,
    /// with every object of it there.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn uploaded(
        &self,
        episode: &str,
        destination: &str,
        objects: Vec<UploadedObject>,
    ) -> Result<(), KaptoError> {
        let (episode, destination) = (episode.into(), destination.into());
        self.ask(|reply| Command::Uploaded(episode, destination, objects, reply))
    }

    /// Records `episode`'s upload to `destination` as failed with
    /// `failure`, keeping how far it got for the next try. An upload
    /// already done stays done.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if writing failed.
    pub fn upload_failed(
        &self,
        episode: &str,
        destination: &str,
        failure: JobFailure,
    ) -> Result<(), KaptoError> {
        let (episode, destination) = (episode.into(), destination.into());
        self.ask(|reply| Command::UploadFailed(episode, destination, failure, reply))
    }

    /// A read-only connection to the same database.
    ///
    /// # Errors
    ///
    /// `store.open_failed` if it can't be opened.
    pub fn reader(&self) -> Result<Reader, KaptoError> {
        Reader::open(&self.path)
    }

    fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, KaptoError> {
        let stopped = || {
            KaptoError::new(ErrorCode::StoreQueryFailed)
                .detail("the database's writer thread stopped")
        };
        let (reply, answer) = mpsc::channel();
        self.commands.send(command(reply)).map_err(|_| stopped())?;
        answer.recv().map_err(|_| stopped())?
    }
}

/// The writer thread: carries out each command in a transaction of its own.
fn serve(mut connection: Connection, commands: &mpsc::Receiver<Command>) {
    // Whoever asked may have stopped waiting; the change stands anyway.
    for command in commands {
        let connection = &mut connection;
        let now = utc_now_ns();
        match command {
            Command::Index(episode, reply) => {
                let _ = reply.send(write(connection, |tx| insert(tx, &episode)));
            }
            Command::Replace(episodes, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    tx.execute("DELETE FROM episodes", [])?;
                    episodes
                        .iter()
                        .try_for_each(|episode| insert(tx, episode))?;
                    // Deleted here, but still episodes: from their summaries.
                    let mut unreadable = Vec::new();
                    for (id, summary) in deletions::not_indexed(tx)? {
                        match Episode::parse(summary.as_bytes()) {
                            Ok(episode) => insert(tx, &episode.into())?,
                            Err(why) => unreadable.push((id, why)),
                        }
                    }
                    Ok(unreadable)
                }));
            }
            Command::Enqueue(kind, episode, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    jobs::enqueue(tx, &kind, &episode, now)
                }));
            }
            Command::EnqueueAgain(kind, episode, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    jobs::enqueue_again(tx, &kind, &episode, now)
                }));
            }
            Command::Claim(kinds, reply) => {
                let _ = reply.send(write(connection, |tx| jobs::claim(tx, &kinds, now)));
            }
            Command::Finish(id, failure, warnings, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    jobs::finish(tx, id, failure.as_ref(), &warnings, now)
                }));
            }
            Command::Release(id, reply) => {
                let _ = reply.send(write(connection, |tx| jobs::release(tx, id)));
            }
            Command::RecordQa(run, reply) => {
                let _ = reply.send(write(connection, |tx| qa::record(tx, &run)));
            }
            Command::RecordEvents(episode, events, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    indexed_events::record(tx, &episode, &events, now)
                }));
            }
            Command::StartUpload(episode, destination, key, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    uploads::start(tx, &episode, &destination, &key, now)
                }));
            }
            Command::UploadProgress(episode, destination, progress, counts, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    uploads::progress(tx, (&episode, &destination), &progress, counts, now)
                }));
            }
            Command::Uploaded(episode, destination, objects, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    uploads::uploaded(tx, &episode, &destination, &objects, now)
                }));
            }
            Command::RetryUploads(episode, reply) => {
                let _ = reply.send(write(connection, |tx| uploads::retry_now(tx, &episode)));
            }
            Command::RecordDeletion(deletion, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    deletions::record(tx, &deletion, now)
                }));
            }
            Command::Bin(binned, reply) => {
                let _ = reply.send(write(connection, |tx| binned::record(tx, &binned)));
            }
            Command::Unbin(episode, reply) => {
                let _ = reply.send(write(connection, |tx| binned::remove(tx, &episode)));
            }
            Command::EndQueued(episode, reply) => {
                let _ = reply.send(write(connection, |tx| jobs::end_queued(tx, &episode, now)));
            }
            Command::UploadFailed(episode, destination, failure, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    uploads::failed(tx, &episode, &destination, &failure, now)
                }));
            }
            Command::Requeue(max_attempts, reply) => {
                let _ = reply.send(write(connection, |tx| {
                    jobs::requeue_interrupted(tx, max_attempts, now)
                }));
            }
        }
    }
}

fn write<T>(
    connection: &mut Connection,
    change: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
) -> Result<T, KaptoError> {
    let transaction = connection.transaction().map_err(query_failed)?;
    let done = change(&transaction).map_err(query_failed)?;
    transaction.commit().map_err(query_failed)?;
    Ok(done)
}

fn insert(tx: &Transaction<'_>, episode: &IndexedEpisode) -> rusqlite::Result<()> {
    // Its streams go with it.
    tx.execute("DELETE FROM episodes WHERE id = ?1", [&episode.id])?;
    let package = episode.package.as_ref();
    let recovered = episode.recovered.as_ref();
    tx.execute(
        "INSERT INTO episodes
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            episode.id,
            episode.started_utc_ns,
            episode.ended_utc_ns,
            episode.kapto_version,
            package.map(|package| &package.id),
            package.map(|package| &package.version),
            package.map(|package| &package.hash),
            episode.profile,
            episode.profile_hash,
            episode.mode.map(OperationMode::as_str),
            episode.task,
            episode.task_hash,
            episode.task_name,
            recovered.map(|recovered| recovered.at_utc_ns),
            recovered.map(|recovered| recovered.bytes_cut),
            recovered.and_then(|recovered| recovered.last_message_utc_ns),
            recovered.map(|recovered| &recovered.reason),
        ],
    )?;
    let mut statement =
        tx.prepare_cached("INSERT INTO episode_streams VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
    for (position, stream) in episode.streams.iter().enumerate() {
        statement.execute(params![
            episode.id,
            position,
            stream.stream,
            stream.received,
            stream.written,
            stream.dropped_by_us,
            stream.lost_upstream,
        ])?;
    }
    Ok(())
}

/// A read-only connection to the database.
#[derive(Debug)]
pub struct Reader(Connection);

impl Reader {
    /// Opens the database at `path` for reading. It must already exist:
    /// [`Writer::open`] creates it.
    ///
    /// # Errors
    ///
    /// `store.open_failed` if it can't be opened.
    pub fn open(path: &Path) -> Result<Self, KaptoError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        Connection::open_with_flags(path, flags)
            .and_then(|connection| {
                connection.busy_timeout(BUSY_TIMEOUT)?;
                Ok(connection)
            })
            .map(Self)
            .map_err(|error| open_failed(path, &error))
    }

    /// The `limit` newest indexed episodes, newest first.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn episodes(&self, limit: usize) -> Result<Vec<IndexedEpisode>, KaptoError> {
        self.list(limit, None, None, false)
            .map(|listed| listed.into_iter().map(|listed| listed.episode).collect())
    }

    /// The `limit` newest indexed episodes, newest first, each with whether
    /// its quality checks accepted it and how many events of each kind it
    /// holds; only those `accepted` or not, if asked, which leaves out the
    /// episodes not checked yet, and only those with an `event` of that
    /// kind, if asked. Only those in the bin if `in_bin`, and none of them
    /// otherwise.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn list(
        &self,
        limit: usize,
        accepted: Option<bool>,
        event: Option<&str>,
        in_bin: bool,
    ) -> Result<Vec<ListedEpisode>, KaptoError> {
        // Accepted unless a gate failed; unknown until checked.
        let mut episodes = self
            .0
            .prepare_cached(
                "SELECT * FROM (
                     SELECT episodes.*, CASE
                         WHEN NOT EXISTS (SELECT 1 FROM qa_runs WHERE episode = id) THEN NULL
                         WHEN EXISTS (SELECT 1 FROM qa_results WHERE episode = id
                                      AND kind = 'gate' AND verdict = 'failed') THEN 0
                         ELSE 1
                     END AS accepted
                     FROM episodes)
                 WHERE (?2 IS NULL OR accepted = ?2)
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM episode_events
                                              WHERE episode = id AND kind = ?3))
                   AND ?4 = EXISTS (SELECT 1 FROM binned WHERE binned.episode = id)
                 ORDER BY id DESC LIMIT ?1",
            )
            .map_err(query_failed)?;
        let mut streams = self
            .0
            .prepare_cached(
                "SELECT stream, received, written, dropped_by_us, lost_upstream
                 FROM episode_streams WHERE episode = ?1 ORDER BY position",
            )
            .map_err(query_failed)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = episodes.query_map(params![limit, accepted, event, in_bin], |row| {
            let package = match (row.get(4)?, row.get(5)?, row.get(6)?) {
                (Some(id), Some(version), Some(hash)) => {
                    Some(PackageIdentity { id, version, hash })
                }
                _ => None,
            };
            let recovered = match (row.get(13)?, row.get(14)?, row.get(16)?) {
                (Some(at_utc_ns), Some(bytes_cut), Some(reason)) => Some(Recovered {
                    at_utc_ns,
                    bytes_cut,
                    last_message_utc_ns: row.get(15)?,
                    reason,
                }),
                _ => None,
            };
            let episode = IndexedEpisode {
                id: row.get(0)?,
                started_utc_ns: row.get(1)?,
                ended_utc_ns: row.get(2)?,
                kapto_version: row.get(3)?,
                package,
                profile: row.get(7)?,
                profile_hash: row.get(8)?,
                mode: row
                    .get::<_, Option<String>>(9)?
                    .and_then(|mode| OperationMode::parse(&mode)),
                task: row.get(10)?,
                task_hash: row.get(11)?,
                task_name: row.get(12)?,
                recovered,
                streams: Vec::new(),
            };
            Ok(ListedEpisode {
                episode,
                accepted: row.get(17)?,
                events: None,
            })
        });
        let mut listed = rows
            .and_then(Iterator::collect::<rusqlite::Result<Vec<_>>>)
            .map_err(query_failed)?;
        for ListedEpisode {
            episode, events, ..
        } in &mut listed
        {
            *events = indexed_events::counts(&self.0, &episode.id).map_err(query_failed)?;
            episode.streams = streams
                .query_map([&episode.id], |row| {
                    Ok(IndexedStream {
                        stream: row.get(0)?,
                        received: row.get(1)?,
                        written: row.get(2)?,
                        dropped_by_us: row.get(3)?,
                        lost_upstream: row.get(4)?,
                    })
                })
                .and_then(Iterator::collect)
                .map_err(query_failed)?;
        }
        Ok(listed)
    }

    /// The quality-check results of `episode`, if it has any.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn qa(&self, episode: &str) -> Result<Option<QaRun>, KaptoError> {
        qa::read(&self.0, episode).map_err(query_failed)
    }

    /// `episode`'s events, in the order its recording holds them, or
    /// `None` if they weren't read from it yet.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn events(&self, episode: &str) -> Result<Option<Vec<Event>>, KaptoError> {
        indexed_events::read(&self.0, episode).map_err(query_failed)
    }

    /// The statistics of the `limit` newest other checked episodes of
    /// `episode`'s session, by episode: those recorded with the same robot
    /// package, profile and task, each with exactly the same content (or
    /// all without one), on the same day in the machine's time zone. What
    /// an episode is compared with to find what is unusual about it.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn session_statistics(
        &self,
        episode: &str,
        limit: usize,
    ) -> Result<BTreeMap<String, BTreeMap<String, serde_json::Value>>, KaptoError> {
        qa::session(&self.0, episode, i64::try_from(limit).unwrap_or(i64::MAX))
            .map_err(query_failed)
    }

    /// The newest model of `kind` learned for `episode`'s robot package and
    /// task, if there is one.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn latest_model(
        &self,
        episode: &str,
        kind: &str,
    ) -> Result<Option<AnomalyModel>, KaptoError> {
        models::latest(&self.0, episode, kind).map_err(query_failed)
    }

    /// The statistics of the `limit` newest accepted episodes recorded with
    /// `episode`'s robot package and task, other than it, by episode: what
    /// a model of them learns from. An episode not checked yet, or one a
    /// gate rejected, isn't learned from.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn training_statistics(
        &self,
        episode: &str,
        limit: usize,
    ) -> Result<BTreeMap<String, BTreeMap<String, serde_json::Value>>, KaptoError> {
        models::training(&self.0, episode, i64::try_from(limit).unwrap_or(i64::MAX))
            .map_err(query_failed)
    }

    /// The `limit` newest jobs, newest first.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn jobs(&self, limit: usize) -> Result<Vec<Job>, KaptoError> {
        jobs::list(&self.0, i64::try_from(limit).unwrap_or(i64::MAX)).map_err(query_failed)
    }

    /// `episode`'s upload to `destination`, if it was ever started.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn upload(&self, episode: &str, destination: &str) -> Result<Option<Upload>, KaptoError> {
        uploads::find(&self.0, episode, destination).map_err(query_failed)
    }

    /// Every upload of `episode`, by destination.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn uploads(&self, episode: &str) -> Result<Vec<Upload>, KaptoError> {
        uploads::of_episode(&self.0, episode).map_err(query_failed)
    }

    /// How `episode` was deleted here, if it was: then it is stored only
    /// at the destinations its deletion names.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn deletion(&self, episode: &str) -> Result<Option<Deletion>, KaptoError> {
        deletions::find(&self.0, episode).map_err(query_failed)
    }

    /// When and from where `episode` went into the bin, if it is in it.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn binned(&self, episode: &str) -> Result<Option<Binned>, KaptoError> {
        binned::find(&self.0, episode).map_err(query_failed)
    }

    /// Every episode in the bin, the longest there first.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn bin(&self) -> Result<Vec<Binned>, KaptoError> {
        binned::all(&self.0).map_err(query_failed)
    }

    /// Every episode deleted here, oldest deletion first: the audit.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn deletions(&self) -> Result<Vec<Deletion>, KaptoError> {
        deletions::all(&self.0).map_err(query_failed)
    }

    /// Every upload that failed, by episode and destination, without its
    /// objects: none are recorded before an upload is done.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn failed_uploads(&self) -> Result<Vec<Upload>, KaptoError> {
        uploads::failed_ones(&self.0).map_err(query_failed)
    }

    /// Job `id`, if there is one.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn job_with_id(&self, id: JobId) -> Result<Option<Job>, KaptoError> {
        jobs::with_id(&self.0, id).map_err(query_failed)
    }

    /// The job of `kind` for `episode`, if one was ever queued.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn job(&self, kind: &str, episode: &str) -> Result<Option<Job>, KaptoError> {
        jobs::find(&self.0, kind, episode).map_err(query_failed)
    }

    /// How many jobs of `kind` are in `state`, e.g. uploads waiting.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn count_jobs(&self, kind: &str, state: JobState) -> Result<u64, KaptoError> {
        jobs::count(&self.0, kind, state).map_err(query_failed)
    }

    /// Every upload of every episode, by episode and destination, without
    /// their objects.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn all_uploads(&self) -> Result<Vec<Upload>, KaptoError> {
        uploads::all(&self.0).map_err(query_failed)
    }

    /// Every job of `episode`, oldest first.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn jobs_of(&self, episode: &str) -> Result<Vec<Job>, KaptoError> {
        jobs::of_episode(&self.0, episode).map_err(query_failed)
    }

    /// Each indexed episode still stored here, oldest first, with when it
    /// started and on how many destinations it is uploaded and verified:
    /// what retention chooses from.
    ///
    /// # Errors
    ///
    /// `store.query_failed` if reading failed.
    pub fn kept_here(&self) -> Result<Vec<(String, i64, u32)>, KaptoError> {
        deletions::kept_here(&self.0).map_err(query_failed)
    }
}

/// The finalised episodes' folders in `data_dir` and in its bin, newest
/// first: episode folders sort by start time, because their IDs begin with
/// it.
#[must_use]
pub fn finalised_folders(data_dir: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = [data_dir.to_path_buf(), data_dir.join(BIN_DIR)]
        .iter()
        .filter_map(|folder| std::fs::read_dir(folder).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| EpisodeId::from_str(name).is_ok())
                && path.join(FINALISED_FILE).exists()
        })
        .collect();
    folders.sort_unstable_by(|a, b| b.file_name().cmp(&a.file_name()));
    folders
}

/// Opens the database for writing, made again if its tables aren't the
/// ones [`SCHEMA`] makes or it was made for another [`SCHEMA_VERSION`].
fn open(path: &Path) -> Result<Connection, KaptoError> {
    let failed = |error: rusqlite::Error| open_failed(path, &error);
    let mut connection = connect(path)?;
    let expected = Connection::open_in_memory()
        .and_then(|blank| {
            for table in SCHEMA {
                blank.execute_batch(table)?;
            }
            tables(&blank)
        })
        .map_err(failed)?;
    let found = tables(&connection).map_err(failed)?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(failed)?;
    if found == expected && version == SCHEMA_VERSION {
        return Ok(connection);
    }
    if !found.is_empty() {
        drop(connection);
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_owned();
            file.push(suffix);
            match std::fs::remove_file(&file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(open_failed(
                        path,
                        &format!("can't remove it, made by another build of Kapto: {error}"),
                    ));
                }
                _ => {}
            }
        }
        connection = connect(path)?;
    }
    let transaction = connection.transaction().map_err(failed)?;
    for table in SCHEMA {
        transaction.execute_batch(table).map_err(failed)?;
    }
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .and_then(|()| transaction.commit())
        .map_err(failed)?;
    Ok(connection)
}

/// How `connection`'s tables and indexes were made, as SQLite keeps it.
fn tables(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    connection
        .prepare("SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type, name")?
        .query_map([], |row| row.get(0))?
        .collect()
}

/// How long a connection waits for another's transaction to end before
/// its own fails as busy: another process (a command run beside serve, or
/// the worker) may be writing for a moment.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens the database's file, creating it if needed, in WAL mode.
fn connect(path: &Path) -> Result<Connection, KaptoError> {
    let failed = |error: rusqlite::Error| open_failed(path, &error);
    let connection = Connection::open(path).map_err(failed)?;
    connection.busy_timeout(BUSY_TIMEOUT).map_err(failed)?;
    // WAL lets readers go on while the writer writes. `NORMAL` syncs at
    // checkpoints rather than every commit: a power cut can lose the last
    // changes, which is runtime state that is rebuilt or redone, never a
    // recording.
    let mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(failed)?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(open_failed(
            path,
            &format!("SQLite kept journal mode `{mode}`"),
        ));
    }
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .and_then(|()| connection.pragma_update(None, "foreign_keys", true))
        .map_err(failed)?;
    Ok(connection)
}

fn open_failed(path: &Path, error: &dyn std::fmt::Display) -> KaptoError {
    KaptoError::new(ErrorCode::StoreOpenFailed)
        .detail(error.to_string())
        .context("file", path.display().to_string())
}

fn query_failed(error: impl std::fmt::Display) -> KaptoError {
    KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use kapto_core::{ClockMap, ClockSample, RecvTime};

    use super::*;
    use crate::episode::{
        BufferSummary, DurabilitySummary, LossDetectionKind, SessionProfile, SessionTask,
        StreamSummary,
    };

    /// An empty folder unique to this test, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("kapto-{}-db-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn database(&self) -> PathBuf {
            self.0.join(DATABASE_FILE)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Episode `id` with two streams, one with counts beyond 32 bits, and,
    /// if `packaged`, a package, a profile, a task and a recovery.
    fn episode(id: &str, packaged: bool) -> IndexedEpisode {
        let stream = |stream: &str, received| IndexedStream {
            stream: stream.to_owned(),
            received,
            written: received - 1,
            dropped_by_us: 1,
            lost_upstream: 2,
        };
        IndexedEpisode {
            id: id.to_owned(),
            started_utc_ns: 1_772_700_400_000_000_000,
            ended_utc_ns: 1_772_700_460_000_000_000,
            kapto_version: "0.1.0".to_owned(),
            package: packaged.then(|| PackageIdentity {
                id: "arm7".to_owned(),
                version: "1.2.0".to_owned(),
                hash: "ab12".to_owned(),
            }),
            profile: packaged.then(|| "lab_ana".to_owned()),
            profile_hash: packaged.then(|| "cd34".to_owned()),
            mode: packaged.then_some(OperationMode::Autonomous),
            task: packaged.then(|| "pick_and_place".to_owned()),
            task_hash: packaged.then(|| "ef56".to_owned()),
            task_name: packaged.then(|| "Pick and place".to_owned()),
            recovered: packaged.then(|| Recovered {
                at_utc_ns: 1_772_700_500_000_000_000,
                bytes_cut: u64::from(u32::MAX) + 3,
                last_message_utc_ns: None,
                reason: "recording stopped".to_owned(),
            }),
            streams: vec![
                stream("arm_joints", 120_000),
                stream("gripper", u64::from(u32::MAX) * 4),
            ],
        }
    }

    const OLDER: &str = "20260305T192345.123Z-1a2b3c4d";
    const NEWER: &str = "20260306T080000.000Z-5e6f7a8b";

    #[test]
    fn episodes_are_listed_with_whether_their_checks_accepted_them_and_filtered_by_it() {
        use crate::qa::{CheckKind, QaResult, QaRun, Verdict};

        const UNCHECKED: &str = "20260307T080000.000Z-9a0b1c2d";
        let dir = TempDir::new("accepted");
        let writer = Writer::open(&dir.database()).unwrap();
        for id in [OLDER, NEWER, UNCHECKED] {
            writer.index(episode(id, false)).unwrap();
        }
        let checked = |episode: &str, kind, verdict| QaRun {
            episode: episode.to_owned(),
            params_version: None,
            computed_utc_ns: 1,
            statistics: BTreeMap::new(),
            results: vec![QaResult {
                check: "streams.complete".to_owned(),
                version: 1,
                kind,
                verdict,
                message: String::new(),
                values: BTreeMap::new(),
                ranges: Vec::new(),
            }],
            model: None,
        };
        // A failed gate rejects; a failed advisory doesn't.
        writer
            .record_qa(checked(OLDER, CheckKind::Gate, Verdict::Failed))
            .unwrap();
        writer
            .record_qa(checked(NEWER, CheckKind::Advisory, Verdict::Failed))
            .unwrap();
        let reader = writer.reader().unwrap();
        let listed = |accepted| {
            reader
                .list(10, accepted, None, false)
                .unwrap()
                .into_iter()
                .map(|listed| (listed.episode.id, listed.accepted))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            listed(None),
            [
                (UNCHECKED.to_owned(), None),
                (NEWER.to_owned(), Some(true)),
                (OLDER.to_owned(), Some(false)),
            ]
        );
        assert_eq!(listed(Some(true)), [(NEWER.to_owned(), Some(true))]);
        assert_eq!(listed(Some(false)), [(OLDER.to_owned(), Some(false))]);
    }

    #[test]
    fn episodes_are_listed_newest_first_with_their_streams_and_package() {
        let dir = TempDir::new("listed");
        let writer = Writer::open(&dir.database()).unwrap();
        writer.index(episode(OLDER, true)).unwrap();
        writer.index(episode(NEWER, false)).unwrap();
        // Indexing again replaces rather than duplicates.
        writer.index(episode(OLDER, true)).unwrap();
        let reader = writer.reader().unwrap();
        assert_eq!(
            reader.episodes(10).unwrap(),
            [episode(NEWER, false), episode(OLDER, true)]
        );
        assert_eq!(reader.episodes(1).unwrap(), [episode(NEWER, false)]);
    }

    #[test]
    fn an_episodes_events_are_kept_in_order_told_from_none_read_and_outlive_a_rebuild() {
        use crate::events::{Event, Phase};

        let dir = TempDir::new("events");
        let writer = Writer::open(&dir.database()).unwrap();
        writer.index(episode(OLDER, false)).unwrap();
        let event = |kind: &str, phase, at_utc_ns| Event {
            kind: kind.to_owned(),
            at_utc_ns,
            until_utc_ns: None,
            phase,
            id: (phase != Phase::Moment).then(|| "i1".to_owned()),
            source: "page".to_owned(),
            text: Some(format!("{kind} here")),
            attributes: BTreeMap::from([("by".to_owned(), "ana".to_owned())]),
        };
        let events = vec![
            event("intervention", Phase::Begin, 20),
            event("note", Phase::Moment, 10),
            event("intervention", Phase::End, 30),
        ];
        let reader = writer.reader().unwrap();
        // Not read yet, then read: none is told from not read.
        assert_eq!(reader.events(OLDER).unwrap(), None);
        writer.record_events(OLDER, Vec::new()).unwrap();
        assert_eq!(reader.events(OLDER).unwrap(), Some(Vec::new()));
        // Read again, replacing: in the order the recording holds them.
        writer.record_events(OLDER, events.clone()).unwrap();
        assert_eq!(reader.events(OLDER).unwrap(), Some(events.clone()));
        // The episode index made again from an empty folder, then the
        // episode back: its events weren't lost meanwhile.
        writer.rebuild(&dir.0).unwrap();
        writer.index(episode(OLDER, false)).unwrap();
        assert_eq!(reader.events(OLDER).unwrap(), Some(events));
    }

    #[test]
    fn an_episode_in_the_bin_is_listed_only_with_it_and_its_jobs_wait_but_its_deletion() {
        let dir = TempDir::new("bin");
        let writer = Writer::open(&dir.database()).unwrap();
        for id in [OLDER, NEWER] {
            writer.index(episode(id, false)).unwrap();
        }
        writer.enqueue("verify", NEWER).unwrap();
        writer.enqueue("verify", OLDER).unwrap();
        writer
            .record_binned(Binned {
                episode: NEWER.to_owned(),
                binned_utc_ns: 7,
                from: "web UI".to_owned(),
            })
            .unwrap();
        let reader = writer.reader().unwrap();
        let listed = |in_bin| {
            reader
                .list(10, None, None, in_bin)
                .unwrap()
                .into_iter()
                .map(|listed| listed.episode.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(listed(false), [OLDER]);
        assert_eq!(listed(true), [NEWER]);
        assert_eq!(
            reader.binned(NEWER).unwrap().map(|binned| binned.from),
            Some("web UI".to_owned())
        );
        assert_eq!(reader.bin().unwrap().len(), 1);
        // Its verification waits; the other episode's goes ahead, and its
        // deletion, which empties the bin, would too.
        assert_eq!(
            writer.claim().unwrap().map(|job| job.episode),
            Some(OLDER.to_owned())
        );
        assert_eq!(writer.claim().unwrap(), None);
        writer.enqueue(jobs::DELETE, NEWER).unwrap();
        let deleting = writer.claim().unwrap().unwrap();
        assert_eq!(
            (deleting.kind.as_str(), deleting.episode.as_str()),
            (jobs::DELETE, NEWER)
        );
        // Put back: listed again, its jobs handed out.
        assert!(writer.forget_binned(NEWER).unwrap());
        assert!(!writer.forget_binned(NEWER).unwrap());
        assert_eq!(listed(false), [NEWER, OLDER]);
        assert_eq!(
            writer.claim().unwrap().map(|job| job.episode),
            Some(NEWER.to_owned())
        );
    }

    #[test]
    fn episodes_are_listed_with_their_events_by_kind_and_found_by_one() {
        use crate::events::{Event, Phase};

        const UNREAD: &str = "20260307T080000.000Z-9a0b1c2d";
        let dir = TempDir::new("by-event");
        let writer = Writer::open(&dir.database()).unwrap();
        for id in [OLDER, NEWER, UNREAD] {
            writer.index(episode(id, false)).unwrap();
        }
        let note = |kind: &str| Event {
            kind: kind.to_owned(),
            at_utc_ns: 1,
            until_utc_ns: None,
            phase: Phase::Moment,
            id: None,
            source: "api".to_owned(),
            text: None,
            attributes: BTreeMap::new(),
        };
        writer
            .record_events(OLDER, vec![note("note"), note("grasp"), note("note")])
            .unwrap();
        writer.record_events(NEWER, Vec::new()).unwrap();
        let reader = writer.reader().unwrap();
        let listed = |event| {
            reader
                .list(10, None, event, false)
                .unwrap()
                .into_iter()
                .map(|listed| (listed.episode.id, listed.events))
                .collect::<Vec<_>>()
        };
        let counted = BTreeMap::from([("grasp".to_owned(), 1), ("note".to_owned(), 2)]);
        assert_eq!(
            listed(None),
            [
                (UNREAD.to_owned(), None),
                (NEWER.to_owned(), Some(BTreeMap::new())),
                (OLDER.to_owned(), Some(counted.clone())),
            ]
        );
        assert_eq!(listed(Some("grasp")), [(OLDER.to_owned(), Some(counted))]);
        assert_eq!(listed(Some("intervention")), []);
    }

    #[test]
    fn a_session_is_the_other_checked_episodes_of_the_same_package_profile_task_and_day() {
        use crate::qa::QaRun;

        let dir = TempDir::new("session");
        let writer = Writer::open(&dir.database()).unwrap();
        // A minute apart, so on the same day in any time zone, and the last
        // two days later, so on another day in any.
        let at = |id: &str, minutes: i64, packaged| {
            let started_utc_ns = 1_772_712_600_000_000_000 + minutes * 60_000_000_000;
            IndexedEpisode {
                started_utc_ns,
                ended_utc_ns: started_utc_ns + 30_000_000_000,
                ..episode(id, packaged)
            }
        };
        let this = at("20260305T121000.000Z-00000001", 0, true);
        let later = at("20260305T121100.000Z-00000002", 1, true);
        let newest = at("20260305T121200.000Z-00000003", 2, true);
        let unchecked = at("20260305T121300.000Z-00000004", 3, true);
        let other_profile = IndexedEpisode {
            profile_hash: Some("9999".to_owned()),
            ..at("20260305T121400.000Z-00000005", 4, true)
        };
        let other_day = at("20260307T121000.000Z-00000006", 2880, true);
        let unpackaged = at("20260305T121100.000Z-00000007", 1, false);
        let alone = at("20260305T121200.000Z-00000008", 2, false);
        for episode in [
            &this,
            &later,
            &newest,
            &unchecked,
            &other_profile,
            &other_day,
            &unpackaged,
            &alone,
        ] {
            writer.index(episode.clone()).unwrap();
            if episode.id != unchecked.id {
                writer
                    .record_qa(QaRun {
                        episode: episode.id.clone(),
                        params_version: None,
                        computed_utc_ns: 1,
                        results: Vec::new(),
                        statistics: BTreeMap::from([(
                            "episode.duration_s".to_owned(),
                            serde_json::json!({"value": 30.0}),
                        )]),
                        model: None,
                    })
                    .unwrap();
            }
        }
        let reader = writer.reader().unwrap();
        let session = |episode: &IndexedEpisode, limit| {
            reader
                .session_statistics(&episode.id, limit)
                .unwrap()
                .into_keys()
                .collect::<Vec<_>>()
        };
        assert_eq!(session(&this, 200), [later.id.as_str(), newest.id.as_str()]);
        // Only the newest, when there are more.
        assert_eq!(session(&this, 1), [newest.id.as_str()]);
        assert_eq!(
            reader.session_statistics(&this.id, 1).unwrap()[&newest.id]["episode.duration_s"],
            serde_json::json!({"value": 30.0})
        );
        // Episodes without a package, profile or task are a session too.
        assert_eq!(session(&alone, 200), [unpackaged.id.as_str()]);
        assert_eq!(session(&other_day, 200), [] as [&str; 0]);
        assert_eq!(
            session(&unchecked, 200),
            [this.id.as_str(), later.id.as_str(), newest.id.as_str()]
        );
    }

    /// Records a check of `episode` with `statistics`, rejected if
    /// `rejected`.
    fn checked(writer: &Writer, episode: &str, rejected: bool, duration_s: f64) {
        use crate::qa::{CheckKind, QaResult, Verdict};
        writer
            .record_qa(QaRun {
                episode: episode.to_owned(),
                params_version: None,
                computed_utc_ns: 1,
                results: vec![QaResult {
                    check: "streams.complete".to_owned(),
                    version: 1,
                    kind: CheckKind::Gate,
                    verdict: if rejected {
                        Verdict::Failed
                    } else {
                        Verdict::Passed
                    },
                    message: String::new(),
                    values: BTreeMap::new(),
                    ranges: Vec::new(),
                }],
                statistics: BTreeMap::from([(
                    "episode.duration_s".to_owned(),
                    serde_json::json!({"value": duration_s}),
                )]),
                model: None,
            })
            .unwrap();
    }

    #[test]
    fn a_model_learns_from_the_accepted_episodes_of_its_package_and_task() {
        let dir = TempDir::new("training");
        let writer = Writer::open(&dir.database()).unwrap();
        let id = |n: u32| format!("2026100{n}T120000.000Z-0000000{n}");
        let other_task = IndexedEpisode {
            task: Some("stack".to_owned()),
            ..episode(&id(5), true)
        };
        for episode in [
            episode(&id(1), true),
            episode(&id(2), true),
            episode(&id(3), true),
            episode(&id(4), true),
            other_task,
            episode(&id(6), false),
            episode(&id(7), true),
        ] {
            writer.index(episode).unwrap();
        }
        // 1 and 2 accepted, 3 rejected, 4 unchecked; 5 for another task, 6
        // without a package; 7 is the one learning.
        for (n, rejected) in [
            (1, false),
            (2, false),
            (3, true),
            (5, false),
            (6, false),
            (7, false),
        ] {
            checked(&writer, &id(n), rejected, f64::from(n));
        }
        let reader = writer.reader().unwrap();
        let learned = |limit| {
            reader
                .training_statistics(&id(7), limit)
                .unwrap()
                .into_keys()
                .collect::<Vec<_>>()
        };
        assert_eq!(learned(200), [id(1), id(2)]);
        assert_eq!(learned(1), [id(2)]);
        assert_eq!(
            reader.training_statistics(&id(7), 1).unwrap()[&id(2)]["episode.duration_s"],
            serde_json::json!({"value": 2.0})
        );
        // Without a package, those without one.
        assert!(reader.training_statistics(&id(6), 200).unwrap().is_empty());
    }

    #[test]
    fn each_model_learned_is_kept_with_its_run_and_never_replaced() {
        let dir = TempDir::new("models");
        let writer = Writer::open(&dir.database()).unwrap();
        let (this, other_task) = (OLDER, NEWER);
        writer.index(episode(this, true)).unwrap();
        writer
            .index(IndexedEpisode {
                task: Some("stack".to_owned()),
                ..episode(other_task, true)
            })
            .unwrap();
        let model = |task: &str, version, weights: &str| AnomalyModel {
            package_id: Some("arm7".to_owned()),
            task: Some(task.to_owned()),
            kind: "anomalies.autoencoder".to_owned(),
            version,
            trained_utc_ns: 1_772_700_400_000_000_000,
            episodes: vec![OLDER.to_owned()],
            settings: serde_json::json!({"variance": 0.9}),
            model: weights.to_owned(),
        };
        let learned = |episode: &str, model| {
            writer.record_qa(QaRun {
                episode: episode.to_owned(),
                params_version: None,
                computed_utc_ns: 1,
                results: Vec::new(),
                statistics: BTreeMap::new(),
                model: Some(Box::new(model)),
            })
        };
        let reader = writer.reader().unwrap();
        assert_eq!(
            reader.latest_model(this, "anomalies.autoencoder").unwrap(),
            None
        );
        learned(this, model("pick_and_place", 1, "first")).unwrap();
        learned(this, model("pick_and_place", 2, "second")).unwrap();
        learned(other_task, model("stack", 1, "stacking")).unwrap();
        assert_eq!(
            reader.latest_model(this, "anomalies.autoencoder").unwrap(),
            Some(model("pick_and_place", 2, "second"))
        );
        assert_eq!(
            reader
                .latest_model(other_task, "anomalies.autoencoder")
                .unwrap()
                .unwrap()
                .model,
            "stacking"
        );
        assert_eq!(reader.latest_model(this, "anomalies.other").unwrap(), None);
        // A version already kept is refused, with the run it came with.
        let again = learned(this, model("pick_and_place", 2, "replaced")).unwrap_err();
        assert_eq!(again.code(), ErrorCode::StoreQueryFailed);
        assert_eq!(
            reader
                .latest_model(this, "anomalies.autoencoder")
                .unwrap()
                .unwrap()
                .model,
            "second"
        );
        // The one before stays: a result may name it.
        let kept: u32 = reader
            .0
            .query_row("SELECT COUNT(*) FROM anomaly_models", [], |row| row.get(0))
            .unwrap();
        assert_eq!(kept, 3);
    }

    /// Episode `id` written into its folder in `data_dir` and finalised.
    fn finalised(data_dir: &Path, id: &str, packaged: bool) -> IndexedEpisode {
        let clock = ClockMap::new(ClockSample {
            mono: RecvTime::from_nanos(1),
            utc_ns: 1_772_700_400_000_000_000,
            uncertainty: Duration::ZERO,
        });
        let indexed = episode(id, packaged);
        let mut written = Episode::new(
            &id.parse().unwrap(),
            (indexed.started_utc_ns, indexed.ended_utc_ns),
            "recording.mcap",
            &clock,
            (
                BufferSummary {
                    batches: 1,
                    peak_batches: 1,
                    memory_bytes: 1,
                },
                DurabilitySummary {
                    sync_interval_ns: 1,
                    syncs: 1,
                    max_sync_ns: 1,
                    p50_unsynced_ns: 1,
                    p99_unsynced_ns: 1,
                    max_unsynced_ns: 1,
                },
            ),
            indexed
                .streams
                .iter()
                .map(|stream| StreamSummary {
                    id: stream.stream.clone(),
                    loss_detection: LossDetectionKind::Sequence,
                    received: stream.received,
                    decoded: stream.received,
                    written: stream.written,
                    dropped_by_us: stream.dropped_by_us,
                    lost_upstream: stream.lost_upstream,
                    disconnects: None,
                    rejected: None,
                    carried: None,
                    pre_roll: None,
                    post_roll: None,
                })
                .collect(),
        );
        written.package.clone_from(&indexed.package);
        written.recovered.clone_from(&indexed.recovered);
        written.profile = indexed.profile.clone().map(|id| SessionProfile {
            id,
            hash: indexed.profile_hash.clone(),
            operator: Some("Ana".to_owned()),
            environment: None,
            mode: indexed.mode,
            tags: Vec::new(),
            notes: None,
        });
        written.task = indexed.task.clone().map(|id| SessionTask {
            id,
            hash: indexed.task_hash.clone(),
            name: indexed.task_name.clone(),
            description: None,
            prompt: None,
            objects: Vec::new(),
            subtasks: Vec::new(),
        });
        let folder = data_dir.join(id);
        fs::create_dir_all(&folder).unwrap();
        written.write(&folder).unwrap();
        fs::write(folder.join(FINALISED_FILE), b"").unwrap();
        IndexedEpisode {
            kapto_version: written.kapto_version,
            ..indexed
        }
    }

    #[test]
    fn deleting_the_database_and_rebuilding_gives_the_same_index() {
        let dir = TempDir::new("rebuilt");
        let writer = Writer::open(&dir.database()).unwrap();
        for (id, packaged) in [(OLDER, true), (NEWER, false)] {
            writer.index(finalised(&dir.0, id, packaged)).unwrap();
        }
        // Neither unfinalised nor unreadable episodes are indexed.
        fs::create_dir(dir.0.join("20260307T080000.000Z-00000000")).unwrap();
        let unreadable = dir.0.join("20260308T080000.000Z-11111111");
        fs::create_dir(&unreadable).unwrap();
        fs::write(unreadable.join(FINALISED_FILE), b"").unwrap();
        let before = writer.reader().unwrap().episodes(10).unwrap();
        drop(writer);
        fs::remove_file(dir.database()).unwrap();

        let writer = Writer::open(&dir.database()).unwrap();
        assert_eq!(writer.reader().unwrap().episodes(10).unwrap(), []);
        let skipped = writer.rebuild(&dir.0).unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, unreadable);
        assert_eq!(skipped[0].1.code(), ErrorCode::EpisodeUnreadable);
        assert_eq!(writer.reader().unwrap().episodes(10).unwrap(), before);

        // A rebuild also forgets episodes whose folders are gone.
        fs::remove_dir_all(dir.0.join(NEWER)).unwrap();
        writer.rebuild(&dir.0).unwrap();
        assert_eq!(writer.reader().unwrap().episodes(10).unwrap(), before[1..]);
    }

    #[test]
    fn an_episode_deleted_here_stays_in_the_index_from_its_summary_with_its_audit_entry() {
        use crate::deletions::{Deletion, DeletionReason, RemoteCopy};

        let dir = TempDir::new("deleted");
        let writer = Writer::open(&dir.database()).unwrap();
        for (id, packaged) in [(OLDER, true), (NEWER, false)] {
            writer.index(finalised(&dir.0, id, packaged)).unwrap();
        }
        let before = writer.reader().unwrap().episodes(10).unwrap();
        let summary =
            fs::read_to_string(dir.0.join(OLDER).join(crate::episode::EPISODE_FILE)).unwrap();
        let copies = vec![RemoteCopy {
            destination: "nas".to_owned(),
            key: format!("fleet-a/arm7/{OLDER}"),
        }];
        writer
            .record_deletion(Deletion {
                episode: OLDER.to_owned(),
                deleted_utc_ns: 0,
                reason: DeletionReason::DiskLow,
                deleted_by: "worker".to_owned(),
                copies: copies.clone(),
                summary,
            })
            .unwrap();
        fs::remove_dir_all(dir.0.join(OLDER)).unwrap();

        // Rebuilt from the folders, as when serve starts: still listed.
        assert_eq!(writer.rebuild(&dir.0).unwrap().len(), 0);
        let reader = writer.reader().unwrap();
        assert_eq!(reader.episodes(10).unwrap(), before);
        let deletion = reader.deletion(OLDER).unwrap().unwrap();
        assert_eq!(
            (
                deletion.reason,
                deletion.deleted_by.as_str(),
                &deletion.copies
            ),
            (DeletionReason::DiskLow, "worker", &copies)
        );
        assert!(deletion.deleted_utc_ns > 0, "when, as recorded");
        assert_eq!(reader.deletion(NEWER).unwrap(), None);
        assert_eq!(reader.deletions().unwrap(), [deletion]);
    }

    #[test]
    fn what_was_indexed_is_still_there_after_opening_again() {
        let dir = TempDir::new("reopened");
        Writer::open(&dir.database())
            .unwrap()
            .index(episode(OLDER, true))
            .unwrap();
        let writer = Writer::open(&dir.database()).unwrap();
        assert_eq!(
            writer.reader().unwrap().episodes(10).unwrap(),
            [episode(OLDER, true)]
        );
    }

    #[test]
    fn a_database_made_by_a_build_with_other_tables_is_made_again_empty() {
        let changes = [
            "DROP TABLE qa_results",
            "ALTER TABLE episodes ADD COLUMN removed_since TEXT",
            "CREATE TABLE gone (id INTEGER)",
        ];
        for (n, change) in changes.into_iter().enumerate() {
            let dir = TempDir::new(&format!("schema-{n}"));
            Writer::open(&dir.database())
                .unwrap()
                .index(episode(OLDER, true))
                .unwrap();
            Connection::open(dir.database())
                .unwrap()
                .execute_batch(change)
                .unwrap();
            let writer = Writer::open(&dir.database()).unwrap();
            assert_eq!(
                writer.reader().unwrap().episodes(10).unwrap(),
                [],
                "{change}"
            );
            writer.index(episode(OLDER, true)).unwrap();
            assert_eq!(writer.reader().unwrap().episodes(10).unwrap().len(), 1);
            assert_eq!(writer.reader().unwrap().qa(OLDER).unwrap(), None);
        }
    }

    #[test]
    fn a_database_made_for_another_schema_version_is_made_again_empty() {
        for other in [SCHEMA_VERSION + 1, 4] {
            let dir = TempDir::new(&format!("version-{other}"));
            Writer::open(&dir.database())
                .unwrap()
                .index(episode(OLDER, true))
                .unwrap();
            Connection::open(dir.database())
                .unwrap()
                .pragma_update(None, "user_version", other)
                .unwrap();
            let writer = Writer::open(&dir.database()).unwrap();
            assert_eq!(
                writer.reader().unwrap().episodes(10).unwrap(),
                [],
                "{other}"
            );
            let version: u32 = Connection::open(dir.database())
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);
        }
    }

    #[test]
    fn a_write_waits_for_another_connections_transaction_rather_than_failing_busy() {
        let dir = TempDir::new("busy");
        let writer = Writer::open(&dir.database()).unwrap();
        let other = Connection::open(dir.database()).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let held = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            other.execute_batch("COMMIT").unwrap();
        });
        writer.index(episode(OLDER, true)).unwrap();
        held.join().unwrap();
        assert_eq!(writer.reader().unwrap().episodes(10).unwrap().len(), 1);
    }

    #[test]
    fn a_database_with_the_same_tables_is_kept() {
        let dir = TempDir::new("same-schema");
        Writer::open(&dir.database())
            .unwrap()
            .index(episode(OLDER, true))
            .unwrap();
        let writer = Writer::open(&dir.database()).unwrap();
        assert_eq!(writer.reader().unwrap().episodes(10).unwrap().len(), 1);
    }

    #[test]
    fn a_file_that_is_not_a_database_or_cannot_be_created_is_refused_with_its_cause() {
        let dir = TempDir::new("unopenable");
        fs::write(
            dir.database(),
            b"not a database, though long enough to look at",
        )
        .unwrap();
        let error = Writer::open(&dir.database()).unwrap_err();
        assert_eq!(error.code(), ErrorCode::StoreOpenFailed);
        let inside_a_file = dir.database().join(DATABASE_FILE);
        assert_eq!(
            Writer::open(&inside_a_file).unwrap_err().code(),
            ErrorCode::StoreOpenFailed
        );
        assert_eq!(
            Reader::open(&dir.0.join("missing.db")).unwrap_err().code(),
            ErrorCode::StoreOpenFailed
        );
    }

    #[test]
    fn a_failed_write_changes_nothing_and_reports_its_cause() {
        let dir = TempDir::new("failed");
        let writer = Writer::open(&dir.database()).unwrap();
        writer.index(episode(OLDER, true)).unwrap();
        // Replacing an episode with counts SQLite can't hold leaves the old
        // row and its streams as they were.
        let mut too_large = episode(OLDER, false);
        too_large.streams[1].received = u64::MAX;
        let error = writer.index(too_large).unwrap_err();
        assert_eq!(error.code(), ErrorCode::StoreQueryFailed);
        assert_eq!(
            writer.reader().unwrap().episodes(10).unwrap(),
            [episode(OLDER, true)]
        );
    }

    fn object(path: &str) -> UploadedObject {
        UploadedObject {
            path: path.to_owned(),
            key: format!("fleet-a/arm7/{path}"),
            bytes: 10,
            sha256: "a".repeat(64),
            e_tag: Some("\"etag\"".to_owned()),
        }
    }

    #[test]
    fn an_upload_keeps_its_first_key_and_its_progress_and_counts_only_once_done() {
        use crate::uploads::UploadState;

        let dir = TempDir::new("uploads");
        let writer = Writer::open(&dir.database()).unwrap();
        let started = writer.start_upload("e1", "nas", "fleet-a/arm7/e1").unwrap();
        assert_eq!(
            (started.state, started.progress.as_str()),
            (UploadState::Uploading, "{}")
        );
        let counts = UploadCounts {
            bytes_sent: 16,
            bytes_total: 40,
            throttled: 2,
        };
        writer
            .upload_progress("e1", "nas", "{\"files\":{}}", counts)
            .unwrap();
        let reported = writer
            .reader()
            .unwrap()
            .upload("e1", "nas")
            .unwrap()
            .unwrap();
        assert_eq!(reported.counts, counts);
        assert!(
            reported.reported_utc_ns.is_some(),
            "when, for the throughput"
        );

        // Started again, as after a restart or with another layout: the key
        // and the progress it had.
        let again = writer.start_upload("e1", "nas", "other/layout/e1").unwrap();
        assert_eq!(again.key, "fleet-a/arm7/e1");
        assert_eq!(again.progress, "{\"files\":{}}");

        let failure = JobFailure {
            code: "upload.unreachable".into(),
            message: "connection reset".into(),
        };
        writer.upload_failed("e1", "nas", failure.clone()).unwrap();
        let reader = writer.reader().unwrap();
        let failed = reader.upload("e1", "nas").unwrap().unwrap();
        assert_eq!(
            (failed.state, failed.failure),
            (UploadState::Failed, Some(failure.clone()))
        );
        assert_eq!(failed.progress, "{\"files\":{}}", "kept for the next try");
        assert!(
            failed.failed_utc_ns.is_some(),
            "when, to try it again after"
        );
        let listed: Vec<_> = reader
            .failed_uploads()
            .unwrap()
            .into_iter()
            .map(|upload| (upload.episode, upload.destination))
            .collect();
        assert_eq!(listed, [("e1".to_owned(), "nas".to_owned())]);

        let retried = writer.start_upload("e1", "nas", "fleet-a/arm7/e1").unwrap();
        assert_eq!(
            (retried.state, &retried.failure),
            (UploadState::Uploading, &None)
        );
        writer
            .uploaded(
                "e1",
                "nas",
                vec![object("recording.mcap"), object("manifest.json")],
            )
            .unwrap();
        let done = reader.upload("e1", "nas").unwrap().unwrap();
        assert_eq!(done.state, UploadState::Uploaded);
        assert!(done.uploaded_utc_ns.is_some());
        assert_eq!(reader.failed_uploads().unwrap(), []);
        let paths: Vec<&str> = done
            .objects
            .iter()
            .map(|object| object.path.as_str())
            .collect();
        assert_eq!(paths, ["manifest.json", "recording.mcap"]);
        assert_eq!(done.objects[1], object("recording.mcap"));

        // Done stays done: a late failure report doesn't undo it.
        writer.upload_failed("e1", "nas", failure).unwrap();
        assert_eq!(
            reader.upload("e1", "nas").unwrap().unwrap().state,
            UploadState::Uploaded
        );
        writer.start_upload("e1", "cloud", "archive/e1").unwrap();
        let destinations: Vec<String> = reader
            .uploads("e1")
            .unwrap()
            .into_iter()
            .map(|upload| upload.destination)
            .collect();
        assert_eq!(destinations, ["cloud", "nas"]);
        assert_eq!(reader.upload("e2", "nas").unwrap(), None);
    }
}
