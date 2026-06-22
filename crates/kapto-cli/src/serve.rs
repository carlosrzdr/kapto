// SPDX-License-Identifier: Apache-2.0

//! `kapto serve`: runs Kapto as its processes. The capture process and the
//! viewer bridge are started from this same binary and restarted if they
//! exit, and the API is served on `server.bind:server.port`; SIGINT or
//! SIGTERM stops them, giving capture time to finalise an episode.
//!
//! Serve keeps a connection to capture open the whole time and sends a sign
//! of life on it every second, so capture can tell when serve is gone (see
//! `capture.control_timeout`).

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;
use kapto_capture::control::{ControlClient, Request};
use kapto_config::DeploymentConfig;
use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::Declared;
use kapto_server::api::ApiError;
use kapto_server::api::{self, ApiState, CaptureAnnouncements};
use kapto_server::index;
use kapto_server::jobs::{Destination, JOB_SOCKET, JobServer, Uploads};
use kapto_server::layouts::Layouts;
use kapto_server::processes::sample_while;
use kapto_server::retention::Retention;
use kapto_server::robot::RobotOverview;
use kapto_server::scene::Scene;
use kapto_server::{ChildSpec, Event, Listener, Processes, RestartPolicy, Supervisor};
use kapto_store::Writer;

use crate::SettingFlags;
use crate::capture::disk_limits;
use crate::overview::overview;
use crate::record::{RecordSettings, load_plan};
use crate::station::Outcome;
use crate::viz_bridge::prepare;

/// How long the capture process may take to start answering orders.
const CAPTURE_START_TIMEOUT: Duration = Duration::from_secs(10);
/// The processes serve runs, by name.
const CAPTURE: &str = "capture";
const VIZ_BRIDGE: &str = "viz-bridge";
const WORKER: &str = "worker";
const CHILDREN: [&str; 3] = [CAPTURE, VIZ_BRIDGE, WORKER];
/// How long the worker may take to stop: it stops at once, leaving a job it
/// was doing to be done again, so it has nothing to finish.
const WORKER_STOP_GRACE: Duration = Duration::from_secs(5);
/// How often serve tells capture it is still there.
const SIGN_OF_LIFE: Duration = Duration::from_secs(1);

/// Arguments of `kapto serve`.
#[derive(Args)]
pub(crate) struct ServeArgs {
    /// The robot's URDF file.
    #[arg(long, value_name = "FILE")]
    pub(crate) robot: PathBuf,
    /// The streams.toml declaring each stream's input and rate.
    #[arg(long, value_name = "FILE")]
    pub(crate) streams: PathBuf,
    /// The mapping.toml saying which decoded value drives which joint.
    #[arg(long, value_name = "FILE")]
    pub(crate) mapping: PathBuf,
}

