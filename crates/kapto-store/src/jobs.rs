// SPDX-License-Identifier: Apache-2.0

//! The job queue in `kapto.db`: work to do on an episode after it was
//! recorded, such as checking or exporting it, done by the worker one job
//! at a time and oldest first. A job of a kind is queued at most once per
//! episode. One that was running when its worker or serve stopped is queued
//! again, so it is done after a restart, until it has been interrupted too
//! often (`worker.job_interrupted`).
//!
//! [`crate::Writer`] changes the queue and [`crate::Reader`] lists it; the
//! SQL is here.

use kapto_core::{ErrorCode, KaptoError};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

/// A job's number in the queue, in the order jobs were queued.
pub type JobId = i64;

/// A job and how far it got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Its number.
    pub id: JobId,
    /// What to do, e.g. `verify`.
    pub kind: String,
    /// The episode to do it on.
    pub episode: String,
    /// Where it is.
    pub state: JobState,
    /// How often it was started.
    pub attempts: u32,
    /// When it was queued, in UTC nanoseconds since the Unix epoch.
    pub queued_utc_ns: i64,
    /// When it was last started, the same way.
    pub started_utc_ns: Option<i64>,
    /// When it ended, the same way.
    pub ended_utc_ns: Option<i64>,
    /// Why it failed.
    pub failure: Option<JobFailure>,
    /// What it couldn't do in full though it was done, each with its cause
    /// code and message as a failure has: e.g. a replay without its robot.
    pub warnings: Vec<JobFailure>,
}

/// Where a job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    /// Waiting for the worker.
    Queued,
    /// Being done.
    Running,
    /// Done.
    Done,
    /// Ended with an error; see [`Job::failure`].
    Failed,
}

/// Why a job failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFailure {
    /// The cause code, e.g. `episode.unreadable`.
    pub code: String,
    /// The message.
    pub message: String,
}

impl From<&KaptoError> for JobFailure {
    fn from(error: &KaptoError) -> Self {
        Self {
            code: error.code().id().to_owned(),
            message: error.message(),
        }
    }
}

/// What putting interrupted jobs back did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Requeued {
    /// Jobs queued again.
    pub queued: usize,
    /// Jobs failed, having been started the most times allowed.
    pub failed: usize,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Queued, Self::Running, Self::Done, Self::Failed]
            .into_iter()
            .find(|state| state.as_str() == text)
    }
}

/// Which kinds of job a claim takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kinds {
    /// Any.
    Any,
    /// Only these, as a lane doing only them asks.
    Only(Vec<String>),
    /// Any but these, as a lane leaving them to another asks: a job of a
    /// kind nobody does is still taken, and reported as such.
    Except(Vec<String>),
}

impl Kinds {
    /// Whether a claim of these may take a job of `kind`.
    #[must_use]
    pub fn includes(&self, kind: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Only(kinds) => kinds.iter().any(|only| only == kind),
            Self::Except(kinds) => kinds.iter().all(|except| except != kind),
        }
    }
}

/// Its table, part of the database's schema.
/// The kind of job that deletes an episode's files, the retention
/// executor's: the only one done for an episode in the bin, to empty it.
pub const DELETE: &str = "delete";

pub(crate) const SCHEMA: &str = "CREATE TABLE jobs (
        id INTEGER PRIMARY KEY,
        kind TEXT NOT NULL,
        episode TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'done', 'failed')),
        attempts INTEGER NOT NULL DEFAULT 0,
        queued_utc_ns INTEGER NOT NULL,
        started_utc_ns INTEGER,
        ended_utc_ns INTEGER,
        failure_code TEXT,
        failure_message TEXT,
        warnings TEXT,
        UNIQUE (kind, episode)
    ) STRICT;
    CREATE INDEX jobs_by_state ON jobs (state, id);";

const COLUMNS: &str = "id, kind, episode, state, attempts, queued_utc_ns, started_utc_ns, \
     ended_utc_ns, failure_code, failure_message, warnings";

/// Queues `kind` for `episode` unless it already is, in any state; returns
/// the job's number either way.
pub(crate) fn enqueue(
    tx: &Transaction<'_>,
    kind: &str,
    episode: &str,
    now: i64,
) -> rusqlite::Result<JobId> {
    tx.execute(
        "INSERT INTO jobs (kind, episode, state, queued_utc_ns) VALUES (?1, ?2, 'queued', ?3)
         ON CONFLICT (kind, episode) DO NOTHING",
        params![kind, episode, now],
    )?;
    tx.query_row(
        "SELECT id FROM jobs WHERE kind = ?1 AND episode = ?2",
        [kind, episode],
        |row| row.get(0),
    )
}

