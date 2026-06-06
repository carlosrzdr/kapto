// SPDX-License-Identifier: Apache-2.0

//! Quality-check results in `kapto.db`: what each check found on an
//! episode. Checks are either gates, deterministic and structural, which
//! decide whether an episode is accepted, or advisories, which only flag
//! and can never reject anything.
//!
//! For now results stay in the database of the machine that computed them;
//! an uploaded episode carries none. [`crate::Writer`] records an episode's
//! results and [`crate::Reader`] reads them; the SQL is here.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::models::AnomalyModel;

/// Whether a check can reject an episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// Deterministic and structural: an episode with a failed gate isn't
    /// accepted.
    Gate,
    /// Statistical or a matter of judgement: it only flags, and never
    /// rejects an episode.
    Advisory,
}

/// What a check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Nothing wrong.
    Passed,
    /// Something wrong, said in the result's message and ranges.
    Failed,
    /// The episode has nothing the check looks at, e.g. no camera for a
    /// camera check.
    NotApplicable,
}

/// A span of an episode, in UTC nanoseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    /// Where it starts.
    pub start_utc_ns: i64,
    /// Where it ends.
    pub end_utc_ns: i64,
}

/// One check's result on one episode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QaResult {
    /// The check's ID, e.g. `streams.complete`.
    pub check: String,
    /// The check's version: raised whenever what it computes changes, so
    /// results of different versions aren't compared as alike.
    pub version: u32,
    /// Whether it could reject the episode.
    pub kind: CheckKind,
    /// What it found.
    pub verdict: Verdict,
    /// What it found, in words.
    pub message: String,
    /// What it measured, by name, e.g. `dropped: 12`. Only finite numbers
    /// are kept.
    #[serde(default)]
    pub values: BTreeMap<String, f64>,
    /// Where in the episode what it found is, if it is in parts of it.
    #[serde(default)]
    pub ranges: Vec<TimeRange>,
}

/// Every check's result on one episode, from one run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QaRun {
    /// The episode's ID.
    pub episode: String,
    /// The version of the checks' parameters (`qa_params_version` of the
    /// robot package's `qa.toml`), if they have one.
    pub params_version: Option<String>,
    /// When the checks ran, in UTC nanoseconds since the Unix epoch.
    pub computed_utc_ns: i64,
    /// Each check's result, in the order they ran.
    pub results: Vec<QaResult>,
    /// The episode's statistics by name, as the quality checks' crate
    /// writes them (a single value, or a distribution that pools): kept
    /// here as they were given, read back the same.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub statistics: BTreeMap<String, serde_json::Value>,
    /// A model an anomaly detector learned during the run, kept with its
    /// results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Box<AnomalyModel>>,
}

impl QaRun {
    /// Whether the episode is accepted: no gate failed. Advisories never
    /// count against it.
    #[must_use]
    pub fn accepted(&self) -> bool {
        !self
            .results
            .iter()
            .any(|result| result.kind == CheckKind::Gate && result.verdict == Verdict::Failed)
    }
}

/// The tables, part of the database's schema: one row per run, so an
/// episode checked with no check turned on is told from one not checked
/// yet, and one per result.
pub(crate) const SCHEMA: &str = "CREATE TABLE qa_runs (
        episode TEXT PRIMARY KEY NOT NULL,
        params_version TEXT,
        computed_utc_ns INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE qa_statistics (
        episode TEXT NOT NULL,
        name TEXT NOT NULL,
        value TEXT NOT NULL,
        PRIMARY KEY (episode, name)
    ) STRICT;
    CREATE TABLE qa_results (
        episode TEXT NOT NULL,
        position INTEGER NOT NULL,
        check_id TEXT NOT NULL,
        check_version INTEGER NOT NULL,
        kind TEXT NOT NULL,
        verdict TEXT NOT NULL,
        message TEXT NOT NULL,
        measured TEXT NOT NULL,
        ranges TEXT NOT NULL,
        PRIMARY KEY (episode, position)
    ) STRICT;";

