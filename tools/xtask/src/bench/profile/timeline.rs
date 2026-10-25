// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask profile trace`: the timeline of a scenario. Kapto, built with
//! the `profiling` feature, records the load, then the worker does every job
//! serve would queue for the episode; each process writes its spans, which
//! are merged into one trace for Perfetto, and summed into a report: what
//! each stage costs a batch, how long batches wait for the writer, and what
//! each job costs per minute recorded.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kapto_server::jobs::{EVENTS, JOB_SOCKET, JobServer, QA, REPLAY, VERIFY};
use kapto_store::db::DATABASE_FILE;
use kapto_store::episode::EPISODE_FILE;
use kapto_store::{Episode, JobState, Writer};
use serde_json::Value;

use super::super::{record_load_watched, tidy, work_dir};
use super::ScenarioArgs;

/// Where the profiling build goes, apart from the others.
const TARGET_DIR: &str = "target/tracing";

/// The folder each process of a profiling build writes its trace to; the
/// name `kapto` reads.
const TRACE_VAR: &str = "KAPTO_PROFILE_TRACE";

/// The jobs serve queues for each finalised episode, uploads aside: they
/// need a destination.
const JOBS: [&str; 4] = [VERIFY, EVENTS, QA, REPLAY];

/// How long the worker is given for them.
const JOBS_WAIT: Duration = Duration::from_mins(10);

/// Records the scenario, has the worker do its jobs, and writes the trace
/// and its report; the folder they are in and the report.
pub(super) fn traced(root: &Path, args: ScenarioArgs) -> Result<(PathBuf, String), String> {
    let built = Command::new(env!("CARGO"))
        .args(["build", "--release", "--quiet", "-p", "kapto-loadgen"])
        .args(["-p", "kapto-cli", "--features", "kapto-cli/profiling"])
        .env("CARGO_TARGET_DIR", root.join(TARGET_DIR))
        .current_dir(root)
        .status()
        .map_err(|error| format!("can't run cargo: {error}"))?;
    if !built.success() {
        return Err("the profiling build failed".to_owned());
    }
    let binaries = root.join(TARGET_DIR).join("release");
    let work = work_dir(root, &format!("trace-{}", args.scenario.name()));
    let traces = work.join("traces");
    std::fs::create_dir_all(&traces).map_err(|error| error.to_string())?;
    let spec = args.scenario.spec(args.seconds);
    let run = record_load_watched(&binaries, &work, &spec, &[(TRACE_VAR, &traces)], |_| {})?;
    let (minutes, failed) = worker_jobs(&binaries, &work, &traces)?;
    let events = merged(&traces)?;
    std::fs::write(
        work.join("trace.json"),
        serde_json::to_string(&events).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("can't write trace.json: {error}"))?;
    let mut report = markdown(&events, args.scenario.name(), args.seconds, minutes);
    for failure in failed {
        let _ = writeln!(report, "\nNot done: {failure}");
    }
    std::fs::write(work.join("trace.md"), &report)
        .map_err(|error| format!("can't write trace.md: {error}"))?;
    tidy(&run.work, args.keep_recordings);
    Ok((work, report))
}

