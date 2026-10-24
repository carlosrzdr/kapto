// SPDX-License-Identifier: Apache-2.0

//! Episodes deleted by hand, in the bin until the retention executor
//! empties it or they are put back, in `kapto.db`: when each went into the
//! bin and from where. Its folder is in the data directory's [`BIN_DIR`].
//!
//! [`crate::Writer`] records them and [`crate::Reader`] lists them; the SQL
//! is here.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

/// The folder in the data directory that episodes deleted by hand are
/// moved to, each under its ID, until the bin is emptied.
pub const BIN_DIR: &str = ".bin";

/// Its table, part of the database's schema.
pub(crate) const SCHEMA: &str = "CREATE TABLE binned (
        episode TEXT PRIMARY KEY NOT NULL,
        binned_utc_ns INTEGER NOT NULL,
        binned_from TEXT NOT NULL
    ) STRICT";

/// An episode in the bin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binned {
    /// The episode's ID.
    pub episode: String,
    /// When it went into the bin, in UTC nanoseconds since the Unix epoch.
    pub binned_utc_ns: i64,
    /// Where it was deleted from, e.g. `web UI`, `API` or `command line`.
    pub from: String,
}

/// Records `binned`; one recorded before for the same episode is replaced.
pub(crate) fn record(tx: &Transaction<'_>, binned: &Binned) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO binned VALUES (?1, ?2, ?3)",
        params![binned.episode, binned.binned_utc_ns, binned.from],
    )?;
    Ok(())
}

/// Forgets that `episode` is in the bin; whether it was.
pub(crate) fn remove(tx: &Transaction<'_>, episode: &str) -> rusqlite::Result<bool> {
    Ok(tx.execute("DELETE FROM binned WHERE episode = ?1", [episode])? > 0)
}

/// When and from where `episode` went into the bin, if it is in it.
pub(crate) fn find(connection: &Connection, episode: &str) -> rusqlite::Result<Option<Binned>> {
    connection
        .prepare_cached("SELECT * FROM binned WHERE episode = ?1")?
        .query_row([episode], binned)
        .optional()
}

/// Every episode in the bin, the longest there first.
pub(crate) fn all(connection: &Connection) -> rusqlite::Result<Vec<Binned>> {
    connection
        .prepare_cached("SELECT * FROM binned ORDER BY binned_utc_ns, episode")?
        .query_map([], binned)?
        .collect()
}

fn binned(row: &Row<'_>) -> rusqlite::Result<Binned> {
    Ok(Binned {
        episode: row.get(0)?,
        binned_utc_ns: row.get(1)?,
        from: row.get(2)?,
    })
}
