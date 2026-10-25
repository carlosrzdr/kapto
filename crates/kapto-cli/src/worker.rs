// SPDX-License-Identifier: Apache-2.0

//! `kapto worker`: does the jobs serve queues, such as checking an episode's
//! files and then its quality once it is finalised, or making its replay,
//! one at a time and oldest first. Between jobs it empties the replay
//! cache as its settings say. `wrk-sched` takes
//! each job from serve's job socket and reports how it ended; `wrk-cpu-0`
//! does it, both at low priority (see `priority`). While capture records an
//! episode (`worker.pause_while_recording`), `wrk-watch` sees it and the
//! worker waits: no job is started, and a job being done waits at its next
//! step. Serve starts the worker and starts it again if it exits. Stopped
//! with SIGINT or SIGTERM, it stops a job at its next step and gives it back
//! to the queue, to be done again without counting as an attempt; a job it
//! was doing when it died is queued again when serve next starts.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use kapto_capture::control::{ControlClient, Request, Response};
use kapto_capture::disk::free_bytes;
use kapto_capture::reader::read_events;
use kapto_config::{DeploymentConfig, QA_FILE, StatisticsSelection, UploadDestination, parse_qa};
use kapto_core::{ErrorCode, KaptoError};
use kapto_qa::anomalies::{self, AnomalyDetector, Session, run_detectors};
use kapto_qa::autoencoder::AutoencoderAnomalies;
use kapto_qa::carried::carried;
use kapto_qa::episode_statistics::Statistics;
use kapto_qa::roles::read_episode;
use kapto_qa::{
    Check, CheckKind, EpisodeView, Finding, QaResult, QaRun, checks, episode_statistics, run_checks,
};
use kapto_server::jobs::{
    DELETE, EVENTS, EpisodeEvents, Found, JOB_SOCKET, JobClient, JobOrder, QA, REPLAY, UPLOAD,
    VERIFY,
};
use kapto_server::replays::{CacheLimits, trim};
use kapto_store::db::DATABASE_FILE;
use kapto_store::{Episode, JobFailure, JobId, Reader, episode_folder, verify_episode_with};

mod learned;
mod memory;
mod priority;
#[cfg(feature = "run")]
mod replay;
mod retention;
mod upload;

use clap::Args;

use crate::SettingFlags;
use crate::station::Outcome;

/// How long to wait before asking again when no job is queued, or before
/// connecting again when serve can't be reached.
const WAIT: Duration = Duration::from_secs(1);

/// How often a wait looks whether to stop.
const POLL: Duration = Duration::from_millis(100);

/// How often the replay cache is trimmed while no job is being done, so
/// replays go once unopened too long or the disk runs low, whether or not
/// others are made.
const TRIM_EVERY: Duration = Duration::from_secs(60);

/// One kind of job: what the worker does for a job of this kind.
trait Job: Send + Sync {
    /// The kind it does, as jobs are queued with it.
    fn kind(&self) -> &'static str;

    /// Does it for the episode in `folder`, calling `between` after each
    /// step of work, where it may wait.
    fn run(
        &self,
        folder: &Path,
        between: &mut dyn FnMut() -> Result<(), KaptoError>,
    ) -> Result<Done, KaptoError>;
}

/// What a job found once done.
#[derive(Default)]
struct Done {
    /// What the quality checks found, if it ran them.
    qa: Option<QaRun>,
    /// The episode's events, if it read them.
    events: Option<EpisodeEvents>,
    /// What it couldn't do in full, and why: e.g. a replay without its
    /// robot.
    warnings: Vec<KaptoError>,
}

/// Checks an episode's files against its manifest's sizes and SHA-256, so
/// a disk that changes recordings is noticed.
struct Verify;

impl Job for Verify {
    fn kind(&self) -> &'static str {
        VERIFY
    }

    fn run(
        &self,
        folder: &Path,
        between: &mut dyn FnMut() -> Result<(), KaptoError>,
    ) -> Result<Done, KaptoError> {
        verify_episode_with(folder, between).map(|_| Done::default())
    }
}

