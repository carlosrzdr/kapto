// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask bench model`: what each recording stage costs per batch, per
//! message and per KiB, fitted from the stage benchmarks, and whether that
//! matches the budget in `perf-budget.toml`.
//!
//! The stage benchmarks run each stage across a grid of messages per batch
//! and message sizes. For each stage this fits
//! `time = per batch + per message × messages + per KiB × KiB` by least
//! squares, then works out the stage's share of one core at the reference
//! load and compares it with the budget.

#![allow(
    clippy::cast_precision_loss,
    reason = "nanoseconds and counts as fractions in a fit and a report"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, ExitCode};

use clap::Args;
use serde::Serialize;

use super::place::Place;
use super::report::Machine;

/// Options for `bench model`.
#[derive(Args)]
pub(crate) struct ModelArgs {
    /// Use criterion's quick mode: a few seconds per benchmark instead of
    /// the full statistical run. Good for a first look, noisier.
    #[arg(long)]
    quick: bool,
}

/// The reference load: 16 streams at 2 kHz of 68-byte messages, which the
/// receiver groups into full batches of 64.
const REFERENCE_MESSAGES_PER_SECOND: f64 = 32_000.0;
const REFERENCE_MESSAGE_BYTES: f64 = 68.0;
const REFERENCE_BATCH_MESSAGES: f64 = 64.0;

/// A stage's measured cost as a fraction of its budget below which it is
/// "unexpectedly cheap": usually a sign the benchmark skips the real work.
const CHEAP_FRACTION: f64 = 0.1;

/// The stages a default recording runs: UDP receive, sequence-number loss
/// accounting, hand-off, lz4-compressed writing, and the clock reads.
const DEFAULT_RECORDING: [&str; 6] = [
    "udp_receive",
    "accounting_sequence",
    "hand_off",
    "mcap_write_lz4",
    "timestamp/monotonic_now",
    "timestamp/to_utc",
];

/// The benchmark suites that measure the stages: the recording path, and
/// reading a recorded episode's events, checking it and making its replay, where a whole
/// episode is one batch.
const SUITES: [(&str, &str); 5] = [
    ("kapto-transport", "receive"),
    ("kapto-capture", "stages"),
    ("kapto-capture", "events_job"),
    ("kapto-qa", "qa_job"),
    ("kapto-viz", "replay_job"),
];

/// One benchmark result: a batch of `messages` messages, `bytes` in total,
/// took `ns` nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Point {
    pub(crate) messages: f64,
    pub(crate) bytes: f64,
    pub(crate) ns: f64,
}

/// A stage's fitted cost.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[allow(
    clippy::struct_field_names,
    reason = "each field is a cost per something; the prefix is the point"
)]
pub(crate) struct Cost {
    pub(crate) per_batch_ns: f64,
    pub(crate) per_message_ns: f64,
    pub(crate) per_kib_ns: f64,
}

impl Cost {
    /// Nanoseconds per second this cost adds up to at the reference load.
    pub(crate) fn at_reference(&self) -> f64 {
        let batches = REFERENCE_MESSAGES_PER_SECOND / REFERENCE_BATCH_MESSAGES;
        let kib = REFERENCE_MESSAGES_PER_SECOND * REFERENCE_MESSAGE_BYTES / 1024.0;
        self.per_batch_ns * batches
            + self.per_message_ns * REFERENCE_MESSAGES_PER_SECOND
            + self.per_kib_ns * kib
    }

    /// Share of one core at the reference load, in percent.
    pub(crate) fn core_percent(&self) -> f64 {
        self.at_reference() / 1e9 * 100.0
    }
}

/// A fit and how well it explains the measurements.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct Fit {
    pub(crate) cost: Cost,
    /// 1 is a perfect fit; near 0 means the model explains little and the
    /// coefficients shouldn't be trusted.
    pub(crate) r_squared: f64,
}

