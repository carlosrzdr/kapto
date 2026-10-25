// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask profile report`: every measurement against
//! `perf-budget.toml` in one place, from the newest runs in `target/bench/`
//! of the loads the budgets are stated for:
//! the hot path's stages and the jobs per batch from `bench model`, each
//! job per minute recorded and checksums and uploads per GB from `profile
//! trace`, and the live view from `profile page`. Each is within its budget,
//! over it, or unexpectedly cheap: under a tenth of it, often a sign the
//! work is skipped.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::page::{MEASURES_FILE, PageCheck, PageMeasures, read_budget};
use super::timeline::{SUMMARY_FILE, TraceSummary};

/// How a measurement compares with its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Within,
    Over,
    /// Under a tenth of the budget.
    UnexpectedlyCheap,
}

impl Verdict {
    fn of(measured: f64, budget: f64) -> Self {
        if measured > budget {
            Self::Over
        } else if measured < budget / 10.0 {
            Self::UnexpectedlyCheap
        } else {
            Self::Within
        }
    }

    fn shown(self) -> &'static str {
        match self {
            Self::Within => "within",
            Self::Over => "**over**",
            Self::UnexpectedlyCheap => "**unexpectedly cheap**",
        }
    }
}

/// The budgets `profile trace`'s measurements are held to.
#[derive(Debug, PartialEq)]
struct Budgets {
    /// Each job's milliseconds per minute recorded, by its kind.
    job_ms_per_minute: BTreeMap<String, f64>,
    checksum_s_per_gb: f64,
    upload_cpu_s_per_gb: f64,
}

/// Reads `[job.<kind>]` tables' `ms_per_minute`, `[checksum]`'s `s_per_gb`
/// and `[upload]`'s `cpu_s_per_gb`, each with a `reason`.
fn read_budgets(text: &str) -> Result<Budgets, String> {
    let document = toml_edit::Document::parse(text).map_err(|error| error.to_string())?;
    let number = |table: &dyn toml_edit::TableLike, name: &str, key: &str| {
        table
            .get("reason")
            .and_then(toml_edit::Item::as_str)
            .ok_or_else(|| format!("{name} needs a reason"))?;
        table
            .get(key)
            .and_then(|value| {
                value
                    .as_float()
                    .or_else(|| value.as_integer().map(|n| n as f64))
            })
            .filter(|n| *n > 0.0)
            .ok_or_else(|| format!("{name}.{key} must be a number over 0"))
    };
    let table = |name: &str| {
        document
            .get(name)
            .and_then(toml_edit::Item::as_table_like)
            .ok_or_else(|| format!("perf-budget.toml has no [{name}]"))
    };
    let mut job_ms_per_minute = BTreeMap::new();
    for (kind, item) in table("job")?.iter() {
        let job = item
            .as_table_like()
            .ok_or_else(|| format!("job.{kind} isn't a table"))?;
        job_ms_per_minute.insert(
            kind.to_owned(),
            number(job, &format!("job.{kind}"), "ms_per_minute")?,
        );
    }
    Ok(Budgets {
        job_ms_per_minute,
        checksum_s_per_gb: number(table("checksum")?, "checksum", "s_per_gb")?,
        upload_cpu_s_per_gb: number(table("upload")?, "upload", "cpu_s_per_gb")?,
    })
}

/// The newest run in `target/bench/` whose folder starts with `kind-`.
fn newest(root: &Path, kind: &str) -> Option<PathBuf> {
    std::fs::read_dir(root.join("target/bench"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let rest = name.strip_prefix(kind)?.strip_prefix('-')?;
            // `trace-reference-1793232564`: the time it started comes last.
            let started: u64 = rest.rsplit('-').next()?.parse().ok()?;
            Some((started, entry.path()))
        })
        .max()
        .map(|(_, path)| path)
}

/// The report from the newest runs, and whether anything is over budget.
pub(super) fn report(root: &Path) -> Result<(String, bool), String> {
    let text = std::fs::read_to_string(root.join("perf-budget.toml"))
        .map_err(|error| format!("can't read perf-budget.toml: {error}"))?;
    let budgets = read_budgets(&text)?;
    let page_budget = read_budget(&text)?;
    let read = |kind: &str, file: &str| -> Option<(PathBuf, String)> {
        let folder = newest(root, kind)?;
        let text = std::fs::read_to_string(folder.join(file)).ok()?;
        Some((folder, text))
    };
    let model = read("model", "report.json")
        .and_then(|(folder, text)| Some((folder, serde_json::from_str(&text).ok()?)));
    // Of the loads the budgets are stated for: two cameras for the jobs,
    // the largest robot for the page.
    let trace = read("trace-cameras", SUMMARY_FILE)
        .and_then(|(folder, text)| Some((folder, serde_json::from_str(&text).ok()?)));
    // Held against the budget as it is now, not as it was when measured.
    let page = read("page-large", MEASURES_FILE).and_then(|(folder, text)| {
        let measures: PageMeasures = serde_json::from_str(&text).ok()?;
        Some((folder, measures.checks(&page_budget)))
    });
    Ok(markdown(model, trace, page, &budgets))
}