/// Reads an episode's events from its recording, for serve to keep in the
/// episode index, so episodes can be found by what happened in them.
struct Events;

impl Job for Events {
    fn kind(&self) -> &'static str {
        EVENTS
    }

    fn run(
        &self,
        folder: &Path,
        between: &mut dyn FnMut() -> Result<(), KaptoError>,
    ) -> Result<Done, KaptoError> {
        let episode = Episode::read(folder)?;
        between()?;
        let events = read_events(&folder.join(&episode.recording))?;
        Ok(Done {
            events: Some(EpisodeEvents {
                episode: episode.id,
                events,
            }),
            ..Done::default()
        })
    }
}

/// Runs the quality checks on an episode, made from the `qa.toml` its
/// recording carries: the settings it was recorded with, never the package
/// as it is now, works out its statistics by role, and flags what is
/// unusual about them for its session, from the statistics `kapto.db` has
/// of the session's other episodes, and for its robot package and task,
/// with a model of their accepted episodes, learned again here when enough
/// new ones came. Whether a check
/// passed is its result, not the job's: the job fails only if the episode,
/// its recording or its settings can't be read.
struct Quality;

impl Job for Quality {
    fn kind(&self) -> &'static str {
        QA
    }

    fn run(
        &self,
        folder: &Path,
        between: &mut dyn FnMut() -> Result<(), KaptoError>,
    ) -> Result<Done, KaptoError> {
        let episode = EpisodeView::open(folder)?;
        let recording = folder.join(&episode.summary().recording);
        // Every check's defaults for a recording without `qa.toml`, as one
        // made without a package; results made with them have no
        // `qa_params_version`.
        let settings = carried(&recording, QA_FILE, parse_qa)?.unwrap_or_default();
        let summary = episode.summary();
        let span = (summary.started_utc_ns, summary.ended_utc_ns);
        let recorded = read_episode(&recording, span, between)?;
        let episode = episode
            .with_roles(recorded.roles)
            .with_cameras(recorded.cameras)
            .with_timing(recorded.timing)
            .with_events(recorded.events);
        between()?;
        let made = checks::from_settings(&settings);
        let checks: Vec<&dyn Check> = made.iter().map(AsRef::as_ref).collect();
        let mut run = run_checks(&checks, &episode, settings.params_version.clone());
        let statistics = selected(&settings.statistics, &episode);
        let mut made = anomalies::from_settings(&settings);
        let learning = &settings.autoencoder_anomalies;
        if learning.enabled {
            match learned::autoencoder(learning, &episode, folder) {
                Ok((detector, model)) => {
                    made.push(Box::new(detector));
                    run.model = model.map(Box::new);
                }
                Err(error) => run.results.push(
                    Finding::failed(format!("couldn't read its model: {error}")).into_result(
                        AutoencoderAnomalies::ID,
                        1,
                        CheckKind::Advisory,
                    ),
                ),
            }
        }
        let detectors: Vec<&dyn AnomalyDetector> = made.iter().map(AsRef::as_ref).collect();
        run.results
            .extend(detect(&detectors, &episode, &statistics, folder));
        run.statistics = episode_statistics::to_stored(statistics);
        Ok(Done {
            qa: Some(run),
            ..Done::default()
        })
    }
}

/// The statistics of `episode` that `selection` keeps. A pattern naming
/// none of them is likely mistyped, and is said so.
fn selected(selection: &StatisticsSelection, episode: &EpisodeView) -> Statistics {
    let all = episode_statistics::of_episode(episode);
    let names: Vec<&str> = all.keys().map(String::as_str).collect();
    for pattern in selection.unmatched(&names) {
        eprintln!(
            "[worker] qa {}: `{pattern}` in the `[statistics]` of its qa.toml names none of its statistics",
            episode.summary().id
        );
    }
    all.into_iter()
        .filter(|(name, _)| selection.keeps(name))
        .collect()
}

