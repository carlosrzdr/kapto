// SPDX-License-Identifier: Apache-2.0

//! `kapto capture`: the capture process. It receives every declared stream
//! for as long as it runs, feeding the live view, and records episodes when
//! told to over its control socket (see `kapto_capture::control`).
//!
//! Between episodes nothing is written: messages are received, shown live
//! and discarded. An episode is recorded exactly as `kapto record` records
//! one, with its own counts. An aborted episode is deleted instead of
//! finalised. SIGINT or SIGTERM finalises the episode being recorded, if
//! any, and ends the process.
//!
//! When `kapto serve` has gone for longer than `capture.control_timeout`
//! during an episode, the episode is finalised, so a crashed server loses
//! nothing. A capture process serve started (`--parent`) then ends, as it
//! does whenever serve is gone and no episode is recorded: left running, it
//! would hold the streams' inputs and the control socket from the next
//! serve. One started by hand receives until it is told to stop.
//!
//! Episodes that recording left unfinished, by a crash, a kill or a power
//! cut, are recovered when the process starts, on a thread of its own
//! while receiving goes on, and each one is announced to every watcher.
//!
//! Pre-flight runs once when the process starts and again whenever asked,
//! between episodes; a start asked for while it runs waits for it.
//!
//! The disk holding the recordings is watched once a second: below
//! `capture.disk_warn_below` free, Kapto warns; below
//! `capture.disk_stop_below`, the episode is finalised cleanly and no other
//! is started, so a recording is never cut off by a full disk.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::control::{
    Announcement, ControlServer, LiveViewCounts, NewEvent, Pending, Request, Response, Status,
    StreamStatus,
};
use kapto_capture::disk::{DiskLimits, DiskWatch, Room};
use kapto_capture::tap::LatestValues;
use kapto_capture::{Compression, WriterConfig};
use kapto_config::{DeploymentConfig, StreamDeclaration};
use kapto_core::clock::{monotonic_now, utc_now_ns};
use kapto_core::{EpisodeId, ErrorCode, KaptoError, RecvTime};
use kapto_store::{Event, PackageIdentity, Preflight as PreflightReport};

use crate::SettingFlags;
use crate::closing::Closer;
use crate::own_events::{Counts, OwnEvents, silences};
use crate::preflight::Preflight;
use crate::record::{Plan, RecordSettings, describe, load_plan, package_of, recover};
use crate::station::{Live, Next, Outcome, Station};
use crate::viz_bridge::warning;