type Found<T> = Option<(PathBuf, T)>;

fn markdown(
    model: Found<Value>,
    trace: Found<TraceSummary>,
    page: Found<Vec<PageCheck>>,
    budgets: &Budgets,
) -> (String, bool) {
    let mut text = String::from(
        "# Where the time goes, against perf-budget.toml\n\n\
         From the newest run of each in `target/bench/`. Unexpectedly cheap is under a tenth \
         of the budget, often a sign the work is skipped.\n",
    );
    let mut over = false;
    for (section, section_over) in [
        model_section(model),
        trace_section(trace, budgets),
        page_section(page),
    ] {
        text.push_str(&section);
        over |= section_over;
    }
    (text, over)
}

/// The run's folder name, which says what and when.
fn from(folder: &Path) -> String {
    folder
        .file_name()
        .map_or(String::new(), |name| name.to_string_lossy().into_owned())
}

/// The stages' verdicts `bench model` gave, and whether any is over.
fn model_section(model: Found<Value>) -> (String, bool) {
    let mut text = String::from("\n## The hot path and the jobs per batch\n\n");
    let Some((folder, report)) = model else {
        text.push_str("Not measured: run `cargo xtask bench model`.\n");
        return (text, false);
    };
    let mut over = false;
    let _ = write!(
        text,
        "From `{}`.\n\n| Stage | Verdict |\n|---|---|\n",
        from(&folder)
    );
    for stage in report["stages"].as_array().into_iter().flatten() {
        let verdict = stage["verdict"].as_str().unwrap_or("not measured");
        over |= verdict == "Over";
        let _ = writeln!(
            text,
            "| {} | {verdict} |",
            stage["stage"].as_str().unwrap_or("")
        );
    }
    (text, over)
}

/// Each job, checksums and uploading against their budgets, and whether any
/// is over or has none.
fn trace_section(trace: Found<TraceSummary>, budgets: &Budgets) -> (String, bool) {
    let mut text =
        String::from("\n## Each job per minute recorded, checksums and uploads per GB\n\n");
    let Some((folder, summary)) = trace else {
        text.push_str("Not measured: run `cargo xtask profile trace --upload`.\n");
        return (text, false);
    };
    let mut over = false;
    let _ = write!(
        text,
        "From `{}`.\n\n| Measure | Measured | Budget | Verdict |\n|---|---|---|---|\n",
        from(&folder)
    );
    let mut row = |measure: String, measured: Option<f64>, budget: Option<f64>, unit: &str| {
        let line = match (measured, budget) {
            (Some(measured), Some(budget)) => {
                let verdict = Verdict::of(measured, budget);
                over |= verdict == Verdict::Over;
                format!(
                    "| {measure} | {measured:.2} {unit} | {budget} {unit} | {} |",
                    verdict.shown()
                )
            }
            (None, Some(budget)) => format!("| {measure} | not measured | {budget} {unit} | — |"),
            (Some(measured), None) => {
                over = true;
                format!("| {measure} | {measured:.2} {unit} | **none** | — |")
            }
            (None, None) => return,
        };
        let _ = writeln!(text, "{line}");
    };
    let kinds: std::collections::BTreeSet<&String> = summary
        .job_ms_per_minute
        .keys()
        .chain(budgets.job_ms_per_minute.keys())
        .collect();
    for kind in kinds {
        row(
            format!("Job `{kind}`"),
            summary.job_ms_per_minute.get(kind).copied(),
            budgets.job_ms_per_minute.get(kind).copied(),
            "ms per minute",
        );
    }
    row(
        "Checksums".to_owned(),
        summary.checksum_s_per_gb,
        Some(budgets.checksum_s_per_gb),
        "s per GB",
    );
    row(
        "Uploading".to_owned(),
        summary.upload_cpu_s_per_gb,
        Some(budgets.upload_cpu_s_per_gb),
        "CPU-s per GB",
    );
    (text, over)
}

