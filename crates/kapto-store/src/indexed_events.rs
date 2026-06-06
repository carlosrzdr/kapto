// SPDX-License-Identifier: Apache-2.0

//! Each episode's events in `kapto.db`, so episodes can be found by what
//! happened in them without reading their recordings. The recording stays
//! the source: the worker reads its events back after the episode is
//! finalised, and they are read again into a new database. [`crate::Writer`]
//! records an episode's events and [`crate::Reader`] reads them; the SQL
//! is here.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::events::{Event, Phase};

/// The tables, part of the database's schema: one row per episode whose
/// events were read, so one without events is told from one not read yet,
/// and one per event, its kind and moment apart to find episodes by them.
/// They outlive a rebuild of the episode index, as quality results do.
pub(crate) const SCHEMA: &str = "CREATE TABLE events_read (
        episode TEXT PRIMARY KEY NOT NULL,
        read_utc_ns INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE episode_events (
        episode TEXT NOT NULL,
        position INTEGER NOT NULL,
        kind TEXT NOT NULL,
        phase TEXT NOT NULL,
        at_utc_ns INTEGER NOT NULL,
        event TEXT NOT NULL,
        PRIMARY KEY (episode, position)
    ) STRICT;
    CREATE INDEX episode_events_by_kind ON episode_events (kind, episode);";

/// Replaces what the tables hold for `episode` with `events`, in order.
pub(crate) fn record(
    tx: &Transaction<'_>,
    episode: &str,
    events: &[Event],
    now_utc_ns: i64,
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM episode_events WHERE episode = ?1", [episode])?;
    tx.execute(
        "INSERT OR REPLACE INTO events_read VALUES (?1, ?2)",
        params![episode, now_utc_ns],
    )?;
    let mut insert =
        tx.prepare_cached("INSERT INTO episode_events VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for (position, event) in events.iter().enumerate() {
        insert.execute(params![
            episode,
            position,
            event.kind,
            phase(event.phase),
            event.at_utc_ns,
            serde_json::to_string(event).unwrap_or_default(),
        ])?;
    }
    Ok(())
}

/// `episode`'s events in order, or `None` if they weren't read yet. One
/// that can't be read back is left out.
pub(crate) fn read(connection: &Connection, episode: &str) -> rusqlite::Result<Option<Vec<Event>>> {
    let read = connection
        .prepare_cached("SELECT 1 FROM events_read WHERE episode = ?1")?
        .query_row([episode], |_| Ok(()))
        .optional()?;
    if read.is_none() {
        return Ok(None);
    }
    let mut select = connection
        .prepare_cached("SELECT event FROM episode_events WHERE episode = ?1 ORDER BY position")?;
    let mut rows = select.query([episode])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        events.extend(serde_json::from_str::<Event>(&row.get::<_, String>(0)?).ok());
    }
    Ok(Some(events))
}

/// How many of `episode`'s events are of each kind, or `None` if they
/// weren't read yet.
pub(crate) fn counts(
    connection: &Connection,
    episode: &str,
) -> rusqlite::Result<Option<BTreeMap<String, u64>>> {
    let read = connection
        .prepare_cached("SELECT 1 FROM events_read WHERE episode = ?1")?
        .exists([episode])?;
    if !read {
        return Ok(None);
    }
    connection
        .prepare_cached(
            "SELECT kind, count(*) FROM episode_events WHERE episode = ?1
             GROUP BY kind ORDER BY kind",
        )?
        .query_map([episode], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()
        .map(Some)
}

/// How a phase is written in the table, as in an event's JSON.
fn phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Moment => "moment",
        Phase::Begin => "begin",
        Phase::End => "end",
        Phase::Stretch => "stretch",
    }
}