/// Runs `kapto capture`.
pub(crate) fn capture(
    flags: &SettingFlags,
    streams: &Path,
    mapping: Option<&Path>,
    parent: Option<u32>,
) -> ExitCode {
    match serve(flags, streams, mapping, parent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

/// Says what capture receives and where it takes orders, and what its
/// pre-roll keeps.
fn say_receiving(station: &Station, deployment: &DeploymentConfig) {
    eprintln!(
        "Receiving {} streams, not recording. Orders on {}; live view on {}. Episodes are recorded with {}.",
        station.plan.streams.len(),
        deployment.control_socket.value.display(),
        deployment.tap_socket.value.display(),
        describe(station.package.as_ref()),
    );
    if let Some(pre_roll) = station.pre_roll_said() {
        eprintln!("{pre_roll}");
    }
}

fn serve(
    flags: &SettingFlags,
    streams: &Path,
    mapping: Option<&Path>,
    parent: Option<u32>,
) -> Outcome<()> {
    let (_, deployment) = crate::config::load(flags)?;
    let (declarations, plan) = load_plan(streams, &RecordSettings::of(&deployment))?;
    let shutdown = on_signals()?;
    // Before the streams' inputs are opened: a capture process already
    // running holds them, and is what to report.
    let control =
        ControlServer::listen(&deployment.control_socket.value).map_err(|error| vec![error])?;
    let mut station = open_station(&deployment, (streams, mapping), plan, &declarations)?;
    recover_meanwhile(&control, deployment.data_dir.value.clone())?;
    let mut closer = Closer::start()?;
    let mut disk = DiskWatch::new(deployment.data_dir.value.clone(), disk_limits(&deployment));
    say_receiving(&station, &deployment);

    let mut preflight = Preflight::new(&deployment, &declarations);
    let silences = silences(&declarations);
    // What profiles are checked against; it doesn't change while capture runs.
    let package = station.package.clone();
    let mut first = true;
    // Serve, once it has gone, between episodes.
    let mut orphaned = false;
    loop {
        let mut start = None;
        // Starts asked for while pre-flight runs, oldest first, each with
        // when it was asked: its pre-roll is reckoned from then.
        let mut waiting: Vec<(Pending, RecvTime)> = Vec::new();
        station.idle(&mut |live, elapsed| {
            closer.answer(&control);
            if first {
                preflight.begin(live, elapsed, None);
                first = false;
            }
            for pending in control.pending() {
                match pending.request {
                    Request::Start { .. } => waiting.push((pending, monotonic_now())),
                    Request::Preflight => preflight.begin(live, elapsed, Some(pending)),
                    Request::Status => {
                        let status =
                            status((None, closer.closing()), live, elapsed, preflight.latest());
                        pending.answer(Response::Status(status));
                    }
                    Request::Stop | Request::Abort => {
                        refuse(pending, ErrorCode::CaptureNotRecording);
                    }
                    // Told to watchers, but there's no recording to keep it.
                    Request::Event(ref asked) => {
                        let noted = note(asked, live, &control, None);
                        pending.answer(noted);
                    }
                    // Answered by the connection itself.
                    Request::Hello { .. } | Request::Supervise | Request::Watch => {}
                }
            }
            // From when the first was asked, the episode it would start is
            // held, so it starts then however long it waits.
            if let Some((_, at)) = waiting.first() {
                live.start_waiting(Some(*at));
            }
            preflight.finish(live, elapsed);
            if !preflight.running() {
                let mut asked = waiting.drain(..);
                start = asked.next().and_then(|(pending, at)| {
                    startable(pending, &mut disk, package.as_ref(), &preflight)
                        .map(|(pending, cleared)| (pending, cleared, at))
                });
                // Only one episode at a time: the first start wins.
                asked.for_each(|(pending, _)| refuse(pending, ErrorCode::CaptureAlreadyRecording));
                if start.is_none() {
                    live.start_waiting(None);
                }
            }
            orphaned =
                start.is_none() && waiting.is_empty() && parent.is_some_and(crate::parent::gone);
            start.is_none() && !shutdown.load(Ordering::Relaxed) && !orphaned
        })?;
        if let Some((pending, cleared, asked)) = start {
            let Request::Start {
                duration_s,
                profile,
                task,
                ..
            } = pending.request.clone()
            else {
                continue;
            };
            station.session = (profile.map(|chosen| chosen.profile), task.map(|task| *task));
            station.preflight = cleared;
            let duration = duration_s.map(|seconds| Duration::from_secs(u64::from(seconds)));
            let timeout = deployment.control_timeout.value;
            episode(
                &mut station,
                (&control, &mut disk, &mut closer),
                &shutdown,
                (pending, duration, asked),
                (timeout, &silences),
            );
        }
        if shutdown.load(Ordering::Relaxed) || orphaned {
            stop(closer, &control, orphaned);
            return Ok(());
        }
    }
}

/// Opens the streams' inputs, with what each episode carries and how it
/// is written.
fn open_station(
    deployment: &DeploymentConfig,
    (streams, mapping): (&Path, Option<&Path>),
    plan: Plan,
    declarations: &[StreamDeclaration],
) -> Outcome<Station> {
    let mut station = Station::open(
        plan,
        declarations,
        live_view(deployment),
        deployment.data_dir.value.clone(),
    )
    .map_err(|error| vec![error])?;
    (station.package, station.carried) =
        package_of(streams, mapping, deployment.attach_meshes.value);
    station.writer = writer_config(deployment);
    Ok(station)
}

/// Ends capture once what was started is finished: the episodes still
/// closing.
fn stop(closer: Closer, control: &ControlServer, orphaned: bool) {
    if orphaned {
        eprintln!("kapto serve has gone and no episode is recorded: capture stops.");
    }
    closer.finish(control);
    eprintln!("Stopped.");
}

/// A flag SIGINT and SIGTERM set, which ends capture once what it is doing
/// is finished.
fn on_signals() -> Outcome<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&shutdown)).map_err(|error| {
            vec![
                KaptoError::new(ErrorCode::CaptureThreadFailed)
                    .detail(format!("can't handle signal {signal}: {error}")),
            ]
        })?;
    }
    Ok(shutdown)
}