/// Replaces what the tables hold for `run`'s episode with `run`.
pub(crate) fn record(tx: &Transaction<'_>, run: &QaRun) -> rusqlite::Result<()> {
    if let Some(model) = &run.model {
        crate::models::record(tx, model)?;
    }
    tx.execute("DELETE FROM qa_results WHERE episode = ?1", [&run.episode])?;
    tx.execute(
        "DELETE FROM qa_statistics WHERE episode = ?1",
        [&run.episode],
    )?;
    let mut statistic = tx.prepare_cached("INSERT INTO qa_statistics VALUES (?1, ?2, ?3)")?;
    for (name, value) in &run.statistics {
        statistic.execute(params![run.episode, name, json(value)])?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO qa_runs VALUES (?1, ?2, ?3)",
        params![run.episode, run.params_version, run.computed_utc_ns],
    )?;
    let mut insert =
        tx.prepare_cached("INSERT INTO qa_results VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?;
    for (position, result) in run.results.iter().enumerate() {
        let measured: BTreeMap<&String, f64> = result
            .values
            .iter()
            .filter(|(_, value)| value.is_finite())
            .map(|(name, value)| (name, *value))
            .collect();
        insert.execute(params![
            run.episode,
            position,
            result.check,
            result.version,
            text(result.kind),
            text(result.verdict),
            result.message,
            json(&measured),
            json(&result.ranges),
        ])?;
    }
    Ok(())
}

/// The last run recorded for `episode`, if it was checked.
pub(crate) fn read(connection: &Connection, episode: &str) -> rusqlite::Result<Option<QaRun>> {
    let run = connection
        .prepare_cached("SELECT params_version, computed_utc_ns FROM qa_runs WHERE episode = ?1")?
        .query_row([episode], |row| {
            Ok(QaRun {
                episode: episode.to_owned(),
                params_version: row.get(0)?,
                computed_utc_ns: row.get(1)?,
                results: Vec::new(),
                statistics: BTreeMap::new(),
                model: None,
            })
        })
        .optional()?;
    let Some(mut run) = run else {
        return Ok(None);
    };
    let mut select = connection.prepare_cached(
        "SELECT check_id, check_version, kind, verdict, message, measured, ranges
         FROM qa_results WHERE episode = ?1 ORDER BY position",
    )?;
    let mut rows = select.query([episode])?;
    while let Some(row) = rows.next()? {
        run.results.push(QaResult {
            check: row.get(0)?,
            version: row.get(1)?,
            kind: parsed(&row.get::<_, String>(2)?).unwrap_or(CheckKind::Advisory),
            verdict: parsed(&row.get::<_, String>(3)?).unwrap_or(Verdict::NotApplicable),
            message: row.get(4)?,
            values: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
            ranges: serde_json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
        });
    }
    let mut statistics = connection
        .prepare_cached("SELECT name, value FROM qa_statistics WHERE episode = ?1 ORDER BY name")?;
    let mut rows = statistics.query([episode])?;
    while let Some(row) = rows.next()? {
        if let Ok(value) = serde_json::from_str(&row.get::<_, String>(1)?) {
            run.statistics.insert(row.get(0)?, value);
        }
    }
    Ok(Some(run))
}

/// The statistics of the `limit` newest other episodes of `episode`'s
/// session that were checked, by episode: those recorded with the same
/// robot package, profile and task, each as exactly the same content (or
/// all without one), on the same day in the machine's time zone.
pub(crate) fn session(
    connection: &Connection,
    episode: &str,
    limit: i64,
) -> rusqlite::Result<BTreeMap<String, BTreeMap<String, serde_json::Value>>> {
    let mut select = connection.prepare_cached(
        "WITH session AS (
             SELECT other.id FROM episodes AS this JOIN episodes AS other
             ON other.package_hash IS this.package_hash
                 AND other.profile_hash IS this.profile_hash
                 AND other.task_hash IS this.task_hash
                 AND date(other.started_utc_ns / 1000000000, 'unixepoch', 'localtime')
                     = date(this.started_utc_ns / 1000000000, 'unixepoch', 'localtime')
             WHERE this.id = ?1 AND other.id != this.id
                 AND EXISTS (SELECT 1 FROM qa_runs WHERE qa_runs.episode = other.id)
             ORDER BY other.id DESC LIMIT ?2)
         SELECT episode, name, value FROM qa_statistics
         WHERE episode IN (SELECT id FROM session)",
    )?;
    by_episode(select.query(params![episode, limit])?)
}

