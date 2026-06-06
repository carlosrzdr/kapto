// SPDX-License-Identifier: Apache-2.0

//! Uploads in `kapto.db`: each episode's upload to each destination, and
//! where every object of it went.
//!
//! An upload's key is kept as it was first worked out, never worked out
//! again: changing a destination's layout doesn't move an episode already
//! (or partly) uploaded. Its progress is the upload engine's own, kept as
//! text, so an upload carries on after a restart from where it was. It
//! counts as uploaded only once the engine said so, its completeness marker
//! written; before that it is uploading or failed, however many of its
//! files are up.
//!
//! [`crate::Writer`] changes them and [`crate::Reader`] lists them; the SQL
//! is here.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::jobs::JobFailure;

/// Its tables, part of the database's schema.
pub(crate) const SCHEMA: &str = "CREATE TABLE uploads (
        episode TEXT NOT NULL,
        destination TEXT NOT NULL,
        key TEXT NOT NULL,
        state TEXT NOT NULL,
        progress TEXT NOT NULL,
        failure_code TEXT,
        failure_message TEXT,
        started_utc_ns INTEGER NOT NULL,
        uploaded_utc_ns INTEGER,
        failed_utc_ns INTEGER,
        bytes_sent INTEGER NOT NULL,
        bytes_total INTEGER NOT NULL,
        throttled INTEGER NOT NULL,
        reported_utc_ns INTEGER,
        PRIMARY KEY (episode, destination)
    ) STRICT;
    CREATE TABLE uploaded_objects (
        episode TEXT NOT NULL,
        destination TEXT NOT NULL,
        path TEXT NOT NULL,
        key TEXT NOT NULL,
        bytes INTEGER NOT NULL,
        sha256 TEXT NOT NULL,
        e_tag TEXT,
        PRIMARY KEY (episode, destination, path)
    ) STRICT";

/// One episode's upload to one destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// The episode's ID.
    pub episode: String,
    /// The destination's name.
    pub destination: String,
    /// The episode's key there, as it was first worked out.
    pub key: String,
    /// Where it is.
    pub state: UploadState,
    /// How far the engine got, as it wrote it.
    pub progress: String,
    /// Why it failed last, if it did.
    pub failure: Option<JobFailure>,
    /// When it was first started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// When it was uploaded, the same way.
    pub uploaded_utc_ns: Option<i64>,
    /// When it last failed, the same way: it is tried again by itself a
    /// while after.
    pub failed_utc_ns: Option<i64>,
    /// How far it got, in numbers, as last reported.
    pub counts: UploadCounts,
    /// When its progress was last reported, the same way.
    pub reported_utc_ns: Option<i64>,
    /// Every object of it there, once uploaded, by path in the episode.
    pub objects: Vec<UploadedObject>,
}

/// Where an upload is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadState {
    /// Started and not done: going up, or waiting to carry on.
    Uploading,
    /// Every file up and checked, the marker written.
    Uploaded,
    /// Stopped by a failure; [`Upload::failure`] says why.
    Failed,
}

impl UploadState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Uploading => "uploading",
            Self::Uploaded => "uploaded",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::Uploading, Self::Uploaded, Self::Failed]
            .into_iter()
            .find(|state| state.as_str() == text)
    }
}

/// How far an upload got, in numbers, as the worker reports each step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadCounts {
    /// The bytes of the episode's files up so far.
    pub bytes_sent: u64,
    /// The bytes of all its files.
    pub bytes_total: u64,
    /// How many requests the destination throttled.
    pub throttled: u64,
}

/// One object of an uploaded episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedObject {
    /// Its path in the episode, e.g. `recording.mcap`.
    pub path: String,
    /// Its key at the destination.
    pub key: String,
    /// Its size.
    pub bytes: u64,
    /// Its SHA-256, as 64 lowercase hex digits.
    pub sha256: String,
    /// The destination's version tag of it (an S3 `ETag`), if it has one.
    pub e_tag: Option<String>,
}

/// Starts `episode`'s upload to `destination` under `key`, or, if it was
/// started before, carries it on under the key it had: returns which.
pub(crate) fn start(
    tx: &Transaction<'_>,
    episode: &str,
    destination: &str,
    key: &str,
    now: i64,
) -> rusqlite::Result<Upload> {
    tx.execute(
        "INSERT INTO uploads VALUES (?1, ?2, ?3, 'uploading', '{}', NULL, NULL, ?4, NULL, NULL, 0, 0, 0, NULL)
         ON CONFLICT (episode, destination) DO UPDATE
         SET state = 'uploading', failure_code = NULL, failure_message = NULL
         WHERE state = 'failed'",
        params![episode, destination, key, now],
    )?;
    // Written just above, in this transaction.
    find(tx, episode, destination)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
}

/// Saves how far `episode`'s upload to `destination` got, as of `now`.
pub(crate) fn progress(
    tx: &Transaction<'_>,
    (episode, destination): (&str, &str),
    progress: &str,
    counts: UploadCounts,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE uploads SET progress = ?3, bytes_sent = ?4, bytes_total = ?5, throttled = ?6,
         reported_utc_ns = ?7
         WHERE episode = ?1 AND destination = ?2",
        params![
            episode,
            destination,
            progress,
            counts.bytes_sent,
            counts.bytes_total,
            counts.throttled,
            now
        ],
    )?;
    Ok(())
}