/// Fits `time = a + b × messages + c × KiB` with every coefficient zero or
/// more. Tries every combination of terms and keeps the best fit whose
/// coefficients are all non-negative: with three terms that is exact.
pub(crate) fn fit(points: &[Point]) -> Option<Fit> {
    let terms: [fn(&Point) -> f64; 3] = [|_| 1.0, |p| p.messages, |p| p.bytes / 1024.0];
    let mut best: Option<(f64, [f64; 3])> = None;
    for mask in 1_u8..8 {
        let used: Vec<usize> = (0..3).filter(|bit| mask & (1 << bit) != 0).collect();
        let Some(solution) = least_squares(points, &used, &terms) else {
            continue;
        };
        if solution.iter().any(|coefficient| *coefficient < 0.0) {
            continue;
        }
        let mut coefficients = [0.0; 3];
        for (term, value) in used.iter().zip(solution) {
            coefficients[*term] = value;
        }
        let residual: f64 = points
            .iter()
            .map(|point| {
                let predicted: f64 = (0..3).map(|t| coefficients[t] * terms[t](point)).sum();
                (point.ns - predicted).powi(2)
            })
            .sum();
        if best.is_none_or(|(least, _)| residual < least) {
            best = Some((residual, coefficients));
        }
    }
    let (residual, [a, b, c]) = best?;
    let mean = points.iter().map(|point| point.ns).sum::<f64>() / points.len() as f64;
    let total: f64 = points.iter().map(|point| (point.ns - mean).powi(2)).sum();
    Some(Fit {
        cost: Cost {
            per_batch_ns: a,
            per_message_ns: b,
            per_kib_ns: c,
        },
        r_squared: if total > 0.0 {
            1.0 - residual / total
        } else {
            1.0
        },
    })
}

/// Least-squares coefficients for the `used` terms: solves the normal
/// equations by Gaussian elimination. `None` if they are singular (e.g. a
/// term doesn't vary across the points).
fn least_squares(
    points: &[Point],
    used: &[usize],
    terms: &[fn(&Point) -> f64; 3],
) -> Option<Vec<f64>> {
    let n = used.len();
    if points.len() < n {
        return None;
    }
    // [XᵀX | Xᵀy]
    let mut matrix = vec![vec![0.0; n + 1]; n];
    for point in points {
        for (row, &i) in used.iter().enumerate() {
            for (column, &j) in used.iter().enumerate() {
                matrix[row][column] += terms[i](point) * terms[j](point);
            }
            matrix[row][n] += terms[i](point) * point.ns;
        }
    }
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|&a, &b| matrix[a][column].abs().total_cmp(&matrix[b][column].abs()))?;
        if matrix[pivot][column].abs() < 1e-9 {
            return None;
        }
        matrix.swap(column, pivot);
        let pivot_row = matrix[column].clone();
        for (row, values) in matrix.iter_mut().enumerate() {
            if row != column {
                let factor = values[column] / pivot_row[column];
                for (value, pivot_value) in values.iter_mut().zip(&pivot_row).skip(column) {
                    *value -= factor * pivot_value;
                }
            }
        }
    }
    Some(
        (0..n)
            .map(|row| matrix[row][n] / matrix[row][row])
            .collect(),
    )
}

/// Parses a benchmark ID such as `64x68B` into messages and message size.
fn grid_point(id: &str) -> Option<(f64, f64)> {
    let (messages, size) = id.strip_suffix('B')?.split_once('x')?;
    Some((
        messages.parse::<u32>().ok()?.into(),
        size.parse::<u32>().ok()?.into(),
    ))
}

/// Measurements per stage from criterion's results in `criterion_dir`. A
/// group of grid benchmarks (`64x68B`) is one stage; any other benchmark is
/// its own stage (`timestamp/monotonic_now`), called once per batch.
pub(crate) fn read_results(criterion_dir: &Path) -> BTreeMap<String, Vec<Point>> {
    let mut stages: BTreeMap<String, Vec<Point>> = BTreeMap::new();
    let Ok(groups) = std::fs::read_dir(criterion_dir) else {
        return stages;
    };
    for group in groups.flatten().filter(|entry| entry.path().is_dir()) {
        let group_name = group.file_name().to_string_lossy().into_owned();
        let Ok(benchmarks) = std::fs::read_dir(group.path()) else {
            continue;
        };
        for benchmark in benchmarks.flatten() {
            let id = benchmark.file_name().to_string_lossy().into_owned();
            let Some(ns) = mean_ns(&benchmark.path().join("new/estimates.json")) else {
                continue;
            };
            match grid_point(&id) {
                Some((messages, size)) => {
                    stages.entry(group_name.clone()).or_default().push(Point {
                        messages,
                        bytes: messages * size,
                        ns,
                    });
                }
                None => stages
                    .entry(format!("{group_name}/{id}"))
                    .or_default()
                    .push(Point {
                        messages: 0.0,
                        bytes: 0.0,
                        ns,
                    }),
            }
        }
    }
    stages
}