/// Has `kapto worker` do [`JOBS`] for the episode recorded in `work`,
/// serve's job socket opened here; the episode's length in minutes and
/// each job that failed.
fn worker_jobs(binaries: &Path, work: &Path, traces: &Path) -> Result<(f64, Vec<String>), String> {
    let data = work.join("data");
    let folder = std::fs::read_dir(&data)
        .map_err(|error| error.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join(EPISODE_FILE).exists())
        .ok_or("no finished episode")?;
    let episode = Episode::read(&folder).map_err(|error| error.report())?;
    let minutes = Duration::from_nanos(
        u64::try_from(episode.ended_utc_ns.saturating_sub(episode.started_utc_ns)).unwrap_or(0),
    )
    .as_secs_f64()
        / 60.0;
    let id = episode.id.clone();
    let store = Writer::open(&data.join(DATABASE_FILE)).map_err(|error| error.report())?;
    store
        .index(episode.into())
        .map_err(|error| error.report())?;
    for kind in JOBS {
        store.enqueue(kind, &id).map_err(|error| error.report())?;
    }
    let _serve =
        JobServer::listen(&data.join(JOB_SOCKET), store.clone()).map_err(|error| error.report())?;
    let log = std::fs::File::create(work.join("worker.log")).map_err(|error| error.to_string())?;
    let mut worker = Command::new(binaries.join("kapto"))
        .args([
            "worker",
            "--parent",
            &std::process::id().to_string(),
            "--data-dir",
        ])
        .arg(&data)
        .env_clear()
        .env(TRACE_VAR, traces)
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|error| format!("can't start the worker: {error}"))?;
    let reader = store.reader().map_err(|error| error.report())?;
    let started = Instant::now();
    let jobs = loop {
        let jobs = reader.jobs(JOBS.len()).map_err(|error| error.report())?;
        if jobs
            .iter()
            .all(|job| matches!(job.state, JobState::Done | JobState::Failed))
            || started.elapsed() > JOBS_WAIT
        {
            break jobs;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Stopped as serve stops it, so its trace is flushed.
    let _ = Command::new("kill")
        .args(["-TERM", &worker.id().to_string()])
        .status();
    let _ = worker.wait();
    let failed = jobs
        .into_iter()
        .filter(|job| job.state != JobState::Done)
        .map(|job| match job.failure {
            Some(failure) => format!("{}, {}: {}", job.kind, failure.code, failure.message),
            None => format!("{}, still {:?} after {JOBS_WAIT:?}", job.kind, job.state),
        })
        .collect();
    Ok((minutes, failed))
}

/// Every event of every trace in `traces`, each a JSON array its process
/// left open.
fn merged(traces: &Path) -> Result<Vec<Value>, String> {
    let mut events = Vec::new();
    for entry in std::fs::read_dir(traces)
        .map_err(|error| error.to_string())?
        .flatten()
    {
        let text = std::fs::read_to_string(entry.path()).map_err(|error| error.to_string())?;
        let closed = format!("{}]", text.trim_end().trim_end_matches(','));
        let read: Vec<Value> = serde_json::from_str(&closed)
            .map_err(|error| format!("{}: {error}", entry.path().display()))?;
        events.extend(read);
    }
    Ok(events)
}

/// One span's name on one thread, over the run.
#[derive(Default)]
struct Spans {
    durations_us: Vec<f64>,
}

/// The report: each stage's spans, how long batches waited for the writer,
/// and each job's cost per minute recorded.
fn markdown(events: &[Value], scenario: &str, seconds: u32, minutes: f64) -> String {
    let mut processes = BTreeMap::new();
    let mut threads = BTreeMap::new();
    for event in events.iter().filter(|event| event["ph"] == "M") {
        let name = event["args"]["name"].as_str().unwrap_or("").to_owned();
        match event["name"].as_str() {
            Some("process_name") => {
                processes.insert(event["pid"].as_u64(), name);
            }
            Some("thread_name") => {
                threads.insert((event["pid"].as_u64(), event["tid"].as_u64()), name);
            }
            _ => {}
        }
    }
    let mut spans: BTreeMap<(String, String, String), Spans> = BTreeMap::new();
    let mut waited_us = Vec::new();
    let mut jobs: BTreeMap<String, Spans> = BTreeMap::new();
    for event in events.iter().filter(|event| event["ph"] == "X") {
        let (pid, tid) = (event["pid"].as_u64(), event["tid"].as_u64());
        let name = event["name"].as_str().unwrap_or("").to_owned();
        let duration = event["dur"].as_f64().unwrap_or(0.0);
        if name == "job" {
            let kind = event["args"]["kind"].as_str().unwrap_or("").to_owned();
            jobs.entry(kind).or_default().durations_us.push(duration);
            continue;
        }
        if let Some(waited) = event["args"]["waited_us"].as_f64() {
            waited_us.push(waited);
        }
        let process = processes.get(&pid).cloned().unwrap_or_default();
        let thread = threads.get(&(pid, tid)).cloned().unwrap_or_default();
        spans
            .entry((process, thread, name))
            .or_default()
            .durations_us
            .push(duration);
    }
    let mut text = format!(
        "# Trace of `kapto record` and the worker: the {scenario} load, {seconds} s\n\n\
         The whole timeline is `trace.json`, for Perfetto or `chrome://tracing`. Spans are \
         timed in a profiling build, a little slower than a release one.\n\n\
         ## Stages\n\n\
         | Process | Thread | Span | Count | Mean | Longest | All |\n\
         |---|---|---|---:|---:|---:|---:|\n"
    );
    for ((process, thread, name), spans) in &spans {
        let (count, mean, longest, all) = summary(&spans.durations_us);
        let _ = writeln!(
            text,
            "| {process} | `{thread}` | {name} | {count} | {} | {} | {} |",
            shown(mean),
            shown(longest),
            shown(all)
        );
    }
    if !waited_us.is_empty() {
        waited_us.sort_by(f64::total_cmp);
        // The nearest rank, `share` in thousandths.
        let at = |share: usize| {
            waited_us[(waited_us.len() * share)
                .div_ceil(1_000)
                .clamp(1, waited_us.len())
                - 1]
        };
        let _ = write!(
            text,
            "\n## Batches waiting for the writer\n\n\
             From a batch's first message to its write: half within {}, 99 % within {}, the \
             longest {}.\n",
            shown(at(500)),
            shown(at(990)),
            shown(at(1_000))
        );
    }
    if !jobs.is_empty() {
        let _ = write!(
            text,
            "\n## Jobs\n\nFor an episode of {minutes:.2} minutes.\n\n\
             | Job | Done | Time | Per minute recorded |\n|---|---:|---:|---:|\n"
        );
        for (kind, spans) in &jobs {
            let (count, _, _, all) = summary(&spans.durations_us);
            let per_minute = if minutes > 0.0 { all / minutes } else { 0.0 };
            let _ = writeln!(
                text,
                "| {kind} | {count} | {} | {} |",
                shown(all),
                shown(per_minute)
            );
        }
    }
    text
}

/// The count, mean, longest and total of `durations_us`.
fn summary(durations_us: &[f64]) -> (usize, f64, f64, f64) {
    let all: f64 = durations_us.iter().sum();
    let longest = durations_us.iter().copied().fold(0.0, f64::max);
    let mean = if durations_us.is_empty() {
        0.0
    } else {
        all / durations_us.len() as f64
    };
    (durations_us.len(), mean, longest, all)
}

/// Microseconds in the unit that reads best.
fn shown(us: f64) -> String {
    if us >= 1e6 {
        format!("{:.2} s", us / 1e6)
    } else if us >= 1e3 {
        format!("{:.2} ms", us / 1e3)
    } else {
        format!("{us:.1} µs")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_report_sums_each_stage_the_batches_wait_and_each_job_per_minute_recorded() {
        let events = [
            json!({"ph": "M", "name": "process_name", "pid": 1, "args": {"name": "kapto record"}}),
            json!({"ph": "M", "name": "thread_name", "pid": 1, "tid": 7, "args": {"name": "cap-writer"}}),
            json!({"ph": "X", "name": "write", "pid": 1, "tid": 7, "ts": 0.0, "dur": 100.0, "args": {"waited_us": 2000}}),
            json!({"ph": "X", "name": "write", "pid": 1, "tid": 7, "ts": 500.0, "dur": 300.0, "args": {"waited_us": 4000}}),
            json!({"ph": "M", "name": "process_name", "pid": 2, "args": {"name": "kapto worker"}}),
            json!({"ph": "X", "name": "job", "pid": 2, "tid": 9, "ts": 0.0, "dur": 3e6, "args": {"kind": "qa"}}),
        ];
        let report = markdown(&events, "reference", 20, 0.5);
        assert!(
            report.contains(
                "| kapto record | `cap-writer` | write | 2 | 200.0 µs | 300.0 µs | 400.0 µs |"
            ),
            "{report}"
        );
        assert!(
            report.contains("half within 2.00 ms, 99 % within 4.00 ms, the longest 4.00 ms"),
            "{report}"
        );
        assert!(report.contains("| qa | 1 | 3.00 s | 6.00 s |"), "{report}");
    }

    #[test]
    fn traces_left_open_by_their_processes_are_merged() {
        let folder = std::env::temp_dir().join(format!("xtask-trace-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("record-1.json"),
            "[\n{\"ph\": \"M\"},\n{\"ph\": \"X\"},\n",
        )
        .unwrap();
        std::fs::write(folder.join("worker-2.json"), "[\n{\"ph\": \"X\"},\n").unwrap();
        assert_eq!(merged(&folder).unwrap().len(), 3);
        std::fs::write(folder.join("broken.json"), "[\n{\"ph\": ").unwrap();
        let broken = merged(&folder).unwrap_err();
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(broken.contains("broken.json"), "{broken}");
    }
}