/// What `detectors` find on `episode`, whose statistics are `statistics`,
/// compared with the other episodes of its session as the database beside
/// its `folder` has them. If they can't be read, each detector's result
/// says so rather than comparing with nothing, and the checks still stand.
fn detect(
    detectors: &[&dyn AnomalyDetector],
    episode: &EpisodeView,
    statistics: &Statistics,
    folder: &Path,
) -> Vec<QaResult> {
    let wanted = detectors
        .iter()
        .map(|detector| detector.session_episodes())
        .max()
        .unwrap_or(0);
    let others = if wanted == 0 {
        Ok(Vec::new())
    } else {
        session(folder, &episode.summary().id, wanted)
    };
    match others {
        Ok(others) => run_detectors(
            detectors,
            episode,
            Session {
                statistics,
                others: &others,
            },
        ),
        Err(error) => detectors
            .iter()
            .map(|detector| {
                Finding::failed(format!("couldn't read its session: {error}")).into_result(
                    detector.id(),
                    detector.version(),
                    CheckKind::Advisory,
                )
            })
            .collect(),
    }
}

/// The statistics of the `wanted` newest other checked episodes of
/// episode `id`'s session, newest first, from the database in the data
/// directory holding `folder`.
fn session(folder: &Path, id: &str, wanted: usize) -> Result<Vec<Statistics>, KaptoError> {
    let data_dir = folder.parent().unwrap_or(folder);
    let stored = Reader::open(&data_dir.join(DATABASE_FILE))?.session_statistics(id, wanted)?;
    // Keyed by ID, which begins with the start time: the newest last.
    Ok(stored
        .into_values()
        .rev()
        .map(episode_statistics::from_stored)
        .collect())
}

/// Every kind of job the worker does, set up with `deployment`'s settings.
#[cfg_attr(
    not(feature = "run"),
    allow(
        unused_mut,
        unused_variables,
        reason = "without `run`, the worker is never called"
    )
)]
fn jobs(deployment: &DeploymentConfig) -> Vec<Box<dyn Job>> {
    let mut jobs: Vec<Box<dyn Job>> = vec![Box::new(Verify), Box::new(Events), Box::new(Quality)];
    #[cfg(feature = "run")]
    jobs.push(Box::new(replay::Replay::new(deployment)));
    jobs
}

/// How far the replay cache may grow, from `deployment`'s settings.
fn cache_limits(deployment: &DeploymentConfig) -> CacheLimits {
    CacheLimits {
        size: deployment
            .replay_cache_size_mib
            .value
            .saturating_mul(1 << 20),
        age: Duration::from_hours(u64::from(deployment.replay_cache_age_days.value) * 24),
        free_below: deployment.disk_warn_below_mib.value.saturating_mul(1 << 20),
    }
}

/// Arguments of `kapto worker`.
#[derive(Args)]
pub(crate) struct WorkerArgs {
    /// End when process PID ends, even if it is killed: serve passes its
    /// own, so this ends with it.
    #[arg(long, value_name = "PID")]
    pub(crate) parent: Option<u32>,
}