pub(crate) fn serve(flags: &SettingFlags, args: &ServeArgs) -> ExitCode {
    match run(flags, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

/// The robot and its streams as the API shows them, and the robot as the
/// web UI draws it, from the files serve was given.
fn robot(
    args: &ServeArgs,
    packages: &Path,
    config_folder: &Path,
) -> Outcome<(Arc<RobotOverview>, Arc<Scene>, Layouts)> {
    let prepared = prepare(&args.robot, &args.streams, &args.mapping, packages)?;
    let package = crate::record::loaded_package(&args.streams);
    let mut overview = overview(
        &prepared,
        [&args.robot, &args.streams, &args.mapping],
        package.as_ref(),
    );
    // A default saved for the package comes before its own layout.
    let declared = Declared::new(Some(&prepared.robot), &prepared.streams, &prepared.mapping);
    let id = package.as_ref().map(|package| package.manifest.id.as_str());
    let (layouts, problems) = Layouts::load(config_folder, id, declared, overview.layout.clone());
    overview
        .warnings
        .extend(problems.iter().map(ApiError::from));
    if let Some(layout) = layouts.current() {
        overview.layout = layout;
    }
    Ok((
        Arc::new(overview),
        Arc::new(Scene::of(&prepared.robot)),
        layouts,
    ))
}

/// Runs Kapto until SIGINT or SIGTERM.
pub(crate) fn run(flags: &SettingFlags, args: &ServeArgs) -> Outcome<()> {
    let (file, deployment) = crate::config::load(flags)?;
    let settings = Arc::new(crate::config::overview(&file, &deployment));
    // Checked here once, rather than by children that would fail and be
    // restarted over and over.
    load_plan(&args.streams, &RecordSettings::of(&deployment))?;
    let config_folder = crate::config::config_folder(flags);
    let (robot, scene, layouts) = robot(args, &deployment.packages_dir.value, &config_folder)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&shutdown)).map_err(|error| {
            vec![
                KaptoError::new(ErrorCode::CaptureThreadFailed)
                    .detail(format!("can't handle signal {signal}: {error}")),
            ]
        })?;
    }

    // The API's address is taken before anything starts, so a port in use
    // is reported at once.
    let address = SocketAddr::new(deployment.server_bind.value, deployment.server_port.value);
    let api_listener = TcpListener::bind(address).map_err(|error| {
        vec![
            KaptoError::new(ErrorCode::ServerListenFailed)
                .detail(error.to_string())
                .context("address", address.to_string()),
        ]
    })?;
    let program = std::env::current_exe().map_err(|error| {
        vec![
            KaptoError::new(ErrorCode::ServerChildFailed)
                .detail(format!("can't find the kapto binary to start: {error}")),
        ]
    })?;
    // The job socket listens until serve returns, when dropping it removes
    // the socket.
    let (store, _jobs) = open_state(&deployment)?;
    let processes = Arc::new(Processes::new("serve", &CHILDREN));
    let announcements = CaptureAnnouncements::default();
    let running = start_children(
        &program,
        flags,
        args,
        &deployment,
        (&shutdown, &processes, &announcements, &store),
    )?;
    let state = ApiState {
        processes: Arc::clone(&processes),
        robot,
        scene,
        layouts,
        settings,
        config_folder,
        package: crate::record::loaded_package(&args.streams).map(|package| package.manifest.id),
        bind: deployment.server_bind.value,
        control_socket: deployment.control_socket.value.clone(),
        data_dir: deployment.data_dir.value.clone(),
        store,
        viewer_stream_port: deployment.viewer_stream_port.value,
        announcements,
        disk_limits: disk_limits(&deployment),
        destinations: deployment.upload_destinations.clone(),
    };
    let api_stop = Arc::new(AtomicBool::new(false));
    let api = thread::Builder::new().name("srv-api".into()).spawn({
        let api_stop = Arc::clone(&api_stop);
        move || api::run(api_listener, state, &api_stop)
    });
    let api = match api {
        Ok(api) => api,
        Err(error) => {
            running.stop();
            return Err(vec![
                KaptoError::new(ErrorCode::ServerListenFailed).detail(error.to_string()),
            ]);
        }
    };
    // Listening on every address (as in a container) is reached on this
    // machine as localhost.
    let host = if deployment.server_bind.value.is_unspecified() {
        "localhost".to_owned()
    } else {
        deployment.server_bind.value.to_string()
    };
    eprintln!(
        "Kapto is running: http://{host}:{}  (API under /api). Ctrl-C stops.",
        deployment.server_port.value
    );
    while !shutdown.load(Ordering::Relaxed) && !api.is_finished() {
        thread::sleep(Duration::from_millis(100));
    }
    eprintln!("Stopping…");
    api_stop.store(true, Ordering::Relaxed);
    let served = api.join();
    running.stop();
    if let Ok(Err(error)) = served {
        return Err(vec![
            KaptoError::new(ErrorCode::ServerListenFailed)
                .detail(format!("the API stopped: {error}")),
        ]);
    }
    eprintln!("Stopped.");
    Ok(())
}