/// The mean time from one criterion `estimates.json`.
fn mean_ns(path: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("mean")?.get("point_estimate")?.as_f64()
}

/// A stage's budget from `perf-budget.toml`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Budget {
    pub(crate) cost: Cost,
    /// Why the budget is what it is.
    pub(crate) reason: String,
}

/// Reads `[stage.<name>]` tables: `per_batch_ns`, `per_message_ns`,
/// `per_kib_ns` (each optional, 0 if absent) and `reason`.
pub(crate) fn read_budgets(text: &str) -> Result<BTreeMap<String, Budget>, String> {
    let document = toml_edit::Document::parse(text).map_err(|error| error.to_string())?;
    let stages = document
        .get("stage")
        .and_then(toml_edit::Item::as_table_like)
        .ok_or("perf-budget.toml has no [stage.<name>] tables")?;
    let mut budgets = BTreeMap::new();
    for (name, item) in stages.iter() {
        let table = item
            .as_table_like()
            .ok_or_else(|| format!("stage.{name} isn't a table"))?;
        let number = |key: &str| -> Result<f64, String> {
            match table.get(key) {
                None => Ok(0.0),
                Some(value) => value
                    .as_float()
                    .or_else(|| value.as_integer().map(|n| n as f64))
                    .filter(|n| *n >= 0.0)
                    .ok_or_else(|| {
                        format!("stage.{name}.{key} must be a number of nanoseconds, 0 or more")
                    }),
            }
        };
        budgets.insert(
            name.to_owned(),
            Budget {
                cost: Cost {
                    per_batch_ns: number("per_batch_ns")?,
                    per_message_ns: number("per_message_ns")?,
                    per_kib_ns: number("per_kib_ns")?,
                },
                reason: table
                    .get("reason")
                    .and_then(toml_edit::Item::as_str)
                    .ok_or_else(|| format!("stage.{name} needs a reason"))?
                    .to_owned(),
            },
        );
    }
    Ok(budgets)
}

/// How a stage's measurement compares with its budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Verdict {
    /// Every part within budget.
    Within,
    /// These parts cost more than budgeted.
    Over(Vec<&'static str>),
    /// Far below budget at the reference load: the benchmark may be skipping
    /// the real work, or the budget is stale.
    UnexpectedlyCheap,
    /// No budget for this stage.
    NoBudget,
}

pub(crate) fn verdict(measured: &Cost, budget: Option<&Budget>) -> Verdict {
    let Some(budget) = budget else {
        return Verdict::NoBudget;
    };
    let over: Vec<&'static str> = [
        ("per batch", measured.per_batch_ns, budget.cost.per_batch_ns),
        (
            "per message",
            measured.per_message_ns,
            budget.cost.per_message_ns,
        ),
        ("per KiB", measured.per_kib_ns, budget.cost.per_kib_ns),
    ]
    .into_iter()
    .filter(|(_, measured, budgeted)| measured > budgeted)
    .map(|(part, _, _)| part)
    .collect();
    if !over.is_empty() {
        Verdict::Over(over)
    } else if measured.at_reference() < CHEAP_FRACTION * budget.cost.at_reference() {
        Verdict::UnexpectedlyCheap
    } else {
        Verdict::Within
    }
}

/// One stage in the report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct StageRow {
    pub(crate) stage: String,
    pub(crate) fit: Fit,
    pub(crate) budget: Option<Budget>,
    pub(crate) verdict: Verdict,
}

/// The whole cost model.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ModelReport {
    pub(crate) machine: Machine,
    pub(crate) quick: bool,
    pub(crate) stages: Vec<StageRow>,
    /// Budgeted stages with no measurement.
    pub(crate) unmeasured: Vec<String>,
}

pub(crate) fn model(
    results: &BTreeMap<String, Vec<Point>>,
    budgets: &BTreeMap<String, Budget>,
    machine: Machine,
    quick: bool,
) -> ModelReport {
    let stages = results
        .iter()
        .filter_map(|(stage, points)| {
            let fit = fit(points)?;
            let budget = budgets.get(stage).cloned();
            let verdict = verdict(&fit.cost, budget.as_ref());
            Some(StageRow {
                stage: stage.clone(),
                fit,
                budget,
                verdict,
            })
        })
        .collect();
    let unmeasured = budgets
        .keys()
        .filter(|stage| !results.contains_key(*stage))
        .cloned()
        .collect();
    ModelReport {
        machine,
        quick,
        stages,
        unmeasured,
    }
}

