// SPDX-License-Identifier: Apache-2.0

//! `kapto episode`: orders for a running `kapto capture`, over its control
//! socket.

use std::fmt::Write as _;
use std::process::ExitCode;

use clap::Subcommand;
use kapto_capture::control::{ANSWER_TIMEOUT, ControlClient, NewEvent, Request, Response, Status};
use kapto_core::clock::utc_now_ns;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{Event, Phase, Preflight};

use crate::SettingFlags;

#[derive(Subcommand)]
pub(crate) enum EpisodeCommand {
    /// Start recording an episode.
    Start {
        /// Stop by itself after this many seconds; without it, records until
        /// `kapto episode stop`.
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u32).range(1..=604_800))]
        duration: Option<u32>,
        /// Record it with this session profile from the configuration
        /// folder (`kapto profile list`), for the robot package capture
        /// records; a copy is kept in the episode.
        #[arg(long, value_name = "ID")]
        profile: Option<String>,
        /// Record it for this task from the configuration folder (`kapto
        /// task list`); a copy is kept in the episode.
        #[arg(long, value_name = "ID")]
        task: Option<String>,
        /// Record even though this pre-flight check failed, given by its
        /// cause code (e.g. `preflight.stream_silent`); repeat for each.
        /// The episode says which checks were waived.
        #[arg(long, value_name = "CODE")]
        waive: Vec<String>,
    },
    /// Stop recording, and wait until the episode is finalised.
    Stop,
    /// Stop recording and discard the episode: nothing of it is kept.
    Abort,
    /// Show whether an episode is being recorded, and each stream's counts.
    Status,
    /// Check again, between episodes, that this machine can record what the
    /// robot sends: room and speed of the disk, each stream at its declared
    /// rate. Fails if a check fails.
    Preflight,
    /// Send an event: something that happened now, or the beginning, end or
    /// whole of a stretch. Recorded in the episode being recorded; between
    /// episodes only told to whoever watches.
    Event {
        /// Its kind, from the event vocabulary: `events.toml` in the
        /// configuration folder (or `note`, `problem` and `intervention`
        /// without it), and each task's subtasks as `subtask.<name>`.
        #[arg(long)]
        kind: String,
        /// It begins a stretch, paired with its end by this ID.
        #[arg(long, value_name = "ID", conflicts_with_all = ["end", "last"])]
        begin: Option<String>,
        /// It ends the stretch begun with this ID.
        #[arg(long, value_name = "ID", conflicts_with = "last")]
        end: Option<String>,
        /// It is a whole stretch: the last this many seconds, ending now.
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u32).range(1..=86_400))]
        last: Option<u32>,
        /// What to say about it.
        #[arg(long)]
        text: Option<String>,
    },
}

pub(crate) fn episode(flags: &SettingFlags, command: &EpisodeCommand) -> ExitCode {
    let deployment = match crate::config::load(flags) {
        Ok((_, deployment)) => deployment,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            return ExitCode::FAILURE;
        }
    };
    match order(flags, &deployment, command) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{}", error.report());
            ExitCode::FAILURE
        }
    }
}

/// Carries out `command`. Returns whether what it found is good: false
/// only for a pre-flight with a failed check.
fn order(
    flags: &SettingFlags,
    deployment: &kapto_config::DeploymentConfig,
    command: &EpisodeCommand,
) -> Result<bool, KaptoError> {
    let mut client = ControlClient::connect(&deployment.control_socket.value, ANSWER_TIMEOUT)?;
    let request = match command {
        EpisodeCommand::Start {
            duration,
            profile,
            task,
            waive,
        } => {
            let (profile, task) = kapto_server::sessions::resolve(
                &crate::config::config_folder(flags),
                profile.as_deref(),
                task.as_deref(),
            )?;
            Request::Start {
                duration_s: *duration,
                profile: profile.map(Box::new),
                task: task.map(Box::new),
                waive: waive.clone(),
            }
        }
        EpisodeCommand::Stop => Request::Stop,
        EpisodeCommand::Abort => Request::Abort,
        EpisodeCommand::Status => Request::Status,
        EpisodeCommand::Preflight => Request::Preflight,
        EpisodeCommand::Event {
            kind,
            begin,
            end,
            last,
            text,
        } => {
            let event = new_event(
                kind,
                begin.as_deref(),
                end.as_deref(),
                *last,
                text.as_deref(),
            );
            kapto_server::vocabulary::check(&crate::config::config_folder(flags), &event)?;
            Request::Event(Box::new(event))
        }
    };
    match client.ask(&request)? {
        Response::Started { episode } => println!("Recording episode {episode}."),
        Response::Stopped { folder, .. } => println!("Finalised {folder}"),
        Response::Aborted { episode } => {
            println!("Aborted episode {episode}; nothing of it was kept.");
        }
        Response::Status(status) => print!("{}", table(&status)),
        Response::Noted { event, episode } => println!("{}", noted(&event, episode.as_deref())),
        Response::Preflight(report) => {
            print!("{}", checks(&report));
            return Ok(report.checks.iter().all(|check| check.passed));
        }
        other => return Err(unexpected(&format!("{other:?}"))),
    }
    Ok(true)
}