/// Opens `kapto.db`, with its episode index rebuilt and the jobs left
/// running when serve or the worker last stopped queued again (up to the
/// attempts allowed), and listens for the worker.
fn open_state(deployment: &DeploymentConfig) -> Outcome<(Writer, JobServer)> {
    let data_dir = &deployment.data_dir.value;
    let max_attempts = deployment.worker_max_attempts.value;
    let store = index::open(data_dir).map_err(|error| vec![error])?;
    let requeued = store
        .requeue_interrupted(max_attempts)
        .map_err(|error| vec![error])?;
    if requeued.queued + requeued.failed > 0 {
        eprintln!(
            "[serve] {} interrupted jobs queued again, {} failed after {max_attempts} attempts",
            requeued.queued, requeued.failed
        );
    }
    // Episodes are queued for upload only where there is somewhere to
    // upload them, each destination taking them as its own `when` says.
    let uploads = (!deployment.upload_destinations.is_empty()).then(|| Uploads {
        destinations: deployment
            .upload_destinations
            .iter()
            .map(|destination| Destination {
                name: destination.name.clone(),
                when: destination.settings.when.value,
                retry_failed_after: destination.settings.retry_failed_after.value,
            })
            .collect(),
        data_dir: data_dir.clone(),
        retention: Retention {
            delete_local: deployment.upload.delete_local.value,
            min_verified_destinations: deployment.upload.min_verified_destinations.value,
            free_below_bytes: deployment.disk_warn_below_mib.value.saturating_mul(1 << 20),
        },
    });
    let jobs = JobServer::listen_with_uploads(&data_dir.join(JOB_SOCKET), store.clone(), uploads)
        .map_err(|error| vec![error])?;
    Ok((store, jobs))
}

/// The supervised processes, the connection that tells capture serve is
/// there, and the sampling of what each process costs.
struct Running {
    supervisor: Supervisor,
    /// Set while the watch, the sampling and the relay run.
    watching: Arc<AtomicBool>,
    threads: [thread::JoinHandle<()>; 4],
}