fn percent(value: f64) -> String {
    if value > 0.0 && value < 0.01 {
        "<0.01 %".to_owned()
    } else {
        format!("{value:.2} %")
    }
}

fn ns(value: f64) -> String {
    if value >= 1_000.0 {
        format!("{:.2} µs", value / 1_000.0)
    } else {
        format!("{value:.0} ns")
    }
}

impl ModelReport {
    pub(crate) fn markdown(&self) -> String {
        let mut page = String::from("# Recording cost model\n\n");
        page.push_str(
            "Provisional: measured on this machine, not the reference PC. Compare runs only \
             with runs on the same machine.\n\n",
        );
        let machine = &self.machine;
        let _ = writeln!(
            page,
            "Machine: {}, {} threads, {:.1} GiB, kernel {}, CPU governor {}.{}\n",
            machine.cpu,
            machine.threads,
            machine.memory_gib,
            machine.kernel,
            machine.cpu_governor,
            if self.quick {
                " Quick mode: noisier than a full run."
            } else {
                ""
            }
        );
        let _ = writeln!(
            page,
            "Share of a core is at the reference load: {REFERENCE_MESSAGES_PER_SECOND:.0} \
             messages/s of {REFERENCE_MESSAGE_BYTES:.0} bytes in batches of \
             {REFERENCE_BATCH_MESSAGES:.0}.\n"
        );
        page.push_str(
            "| Stage | Per batch | Per message | Per KiB | Fit R² | Share of a core | Budgeted share | Verdict |\n\
             |---|---|---|---|---|---|---|---|\n",
        );
        for row in &self.stages {
            let cost = &row.fit.cost;
            let budgeted = row.budget.as_ref().map_or_else(
                || "—".to_owned(),
                |budget| percent(budget.cost.core_percent()),
            );
            let verdict = match &row.verdict {
                Verdict::Within => "within".to_owned(),
                Verdict::Over(parts) => format!("**over** ({})", parts.join(", ")),
                Verdict::UnexpectedlyCheap => "**unexpectedly cheap**".to_owned(),
                Verdict::NoBudget => "**no budget**".to_owned(),
            };
            let _ = writeln!(
                page,
                "| `{}` | {} | {} | {} | {:.3} | {} | {budgeted} | {verdict} |",
                row.stage,
                ns(cost.per_batch_ns),
                ns(cost.per_message_ns),
                ns(cost.per_kib_ns),
                row.fit.r_squared,
                percent(cost.core_percent()),
            );
        }
        page.push_str(
            "
A low R² on a stage that costs the same for every batch (such as the \
             hand-off) is expected: there is no variation for the fit to explain, only noise.\n",
        );
        let _ = writeln!(
            page,
            "\nA default recording (UDP receive, sequence-number accounting, hand-off, lz4 \
             writing, clock reads) spends {} of a core in these stages at the reference load. \
             Time measured end to end beyond this is spent outside them: waking threads, \
             waiting for sockets, syncing to disk.",
            percent(self.default_recording_percent())
        );
        if !self.unmeasured.is_empty() {
            let _ = writeln!(
                page,
                "\nBudgeted but not measured: {}.",
                self.unmeasured
                    .iter()
                    .map(|stage| format!("`{stage}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        page
    }

    /// The share of a core the stages of a default recording add up to.
    pub(crate) fn default_recording_percent(&self) -> f64 {
        self.stages
            .iter()
            .filter(|row| DEFAULT_RECORDING.contains(&row.stage.as_str()))
            .map(|row| row.fit.cost.core_percent())
            .sum()
    }

    /// Whether any stage is over budget, unbudgeted, or suspiciously cheap.
    pub(crate) fn needs_attention(&self) -> bool {
        !self.unmeasured.is_empty() || self.stages.iter().any(|row| row.verdict != Verdict::Within)
    }
}

/// Runs the stage benchmarks, fits the model and reports it.
pub(crate) fn run(root: &Path, args: &ModelArgs) -> ExitCode {
    match run_model(root, args) {
        Ok(report) => {
            println!("{}", report.markdown());
            if report.needs_attention() {
                // A warning until the reference PC exists; see perf-budget.toml.
                eprintln!("Some stages need attention: see the verdicts above.");
            }
            ExitCode::SUCCESS
        }
        Err(problem) => {
            eprintln!("The cost model couldn't run: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn run_model(root: &Path, args: &ModelArgs) -> Result<ModelReport, String> {
    let budgets = std::fs::read_to_string(root.join("perf-budget.toml"))
        .map_err(|error| format!("can't read perf-budget.toml: {error}"))
        .and_then(|text| read_budgets(&text))?;
    for (package, bench) in SUITES {
        let mut command = Command::new(env!("CARGO"));
        command
            .args(["bench", "--quiet", "-p", package, "--bench", bench])
            .current_dir(root);
        if args.quick {
            command.args(["--", "--quick"]);
        }
        let status = command
            .status()
            .map_err(|error| format!("can't run cargo bench: {error}"))?;
        if !status.success() {
            return Err(format!("the {bench} benchmarks failed"));
        }
    }
    let results = read_results(&root.join("target/criterion"));
    let report = model(&results, &budgets, Machine::this(Place::Native), args.quick);
    let work = super::work_dir(root, "model");
    std::fs::create_dir_all(&work).map_err(|error| error.to_string())?;
    super::write_reports(
        &work,
        &serde_json::to_string_pretty(&report),
        &report.markdown(),
    )?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Points generated exactly from a cost, over the benchmark grid.
    fn exact(cost: Cost) -> Vec<Point> {
        let mut points = Vec::new();
        for messages in [1.0, 8.0, 64.0, 256.0] {
            for size in [68.0, 256.0, 1024.0] {
                let bytes = messages * size;
                points.push(Point {
                    messages,
                    bytes,
                    ns: cost.per_batch_ns
                        + cost.per_message_ns * messages
                        + cost.per_kib_ns * bytes / 1024.0,
                });
            }
        }
        points
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6 * b.abs().max(1.0)
    }

    #[test]
    fn an_exact_cost_is_recovered() {
        let cost = Cost {
            per_batch_ns: 80.0,
            per_message_ns: 250.0,
            per_kib_ns: 40.0,
        };
        let fit = fit(&exact(cost)).unwrap();
        assert!(close(fit.cost.per_batch_ns, 80.0), "{fit:?}");
        assert!(close(fit.cost.per_message_ns, 250.0));
        assert!(close(fit.cost.per_kib_ns, 40.0));
        assert!(close(fit.r_squared, 1.0));
    }

    #[test]
    fn a_cost_that_does_not_depend_on_size_fits_with_zero_per_kib() {
        let fit = fit(&exact(Cost {
            per_batch_ns: 90.0,
            per_message_ns: 0.0,
            per_kib_ns: 0.0,
        }))
        .unwrap();
        assert!(close(fit.cost.per_batch_ns, 90.0));
        assert!(close(fit.cost.per_message_ns, 0.0));
        assert!(close(fit.cost.per_kib_ns, 0.0));
    }

    #[test]
    fn coefficients_are_never_negative() {
        // Time falls slightly with size: an unconstrained fit would give a
        // negative cost per KiB.
        let mut points = exact(Cost {
            per_batch_ns: 100.0,
            per_message_ns: 30.0,
            per_kib_ns: 0.0,
        });
        for point in &mut points {
            point.ns -= point.bytes / 1024.0 * 0.5;
        }
        let fit = fit(&points).unwrap();
        assert!(fit.cost.per_batch_ns >= 0.0);
        assert!(fit.cost.per_message_ns >= 0.0);
        assert!(fit.cost.per_kib_ns >= 0.0);
    }

    #[test]
    fn a_single_measurement_is_a_cost_per_batch() {
        let fit = fit(&[Point {
            messages: 0.0,
            bytes: 0.0,
            ns: 14.0,
        }])
        .unwrap();
        assert!(close(fit.cost.per_batch_ns, 14.0));
        assert_eq!(fit.cost.per_message_ns, 0.0);
    }

    #[test]
    fn grid_ids_are_messages_by_bytes() {
        assert_eq!(grid_point("64x68B"), Some((64.0, 68.0)));
        assert_eq!(grid_point("monotonic_now"), None);
        assert_eq!(grid_point("64x68"), None);
    }

    #[test]
    fn share_of_a_core_uses_the_reference_load() {
        let cost = Cost {
            per_batch_ns: 0.0,
            per_message_ns: 1_000.0,
            per_kib_ns: 0.0,
        };
        // 32,000 messages × 1 µs = 32 ms per second.
        assert!(close(cost.core_percent(), 3.2));
    }

    fn budget(per_batch_ns: f64, per_message_ns: f64, per_kib_ns: f64) -> Budget {
        Budget {
            cost: Cost {
                per_batch_ns,
                per_message_ns,
                per_kib_ns,
            },
            reason: "test".to_owned(),
        }
    }

    #[test]
    fn verdicts_compare_each_part_with_its_budget() {
        let measured = Cost {
            per_batch_ns: 100.0,
            per_message_ns: 300.0,
            per_kib_ns: 10.0,
        };
        assert_eq!(
            verdict(&measured, Some(&budget(200.0, 400.0, 50.0))),
            Verdict::Within
        );
        assert_eq!(
            verdict(&measured, Some(&budget(50.0, 400.0, 5.0))),
            Verdict::Over(vec!["per batch", "per KiB"])
        );
        assert_eq!(
            verdict(&measured, Some(&budget(10_000.0, 10_000.0, 10_000.0))),
            Verdict::UnexpectedlyCheap
        );
        assert_eq!(verdict(&measured, None), Verdict::NoBudget);
    }

    #[test]
    fn tiny_shares_are_shown_as_below_a_hundredth_of_a_percent() {
        assert_eq!(percent(0.004), "<0.01 %");
        assert_eq!(percent(0.0), "0.00 %");
        assert_eq!(percent(1.234), "1.23 %");
    }

    #[test]
    fn the_default_recording_adds_up_its_own_stages_only() {
        let row = |stage: &str, per_message_ns: f64| StageRow {
            stage: stage.to_owned(),
            fit: Fit {
                cost: Cost {
                    per_batch_ns: 0.0,
                    per_message_ns,
                    per_kib_ns: 0.0,
                },
                r_squared: 1.0,
            },
            budget: None,
            verdict: Verdict::NoBudget,
        };
        let report = ModelReport {
            machine: Machine::this(Place::Native),
            quick: true,
            // 1 µs per message is 3.2 % each; zstd isn't part of a default recording.
            stages: vec![
                row("udp_receive", 1_000.0),
                row("mcap_write_lz4", 1_000.0),
                row("mcap_write_zstd", 1_000.0),
            ],
            unmeasured: Vec::new(),
        };
        assert!(close(report.default_recording_percent(), 6.4));
    }

    #[test]
    fn budgets_are_read_with_their_reasons() {
        let budgets = read_budgets(
            "[stage.hand_off]\nper_batch_ns = 300\nreason = \"two queue operations\"\n",
        )
        .unwrap();
        let hand_off = &budgets["hand_off"];
        assert!(close(hand_off.cost.per_batch_ns, 300.0));
        assert_eq!(hand_off.cost.per_message_ns, 0.0);
        assert_eq!(hand_off.reason, "two queue operations");
    }

    #[test]
    fn a_budget_needs_a_reason_and_non_negative_numbers() {
        assert!(
            read_budgets("[stage.x]\nper_batch_ns = 1\n")
                .unwrap_err()
                .contains("needs a reason")
        );
        assert!(
            read_budgets("[stage.x]\nper_batch_ns = -1\nreason = \"r\"\n")
                .unwrap_err()
                .contains("0 or more")
        );
        assert!(read_budgets("[other]\n").is_err());
    }

    #[test]
    fn the_committed_budget_file_is_valid() {
        let text = std::fs::read_to_string(crate::repo_root().join("perf-budget.toml")).unwrap();
        let budgets = read_budgets(&text).unwrap();
        assert!(budgets.contains_key("udp_receive"));
        assert!(budgets.contains_key("mcap_write_lz4"));
    }

    #[test]
    fn criterion_results_are_grouped_into_stages() {
        let dir = std::env::temp_dir().join(format!("kapto-model-test-{}", std::process::id()));
        for (group, id, ns) in [
            ("hand_off", "1x68B", 80.0),
            ("hand_off", "64x68B", 90.0),
            ("timestamp", "monotonic_now", 14.0),
        ] {
            let path = dir.join(group).join(id).join("new");
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("estimates.json"),
                format!("{{\"mean\":{{\"point_estimate\":{ns}}}}}"),
            )
            .unwrap();
        }
        let results = read_results(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(results["hand_off"].len(), 2);
        assert_eq!(results["timestamp/monotonic_now"][0].ns, 14.0);
    }
}