pub(crate) fn worker(flags: &SettingFlags, args: &WorkerArgs) -> ExitCode {
    match run(flags, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

/// Does jobs until SIGINT or SIGTERM.
fn run(flags: &SettingFlags, args: &WorkerArgs) -> Outcome<()> {
    let (_, deployment) = crate::config::load(flags)?;
    let jobs = jobs(&deployment);
    let cache = cache_limits(&deployment);
    let data_dir = deployment.data_dir.value;
    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&shutdown)).map_err(|error| {
            vec![thread_failed(&format!(
                "can't handle signal {signal}: {error}"
            ))]
        })?;
    }
    // After SIGTERM is handled: serve ending sends it, and the job being
    // done is then given back.
    if let Some(parent) = args.parent
        && let Err(error) = crate::parent::end_with(parent)
    {
        eprintln!("[worker] won't end with serve: {error}");
    }
    // Before any thread starts, so every thread has it.
    if let Err(error) = priority::lower() {
        eprintln!("[worker] runs at normal priority: can't lower it: {error}");
    }
    let recording = Arc::new(Recording::new());
    let upload_pauses = deployment.upload.pause_while_recording.value;
    if deployment.worker_pause_while_recording.value
        || (upload_pauses && !deployment.upload_destinations.is_empty())
    {
        thread::Builder::new()
            .name("wrk-watch".into())
            .spawn({
                let (socket, recording) = (deployment.control_socket.value, Arc::clone(&recording));
                let shutdown = Arc::clone(&shutdown);
                move || watch(&socket, &recording, &shutdown)
            })
            .map_err(|error| vec![thread_failed(&error.to_string())])?;
    }
    let lane_ended = Arc::new(AtomicBool::new(false));
    let lane = if deployment.upload_destinations.is_empty() {
        None
    } else {
        let flags = (
            Arc::clone(&shutdown),
            Arc::clone(&recording),
            Arc::clone(&lane_ended),
        );
        let destinations = deployment.upload_destinations.clone();
        Some(upload_lane(&data_dir, destinations, upload_pauses, flags)?)
    };
    let (orders, ordered) = mpsc::channel::<JobOrder>();
    let (ended, results) = mpsc::channel();
    // Ends with the process: a job it is doing is left unfinished.
    thread::Builder::new()
        .name("wrk-cpu-0".into())
        .spawn({
            let (data_dir, recording) = (data_dir.clone(), Arc::clone(&recording));
            let shutdown = Arc::clone(&shutdown);
            move || work(&data_dir, &jobs, (&recording, &shutdown), &ordered, &ended)
        })
        .map_err(|error| vec![thread_failed(&error.to_string())])?;
    let scheduler = thread::Builder::new()
        .name("wrk-sched".into())
        .spawn({
            let data_dir = data_dir.clone();
            move || {
                let cache = Cache {
                    data_dir: &data_dir,
                    limits: cache,
                };
                schedule(&cache, (&shutdown, &recording), &orders, &results);
            }
        })
        .map_err(|error| vec![thread_failed(&error.to_string())])?;
    eprintln!(
        "Worker is running: jobs from {}.",
        data_dir.join(JOB_SOCKET).display()
    );
    let _ = scheduler.join();
    // An upload is stopped between files, so the file going up is let
    // finish and the job given back.
    if let Some(lane) = lane {
        let _ = lane.join();
    }
    if lane_ended.load(Ordering::Relaxed) {
        return Err(vec![
            KaptoError::new(ErrorCode::WorkerLaneEnded).detail("the upload lane, `wrk-upload`"),
        ]);
    }
    eprintln!("Worker stopped.");
    Ok(())
}

/// Starts `wrk-upload`, which ends the worker if it ends by itself.
fn upload_lane(
    data_dir: &Path,
    destinations: Vec<UploadDestination>,
    pause: bool,
    (shutdown, recording, ended): (Arc<AtomicBool>, Arc<Recording>, Arc<AtomicBool>),
) -> Outcome<thread::JoinHandle<()>> {
    let data_dir = data_dir.to_owned();
    thread::Builder::new()
        .name("wrk-upload".into())
        .spawn(move || {
            let _ends = EndsWorker {
                shutdown: &shutdown,
                ended: &ended,
            };
            upload::lane(&data_dir, &destinations, pause, (&shutdown, &recording));
        })
        .map_err(|error| vec![thread_failed(&error.to_string())])
}

/// Held by a lane's thread: its ending by itself, a panic too, stops the
/// worker, which serve then starts again, rather than the worker running on
/// without that lane.
struct EndsWorker<'a> {
    shutdown: &'a AtomicBool,
    ended: &'a AtomicBool,
}

impl Drop for EndsWorker<'_> {
    fn drop(&mut self) {
        if !self.shutdown.load(Ordering::Relaxed) {
            self.ended.store(true, Ordering::Relaxed);
            self.shutdown.store(true, Ordering::Relaxed);
        }
    }
}

/// How long capture's answer that it is recording counts: past it, the
/// worker carries on as if capture weren't recording, so a watch held up
/// anywhere never holds the worker up with it. The watch asks once a
/// second.
const FRESH: Duration = Duration::from_secs(3);