/// Queues `kind` for `episode` unless it is queued or being done: one that
/// ended, done or failed, is queued again as if new, keeping its number.
/// Returns the job.
pub(crate) fn enqueue_again(
    tx: &Transaction<'_>,
    kind: &str,
    episode: &str,
    now: i64,
) -> rusqlite::Result<Job> {
    tx.execute(
        "INSERT INTO jobs (kind, episode, state, queued_utc_ns) VALUES (?1, ?2, 'queued', ?3)
         ON CONFLICT (kind, episode) DO UPDATE SET state = 'queued', attempts = 0,
             queued_utc_ns = ?3, started_utc_ns = NULL, ended_utc_ns = NULL,
             failure_code = NULL, failure_message = NULL, warnings = NULL
         WHERE state IN ('done', 'failed')",
        params![kind, episode, now],
    )?;
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE kind = ?1 AND episode = ?2"),
        [kind, episode],
        job,
    )
}

/// Starts the oldest queued job, if there is one.
pub(crate) fn claim(
    tx: &Transaction<'_>,
    kinds: &Kinds,
    now: i64,
) -> rusqlite::Result<Option<Job>> {
    // An episode in the bin waits there, its jobs with it, until it is put
    // back; only the retention executor's deletion, which empties the bin,
    // goes on. The kinds as a JSON array, matched with SQLite's own JSON
    // reader: one statement however many there are.
    let json = |kinds: &[String]| {
        serde_json::to_string(kinds)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))
    };
    let (only, except) = match kinds {
        Kinds::Any => (None, None),
        Kinds::Only(kinds) => (Some(json(kinds)?), None),
        Kinds::Except(kinds) => (None, Some(json(kinds)?)),
    };
    tx.query_row(
        &format!(
            "UPDATE jobs SET state = 'running', attempts = attempts + 1, started_utc_ns = ?1
             WHERE id = (SELECT id FROM jobs WHERE state = 'queued'
                         AND (?2 IS NULL OR kind IN (SELECT value FROM json_each(?2)))
                         AND (?3 IS NULL OR kind NOT IN (SELECT value FROM json_each(?3)))
                         AND (kind = '{DELETE}'
                              OR episode NOT IN (SELECT episode FROM binned))
                         ORDER BY id LIMIT 1)
             RETURNING {COLUMNS}"
        ),
        params![now, only, except],
        job,
    )
    .optional()
}

/// Ends each queued job of `episode` as done, at `now`, as it is deleted:
/// nothing is left to do them on. Returns how many there were.
pub(crate) fn end_queued(tx: &Transaction<'_>, episode: &str, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE jobs SET state = 'done', ended_utc_ns = ?2 WHERE episode = ?1 AND state = 'queued'",
        params![episode, now],
    )
}

/// Ends running job `id`, failed with `failure` if given, with what it
/// couldn't do in full (`warnings`). Returns whether it was running.
pub(crate) fn finish(
    tx: &Transaction<'_>,
    id: JobId,
    failure: Option<&JobFailure>,
    warnings: &[JobFailure],
    now: i64,
) -> rusqlite::Result<bool> {
    let warnings = if warnings.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(warnings)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
        )
    };
    let changed = tx.execute(
        "UPDATE jobs SET state = ?2, ended_utc_ns = ?3, failure_code = ?4, failure_message = ?5,
             warnings = ?6
         WHERE id = ?1 AND state = 'running'",
        params![
            id,
            if failure.is_some() {
                JobState::Failed
            } else {
                JobState::Done
            }
            .as_str(),
            now,
            failure.map(|failure| &failure.code),
            failure.map(|failure| &failure.message),
            warnings,
        ],
    )?;
    Ok(changed == 1)
}

/// Puts running job `id` back in the queue as if it hadn't been started:
/// stopped on purpose, it doesn't count as an attempt. Returns whether it
/// was running.
pub(crate) fn release(tx: &Transaction<'_>, id: JobId) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE jobs SET state = 'queued', attempts = attempts - 1, started_utc_ns = NULL
         WHERE id = ?1 AND state = 'running'",
        [id],
    )?;
    Ok(changed == 1)
}

/// Queues again every job left running, failing those already started
/// `max_attempts` times.
pub(crate) fn requeue_interrupted(
    tx: &Transaction<'_>,
    max_attempts: u32,
    now: i64,
) -> rusqlite::Result<Requeued> {
    let code = ErrorCode::WorkerJobInterrupted;
    let failed = tx.execute(
        "UPDATE jobs SET state = 'failed', ended_utc_ns = ?2, failure_code = ?3,
             failure_message = ?4 || ': started ' || attempts || ' times'
         WHERE state = 'running' AND attempts >= ?1",
        params![max_attempts, now, code.id(), code.summary()],
    )?;
    let queued = tx.execute(
        "UPDATE jobs SET state = 'queued', started_utc_ns = NULL WHERE state = 'running'",
        [],
    )?;
    Ok(Requeued { queued, failed })
}