/// The page's checks from `profile page`, and whether any failed.
fn page_section(page: Found<Vec<PageCheck>>) -> (String, bool) {
    let mut text = String::from("\n## The live view\n\n");
    let Some((folder, checks)) = page else {
        text.push_str("Not measured: run `cargo xtask profile page`.\n");
        return (text, false);
    };
    let _ = write!(
        text,
        "From `{}`.\n\n| Measure | Measured | Allowed | Within |\n|---|---|---|---|\n",
        from(&folder)
    );
    for check in &checks {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} |",
            check.measure,
            check.measured,
            check.allowed,
            if check.within { "yes" } else { "**no**" }
        );
    }
    (text, checks.iter().any(|check| !check.within))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGETS: &str = "\
[job.qa]
ms_per_minute = 1000
reason = \"r\"

[job.events]
ms_per_minute = 700
reason = \"r\"

[checksum]
s_per_gb = 2
reason = \"r\"

[upload]
cpu_s_per_gb = 4
reason = \"r\"
";

    fn trace(qa: f64, events: f64) -> (PathBuf, TraceSummary) {
        (
            PathBuf::from("target/bench/trace-reference-1"),
            TraceSummary {
                job_ms_per_minute: BTreeMap::from([
                    ("qa".to_owned(), qa),
                    ("events".to_owned(), events),
                ]),
                checksum_s_per_gb: Some(0.8),
                upload_cpu_s_per_gb: None,
            },
        )
    }

    #[test]
    fn the_budgets_are_read_and_the_committed_file_has_them() {
        let budgets = read_budgets(BUDGETS).unwrap();
        assert_eq!(budgets.job_ms_per_minute["qa"], 1000.0);
        assert_eq!(budgets.upload_cpu_s_per_gb, 4.0);
        assert!(
            read_budgets("[job.qa]\nms_per_minute = 1\n")
                .unwrap_err()
                .contains("reason")
        );
        let committed =
            std::fs::read_to_string(crate::repo_root().join("perf-budget.toml")).unwrap();
        let committed = read_budgets(&committed).unwrap();
        for kind in ["verify", "events", "qa", "replay", "upload"] {
            assert!(committed.job_ms_per_minute.contains_key(kind), "{kind}");
        }
    }

    #[test]
    fn a_slowed_job_is_over_its_budget_and_a_skipped_one_unexpectedly_cheap() {
        let budgets = read_budgets(BUDGETS).unwrap();
        let (text, over) = markdown(None, Some(trace(338.0, 246.0)), None, &budgets);
        assert!(!over, "{text}");
        assert!(
            text.contains("| Job `qa` | 338.00 ms per minute | 1000 ms per minute | within |"),
            "{text}"
        );
        // Checking made slower, and finding events skipped.
        let (text, over) = markdown(None, Some(trace(1500.0, 3.0)), None, &budgets);
        assert!(over, "{text}");
        assert!(
            text.contains("| Job `qa` | 1500.00 ms per minute | 1000 ms per minute | **over** |"),
            "{text}"
        );
        assert!(
            text.contains(
                "| Job `events` | 3.00 ms per minute | 700 ms per minute | **unexpectedly cheap** |"
            ),
            "{text}"
        );
        assert!(text.contains("| Uploading | not measured |"), "{text}");
        assert!(
            text.contains("Not measured: run `cargo xtask bench model`."),
            "{text}"
        );
    }

    #[test]
    fn a_job_without_a_budget_counts_against_the_report() {
        let budgets = read_budgets(BUDGETS).unwrap();
        let (folder, mut summary) = trace(338.0, 246.0);
        summary
            .job_ms_per_minute
            .insert("transcribe".to_owned(), 10.0);
        let (text, over) = markdown(None, Some((folder, summary)), None, &budgets);
        assert!(over, "{text}");
        assert!(
            text.contains("| Job `transcribe` | 10.00 ms per minute | **none** |"),
            "{text}"
        );
    }

    #[test]
    fn the_newest_run_of_a_load_is_found_by_when_it_started() {
        let root = std::env::temp_dir().join(format!("xtask-report-{}", std::process::id()));
        for name in [
            "trace-cameras-100",
            "trace-cameras-300",
            "trace-five-cameras-500",
            "trace-reference-900",
        ] {
            std::fs::create_dir_all(root.join("target/bench").join(name)).unwrap();
        }
        let cameras = newest(&root, "trace-cameras");
        let page = newest(&root, "page-large");
        std::fs::remove_dir_all(&root).unwrap();
        // Not the newer run of five cameras, nor of the reference load alone.
        assert_eq!(cameras, Some(root.join("target/bench/trace-cameras-300")));
        assert_eq!(page, None);
    }
}