/// Whether capture is recording an episode, as `wrk-watch` last heard it:
/// what the worker pauses for while it is so, and for [`FRESH`] at most
/// after the last answer saying so.
#[derive(Debug)]
pub(crate) struct Recording {
    since: Instant,
    /// What capture last answered.
    said: AtomicBool,
    /// When it was last heard, in milliseconds after `since`.
    heard_ms: AtomicU64,
}

impl Recording {
    pub(crate) fn new() -> Self {
        Self {
            since: Instant::now(),
            said: AtomicBool::new(false),
            heard_ms: AtomicU64::new(0),
        }
    }

    /// Capture just answered `recording`; returns what it answered before.
    pub(crate) fn heard(&self, recording: bool) -> bool {
        self.heard_at(self.since.elapsed(), recording)
    }

    fn heard_at(&self, at: Duration, recording: bool) -> bool {
        self.heard_ms.store(
            u64::try_from(at.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.said.swap(recording, Ordering::Relaxed)
    }

    /// Whether the worker waits now: capture said it is recording, less
    /// than [`FRESH`] ago.
    pub(crate) fn now(&self) -> bool {
        self.now_at(self.since.elapsed())
    }

    fn now_at(&self, at: Duration) -> bool {
        let heard = Duration::from_millis(self.heard_ms.load(Ordering::Relaxed));
        self.said.load(Ordering::Relaxed) && at.saturating_sub(heard) < FRESH
    }
}

/// `wrk-watch`: asks capture once a second whether it is recording an
/// episode, and says so to `recording`. Capture not answering counts as
/// not recording: the worker doesn't wait on a capture process that isn't
/// there. The first failure after answers is logged with why.
fn watch(socket: &Path, recording: &Recording, shutdown: &AtomicBool) {
    let mut capture: Option<ControlClient> = None;
    let mut failing = false;
    while !shutdown.load(Ordering::Relaxed) {
        let answer = match capture.take() {
            Some(client) => Ok(client),
            None => ControlClient::connect(socket, WAIT),
        }
        .and_then(|mut client| {
            let answer = client.ask(&Request::Status);
            capture = Some(client);
            answer
        });
        let now = match answer {
            Ok(Response::Status(status)) => {
                failing = false;
                status.episode.is_some()
            }
            Ok(other) => {
                capture = None;
                if !failing {
                    eprintln!("[worker] capture answered its status with {other:?}");
                }
                failing = true;
                false
            }
            Err(error) => {
                capture = None;
                if !failing {
                    eprintln!(
                        "[worker] can't ask capture whether it records: {}",
                        error.message()
                    );
                }
                failing = true;
                false
            }
        };
        if now != recording.heard(now) {
            if now {
                eprintln!("[worker] waiting while an episode is recorded");
            } else {
                eprintln!("[worker] carrying on");
            }
        }
        wait(WAIT, shutdown);
    }
}

/// Lowers the calling thread to the worker's CPU and I/O priority, as
/// threads doing slow work beside capture are.
///
/// # Errors
///
/// The operating system's reason if either couldn't be set.
pub(crate) fn lower_priority() -> std::io::Result<()> {
    priority::lower()
}

/// `wrk-cpu-0`: does each job it is given and says how it ended.
fn work(
    data_dir: &Path,
    jobs: &[Box<dyn Job>],
    (recording, shutdown): (&Recording, &AtomicBool),
    ordered: &Receiver<JobOrder>,
    ended: &Sender<(JobId, Result<Done, KaptoError>)>,
) {
    let mut between = || step(recording, shutdown);
    for order in ordered {
        let result = run_job(data_dir, jobs, &order, &mut between);
        memory::give_back();
        if ended.send((order.id, result)).is_err() {
            return;
        }
    }
}

/// Between two steps of a job: waits while an episode is recorded, and
/// stops the job with `worker.job_stopped` once the worker is stopping, so
/// it can be given back rather than cut off.
fn step(recording: &Recording, shutdown: &AtomicBool) -> Result<(), KaptoError> {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Err(KaptoError::new(ErrorCode::WorkerJobStopped));
        }
        if !recording.now() {
            return Ok(());
        }
        thread::sleep(POLL);
    }
}