/// Answers `pending`, asked while `episode` is recorded, unless it is a
/// stop or abort to carry out, which is returned instead: the first one
/// only, while none is `stopping` yet.
fn answer_while_recording(
    pending: Pending,
    (live, elapsed, episodes, preflight): (&Live<'_>, Duration, Episodes, Option<&PreflightReport>),
    control: &ControlServer,
    stopping: bool,
) -> Option<Pending> {
    match pending.request {
        Request::Stop | Request::Abort if !stopping => return Some(pending),
        Request::Status => {
            let status = status(episodes.clone(), live, elapsed, preflight);
            pending.answer(Response::Status(status));
        }
        Request::Start { .. } | Request::Stop | Request::Abort => {
            refuse(pending, ErrorCode::CaptureAlreadyRecording);
        }
        // Its test file would compete with the recording's writes.
        Request::Preflight => pending.answer(Response::error(
            &KaptoError::new(ErrorCode::CaptureAlreadyRecording)
                .detail("pre-flight runs between episodes".to_owned()),
        )),
        Request::Event(ref asked) => {
            let noted = note(asked, live, control, episodes.0.as_deref());
            pending.answer(noted);
        }
        Request::Hello { .. } | Request::Supervise | Request::Watch => {}
    }
    None
}

/// Keeps the events of Kapto's own that each stream's counts call for now.
fn keep_own(
    own: &mut OwnEvents,
    live: &Live<'_>,
    elapsed: Duration,
    control: &ControlServer,
    episode: Option<&str>,
) {
    let counts: Vec<Counts> = live
        .streams()
        .map(|(_, totals)| Counts {
            received: totals.received,
            dropped_by_us: totals.dropped_by_us,
            lost_upstream: totals.lost_upstream,
        })
        .collect();
    for event in own.tick(&counts, elapsed, utc_now_ns()) {
        keep(&event, live, control, episode);
    }
}

/// Has `event` written into the episode being recorded, if one is, and
/// tells every watcher. Returns the episode it was recorded in.
fn keep(
    event: &Event,
    live: &Live<'_>,
    control: &ControlServer,
    episode: Option<&str>,
) -> Option<String> {
    let episode = episode
        .filter(|_| live.record_event(event))
        .map(str::to_owned);
    control.announce(Announcement::EventNoted {
        happened: Box::new(event.clone()),
        episode: episode.clone(),
    });
    episode
}

/// Takes an event a client sent: checks it, has it written into the
/// episode being recorded if there is one, tells every watcher, and returns
/// the answer. Its moment, unless given, is now on the wall clock, read once
/// per event, never per message.
fn note(
    asked: &NewEvent,
    live: &Live<'_>,
    control: &ControlServer,
    episode: Option<&str>,
) -> Response {
    let event = asked.clone().at(utc_now_ns());
    if let Err(error) = event.check(false) {
        return Response::error(&error);
    }
    let episode = keep(&event, live, control, episode);
    Response::Noted {
        event: Box::new(event),
        episode,
    }
}

/// Recovers the episodes in `data_dir` that recording left unfinished, on a
/// thread of its own, announcing each one: salvaging reads each recording
/// through, which may take a while, so not while nothing is received.
/// Episodes recorded meanwhile hold their folders, so it never touches
/// them.
fn recover_meanwhile(control: &ControlServer, data_dir: PathBuf) -> Outcome<()> {
    let announcer = control.announcer();
    thread::Builder::new()
        .name("cap-recover".into())
        .spawn(move || {
            recover(&data_dir, &mut |episode| {
                announcer.announce(Announcement::EpisodeRecovered {
                    episode: episode.to_string(),
                });
            });
        })
        .map(drop)
        .map_err(|error| {
            vec![
                KaptoError::new(ErrorCode::CaptureThreadFailed)
                    .detail(format!("can't start recovering episodes: {error}")),
            ]
        })
}

/// Records one episode, answering control requests as it goes, and
/// answers `start` once it is recording. It ends when told to, after
/// `duration` if one was given, once the supervisor has been gone for
/// `control_timeout`, or once the disk is nearly full, and is handed to
/// `closer`, which answers its stop once it is finalised. A failure is
/// reported and answered, and the process goes back to receiving.
fn episode(
    station: &mut Station,
    (control, disk, closer): (&ControlServer, &mut DiskWatch, &mut Closer),
    shutdown: &AtomicBool,
    (start, duration, asked): (Pending, Option<Duration>, RecvTime),
    (control_timeout, silences): (Duration, &[(String, Option<Duration>)]),
) {
    let start = RefCell::new(Some(start));
    let current: RefCell<Option<EpisodeId>> = RefCell::new(None);
    // The stop or abort request being carried out, answered once done.
    let mut stop: Option<Pending> = None;
    // When the episode ends once stopped: after the longest post-roll.
    let mut ends: Option<Duration> = None;
    let mut next_second = 1;
    let mut told = DiskTold::default();
    let streams = station.plan.streams.len();
    // The run this episode keeps, as status reports it while it is recorded.
    let preflight = station.preflight.clone();
    let mut own = OwnEvents::new(silences.iter().cloned());
    let finished = station.record(
        asked,
        &mut |episode, buffers| {
            eprintln!(
                "Recording episode {episode} from {streams} streams (buffers: {} batches, {} MiB). Started.",
                buffers.batches,
                buffers.memory_bytes / (1024 * 1024),
            );
            *current.borrow_mut() = Some(*episode);
            control.announce(Announcement::EpisodeStarted {
                episode: episode.to_string(),
            });
            if let Some(start) = start.borrow_mut().take() {
                start.answer(Response::Started {
                    episode: episode.to_string(),
                });
            }
        },
        &mut |live, elapsed| {
            // An episode before this one may finish closing meanwhile.
            closer.answer(control);
            let episode = current.borrow().as_ref().map(ToString::to_string);
            keep_own(&mut own, live, elapsed, control, episode.as_deref());
            for pending in control.pending() {
                let episodes = (episode.clone(), closer.closing());
                let recording = (live, elapsed, episodes, preflight.as_ref());
                if let Some(stopping) = answer_while_recording(pending, recording, control, stop.is_some()) {
                    stop = Some(stopping);
                }
            }
            // Reckoned from when it was asked for: the start is answered,
            // and a supervisor may give a sign, before `elapsed` begins.
            let since_asked = monotonic_now().saturating_since(asked);
            let abandoned = abandoned(control, control_timeout, since_asked, stop.is_none());
            if elapsed >= Duration::from_secs(next_second) {
                eprintln!("{}", live.line(next_second));
                next_second += 1;
            }
            let full = told.check(disk);
            let stopping = stop.is_some() || duration.is_some_and(|duration| elapsed >= duration);
            match &stop {
                Some(pending) if pending.request == Request::Abort => Next::Discard,
                _ if abandoned || full || shutdown.load(Ordering::Relaxed) => Next::Finalise,
                // Stopped: the streams with a post-roll go on for theirs.
                _ if stopping && elapsed >= *ends.get_or_insert_with(|| elapsed + live.stop()) => {
                    Next::Finalise
                }
                _ => Next::Record,
            }
        },
    );
    match finished {
        Ok(recorded) => {
            if let Err(errors) = &recorded.receivers {
                report(errors);
            }
            // Closed beside the next phase; its stop answered once it is.
            match recorded.closing {
                Some(closing) => closer.close(closing, stop),
                None => closer.aborted(recorded.episode, stop),
            }
            closer.answer(control);
        }
        Err(errors) => {
            report(&errors);
            // Told it started, so told it ended, though nothing was kept.
            if let Some(episode) = current.into_inner() {
                control.announce(Announcement::EpisodeEnded {
                    episode: episode.to_string(),
                    finalised: false,
                });
            }
            let answer = errors.first().map_or_else(
                || Response::error(&KaptoError::new(ErrorCode::CaptureWriteFailed)),
                Response::error,
            );
            for pending in start.into_inner().into_iter().chain(stop) {
                pending.answer(answer.clone());
            }
        }
    }
}

/// `pending`, a start, if it can be carried out: a profile it asks for
/// records the robot package capture records, the disk has room, and
/// pre-flight's latest run passed or the start waives what failed. With
/// that run as the episode keeps it. Answered with why not otherwise.
fn startable(
    pending: Pending,
    disk: &mut DiskWatch,
    package: Option<&PackageIdentity>,
    preflight: &Preflight,
) -> Option<(Pending, Option<PreflightReport>)> {
    if let Request::Start {
        profile: Some(chosen),
        ..
    } = &pending.request
        && package.is_none_or(|package| package.id != chosen.package)
    {
        let running = package.map_or_else(
            || "no robot package (its streams aren't declared in one)".to_owned(),
            |package| format!("robot package `{}`", package.id),
        );
        let error = KaptoError::new(ErrorCode::ProfileUnavailable).detail(format!(
            "profile `{}` records robot package `{}`, and capture records {running}",
            chosen.profile.id, chosen.package
        ));
        pending.answer(Response::error(&error));
        return None;
    }
    match disk.room(Instant::now()) {
        Room::TooLow(free) => {
            let error = nearly_full(free, disk.limits());
            eprintln!("{}", error.report());
            pending.answer(Response::error(&error));
            None
        }
        Room::Enough | Room::Low(_) => {
            let waive = match &pending.request {
                Request::Start { waive, .. } => waive.as_slice(),
                _ => &[],
            };
            match preflight.clear(waive) {
                Ok(cleared) => {
                    let mut checks = cleared.iter().flat_map(|report| &report.checks);
                    if checks.any(|check| !check.passed && !check.waived) {
                        eprintln!(
                            "Starting despite failed pre-flight checks: capture.preflight is `warn`."
                        );
                    }
                    Some((pending, cleared))
                }
                Err(error) => {
                    eprintln!("{}", error.report());
                    pending.answer(Response::error(&error));
                    None
                }
            }
        }
    }
}

/// The episode being recorded and the one being closed, each if one is.
type Episodes = (Option<String>, Option<String>);

fn status(
    (episode, closing): Episodes,
    live: &Live<'_>,
    elapsed: Duration,
    preflight: Option<&PreflightReport>,
) -> Status {
    let mut latest = live
        .latest
        .map(LatestValues::snapshot)
        .unwrap_or_default()
        .into_iter();
    Status {
        episode,
        seconds: elapsed.as_secs(),
        streams: live
            .streams()
            .map(|(id, totals)| {
                let newest = latest.next().flatten();
                StreamStatus {
                    stream: id.to_string(),
                    received: totals.received,
                    written: totals.written,
                    dropped_by_us: totals.dropped_by_us,
                    lost_upstream: totals.lost_upstream,
                    values_age_ms: newest
                        .as_ref()
                        .map(|newest| u64::try_from(newest.age.as_millis()).unwrap_or(u64::MAX)),
                    values: newest.map(|newest| newest.values),
                    live_view: live.live_view(id).map(|camera| LiveViewCounts {
                        offered: camera.offered,
                        skipped: camera.skipped,
                        superseded: camera.superseded,
                        not_sent: camera.not_sent,
                        sent: camera.sent,
                    }),
                }
            })
            .collect(),
        preflight: preflight.cloned(),
        closing,
    }
}

/// Whether the supervisor has been gone for `control_timeout`, saying so
/// if the episode is to end for it. Only a supervisor that left since this
/// episode was asked for, `since_asked` ago, counts: one started by hand
/// after serve went away is the operator's.
fn abandoned(
    control: &ControlServer,
    control_timeout: Duration,
    since_asked: Duration,
    say: bool,
) -> bool {
    let abandoned = control
        .unsupervised_for()
        .is_some_and(|gone| gone >= control_timeout && gone <= since_asked);
    if abandoned && say {
        eprintln!(
            "kapto serve has been gone for {} s: finalising the episode.",
            control_timeout.as_secs_f64()
        );
    }
    abandoned
}

/// What an episode has said about the disk, so each is said once rather
/// than every time the disk is read.
#[derive(Default)]
struct DiskTold {
    warned: bool,
    full: bool,
}

impl DiskTold {
    /// Reads the room left, says what changed, and tells whether the
    /// episode must end.
    fn check(&mut self, disk: &mut DiskWatch) -> bool {
        match disk.room(Instant::now()) {
            Room::Low(free) if !self.warned => {
                self.warned = true;
                eprintln!("{}", warning(&low(free, disk.limits())));
            }
            Room::TooLow(free) if !self.full => {
                self.full = true;
                eprintln!(
                    "{}\nFinalising the episode. Receiving goes on.",
                    nearly_full(free, disk.limits()).report()
                );
            }
            Room::Enough | Room::Low(_) | Room::TooLow(_) => {}
        }
        self.full
    }
}

/// When the data directory's disk is low and too full to record on, from
/// the settings.
/// How recordings are written: `capture.compression` over the writer's
/// defaults.
pub(crate) fn writer_config(deployment: &DeploymentConfig) -> WriterConfig {
    WriterConfig {
        compression: match deployment.compression.value {
            kapto_config::Compression::Lz4 => Compression::Lz4,
            kapto_config::Compression::Zstd => Compression::Zstd,
            kapto_config::Compression::None => Compression::None,
        },
        ..WriterConfig::default()
    }
}

pub(crate) fn disk_limits(deployment: &DeploymentConfig) -> DiskLimits {
    let bytes = |mib: u64| mib.saturating_mul(1024 * 1024);
    DiskLimits {
        warn_below: bytes(deployment.disk_warn_below_mib.value),
        stop_below: bytes(deployment.disk_stop_below_mib.value),
    }
}

/// Where the live view's samples and frames go, and how many frames a
/// second of each camera: the settings `Station::open` takes.
pub(crate) fn live_view(deployment: &DeploymentConfig) -> (&Path, &Path, u32) {
    (
        &deployment.tap_socket.value,
        &deployment.frame_socket.value,
        deployment.viewer_camera_max_fps.value,
    )
}

/// In MiB, as the settings write it.
fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

fn low(free: u64, limits: DiskLimits) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureDiskLow).detail(format!(
        "{} MiB free, below capture.disk_warn_below ({} MiB); recording stops below {} MiB",
        mib(free),
        mib(limits.warn_below),
        mib(limits.stop_below)
    ))
}

fn nearly_full(free: u64, limits: DiskLimits) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureDiskNearlyFull).detail(format!(
        "{} MiB free, below capture.disk_stop_below ({} MiB)",
        mib(free),
        mib(limits.stop_below)
    ))
}

fn refuse(pending: Pending, code: ErrorCode) {
    pending.answer(Response::error(&KaptoError::new(code)));
}

fn report(errors: &[KaptoError]) {
    let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
    eprintln!("{}", reports.join("\n\n"));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use super::*;

    #[test]
    fn the_disk_is_told_once_low_and_once_too_full_and_only_too_full_ends_the_episode() {
        let free = Arc::new(AtomicU64::new(5_000));
        let mut disk = DiskWatch::with_probe(
            DiskLimits {
                warn_below: 10_000,
                stop_below: 2_000,
            },
            {
                let free = Arc::clone(&free);
                move || Ok(free.load(Ordering::Relaxed))
            },
        );
        let mut told = DiskTold::default();
        assert!(!told.check(&mut disk));
        assert!(told.warned && !told.full);

        free.store(1_000, Ordering::Relaxed);
        thread::sleep(DiskWatch::EVERY);
        assert!(told.check(&mut disk));
        assert!(told.check(&mut disk), "stays ended");
    }
}
