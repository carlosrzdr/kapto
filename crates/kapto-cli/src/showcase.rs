// SPDX-License-Identifier: Apache-2.0

//! `kapto showcase`: Kapto's story in one command, for showing it to
//! people. It runs as `kapto demo` does, then narrates, step by step: the
//! arm live in the page, a dozen ordinary episodes recorded so the quality
//! checks and the anomaly model learn what usual looks like, then one in
//! which the robot misbehaves while an intervention is marked, and what
//! Kapto found in it and where to watch it. Each step waits for Enter, so a
//! presenter can talk over it, or runs on by itself with `--unattended`.

use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;
use kapto_capture::control::{ANSWER_TIMEOUT, ControlClient, NewEvent, Request, Response};
use kapto_core::{ErrorCode, KaptoError};
use kapto_loadgen::faults::{Fault, Faults};
use kapto_loadgen::{Config, Transport, run_with_faults};
use kapto_store::db::DATABASE_FILE;
use kapto_store::{Phase, QaRun, Reader, Verdict};

use crate::SettingFlags;
use crate::demo::write_arm7;
use crate::serve::{self, ServeArgs};
use crate::station::Outcome;

/// The showcase's profile and task, in a configuration folder of its own so
/// the user's is never read or written.
const CONFIG: &[(&str, &str)] = &[
    ("kapto.toml", ""),
    (
        "profiles/showcase.toml",
        "package = \"arm7\"\noperator = \"Showcase\"\nmode = \"teleoperated\"\n",
    ),
    (
        "tasks/pick_and_place.toml",
        "name = \"Pick and place\"\nprompt = \"Pick up the red cube and put it in the bin\"\n\
         subtasks = [\"reach\", \"grasp\", \"lift\", \"place\"]\n",
    ),
];

/// The sample arm's quality settings, which the showcase changes.
const ARM7_QA: &str = include_str!("../../../packages/arm7/qa.toml");

/// How long capture may take to start answering.
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the worker may take to check an episode.
const CHECK_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a step is shown for when no one presses Enter.
const UNATTENDED_PAUSE: Duration = Duration::from_secs(2);

/// Arguments of `kapto showcase`.
#[derive(Args)]
pub(crate) struct ShowcaseArgs {
    /// Run every step on by itself instead of waiting for Enter.
    #[arg(long)]
    unattended: bool,
    /// Ordinary episodes recorded before the robot misbehaves: what the
    /// anomaly model learns from (3 or more).
    #[arg(long, value_name = "N", default_value_t = 12, value_parser = clap::value_parser!(u32).range(3..=200))]
    episodes: u32,
    /// Seconds each ordinary episode lasts.
    #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=600))]
    seconds: u32,
    /// UDP port the synthetic robot sends to and Kapto receives on.
    #[arg(long, value_name = "PORT", default_value_t = 9000)]
    robot_port: u16,
}