/// The job of `kind` for `episode`, if there is one.
pub(crate) fn with_id(connection: &Connection, id: JobId) -> rusqlite::Result<Option<Job>> {
    connection
        .prepare_cached(&format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"))?
        .query_row([id], job)
        .optional()
}

/// How many jobs of `kind` are in `state`.
pub(crate) fn count(connection: &Connection, kind: &str, state: JobState) -> rusqlite::Result<u64> {
    connection
        .prepare_cached("SELECT COUNT(*) FROM jobs WHERE kind = ?1 AND state = ?2")?
        .query_row(params![kind, state.as_str()], |row| row.get(0))
}

/// Every job of `episode`, oldest first.
pub(crate) fn of_episode(connection: &Connection, episode: &str) -> rusqlite::Result<Vec<Job>> {
    connection
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE episode = ?1 ORDER BY id"
        ))?
        .query_map([episode], job)?
        .collect()
}

pub(crate) fn find(
    connection: &Connection,
    kind: &str,
    episode: &str,
) -> rusqlite::Result<Option<Job>> {
    connection
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM jobs WHERE kind = ?1 AND episode = ?2"
        ))?
        .query_row([kind, episode], job)
        .optional()
}

/// The `limit` newest jobs, newest first.
pub(crate) fn list(connection: &Connection, limit: i64) -> rusqlite::Result<Vec<Job>> {
    connection
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM jobs ORDER BY id DESC LIMIT ?1"
        ))?
        .query_map([limit], job)?
        .collect()
}