fn run_job(
    data_dir: &Path,
    jobs: &[Box<dyn Job>],
    order: &JobOrder,
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<Done, KaptoError> {
    let job = jobs
        .iter()
        .find(|job| job.kind() == order.kind)
        .ok_or_else(|| {
            KaptoError::new(ErrorCode::WorkerUnknownJob).detail(format!(
                "`{}` isn't a kind of job this worker does",
                order.kind
            ))
        })?;
    // Only an episode ID, so a job can never name a folder elsewhere.
    let folder = episode_folder(data_dir, &order.episode).ok_or_else(|| {
        KaptoError::new(ErrorCode::EpisodeUnreadable)
            .detail(format!("`{}` isn't an episode ID", order.episode))
    })?;
    let _span = kapto_core::span!("job", kind = %order.kind, episode = %order.episode);
    job.run(&folder, between)
}

/// The replay cache, trimmed by `wrk-sched` only: between jobs, no replay
/// is being written.
struct Cache<'a> {
    data_dir: &'a Path,
    limits: CacheLimits,
}

impl Cache<'_> {
    /// Empties the cache as its limits say, telling what went and why.
    fn trim(&self) {
        let free = free_bytes(self.data_dir).ok();
        let trimmed = trim(self.data_dir, &self.limits, free, SystemTime::now());
        for (name, reason) in &trimmed.removed {
            eprintln!("[worker] removed replay {name}: {}", reason.describe());
        }
        for (name, error) in &trimmed.failed {
            eprintln!("[worker] can't remove replay {name}: {error}");
        }
    }
}