pub(crate) fn showcase(flags: &SettingFlags, args: &ShowcaseArgs) -> ExitCode {
    match run(flags, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

fn run(flags: &SettingFlags, args: &ShowcaseArgs) -> Outcome<()> {
    let (_, deployment) = crate::config::load(flags)?;
    let data_dir = deployment.data_dir.value.clone();
    let mut flags = flags.clone();
    let config = data_dir.join("showcase/config");
    write_files(&config, CONFIG).map_err(|error| vec![error])?;
    flags.config = Some(config.join("kapto.toml"));
    let folder = data_dir.join("showcase/arm7");
    write_arm7(&folder, args.robot_port, RATE_HZ, None).map_err(|error| vec![error])?;
    // A model learned from the episodes the showcase records, not twenty:
    // other settings, so another settings version.
    let qa = format!(
        "{}\n[anomalies.autoencoder]\nmin_episodes = {}\n",
        ARM7_QA.replace(
            "qa_params_version = \"1\"",
            "qa_params_version = \"1-showcase\""
        ),
        args.episodes
    );
    write_files(&folder, &[("qa.toml", &qa)]).map_err(|error| vec![error])?;
    let page = format!(
        "http://{}:{}",
        deployment.server_bind.value, deployment.server_port.value
    );

    let robot = Config {
        target: ([127, 0, 0, 1], args.robot_port).into(),
        streams: 1,
        rate_hz: RATE_HZ,
        duration: None,
        spin: Duration::ZERO,
        transport: Transport::Udp,
        wait_for_subscribers: None,
        camera: None,
        raw_camera: false,
    };
    let faults = Faults::new();
    let stop = AtomicBool::new(false);
    let story = Story {
        control: deployment.control_socket.value.clone(),
        database: data_dir.join(DATABASE_FILE),
        config: config.clone(),
        page,
        args,
        faults: &faults,
        stop: &stop,
    };
    thread::scope(|scope| {
        let sending = thread::Builder::new()
            .name("demo-robot".into())
            .spawn_scoped(scope, || run_with_faults(&robot, &stop, &faults, |_, _| {}))
            .map_err(|error| vec![thread_failed(&error)])?;
        thread::Builder::new()
            .name("showcase".into())
            .spawn_scoped(scope, || {
                if let Err(error) = story.tell() {
                    eprintln!("The showcase stopped: {}", error.report());
                }
            })
            .map_err(|error| vec![thread_failed(&error)])?;
        let served = serve::run(
            &flags,
            &ServeArgs {
                robot: folder.join("urdf/arm7.urdf"),
                streams: folder.join("streams.toml"),
                mapping: folder.join("mapping.toml"),
            },
        );
        stop.store(true, Ordering::Relaxed);
        if let Ok(Err(error)) = sending.join() {
            eprintln!("warning: the synthetic robot stopped: {error}");
        }
        served
    })
}

/// The arm's rate: enough to see it move smoothly, little enough to check
/// a dozen episodes in seconds.
const RATE_HZ: u32 = 200;

/// The showcase's steps and what they need.
struct Story<'a> {
    control: PathBuf,
    database: PathBuf,
    config: PathBuf,
    page: String,
    args: &'a ShowcaseArgs,
    faults: &'a Faults,
    stop: &'a AtomicBool,
}

impl Story<'_> {
    fn tell(&self) -> Result<(), KaptoError> {
        let mut capture = self.connect()?;
        let (profile, task) = kapto_server::sessions::resolve(
            &self.config,
            Some("showcase"),
            Some("pick_and_place"),
        )?;
        let start = |duration_s| Request::Start {
            duration_s,
            profile: profile.clone().map(Box::new),
            task: task.clone().map(Box::new),
            waive: Vec::new(),
        };

        self.step(&format!(
            "1. Open {} in a browser. The arm is live: what it sends arrives in a few milliseconds.\n   \
             Press Environment, then Add a sample cell, for a table, a red cube and a bin around it.",
            self.page
        ));
        self.step(&format!(
            "2. Recording {} ordinary episodes of {} s each, task Pick and place. Each one is checked\n   \
             as soon as it ends; together they teach the anomaly model what usual looks like.",
            self.args.episodes, self.args.seconds
        ));
        for n in 1..=self.args.episodes {
            let episode = started(capture.ask(&start(Some(self.args.seconds)))?)?;
            thread::sleep(Duration::from_secs(u64::from(self.args.seconds)));
            wait_stopped(&mut capture)?;
            let run = self.checked(&episode)?;
            println!(
                "   episode {n} of {}: {}",
                self.args.episodes,
                if run.accepted() {
                    "accepted"
                } else {
                    "rejected"
                }
            );
        }

        self.step(
            "3. Now an episode in which the robot misbehaves, with an intervention marked around it,\n   \
             as an operator would: the shoulder's reading sticks, then jumps back; the wrist vibrates;\n   \
             a burst of messages is lost on the way.",
        );
        let episode = started(capture.ask(&start(None))?)?;
        thread::sleep(Duration::from_secs(2));
        capture.ask(&marked(Phase::Begin))?;
        for (fault, lasts, said) in [
            (Fault::FreezeThenJump, 3, "the shoulder's reading sticks"),
            (Fault::Vibrate, 3, "the wrist vibrates"),
            (Fault::Drop, 1, "messages are lost"),
        ] {
            println!("   {said}");
            self.faults.start(fault, Duration::from_secs(lasts));
            thread::sleep(Duration::from_secs(lasts + 1));
        }
        capture.ask(&marked(Phase::End))?;
        thread::sleep(Duration::from_secs(1));
        capture.ask(&Request::Stop)?;
        let run = self.checked(&episode)?;
        println!("   What Kapto found in it:");
        for found in findings(&run) {
            println!("   - {found}");
        }

        self.step(
            "4. In the page: Episodes, With: Intervention, then open the newest. Its events come first;\n   \
             each check's moments have a Watch button that replays the episode from just before it.",
        );
        println!(
            "The showcase is done. Kapto keeps running for questions; Ctrl-C stops it. Run it again to start over."
        );
        Ok(())
    }

    /// Says `what` and waits for Enter, or a moment when unattended or with
    /// nothing to read Enter from.
    fn step(&self, what: &str) {
        println!("\n{what}");
        if self.args.unattended || !std::io::stdin().is_terminal() {
            thread::sleep(UNATTENDED_PAUSE);
            return;
        }
        println!("   (Enter to go on)");
        let _ = std::io::stdin().lock().read_line(&mut String::new());
    }

    /// Capture, once it answers.
    fn connect(&self) -> Result<ControlClient, KaptoError> {
        let until = Instant::now() + START_TIMEOUT;
        loop {
            match ControlClient::connect(&self.control, ANSWER_TIMEOUT) {
                Ok(client) => return Ok(client),
                Err(error) if Instant::now() > until || self.stop.load(Ordering::Relaxed) => {
                    return Err(error);
                }
                Err(_) => thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    /// What the quality checks found on `episode`, once the worker has checked it.
    fn checked(&self, episode: &str) -> Result<QaRun, KaptoError> {
        let until = Instant::now() + CHECK_TIMEOUT;
        loop {
            if let Ok(reader) = Reader::open(&self.database)
                && let Some(run) = reader.qa(episode)?
            {
                return Ok(run);
            }
            if Instant::now() > until {
                return Err(
                    KaptoError::new(ErrorCode::WorkerServeUnreachable).detail(format!(
                        "episode {episode} wasn't checked within {} s",
                        CHECK_TIMEOUT.as_secs()
                    )),
                );
            }
            thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Waits until the episode capture records has ended and is finalised.
fn wait_stopped(capture: &mut ControlClient) -> Result<(), KaptoError> {
    loop {
        if let Response::Status(status) = capture.ask(&Request::Status)?
            && status.episode.is_none()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// The episode a start answer names.
fn started(answer: Response) -> Result<String, KaptoError> {
    match answer {
        Response::Started { episode } => Ok(episode),
        other => Err(KaptoError::new(ErrorCode::CaptureControlUnreachable)
            .detail(format!("capture didn't start an episode: {other:?}"))),
    }
}

/// One end of the intervention marked around the faults.
fn marked(phase: Phase) -> Request {
    Request::Event(Box::new(NewEvent {
        kind: "intervention".to_owned(),
        at_utc_ns: None,
        until_utc_ns: None,
        phase,
        id: Some("showcase".to_owned()),
        source: "cli".to_owned(),
        text: (phase == Phase::Begin).then(|| "the robot misbehaves".to_owned()),
        attributes: std::collections::BTreeMap::new(),
    }))
}

/// What each check that didn't pass found, in a line: its ID and message.
fn findings(run: &QaRun) -> Vec<String> {
    let found: Vec<String> = run
        .results
        .iter()
        .filter(|result| result.verdict == Verdict::Failed)
        .map(|result| format!("{}: {}", result.check, result.message))
        .collect();
    if found.is_empty() {
        vec!["nothing: every check passed".to_owned()]
    } else {
        found
    }
}

fn write_files(folder: &Path, files: &[(&str, &str)]) -> Result<(), KaptoError> {
    for (path, contents) in files {
        let path = folder.join(path);
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, contents))
            .map_err(|error| {
                KaptoError::new(ErrorCode::CaptureWriteFailed)
                    .detail(format!("can't write the showcase's files: {error}"))
                    .context("path", path.display().to_string())
            })?;
    }
    Ok(())
}

fn thread_failed(error: &std::io::Error) -> KaptoError {
    KaptoError::new(ErrorCode::CaptureThreadFailed).detail(format!("showcase: {error}"))
}