/// The event `kapto episode event` sends.
fn new_event(
    kind: &str,
    begin: Option<&str>,
    end: Option<&str>,
    last: Option<u32>,
    text: Option<&str>,
) -> NewEvent {
    let (phase, id) = match (begin, end) {
        (Some(id), _) => (Phase::Begin, Some(id.to_owned())),
        (None, Some(id)) => (Phase::End, Some(id.to_owned())),
        (None, None) if last.is_some() => (Phase::Stretch, None),
        (None, None) => (Phase::Moment, None),
    };
    // A whole stretch ends now; anything else is when capture takes it.
    let now = utc_now_ns();
    let (at_utc_ns, until_utc_ns) = match last {
        Some(seconds) => (
            Some(now.saturating_sub(i64::from(seconds) * 1_000_000_000)),
            Some(now),
        ),
        None => (None, None),
    };
    NewEvent {
        kind: kind.to_owned(),
        at_utc_ns,
        until_utc_ns,
        phase,
        id,
        source: "cli".to_owned(),
        text: text.map(str::to_owned),
        attributes: std::collections::BTreeMap::new(),
    }
}

fn noted(event: &Event, episode: Option<&str>) -> String {
    let what = match (event.phase, &event.id) {
        (Phase::Begin, Some(id)) => format!("the beginning of {} {id}", event.kind),
        (Phase::End, Some(id)) => format!("the end of {} {id}", event.kind),
        (Phase::Stretch, _) => format!("a stretch of {}", event.kind),
        _ => event.kind.clone(),
    };
    match episode {
        Some(episode) => format!("Recorded {what} in episode {episode}."),
        None => format!("Noted {what}; no episode is being recorded, so it isn't kept."),
    }
}

fn checks(report: &Preflight) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a window of a few seconds, shown to a tenth"
    )]
    let window = report.window_ms as f64 / 1000.0;
    let mut text = format!(
        "Pre-flight at {}, streams watched for {window:.1} s\n",
        report.ran_at
    );
    for check in &report.checks {
        let about = check
            .stream
            .as_ref()
            .map_or_else(String::new, |stream| format!(" ({stream})"));
        let _ = writeln!(
            text,
            "{:<6} {}{about}: {}; needs {}",
            if check.passed { "ok" } else { "FAILED" },
            check.code,
            check.measured,
            check.needed
        );
    }
    text
}

fn table(status: &Status) -> String {
    let mut text = match &status.episode {
        Some(episode) => format!("Recording episode {episode} for {} s\n", status.seconds),
        None => format!("Not recording; receiving for {} s\n", status.seconds),
    };
    let _ = writeln!(
        text,
        "{:<24} {:>10} {:>10} {:>8} {:>8}",
        "STREAM", "RECEIVED", "WRITTEN", "DROPPED", "LOST"
    );
    for stream in &status.streams {
        let _ = writeln!(
            text,
            "{:<24} {:>10} {:>10} {:>8} {:>8}",
            stream.stream,
            stream.received,
            stream.written,
            stream.dropped_by_us,
            stream.lost_upstream
        );
    }
    text
}

fn unexpected(detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureControlUnexpectedAnswer)
        .detail(format!("unexpected answer {detail}"))
}
