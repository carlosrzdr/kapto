// SPDX-License-Identifier: Apache-2.0

//! Learned anomaly models in `kapto.db`: what a detector learned from a
//! robot package and task's episodes, kept as runtime state the user never
//! edits. Each is a row of its own, numbered up per package, task and kind,
//! and is never replaced: a result names the model it came from, so a flag
//! stays explainable after the next one is learned.
//!
//! The store keeps a model as the detector gave it, without reading it. A
//! newly learned model comes with the results of the run that learned it
//! ([`crate::QaRun::model`]) and is kept with them; [`crate::Reader`] reads
//! them; the SQL is here.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// The table, part of the database's schema.
pub(crate) const SCHEMA: &str = "CREATE TABLE anomaly_models (
        package_id TEXT,
        task TEXT,
        kind TEXT NOT NULL,
        version INTEGER NOT NULL,
        trained_utc_ns INTEGER NOT NULL,
        episodes TEXT NOT NULL,
        settings TEXT NOT NULL,
        model TEXT NOT NULL
    ) STRICT;
    CREATE UNIQUE INDEX anomaly_models_by_version
        ON anomaly_models (package_id, task, kind, version);";

/// A model a detector learned from a robot package and task's episodes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnomalyModel {
    /// The robot package's ID; `None` for episodes recorded without one.
    pub package_id: Option<String>,
    /// The task's ID; `None` for episodes recorded without one.
    pub task: Option<String>,
    /// The detector that learned it, e.g. `anomalies.autoencoder`.
    pub kind: String,
    /// Its number among the package, task and kind's models, from 1: the
    /// one after the newest. A version already kept is refused.
    pub version: u32,
    /// When it was learned, in UTC nanoseconds since the Unix epoch.
    pub trained_utc_ns: i64,
    /// The episodes it learned from, by ID.
    pub episodes: Vec<String>,
    /// The settings it was learned with, as the detector writes them.
    pub settings: serde_json::Value,
    /// The model, as the detector writes it.
    pub model: String,
}

/// Keeps `model`; a version its package, task and kind already have is
/// refused, so one is never replaced.
pub(crate) fn record(tx: &Transaction<'_>, model: &AnomalyModel) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO anomaly_models VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            model.package_id,
            model.task,
            model.kind,
            model.version,
            model.trained_utc_ns,
            serde_json::to_string(&model.episodes).unwrap_or_else(|_| "[]".to_owned()),
            model.settings.to_string(),
            model.model,
        ],
    )
    .map(drop)
}

/// The newest model of `kind` for `episode`'s robot package and task.
pub(crate) fn latest(
    connection: &Connection,
    episode: &str,
    kind: &str,
) -> rusqlite::Result<Option<AnomalyModel>> {
    connection
        .prepare_cached(
            "SELECT models.* FROM anomaly_models AS models JOIN episodes
             ON models.package_id IS episodes.package_id AND models.task IS episodes.task
             WHERE episodes.id = ?1 AND models.kind = ?2
             ORDER BY models.version DESC LIMIT 1",
        )?
        .query_row(params![episode, kind], |row| {
            Ok(AnomalyModel {
                package_id: row.get(0)?,
                task: row.get(1)?,
                kind: row.get(2)?,
                version: row.get(3)?,
                trained_utc_ns: row.get(4)?,
                episodes: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
                settings: serde_json::from_str(&row.get::<_, String>(6)?)
                    .unwrap_or(serde_json::Value::Null),
                model: row.get(7)?,
            })
        })
        .optional()
}

/// The statistics of the `limit` newest accepted episodes recorded with
/// `episode`'s robot package and task, other than it, by episode: what a
/// model of them learns from.
pub(crate) fn training(
    connection: &Connection,
    episode: &str,
    limit: i64,
) -> rusqlite::Result<BTreeMap<String, BTreeMap<String, serde_json::Value>>> {
    let mut select = connection.prepare_cached(
        "WITH learned_from AS (
             SELECT other.id FROM episodes AS this JOIN episodes AS other
             ON other.package_id IS this.package_id AND other.task IS this.task
             WHERE this.id = ?1 AND other.id != this.id
                 AND EXISTS (SELECT 1 FROM qa_runs WHERE qa_runs.episode = other.id)
                 AND NOT EXISTS (SELECT 1 FROM qa_results WHERE qa_results.episode = other.id
                                 AND kind = 'gate' AND verdict = 'failed')
             ORDER BY other.id DESC LIMIT ?2)
         SELECT episode, name, value FROM qa_statistics
         WHERE episode IN (SELECT id FROM learned_from)",
    )?;
    crate::qa::by_episode(select.query(params![episode, limit])?)
}