fn job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let state: String = row.get(3)?;
    let code: Option<String> = row.get(8)?;
    let warnings: Option<String> = row.get(10)?;
    Ok(Job {
        id: row.get(0)?,
        kind: row.get(1)?,
        episode: row.get(2)?,
        // The table's CHECK allows only these.
        state: JobState::parse(&state).unwrap_or(JobState::Failed),
        attempts: row.get(4)?,
        queued_utc_ns: row.get(5)?,
        started_utc_ns: row.get(6)?,
        ended_utc_ns: row.get(7)?,
        failure: code
            .map(|code| {
                Ok::<_, rusqlite::Error>(JobFailure {
                    code,
                    message: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                })
            })
            .transpose()?,
        warnings: warnings
            .map(|text| {
                serde_json::from_str(&text).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(10, Type::Text, error.into())
                })
            })
            .transpose()?
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::db::{DATABASE_FILE, Writer};

    /// A database in a folder unique to this test, removed when dropped.
    struct TempDb(PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let folder =
                std::env::temp_dir().join(format!("kapto-{}-jobs-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&folder);
            std::fs::create_dir_all(&folder).unwrap();
            Self(folder)
        }

        fn open(&self) -> Writer {
            Writer::open(&self.0.join(DATABASE_FILE)).unwrap()
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const FIRST: &str = "20260305T192345.123Z-1a2b3c4d";
    const SECOND: &str = "20260306T080000.000Z-5e6f7a8b";

    fn states(writer: &Writer) -> Vec<(String, String, JobState, u32)> {
        let mut jobs = writer.reader().unwrap().jobs(100).unwrap();
        jobs.reverse();
        jobs.into_iter()
            .map(|job| (job.kind, job.episode, job.state, job.attempts))
            .collect()
    }

    #[test]
    fn a_job_is_queued_once_per_kind_and_episode_and_done_oldest_first() {
        let db = TempDb::new("order");
        let writer = db.open();
        let first = writer.enqueue("verify", FIRST).unwrap();
        let second = writer.enqueue("verify", SECOND).unwrap();
        let other = writer.enqueue("export", FIRST).unwrap();
        assert_eq!(writer.enqueue("verify", FIRST).unwrap(), first);

        let claimed = writer.claim().unwrap().unwrap();
        assert_eq!(
            (claimed.id, claimed.state, claimed.attempts),
            (first, JobState::Running, 1)
        );
        assert!(claimed.started_utc_ns.is_some());
        assert!(writer.finish(first, None, Vec::new()).unwrap());
        // Finishing again changes nothing.
        assert!(!writer.finish(first, None, Vec::new()).unwrap());
        // Done, it isn't queued again either.
        assert_eq!(writer.enqueue("verify", FIRST).unwrap(), first);

        assert_eq!(writer.claim().unwrap().unwrap().id, second);
        assert_eq!(writer.claim().unwrap().unwrap().id, other);
        assert_eq!(writer.claim().unwrap(), None);
        assert_eq!(
            states(&writer),
            [
                ("verify".into(), FIRST.into(), JobState::Done, 1),
                ("verify".into(), SECOND.into(), JobState::Running, 1),
                ("export".into(), FIRST.into(), JobState::Running, 1),
            ]
        );
    }

    #[test]
    fn a_failed_job_keeps_its_cause_code_and_message() {
        let db = TempDb::new("failed");
        let writer = db.open();
        let id = writer.enqueue("verify", FIRST).unwrap();
        writer.claim().unwrap();
        let error = KaptoError::new(ErrorCode::EpisodeUnreadable).detail("no episode.json");
        assert!(
            writer
                .finish(id, Some(JobFailure::from(&error)), Vec::new())
                .unwrap()
        );
        let job = &writer.reader().unwrap().jobs(1).unwrap()[0];
        assert_eq!(job.state, JobState::Failed);
        assert!(job.ended_utc_ns.is_some());
        assert_eq!(
            job.failure,
            Some(JobFailure {
                code: "episode.unreadable".into(),
                message: KaptoError::new(ErrorCode::EpisodeUnreadable)
                    .detail("no episode.json")
                    .message(),
            })
        );
    }

    #[test]
    fn a_job_done_keeps_what_it_couldnt_do_in_full_and_is_found_by_kind_and_episode() {
        let db = TempDb::new("warnings");
        let writer = db.open();
        let id = writer.enqueue("replay", FIRST).unwrap();
        writer.claim().unwrap();
        let warning = JobFailure::from(&KaptoError::new(ErrorCode::ViewerReplayWithoutRobot));
        assert!(writer.finish(id, None, vec![warning.clone()]).unwrap());
        let reader = writer.reader().unwrap();
        let job = reader.job("replay", FIRST).unwrap().unwrap();
        assert_eq!(
            (job.state, job.failure, job.warnings),
            (JobState::Done, None, vec![warning])
        );
        assert_eq!(reader.job("replay", SECOND).unwrap(), None);
        assert_eq!(reader.job("verify", FIRST).unwrap(), None);
    }

    #[test]
    fn a_job_asked_for_again_is_queued_again_once_it_ended_and_never_twice() {
        let db = TempDb::new("again");
        let writer = db.open();
        let first = writer.enqueue_again("replay", FIRST).unwrap();
        assert_eq!((first.state, first.attempts), (JobState::Queued, 0));
        // Queued or being done, asking again changes nothing.
        assert_eq!(writer.enqueue_again("replay", FIRST).unwrap(), first);
        writer.claim().unwrap();
        let running = writer.enqueue_again("replay", FIRST).unwrap();
        assert_eq!((running.state, running.attempts), (JobState::Running, 1));
        let failure = JobFailure::from(&KaptoError::new(ErrorCode::ViewerReplayDiskLow));
        writer.finish(first.id, Some(failure), Vec::new()).unwrap();
        // Ended, it is queued again as new, with the same number.
        let again = writer.enqueue_again("replay", FIRST).unwrap();
        assert_eq!(
            (again.id, again.state, again.attempts, again.failure),
            (first.id, JobState::Queued, 0, None)
        );
    }

    #[test]
    fn a_job_given_back_is_queued_again_without_counting_the_attempt() {
        let db = TempDb::new("released");
        let writer = db.open();
        let id = writer.enqueue("verify", FIRST).unwrap();
        writer.claim().unwrap();
        assert!(writer.release(id).unwrap());
        // Only a running job can be given back.
        assert!(!writer.release(id).unwrap());
        assert_eq!(
            states(&writer),
            [("verify".into(), FIRST.into(), JobState::Queued, 0)]
        );
        let again = writer.claim().unwrap().unwrap();
        assert_eq!((again.id, again.attempts), (id, 1));
    }

    #[test]
    fn a_job_running_when_kapto_stopped_is_done_after_a_restart_until_interrupted_too_often() {
        let db = TempDb::new("interrupted");
        let writer = db.open();
        let id = writer.enqueue("verify", FIRST).unwrap();
        writer.claim().unwrap();
        drop(writer);

        // Started again: the job is queued again and started a second time.
        let writer = db.open();
        assert_eq!(
            writer.requeue_interrupted(2).unwrap(),
            Requeued {
                queued: 1,
                failed: 0
            }
        );
        let again = writer.claim().unwrap().unwrap();
        assert_eq!((again.id, again.attempts), (id, 2));
        drop(writer);

        // Interrupted a second time, the most allowed: failed, with why.
        let writer = db.open();
        assert_eq!(
            writer.requeue_interrupted(2).unwrap(),
            Requeued {
                queued: 0,
                failed: 1
            }
        );
        assert_eq!(writer.claim().unwrap(), None);
        let job = &writer.reader().unwrap().jobs(1).unwrap()[0];
        assert_eq!(job.state, JobState::Failed);
        let failure = job.failure.as_ref().unwrap();
        assert_eq!(failure.code, "worker.job_interrupted");
        assert!(
            failure.message.ends_with("started 2 times"),
            "{}",
            failure.message
        );
    }
}