/// `wrk-sched`: takes jobs from serve, hands each to `wrk-cpu-0` and
/// reports how it ended, until `shutdown` is set. It takes none while
/// `recording` is set. It trims the replay cache when it starts, after
/// each replay is made, and every [`TRIM_EVERY`] meanwhile.
fn schedule(
    cache: &Cache<'_>,
    (shutdown, recording): (&AtomicBool, &Recording),
    orders: &Sender<JobOrder>,
    results: &Receiver<(JobId, Result<Done, KaptoError>)>,
) {
    let socket = cache.data_dir.join(JOB_SOCKET);
    let mut serve: Option<JobClient> = None;
    let mut told_unreachable = false;
    cache.trim();
    let mut trimmed = Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        if trimmed.elapsed() >= TRIM_EVERY {
            cache.trim();
            trimmed = Instant::now();
        }
        let Some(client) = serve.as_mut() else {
            match JobClient::connect(&socket) {
                Ok(mut client) => {
                    retention::finish_left_aside(cache.data_dir, &mut client);
                    serve = Some(client);
                    told_unreachable = false;
                }
                Err(error) => {
                    if !told_unreachable {
                        eprintln!("[worker] {error}; trying again");
                        told_unreachable = true;
                    }
                    wait(WAIT, shutdown);
                }
            }
            continue;
        };
        // Uploads are the upload lane's. While an episode is recorded only
        // deleting goes on: it gives a recording the disk is low for room.
        let asked = if recording.now() {
            client.next_job_of(&[DELETE])
        } else {
            client.next_job_except(&[UPLOAD])
        };
        let order = match asked {
            Ok(Some(order)) => order,
            Ok(None) => {
                wait(WAIT, shutdown);
                continue;
            }
            Err(error) => {
                eprintln!("[worker] {error}");
                serve = None;
                continue;
            }
        };
        if order.kind == DELETE {
            if !retention::delete_job(cache.data_dir, &order, client) {
                serve = None;
            }
            continue;
        }
        let (id, kind, episode) = (order.id, order.kind.clone(), order.episode.clone());
        if orders.send(order).is_err() {
            return;
        }
        // Stopping meanwhile, the job ends at its next step: waited for, so
        // it can be given back.
        let result = loop {
            match results.recv_timeout(POLL) {
                Ok((done, result)) if done == id => break result,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                // The job's thread is gone: the job stays running in the
                // queue and is done again after serve next starts.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        if let Err(error) = &result
            && error.code() == ErrorCode::WorkerJobStopped
        {
            match client.released(id) {
                Ok(()) => eprintln!("[worker] {kind} {episode}: stopped part-way, given back"),
                Err(error) => eprintln!("[worker] {error}"),
            }
            return;
        }
        if kind == REPLAY {
            cache.trim();
            trimmed = Instant::now();
        }
        match &result {
            Ok(Done { qa: Some(qa), .. }) if !qa.accepted() => {
                eprintln!("[worker] {kind} {episode}: done, a gate failed: not accepted");
            }
            Ok(done) => {
                eprintln!("[worker] {kind} {episode}: done");
                for warning in &done.warnings {
                    eprintln!("[worker] {kind} {episode}: {warning}");
                }
            }
            Err(error) => eprintln!("[worker] {kind} {episode}: {error}"),
        }
        let (done, failure) = match result {
            Ok(done) => (done, None),
            Err(error) => (Done::default(), Some(JobFailure::from(&error))),
        };
        let found = Found {
            qa: done.qa,
            events: done.events,
            warnings: done.warnings.iter().map(JobFailure::from).collect(),
        };
        if let Err(error) = client.finished(id, failure, found) {
            eprintln!("[worker] {error}");
            serve = None;
        }
    }
}

/// Sleeps for `time`, or less if `shutdown` is set meanwhile.
fn wait(time: Duration, shutdown: &AtomicBool) {
    let until = Instant::now() + time;
    while !shutdown.load(Ordering::Relaxed) && Instant::now() < until {
        thread::sleep(POLL.min(until.saturating_duration_since(Instant::now())));
    }
}

fn thread_failed(detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureThreadFailed).detail(detail.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_waits_between_steps_while_recording_and_stops_once_the_worker_stops() {
        let (recording, shutdown) = (Recording::new(), AtomicBool::new(false));
        assert!(step(&recording, &shutdown).is_ok());
        // Stopping wins over waiting: a paused job is given back too.
        recording.heard(true);
        shutdown.store(true, Ordering::Relaxed);
        let stopped = step(&recording, &shutdown).unwrap_err();
        assert_eq!(stopped.code(), ErrorCode::WorkerJobStopped);
    }

    #[test]
    fn capture_recording_holds_the_worker_only_while_the_answer_saying_so_is_fresh() {
        let recording = Recording::new();
        assert!(!recording.now_at(Duration::ZERO), "nothing heard yet");
        recording.heard_at(Duration::from_secs(10), true);
        assert!(recording.now_at(Duration::from_secs(12)));
        // The watch heard nothing more, wherever it is held: carry on.
        assert!(!recording.now_at(Duration::from_secs(13)));
        recording.heard_at(Duration::from_secs(14), true);
        assert!(recording.now_at(Duration::from_secs(14)));
        recording.heard_at(Duration::from_secs(15), false);
        assert!(!recording.now_at(Duration::from_secs(15)));
    }

    #[test]
    fn a_lane_ending_by_itself_stops_the_worker_but_one_ending_with_it_doesnt() {
        let (shutdown, ended) = (AtomicBool::new(false), AtomicBool::new(false));
        let panicked = thread::scope(|scope| {
            scope
                .spawn(|| {
                    let _ends = EndsWorker {
                        shutdown: &shutdown,
                        ended: &ended,
                    };
                    panic!("a lane's bug");
                })
                .join()
        });
        assert!(panicked.is_err());
        assert!(shutdown.load(Ordering::Relaxed) && ended.load(Ordering::Relaxed));

        // Stopping first, the lane ending is the worker's own doing.
        ended.store(false, Ordering::Relaxed);
        drop(EndsWorker {
            shutdown: &shutdown,
            ended: &ended,
        });
        assert!(!ended.load(Ordering::Relaxed));
    }
}