impl Running {
    fn stop(self) {
        self.supervisor.stop();
        self.watching.store(false, Ordering::Relaxed);
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// Starts capture and the viewer bridge, waits until capture answers, and
/// starts watching over it, sampling what each process costs, passing on
/// what capture announces and indexing the episodes it finalises.
fn start_children(
    program: &Path,
    flags: &SettingFlags,
    args: &ServeArgs,
    deployment: &DeploymentConfig,
    (shutdown, processes, announcements, store): (
        &AtomicBool,
        &Arc<Processes>,
        &CaptureAnnouncements,
        &Writer,
    ),
) -> Outcome<Running> {
    let children = children(
        program,
        flags,
        args,
        (
            deployment.capture_stop_grace.value,
            deployment.viewer_stop_grace.value,
        ),
    );
    let policy = RestartPolicy {
        initial_delay: deployment.restart_delay.value,
        max_delay: deployment.restart_max_delay.value,
        stable_after: deployment.stable_after.value,
    };
    let listener: Listener = Arc::new({
        let processes = Arc::clone(processes);
        move |event: Event| {
            processes.record(&event);
            report(event);
        }
    });
    let supervisor = Supervisor::start(children, policy, &listener).map_err(|error| {
        vec![KaptoError::new(ErrorCode::ServerChildFailed).detail(error.to_string())]
    })?;
    if let Err(error) = greet_capture(&deployment.control_socket.value, shutdown) {
        supervisor.stop();
        return Err(vec![error]);
    }
    let watching = Arc::new(AtomicBool::new(true));
    let watch = thread::Builder::new().name("srv-watch".into()).spawn({
        let socket = deployment.control_socket.value.clone();
        let watching = Arc::clone(&watching);
        move || watch_over_capture(&socket, &watching)
    });
    let stats = thread::Builder::new().name("srv-stats".into()).spawn({
        let (processes, watching) = (Arc::clone(processes), Arc::clone(&watching));
        move || sample_while(&processes, &watching)
    });
    let relay = thread::Builder::new().name("srv-announce".into()).spawn({
        let socket = deployment.control_socket.value.clone();
        let (announcements, watching) = (announcements.clone(), Arc::clone(&watching));
        move || announcements.relay_while(&socket, &watching)
    });
    let indexing = thread::Builder::new().name("srv-index".into()).spawn({
        let (store, data_dir) = (store.clone(), deployment.data_dir.value.clone());
        let (announced, watching) = (announcements.subscribe(), Arc::clone(&watching));
        move || index::keep_up_to_date(&store, &data_dir, announced, &watching)
    });
    match (watch, stats, relay, indexing) {
        (Ok(watch), Ok(stats), Ok(relay), Ok(indexing)) => Ok(Running {
            supervisor,
            watching,
            threads: [watch, stats, relay, indexing],
        }),
        (watch, stats, relay, indexing) => {
            watching.store(false, Ordering::Relaxed);
            supervisor.stop();
            let error = [watch.err(), stats.err(), relay.err(), indexing.err()]
                .into_iter()
                .flatten()
                .next();
            Err(vec![KaptoError::new(ErrorCode::ServerChildFailed).detail(
                error.map_or_else(
                    || "a thread didn't start".to_owned(),
                    |error| error.to_string(),
                ),
            )])
        }
    }
}

/// The capture process and the viewer bridge, each with the same settings
/// as this process, and how long each may take to stop.
fn children(
    program: &Path,
    flags: &SettingFlags,
    args: &ServeArgs,
    (capture_stop_grace, bridge_stop_grace): (Duration, Duration),
) -> Vec<ChildSpec> {
    let settings = flags.to_args();
    // The bridge and the worker end with serve, even killed (see `parent`).
    let parent: Vec<std::ffi::OsString> =
        vec!["--parent".into(), std::process::id().to_string().into()];
    let child = |name: &str, own: Vec<std::ffi::OsString>, stop_grace| ChildSpec {
        name: name.into(),
        program: program.to_path_buf(),
        args: own.into_iter().chain(settings.iter().cloned()).collect(),
        stop_grace,
    };
    vec![
        child(
            CAPTURE,
            vec![
                "capture".into(),
                "--streams".into(),
                args.streams.clone().into(),
                "--mapping".into(),
                args.mapping.clone().into(),
            ],
            capture_stop_grace,
        ),
        child(
            VIZ_BRIDGE,
            vec![
                "viz-bridge".into(),
                "--robot".into(),
                args.robot.clone().into(),
                "--streams".into(),
                args.streams.clone().into(),
                "--mapping".into(),
                args.mapping.clone().into(),
            ]
            .into_iter()
            .chain(parent.iter().cloned())
            .collect(),
            bridge_stop_grace,
        ),
        child(
            WORKER,
            std::iter::once("worker".into())
                .chain(parent.iter().cloned())
                .collect(),
            WORKER_STOP_GRACE,
        ),
    ]
}

/// Waits until the capture process answers on its control socket, which
/// also checks that it speaks this binary's protocol version.
fn greet_capture(socket: &Path, shutdown: &AtomicBool) -> Result<(), KaptoError> {
    let deadline = Instant::now() + CAPTURE_START_TIMEOUT;
    loop {
        match ControlClient::connect(socket, Duration::from_secs(5)) {
            Ok(_) => return Ok(()),
            Err(error) if error.code() == ErrorCode::CaptureControlUnreachable => {
                if Instant::now() >= deadline || shutdown.load(Ordering::Relaxed) {
                    return Err(KaptoError::new(ErrorCode::ServerChildFailed)
                        .detail(format!(
                            "the capture process didn't answer within {} s",
                            CAPTURE_START_TIMEOUT.as_secs()
                        ))
                        .context("socket", socket.display().to_string()));
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Keeps a supervising connection to capture until `watching` turns false,
/// connecting again whenever capture was restarted or didn't answer.
fn watch_over_capture(socket: &Path, watching: &AtomicBool) {
    let mut connection: Option<ControlClient> = None;
    while watching.load(Ordering::Relaxed) {
        let client = match connection.take() {
            Some(client) => Some(client),
            None => ControlClient::connect(socket, SIGN_OF_LIFE).ok(),
        };
        connection =
            client.and_then(|mut client| client.ask(&Request::Supervise).is_ok().then_some(client));
        let next = Instant::now() + SIGN_OF_LIFE;
        while watching.load(Ordering::Relaxed) && Instant::now() < next {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn report(event: Event) {
    match event {
        Event::Started { name, pid } => eprintln!("[serve] started {name} (pid {pid})"),
        Event::Restarting {
            name,
            reason,
            delay,
        } => eprintln!(
            "[serve] {name} ended ({reason}); starting it again in {} s",
            delay.as_secs_f32()
        ),
        Event::Stopped { name, killed: true } => {
            eprintln!("[serve] {name} didn't stop in time and was killed");
        }
        Event::Stopped { name, .. } => eprintln!("[serve] {name} stopped"),
    }
}
