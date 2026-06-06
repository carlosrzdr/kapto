// SPDX-License-Identifier: Apache-2.0

//! Episodes deleted from this machine once verified elsewhere, in
//! `kapto.db`: an audit entry for each, saying when, why, by what and where
//! its copies are, kept with the episode's summary (its `episode.json`), so
//! the index goes on listing it, as stored only remotely, after it is
//! rebuilt from the episode folders.
//!
//! [`crate::Writer`] records them and [`crate::Reader`] lists them; the SQL
//! is here.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

/// Its table, part of the database's schema.
pub(crate) const SCHEMA: &str = "CREATE TABLE deletions (
        episode TEXT PRIMARY KEY NOT NULL,
        deleted_utc_ns INTEGER NOT NULL,
        reason TEXT NOT NULL,
        deleted_by TEXT NOT NULL,
        copies TEXT NOT NULL,
        summary TEXT NOT NULL
    ) STRICT";

/// One episode deleted from this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deletion {
    /// The episode's ID.
    pub episode: String,
    /// When it was deleted, in UTC nanoseconds since the Unix epoch.
    pub deleted_utc_ns: i64,
    /// Why.
    pub reason: DeletionReason,
    /// What deleted it, e.g. `worker`.
    pub deleted_by: String,
    /// Where its verified copies are.
    pub copies: Vec<RemoteCopy>,
    /// Its `episode.json` as it was, so it stays in the index.
    pub summary: String,
}

/// Why an episode was deleted from this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionReason {
    /// `upload.delete_local = "after_upload"`: verified on enough
    /// destinations.
    AfterUpload,
    /// `upload.delete_local = "when_disk_low"`: the disk ran low, and it was
    /// the oldest verified on enough destinations.
    DiskLow,
}

impl DeletionReason {
    /// Its name, as kept and shown.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AfterUpload => "after_upload",
            Self::DiskLow => "disk_low",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::AfterUpload, Self::DiskLow]
            .into_iter()
            .find(|reason| reason.as_str() == text)
    }
}

/// A verified copy of an episode at a destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCopy {
    /// The destination's name.
    pub destination: String,
    /// The episode's key there.
    pub key: String,
}

/// Records `deletion`, made `now`; one recorded before for the same
/// episode, by a deletion carried on after a restart, is replaced.
pub(crate) fn record(tx: &Transaction<'_>, deletion: &Deletion, now: i64) -> rusqlite::Result<()> {
    let copies = serde_json::to_string(&deletion.copies)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
    tx.execute(
        "INSERT OR REPLACE INTO deletions VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            deletion.episode,
            now,
            deletion.reason.as_str(),
            deletion.deleted_by,
            copies,
            deletion.summary,
        ],
    )?;
    Ok(())
}

/// The deletion of `episode`, if it was deleted.
pub(crate) fn find(connection: &Connection, episode: &str) -> rusqlite::Result<Option<Deletion>> {
    connection
        .prepare_cached("SELECT * FROM deletions WHERE episode = ?1")?
        .query_row([episode], deletion)
        .optional()
}

/// Every deletion, oldest first.
pub(crate) fn all(connection: &Connection) -> rusqlite::Result<Vec<Deletion>> {
    connection
        .prepare_cached("SELECT * FROM deletions ORDER BY deleted_utc_ns, episode")?
        .query_map([], deletion)?
        .collect()
}

/// The summary of each deleted episode the index doesn't hold, by ID.
pub(crate) fn not_indexed(tx: &Transaction<'_>) -> rusqlite::Result<Vec<(String, String)>> {
    tx.prepare_cached(
        "SELECT episode, summary FROM deletions
         WHERE episode NOT IN (SELECT id FROM episodes) ORDER BY episode",
    )?
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
    .collect()
}

/// Each indexed episode not deleted here, with when it started and on how
/// many destinations it is uploaded, oldest first.
pub(crate) fn kept_here(connection: &Connection) -> rusqlite::Result<Vec<(String, i64, u32)>> {
    connection
        .prepare_cached(
            "SELECT e.id, e.started_utc_ns,
                (SELECT COUNT(*) FROM uploads u WHERE u.episode = e.id AND u.state = 'uploaded')
             FROM episodes e WHERE e.id NOT IN (SELECT episode FROM deletions)
             ORDER BY e.started_utc_ns, e.id",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect()
}

fn deletion(row: &Row<'_>) -> rusqlite::Result<Deletion> {
    let reason: String = row.get(2)?;
    let copies: String = row.get(4)?;
    let invalid = |column, text: String| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, text.into())
    };
    Ok(Deletion {
        episode: row.get(0)?,
        deleted_utc_ns: row.get(1)?,
        reason: DeletionReason::parse(&reason).ok_or_else(|| invalid(2, reason.clone()))?,
        deleted_by: row.get(3)?,
        copies: serde_json::from_str(&copies).map_err(|error| invalid(4, error.to_string()))?,
        summary: row.get(5)?,
    })
}