/// Records `episode` as uploaded to `destination`, with every object of it.
pub(crate) fn uploaded(
    tx: &Transaction<'_>,
    episode: &str,
    destination: &str,
    objects: &[UploadedObject],
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE uploads SET state = 'uploaded', uploaded_utc_ns = ?3,
         failure_code = NULL, failure_message = NULL
         WHERE episode = ?1 AND destination = ?2",
        params![episode, destination, now],
    )?;
    tx.execute(
        "DELETE FROM uploaded_objects WHERE episode = ?1 AND destination = ?2",
        params![episode, destination],
    )?;
    let mut insert =
        tx.prepare_cached("INSERT INTO uploaded_objects VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
    for object in objects {
        insert.execute(params![
            episode,
            destination,
            object.path,
            object.key,
            object.bytes,
            object.sha256,
            object.e_tag,
        ])?;
    }
    Ok(())
}

/// Records `episode`'s upload to `destination` as failed `now`, keeping
/// its progress for the next try.
pub(crate) fn failed(
    tx: &Transaction<'_>,
    episode: &str,
    destination: &str,
    failure: &JobFailure,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE uploads SET state = 'failed', failure_code = ?3, failure_message = ?4,
         failed_utc_ns = ?5
         WHERE episode = ?1 AND destination = ?2 AND state != 'uploaded'",
        params![episode, destination, failure.code, failure.message, now],
    )?;
    Ok(())
}

/// Makes each failed upload of `episode` due to be tried again now, as if
/// it failed long enough ago; returns their destinations.
pub(crate) fn retry_now(tx: &Transaction<'_>, episode: &str) -> rusqlite::Result<Vec<String>> {
    tx.prepare_cached(
        "UPDATE uploads SET failed_utc_ns = 0 WHERE episode = ?1 AND state = 'failed'
         RETURNING destination",
    )?
    .query_map([episode], |row| row.get(0))?
    .collect()
}

/// Every upload that failed, by episode and destination.
pub(crate) fn failed_ones(connection: &Connection) -> rusqlite::Result<Vec<Upload>> {
    connection
        .prepare_cached(
            "SELECT * FROM uploads WHERE state = 'failed' ORDER BY episode, destination",
        )?
        .query_map([], upload)?
        .collect()
}

/// `episode`'s upload to `destination`, if it was ever started.
pub(crate) fn find(
    connection: &Connection,
    episode: &str,
    destination: &str,
) -> rusqlite::Result<Option<Upload>> {
    let upload = connection
        .prepare_cached("SELECT * FROM uploads WHERE episode = ?1 AND destination = ?2")?
        .query_row(params![episode, destination], upload)
        .optional()?;
    upload
        .map(|upload| with_objects(connection, upload))
        .transpose()
}

/// Every upload of every episode, without their objects.
pub(crate) fn all(connection: &Connection) -> rusqlite::Result<Vec<Upload>> {
    connection
        .prepare_cached("SELECT * FROM uploads ORDER BY episode, destination")?
        .query_map([], upload)?
        .collect()
}

/// Every upload of `episode`, by destination.
pub(crate) fn of_episode(connection: &Connection, episode: &str) -> rusqlite::Result<Vec<Upload>> {
    let uploads: Vec<Upload> = connection
        .prepare_cached("SELECT * FROM uploads WHERE episode = ?1 ORDER BY destination")?
        .query_map([episode], upload)?
        .collect::<rusqlite::Result<_>>()?;
    uploads
        .into_iter()
        .map(|upload| with_objects(connection, upload))
        .collect()
}

fn with_objects(connection: &Connection, mut upload: Upload) -> rusqlite::Result<Upload> {
    upload.objects = connection
        .prepare_cached(
            "SELECT path, key, bytes, sha256, e_tag FROM uploaded_objects
             WHERE episode = ?1 AND destination = ?2 ORDER BY path",
        )?
        .query_map(params![upload.episode, upload.destination], |row| {
            Ok(UploadedObject {
                path: row.get(0)?,
                key: row.get(1)?,
                bytes: row.get(2)?,
                sha256: row.get(3)?,
                e_tag: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(upload)
}

fn upload(row: &Row<'_>) -> rusqlite::Result<Upload> {
    let state: String = row.get(3)?;
    let code: Option<String> = row.get(5)?;
    let message: Option<String> = row.get(6)?;
    Ok(Upload {
        episode: row.get(0)?,
        destination: row.get(1)?,
        key: row.get(2)?,
        state: UploadState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, state.into())
        })?,
        progress: row.get(4)?,
        failure: code.map(|code| JobFailure {
            code,
            message: message.unwrap_or_default(),
        }),
        started_utc_ns: row.get(7)?,
        uploaded_utc_ns: row.get(8)?,
        failed_utc_ns: row.get(9)?,
        counts: UploadCounts {
            bytes_sent: row.get(10)?,
            bytes_total: row.get(11)?,
            throttled: row.get(12)?,
        },
        reported_utc_ns: row.get(13)?,
        objects: Vec::new(),
    })
}