/// Rows of episode, statistic name and stored value, by episode; a value
/// that isn't JSON is left out.
pub(crate) fn by_episode(
    mut rows: rusqlite::Rows<'_>,
) -> rusqlite::Result<BTreeMap<String, BTreeMap<String, serde_json::Value>>> {
    let mut episodes: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        if let Ok(value) = serde_json::from_str(&row.get::<_, String>(2)?) {
            episodes
                .entry(row.get(0)?)
                .or_default()
                .insert(row.get(1)?, value);
        }
    }
    Ok(episodes)
}

/// The name `episode.json` and the API give a kind or verdict.
fn text(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn parsed<T: for<'de> Deserialize<'de>>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DATABASE_FILE, Writer};

    const EPISODE: &str = "20260305T192345.123Z-1a2b3c4d";

    fn result(check: &str, kind: CheckKind, verdict: Verdict) -> QaResult {
        QaResult {
            check: check.into(),
            version: 1,
            kind,
            verdict,
            message: format!("{check} said so"),
            values: BTreeMap::new(),
            ranges: Vec::new(),
        }
    }

    fn run(results: Vec<QaResult>) -> QaRun {
        QaRun {
            episode: EPISODE.into(),
            params_version: Some("3".into()),
            computed_utc_ns: 1_772_700_500_000_000_000,
            results,
            statistics: BTreeMap::new(),
            model: None,
        }
    }

    #[test]
    fn an_episode_is_accepted_unless_a_gate_failed_whatever_the_advisories_say() {
        use CheckKind::{Advisory, Gate};
        use Verdict::{Failed, NotApplicable, Passed};
        assert!(run(Vec::new()).accepted());
        assert!(
            run(vec![
                result("a", Gate, Passed),
                result("b", Advisory, Failed)
            ])
            .accepted()
        );
        assert!(run(vec![result("a", Gate, NotApplicable)]).accepted());
        assert!(
            !run(vec![
                result("a", Gate, Failed),
                result("b", Advisory, Passed)
            ])
            .accepted()
        );
    }

    #[test]
    fn results_and_statistics_are_read_back_a_new_run_replaces_the_last_and_a_run_without_results_is_kept()
     {
        let folder = std::env::temp_dir().join(format!("kapto-{}-qa-results", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let writer = Writer::open(&folder.join(DATABASE_FILE)).unwrap();
        let reader = writer.reader().unwrap();
        assert_eq!(reader.qa(EPISODE).unwrap(), None);

        let mut found = result("streams.complete", CheckKind::Gate, Verdict::Failed);
        found.values = BTreeMap::from([
            ("dropped".to_owned(), 12.0),
            // Not a number: left out rather than stored as something else.
            ("rate".to_owned(), f64::NAN),
        ]);
        found.ranges = vec![TimeRange {
            start_utc_ns: 1_772_700_410_000_000_000,
            end_utc_ns: 1_772_700_411_000_000_000,
        }];
        let mut first = run(vec![
            found.clone(),
            result("duration", CheckKind::Advisory, Verdict::Passed),
        ]);
        first.statistics = BTreeMap::from([
            (
                "episode.duration_s".to_owned(),
                serde_json::json!({"value": 10.0}),
            ),
            (
                "joints.joint_1.speed".to_owned(),
                serde_json::json!({"distribution": {"count": 2, "positive": {"3": 2}}}),
            ),
        ]);
        writer.record_qa(first.clone()).unwrap();
        let read = reader.qa(EPISODE).unwrap().unwrap();
        found.values.remove("rate");
        assert_eq!(read.results[0], found);
        assert_eq!(read.results[1].check, "duration");
        assert_eq!(
            (read.params_version.as_deref(), read.computed_utc_ns),
            (Some("3"), first.computed_utc_ns)
        );
        assert!(!read.accepted());
        // Statistics read back as they were given.
        assert_eq!(read.statistics, first.statistics);

        writer
            .record_qa(run(vec![result(
                "duration",
                CheckKind::Advisory,
                Verdict::Passed,
            )]))
            .unwrap();
        let again = reader.qa(EPISODE).unwrap().unwrap();
        assert_eq!(again.results.len(), 1);
        // Replaced with the run, which had none.
        assert!(again.statistics.is_empty());
        assert!(again.accepted());

        // Checked with no check turned on: recorded, and told from never
        // checked.
        writer.record_qa(run(Vec::new())).unwrap();
        let none_on = reader.qa(EPISODE).unwrap().unwrap();
        assert_eq!(
            (none_on.results.len(), none_on.params_version.as_deref()),
            (0, Some("3"))
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
