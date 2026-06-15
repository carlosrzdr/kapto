// SPDX-License-Identifier: Apache-2.0

//! The local HTTP API: what Kapto is doing, its robot, processes, storage
//! and settings, its episodes and their replays, running pre-flight,
//! starting, stopping and aborting episodes and sending events, with a
//! WebSocket pushing what changes.
//! Described by an OpenAPI document ([`ApiDoc`]), from which the reference
//! pages and the web UI's types are generated.
//!
//! Requests that change something take a JSON body, even an empty one, so a
//! web page from elsewhere can't send them as a plain form: the browser
//! would have to ask first, and the API never says yes. While the API
//! listens only on this machine it also answers only to this machine's
//! names (see `host`), so a page can't reach it by DNS rebinding.

mod events;
mod host;
mod uploads;

pub use uploads::{
    DestinationStatus, DestinationTested, EpisodeDeletion, EpisodeUpload, EpisodeUploads,
    UploadDestination, UploadStatus, UploadedFile, UploadsOverview, UploadsRetried,
    overview as upload_status,
};

use std::collections::BTreeMap;
use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kapto_capture::control::{self, ControlClient, Request};
use kapto_capture::disk::DiskLimits;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{ListedEpisode, Writer};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};

use crate::episode::{
    self as episode_detail, EpisodeBuffers, EpisodeClock, EpisodeDetail, EpisodeDurability,
    EpisodeFile, EpisodePackage, EpisodeProfile, EpisodeRecovered, EpisodeStream, EpisodeTask,
};
use crate::episode_events::{self, EpisodeEvents};
use crate::preflight::{PreflightCheck, PreflightRequest, PreflightRun};
use crate::processes::{ProcessState, ProcessStatus, Processes};
use crate::quality::{
    self, CheckRange, CheckResult, EpisodeQuality, EpisodeStatistics, StatisticSummary,
};
use crate::replays::{self, ReplayOpening, ReplayState, ReplayStatus};
use crate::robot::{
    RobotField, RobotFiles, RobotJoint, RobotOverview, RobotStream, ViewColumn, ViewLayout,
    ViewPanel,
};
use crate::scene::{Placement, RobotScene, Scene, SceneLink, SceneReach, SceneShape, SceneVisual};
use crate::sessions::{self, ProfileSummary, ProfilesOverview, TaskSummary, TasksOverview};
use crate::settings::{Setting, SettingsOverview};
use crate::storage::{self, Storage};
use crate::vocabulary::{
    self, EventPhase, EventSent, EventVocabulary, SendEvent, SentEvent, VocabularyKind,
};

pub use events::{CaptureAnnouncements, Event, EventKind};

/// The API's version: it only ever gains endpoints and fields within one.
pub const API_VERSION: u32 = 1;

/// How long the capture process may take to answer: stopping finalises the
/// episode, which writes and syncs its files.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the API needs to know about this machine.
#[derive(Clone, Debug)]
pub struct ApiState {
    /// Kapto's processes: what each is doing and what it costs.
    pub processes: Arc<Processes>,
    /// The robot and its streams, as serve loaded them when it started.
    pub robot: Arc<RobotOverview>,
    /// The robot as the web UI draws it, and the mesh files it uses.
    pub scene: Arc<Scene>,
    /// The settings serve runs with.
    pub settings: Arc<SettingsOverview>,
    /// The configuration folder, where profiles and tasks are saved.
    pub config_folder: PathBuf,
    /// The robot package Kapto runs, if its streams belong to one: the
    /// profiles for it are the ones that can be used.
    pub package: Option<String>,
    /// The address the API listens on; a loopback one makes it answer only
    /// to this machine's names.
    pub bind: IpAddr,
    /// The capture process's control socket.
    pub control_socket: PathBuf,
    /// Where episodes are stored.
    pub data_dir: PathBuf,
    /// The runtime state database, whose episode index lists the episodes.
    pub store: Writer,
    /// The port the viewer bridge streams poses and plotted values on.
    pub viewer_stream_port: u16,
    /// What the capture process announces, for the event streams.
    pub announcements: CaptureAnnouncements,
    /// When the data directory's disk is low, and too full to record on.
    pub disk_limits: DiskLimits,
    /// Where episodes are uploaded, as the configuration lists them.
    pub destinations: Vec<kapto_config::UploadDestination>,
}

/// The API's routes.
pub fn router(state: ApiState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/api/version", get(version))
        .route("/api/status", get(status))
        .route("/api/processes", get(processes))
        .route("/api/storage", get(storage))
        .route("/api/robot", get(robot))
        .route("/api/robot/scene", get(scene))
        .route("/api/robot/meshes/{n}", get(mesh))
        .route("/api/settings", get(settings))
        .route("/api/profiles", get(profiles))
        .route("/api/tasks", get(tasks))
        .route("/api/event-kinds", get(event_kinds))
        .route("/api/preflight", post(preflight))
        .route("/api/events", get(events))
        .route("/api/episodes", get(episodes))
        .route("/api/episodes/{id}", get(episode))
        .route("/api/uploads", get(upload_overview))
        .route(
            "/api/uploads/destinations/{name}/test",
            post(test_destination),
        )
        .route("/api/episodes/{id}/uploads", get(episode_uploads))
        .route("/api/episodes/{id}/uploads/retry", post(retry_uploads))
        .route("/api/episodes/{id}/qa", get(quality))
        .route("/api/episodes/{id}/events", get(episode_events))
        .route("/api/episodes/{id}/statistics", get(statistics))
        .route("/api/episodes/{id}/replay", get(replay).post(ask_replay))
        .route("/api/episodes/{id}/replay/opening", get(replay_opening))
        .route("/api/episodes/{id}/replay/window", get(replay_window))
        .route("/api/episodes/{id}/replay/scene", get(replay_scene))
        .route("/api/episodes/{id}/replay/meshes/{n}", get(replay_mesh))
        .route(
            "/api/episodes/{id}/cameras/{camera}/frames",
            get(camera_frames),
        )
        .route(
            "/api/episodes/{id}/cameras/{camera}/frames/{n}",
            get(camera_picture),
        )
        .route("/api/episodes/start", post(start))
        .route("/api/episodes/stop", post(stop))
        .route("/api/episodes/abort", post(abort))
        .route("/api/episodes/events", post(send_event))
        .fallback(crate::ui::file)
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            host::check,
        ))
        .with_state(state)
}

/// Serves the API on `listener` until `stop` is set, on a small runtime of
/// its own (`srv-api` threads).
///
/// # Errors
///
/// The runtime couldn't be started or serving failed.
pub fn run(listener: TcpListener, state: ApiState, stop: &Arc<AtomicBool>) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("srv-api")
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let stop = Arc::clone(stop);
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async move {
                while !stop.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
    })
}

/// The OpenAPI document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Kapto API",
        description = "The local API of a Kapto collector: what it is doing, its episodes, recording control, and events as they happen."
    ),
    paths(
        version,
        status,
        processes,
        storage,
        robot,
        scene,
        mesh,
        settings,
        profiles,
        tasks,
        event_kinds,
        preflight,
        events,
        episodes,
        episode,
        upload_overview,
        test_destination,
        episode_uploads,
        retry_uploads,
        quality,
        episode_events,
        statistics,
        replay,
        ask_replay,
        replay_opening,
        replay_window,
        replay_scene,
        replay_mesh,
        camera_frames,
        camera_picture,
        start,
        stop,
        abort,
        send_event
    ),
    components(schemas(
        Version,
        Status,
        Event,
        EventKind,
        CaptureStatus,
        PreflightRun,
        PreflightCheck,
        PreflightRequest,
        StreamValues,
        ProcessStatus,
        ProcessState,
        Storage,
        RobotOverview,
        RobotFiles,
        RobotJoint,
        RobotStream,
        RobotField,
        ViewLayout,
        ViewColumn,
        ViewPanel,
        RobotScene,
        SceneLink,
        SceneReach,
        SceneVisual,
        SceneShape,
        Placement,
        SettingsOverview,
        Setting,
        ProfilesOverview,
        ProfileSummary,
        TasksOverview,
        TaskSummary,
        EventVocabulary,
        VocabularyKind,
        SendEvent,
        EventPhase,
        EventSent,
        SentEvent,
        StreamCounts,
        LiveViewCounts,
        EpisodeQuality,
        EpisodeEvents,
        CheckResult,
        CheckRange,
        EpisodeStatistics,
        StatisticSummary,
        ReplayStatus,
        ReplayOpening,
        crate::cameras::CameraFrames,
        replays::ReplayStream,
        replays::ReplayValue,
        ReplayState,
        ReplayRequest,
        Viewer,
        EpisodeSummary,
        EpisodePackage,
        EpisodeRecovered,
        EpisodeDetail,
        UploadsOverview,
        DestinationStatus,
        UploadStatus,
        EpisodeUploads,
        EpisodeUpload,
        UploadedFile,
        EpisodeDeletion,
        UploadsRetried,
        DestinationTested,
        EpisodeStream,
        EpisodeProfile,
        EpisodeTask,
        EpisodeFile,
        EpisodeBuffers,
        EpisodeDurability,
        EpisodeClock,
        StartRequest,
        StopRequest,
        AbortRequest,
        Started,
        Stopped,
        Aborted,
        ApiError
    ))
)]
pub struct ApiDoc;

/// The OpenAPI document as indented JSON. Its version is the API's, so a
/// client (or a check for breaking changes) can tell versions apart.
#[must_use]
pub fn openapi_json() -> String {
    let mut document = ApiDoc::openapi();
    document.info.version = API_VERSION.to_string();
    document.to_pretty_json().unwrap_or_default()
}

/// The versions of Kapto and of this API.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Version {
    /// Kapto's version.
    pub kapto: String,
    /// The API's version.
    pub api: u32,
}

/// What Kapto is doing.
#[derive(Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Status {
    /// The capture process.
    pub capture: CaptureStatus,
    /// Where the live view's stream is.
    pub viewer: Viewer,
}

/// What the capture process is doing.
#[derive(Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CaptureStatus {
    /// The episode being recorded, if one is.
    pub episode: Option<String>,
    /// Seconds since the episode started, or since receiving started while
    /// none is recorded.
    pub seconds: u64,
    /// Each stream's counts over the same time.
    pub streams: Vec<StreamCounts>,
    /// Each stream's newest decoded values, for the streams the live view
    /// decoded any of. Sampled for display: not every message, and none
    /// while nothing is decoded.
    #[serde(default)]
    pub values: Vec<StreamValues>,
    /// Pre-flight's latest run, `null` until the first one, run when
    /// capture starts, has ended. While an episode is recorded, the run it
    /// keeps, with the checks its start waived.
    #[serde(default)]
    pub preflight: Option<PreflightRun>,
}

/// One stream's newest decoded values.
#[derive(Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StreamValues {
    /// The stream's name.
    pub stream: String,
    /// The values, field after field and array elements in order, as
    /// `/api/robot` lists the stream's fields.
    pub values: Vec<f64>,
    /// How long ago they were decoded, in milliseconds.
    pub age_ms: u64,
}

/// One stream's counts.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StreamCounts {
    /// The stream's ID.
    pub stream: String,
    /// Messages received.
    pub received: u64,
    /// Messages written to the episode; zero while none is recorded.
    pub written: u64,
    /// Messages Kapto discarded.
    pub dropped_by_us: u64,
    /// Messages the source sent that never arrived.
    pub lost_upstream: u64,
    /// For a camera, what became of its frames on their way to the live
    /// view since capture started; absent for other streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_view: Option<LiveViewCounts>,
}

/// What became of a camera's frames on their way to the live view.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LiveViewCounts {
    /// Frames offered: the camera's newest, at most `viewer.camera_max_fps`
    /// a second.
    pub offered: u64,
    /// Offered frames skipped because the live view held every slot.
    pub skipped: u64,
    /// Frames replaced by newer ones before they could be sent: the
    /// viewer bridge was slower than the camera.
    pub superseded: u64,
    /// Frames not sent because no viewer bridge was listening.
    pub not_sent: u64,
    /// Frames the viewer bridge took.
    pub sent: u64,
}

/// Where the live view's stream is, on the host the API was reached on.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Viewer {
    /// The port the robot's poses and plotted values are streamed on, as
    /// they arrive, over a WebSocket (`docs/reference/viewer-stream.md`).
    pub stream_port: u16,
}

/// One finalised episode.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeSummary {
    /// Its ID, the same as its folder name.
    pub id: String,
    /// When recording started, in nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// When recording ended, on the same clock.
    pub ended_utc_ns: i64,
    /// The version of Kapto that recorded it.
    pub kapto_version: String,
    /// The robot package it was recorded with; `null` if it is
    /// `unchecked`.
    pub package: Option<EpisodePackage>,
    /// The ID of the session profile it was recorded with; `null` if none.
    pub profile: Option<String>,
    /// How the robot was driven, as its profile said: `teleoperated`,
    /// `autonomous`, `mixed` or `scripted`; `null` if not said.
    pub mode: Option<String>,
    /// The ID of the task it was recorded for; `null` if none.
    pub task: Option<String>,
    /// What people call that task; `null` if its file didn't say.
    pub task_name: Option<String>,
    /// Each stream's counts.
    pub streams: Vec<StreamCounts>,
    /// Whether the quality checks accepted it: no gate failed. `null` until
    /// the worker has checked it (`/api/episodes/{id}/qa` has the results).
    pub accepted: Option<bool>,
    /// How many of its events are of each kind, by kind; `null` until the
    /// worker has read them (`/api/episodes/{id}/events` has them).
    pub events: Option<BTreeMap<String, u64>>,
    /// How it was recovered, if recording stopped without finishing it;
    /// `null` if recording finished it. Its streams' losses aren't known.
    pub recovered: Option<EpisodeRecovered>,
    /// Whether its folder is still here; `false` once deleted here, verified
    /// elsewhere.
    pub stored_here: bool,
}

/// Start recording an episode.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    /// Stop by itself after this many seconds; without it, records until
    /// stopped.
    #[serde(default)]
    pub duration_s: Option<u32>,
    /// The ID of the session profile to record it with (`/api/profiles`):
    /// it must be for the robot package Kapto runs. A copy of the profile
    /// as it is now is kept in the episode.
    #[serde(default)]
    pub profile: Option<String>,
    /// The ID of the task to record it for (`/api/tasks`). A copy of the
    /// task as it is now is kept in the episode.
    #[serde(default)]
    pub task: Option<String>,
    /// The codes of the failed pre-flight checks to record despite, e.g.
    /// `preflight.stream_silent`: without them, a failed check of the
    /// latest run refuses the start. The episode keeps the run, saying
    /// which checks were waived.
    #[serde(default)]
    pub waive: Vec<String>,
}

/// Ask for an episode's replay; an empty object.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {}

/// Stop recording; an empty object.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StopRequest {}

/// Stop recording and discard the episode; an empty object.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AbortRequest {}

/// An episode has started.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Started {
    /// Its ID.
    pub episode: String,
}

/// The episode has been finalised.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Stopped {
    /// Its ID.
    pub episode: String,
}

/// The episode has been discarded: nothing of it was kept.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Aborted {
    /// Its ID.
    pub episode: String,
}

/// A refused or failed request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApiError {
    /// The cause code, e.g. `capture.not_recording`.
    pub code: String,
    /// What happened: the code's summary and the specifics.
    pub message: String,
    /// What to do about it.
    pub remedy: String,
}

/// Which episodes to list.
#[derive(Debug, Deserialize)]
pub struct EpisodesQuery {
    /// At most this many, newest first; 100 if not given.
    pub limit: Option<usize>,
    /// Only those the quality checks accepted (`true`) or rejected
    /// (`false`); every episode, checked or not, if not given.
    pub accepted: Option<bool>,
    /// Only those with an event of this kind, e.g. `intervention`.
    pub event: Option<String>,
}

/// Which stretch of a replay to send, and in how many parts.
#[derive(Debug, Deserialize)]
pub struct WindowQuery {
    /// Where the stretch begins, UTC nanoseconds.
    pub from: i64,
    /// Where it ends, UTC nanoseconds, included.
    pub to: i64,
    /// How many parts to merge it into: 1 to 10000.
    pub points: u32,
}

/// An error and the HTTP status it is sent with.
struct Failure(StatusCode, KaptoError);

impl From<KaptoError> for Failure {
    fn from(error: KaptoError) -> Self {
        let status = match error.code() {
            ErrorCode::CaptureControlUnreachable | ErrorCode::CaptureControlVersionMismatch => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ErrorCode::ApiInvalidRequest | ErrorCode::CaptureEventInvalid => {
                StatusCode::BAD_REQUEST
            }
            ErrorCode::ApiEpisodeNotFound
            | ErrorCode::ViewerMeshNotFound
            | ErrorCode::ViewerReplayNotReady
            | ErrorCode::ViewerReplayWithoutRobot
            | ErrorCode::ViewerCameraNotFound
            | ErrorCode::ProfileNotFound
            | ErrorCode::TaskNotFound
            | ErrorCode::UploadNoDestination => StatusCode::NOT_FOUND,
            // What the request asks for can't be done with what capture is
            // doing or found, or with what is saved.
            ErrorCode::CaptureAlreadyRecording
            | ErrorCode::CaptureNotRecording
            | ErrorCode::CapturePreflightFailed
            | ErrorCode::ProfileUnavailable
            | ErrorCode::ProfileUnknownKey
            | ErrorCode::TaskUnknownKey
            | ErrorCode::EventsUnknownKind
            | ErrorCode::ConfigInvalidValue
            | ErrorCode::ConfigInvalidSyntax
            | ErrorCode::RobotMeshUnreadable
            | ErrorCode::RobotMeshInvalid
            | ErrorCode::ViewerPictureUnshown
            | ErrorCode::UploadNothingToRetry
            | ErrorCode::UploadDestinationUnusable
            | ErrorCode::UploadCredentialsMissing => StatusCode::CONFLICT,
            // The destination answered, or didn't: its trouble, not the
            // request's.
            ErrorCode::UploadUnreachable
            | ErrorCode::UploadDenied
            | ErrorCode::UploadFailed
            | ErrorCode::UploadVerifyFailed => StatusCode::BAD_GATEWAY,
            ErrorCode::ApiHostNotAllowed | ErrorCode::ApiOriginNotAllowed => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self(status, error)
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let Self(status, error) = self;
        (status, Json(ApiError::from(&error))).into_response()
    }
}

impl From<&KaptoError> for ApiError {
    fn from(error: &KaptoError) -> Self {
        Self {
            code: error.code().id().to_owned(),
            message: error.message(),
            remedy: error.remedy().to_owned(),
        }
    }
}

fn invalid(rejection: &JsonRejection) -> Failure {
    Failure::from(KaptoError::new(ErrorCode::ApiInvalidRequest).detail(rejection.body_text()))
}

/// Asks the capture process, off the async threads: its client blocks.
async fn ask(state: &ApiState, request: Request) -> Result<control::Response, Failure> {
    let socket = state.control_socket.clone();
    tokio::task::spawn_blocking(move || {
        ControlClient::connect(&socket, CAPTURE_TIMEOUT)?.ask(&request)
    })
    .await
    .map_err(|error| {
        Failure::from(
            KaptoError::new(ErrorCode::CaptureControlUnreachable).detail(error.to_string()),
        )
    })?
    .map_err(Failure::from)
}

fn unexpected(response: &control::Response) -> Failure {
    Failure::from(
        KaptoError::new(ErrorCode::CaptureControlUnreachable)
            .detail(format!("unexpected answer: {response:?}")),
    )
}

/// The versions of Kapto and of this API. The web UI checks it when it
/// loads.
#[utoipa::path(get, path = "/api/version", responses((status = 200, body = Version)))]
async fn version() -> Json<Version> {
    Json(Version {
        kapto: env!("CARGO_PKG_VERSION").to_owned(),
        api: API_VERSION,
    })
}

/// What Kapto is doing: the episode being recorded, each stream's counts,
/// and where the live view's stream is.
#[utoipa::path(
    get,
    path = "/api/status",
    responses(
        (status = 200, body = Status),
        (status = 503, description = "The capture process doesn't answer", body = ApiError)
    )
)]
async fn status(State(state): State<Arc<ApiState>>) -> Result<Json<Status>, Failure> {
    current_status(&state).await.map(Json)
}

/// Asks the capture process what it is doing.
async fn current_status(state: &ApiState) -> Result<Status, Failure> {
    let control::Response::Status(status) = ask(state, Request::Status).await? else {
        return Err(Failure::from(KaptoError::new(
            ErrorCode::CaptureControlUnreachable,
        )));
    };
    Ok(Status {
        capture: CaptureStatus {
            episode: status.episode,
            seconds: status.seconds,
            values: status
                .streams
                .iter()
                .filter_map(|stream| {
                    Some(StreamValues {
                        stream: stream.stream.clone(),
                        values: stream.values.clone()?,
                        age_ms: stream.values_age_ms.unwrap_or_default(),
                    })
                })
                .collect(),
            streams: status.streams.into_iter().map(counts).collect(),
            preflight: status.preflight.map(PreflightRun::from),
        },
        viewer: Viewer {
            stream_port: state.viewer_stream_port,
        },
    })
}

fn counts(stream: control::StreamStatus) -> StreamCounts {
    StreamCounts {
        stream: stream.stream,
        received: stream.received,
        written: stream.written,
        dropped_by_us: stream.dropped_by_us,
        lost_upstream: stream.lost_upstream,
        live_view: stream.live_view.map(|camera| LiveViewCounts {
            offered: camera.offered,
            skipped: camera.skipped,
            superseded: camera.superseded,
            not_sent: camera.not_sent,
            sent: camera.sent,
        }),
    }
}

/// Kapto's processes, the server first: whether each runs, since when, how
/// often it was started again, and its CPU, memory and threads. Answers even
/// while the capture process doesn't.
#[utoipa::path(
    get,
    path = "/api/processes",
    responses((status = 200, body = Vec<ProcessStatus>))
)]
async fn processes(State(state): State<Arc<ApiState>>) -> Json<Vec<ProcessStatus>> {
    Json(state.processes.snapshot())
}

/// How much room the data directory has, and how much the episodes in it
/// take, including the one being recorded.
#[utoipa::path(
    get,
    path = "/api/storage",
    responses(
        (status = 200, body = Storage),
        (status = 500, description = "The file system's size can't be read", body = ApiError)
    )
)]
async fn storage(State(state): State<Arc<ApiState>>) -> Result<Json<Storage>, Failure> {
    let data_dir = state.data_dir.clone();
    let limits = state.disk_limits;
    tokio::task::spawn_blocking(move || storage::measure(&data_dir, limits))
        .await
        .map_err(|error| {
            Failure::from(
                KaptoError::new(ErrorCode::ServerStorageUnreadable).detail(error.to_string()),
            )
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// The robot and its streams as Kapto runs them: each joint with its limits
/// and the decoded value that drives it, each stream with its input, rate
/// and decoded fields, and problems that don't stop Kapto.
#[utoipa::path(get, path = "/api/robot", responses((status = 200, body = RobotOverview)))]
async fn robot(State(state): State<Arc<ApiState>>) -> Json<RobotOverview> {
    Json(RobotOverview::clone(&state.robot))
}

/// The robot as the web UI draws it: each link, in the order its poses are
/// sent as the robot moves, with its visual shapes, their colours and where
/// they sit. Meshes are numbered, each at `/api/robot/meshes/{n}`.
#[utoipa::path(get, path = "/api/robot/scene", responses((status = 200, body = RobotScene)))]
async fn scene(State(state): State<Arc<ApiState>>) -> Json<RobotScene> {
    Json(state.scene.robot.clone())
}

/// One of the scene's meshes as plain triangles, read from its file: the
/// binary form `docs/reference/viewer-scene.md` specifies.
#[utoipa::path(
    get,
    path = "/api/robot/meshes/{n}",
    params(("n" = usize, Path, description = "The mesh's number, from 0, as the scene's shapes give it.")),
    responses(
        (status = 200, description = "The mesh's triangles", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 400, description = "The number isn't a number", body = ApiError),
        (status = 404, description = "The scene has no such mesh", body = ApiError),
        (status = 409, description = "The mesh file can't be read", body = ApiError)
    )
)]
async fn mesh(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(n): axum::extract::Path<String>,
) -> Result<Response, Failure> {
    let n = n.parse::<usize>().map_err(|_| {
        Failure::from(
            KaptoError::new(ErrorCode::ApiInvalidRequest)
                .detail(format!("`{n}` isn't a mesh's number")),
        )
    })?;
    let scene = Arc::clone(&state.scene);
    let bytes = tokio::task::spawn_blocking(move || scene.mesh(n))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::RobotMeshUnreadable).detail(error.to_string()))
        })?
        .map_err(Failure::from)?;
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

/// The robot a finalised episode was recorded with, as `/api/robot/scene`
/// gives the one loaded now, once `/api/episodes/{id}/replay` says its
/// replay is ready: from the robot description its recording carries, so a
/// replay shows that robot whatever its package is now. Meshes are
/// numbered, each at `/api/episodes/{id}/replay/meshes/{n}`.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/replay/scene",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = RobotScene),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No finalised episode has this ID, its replay isn't made, or its recording carries no robot", body = ApiError),
        (status = 500, description = "Its replay view can't be read", body = ApiError)
    )
)]
async fn replay_scene(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<RobotScene>, Failure> {
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || replays::scene(&data_dir, &id))
        .await
        .map_err(|error| {
            Failure::from(
                KaptoError::new(ErrorCode::ViewerReplayUnreadable).detail(error.to_string()),
            )
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// One of the meshes of the robot a finalised episode was recorded with,
/// as plain triangles, from its replay: the binary form
/// `docs/reference/viewer-scene.md` specifies.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/replay/meshes/{n}",
    params(
        ("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it."),
        ("n" = usize, Path, description = "The mesh's number, from 0, as the replay's scene's shapes give it.")
    ),
    responses(
        (status = 200, description = "The mesh's triangles", content_type = "application/octet-stream", body = Vec<u8>),
        (status = 400, description = "The ID isn't an episode ID, or the number isn't a number", body = ApiError),
        (status = 404, description = "No finalised episode has this ID, its replay isn't made, or its scene has no such mesh", body = ApiError),
        (status = 409, description = "The mesh couldn't be read when the replay was made", body = ApiError),
        (status = 500, description = "Its replay view can't be read", body = ApiError)
    )
)]
async fn replay_mesh(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((id, n)): axum::extract::Path<(String, String)>,
) -> Result<Response, Failure> {
    let n = n.parse::<usize>().map_err(|_| {
        Failure::from(
            KaptoError::new(ErrorCode::ApiInvalidRequest)
                .detail(format!("`{n}` isn't a mesh's number")),
        )
    })?;
    let data_dir = state.data_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || replays::mesh(&data_dir, &id, n))
        .await
        .map_err(|error| {
            Failure::from(
                KaptoError::new(ErrorCode::ViewerReplayUnreadable).detail(error.to_string()),
            )
        })?
        .map_err(Failure::from)?;
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    Ok(response)
}

/// When each frame of one of a finalised episode's cameras was received,
/// read from its recording by the recording's indexes: frame `n` is the
/// `n`-th. Needs no replay made.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/cameras/{camera}/frames",
    params(
        ("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it."),
        ("camera" = String, Path, description = "The camera's stream, as the replay's opening lists it.")
    ),
    responses(
        (status = 200, body = crate::cameras::CameraFrames),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No finalised episode has this ID, or it has no such camera", body = ApiError),
        (status = 500, description = "Its recording can't be read by its indexes", body = ApiError)
    )
)]
async fn camera_frames(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((id, camera)): axum::extract::Path<(String, String)>,
) -> Result<Json<crate::cameras::CameraFrames>, Failure> {
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || crate::cameras::frames(&data_dir, &id, &camera))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::RecordingUnreadable).detail(error.to_string()))
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// Frame `n` of one of a finalised episode's cameras, as a picture the
/// browser draws: a compressed one as recorded, a raw one encoded as JPEG.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/cameras/{camera}/frames/{n}",
    params(
        ("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it."),
        ("camera" = String, Path, description = "The camera's stream, as the replay's opening lists it."),
        ("n" = usize, Path, description = "The frame's number, from 0, as its frames list them.")
    ),
    responses(
        (status = 200, description = "The frame's picture, JPEG or PNG", content_type = "image/jpeg", body = Vec<u8>),
        (status = 400, description = "The ID isn't an episode ID, or the number isn't a number", body = ApiError),
        (status = 404, description = "No finalised episode has this ID, or it has no such camera or frame", body = ApiError),
        (status = 409, description = "The frame can't be shown, and the message says why", body = ApiError),
        (status = 500, description = "Its recording can't be read by its indexes", body = ApiError)
    )
)]
async fn camera_picture(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path((id, camera, n)): axum::extract::Path<(String, String, String)>,
) -> Result<Response, Failure> {
    let n = n.parse::<usize>().map_err(|_| {
        Failure::from(
            KaptoError::new(ErrorCode::ApiInvalidRequest)
                .detail(format!("`{n}` isn't a frame's number")),
        )
    })?;
    let data_dir = state.data_dir.clone();
    let (media_type, bytes) =
        tokio::task::spawn_blocking(move || crate::cameras::picture(&data_dir, &id, &camera, n))
            .await
            .map_err(|error| {
                Failure::from(
                    KaptoError::new(ErrorCode::RecordingUnreadable).detail(error.to_string()),
                )
            })?
            .map_err(Failure::from)?;
    let mut response = Body::from(bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    // A recorded frame doesn't change while the browser keeps it.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    Ok(response)
}

/// The profiles saved in the configuration folder, each marked available if
/// it records the robot package Kapto runs, and what is wrong with any that
/// can't be read. Read again on each request.
#[utoipa::path(get, path = "/api/profiles", responses((status = 200, body = ProfilesOverview)))]
async fn profiles(State(state): State<Arc<ApiState>>) -> Json<ProfilesOverview> {
    let (folder, package) = (state.config_folder.clone(), state.package.clone());
    let read =
        tokio::task::spawn_blocking(move || sessions::profiles(&folder, package.as_deref())).await;
    Json(read.unwrap_or_else(|_| ProfilesOverview {
        package: state.package.clone(),
        profiles: Vec::new(),
        problems: Vec::new(),
    }))
}

/// The tasks saved in the configuration folder, and what is wrong with any
/// that can't be read. Read again on each request.
#[utoipa::path(get, path = "/api/tasks", responses((status = 200, body = TasksOverview)))]
async fn tasks(State(state): State<Arc<ApiState>>) -> Json<TasksOverview> {
    let folder = state.config_folder.clone();
    let read = tokio::task::spawn_blocking(move || sessions::tasks(&folder)).await;
    Json(read.unwrap_or_else(|_| TasksOverview {
        tasks: Vec::new(),
        problems: Vec::new(),
    }))
}

/// The kinds of events that may be sent: the vocabulary file's, or the
/// defaults without it, then each task's subtasks, and what is wrong with
/// the file. Read again on each request.
#[utoipa::path(get, path = "/api/event-kinds", responses((status = 200, body = EventVocabulary)))]
async fn event_kinds(State(state): State<Arc<ApiState>>) -> Json<EventVocabulary> {
    let folder = state.config_folder.clone();
    let read = tokio::task::spawn_blocking(move || vocabulary::vocabulary(&folder)).await;
    Json(read.unwrap_or_else(|_| EventVocabulary {
        kinds: Vec::new(),
        problems: Vec::new(),
    }))
}

/// Run pre-flight again: watch the streams for `capture.preflight_window`
/// and test the disk, then answer with each check's result. A run already
/// going is waited for rather than started again. Only between episodes:
/// its test file would compete with a recording's writes.
#[utoipa::path(
    post,
    path = "/api/preflight",
    request_body = PreflightRequest,
    responses(
        (status = 200, body = PreflightRun),
        (status = 400, description = "The body isn't a pre-flight request", body = ApiError),
        (status = 409, description = "An episode is being recorded", body = ApiError),
        (status = 503, description = "The capture process doesn't answer", body = ApiError)
    )
)]
async fn preflight(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<PreflightRequest>, JsonRejection>,
) -> Result<Json<PreflightRun>, Failure> {
    let Json(PreflightRequest {}) = request.map_err(|rejection| invalid(&rejection))?;
    match ask(&state, Request::Preflight).await? {
        control::Response::Preflight(run) => Ok(Json(PreflightRun::from(run))),
        other => Err(unexpected(&other)),
    }
}

/// The settings Kapto runs with, each with its value and where it came
/// from. They are read when serve starts.
#[utoipa::path(get, path = "/api/settings", responses((status = 200, body = SettingsOverview)))]
async fn settings(State(state): State<Arc<ApiState>>) -> Json<SettingsOverview> {
    Json(SettingsOverview::clone(&state.settings))
}

/// Events as they happen, over a WebSocket. Each message is one `Event` as
/// JSON. Connections from another site's web page are refused.
#[utoipa::path(
    get,
    path = "/api/events",
    responses(
        (status = 101, description = "A WebSocket, on which each text message is one Event as JSON", body = Event),
        (status = 403, description = "The request came from another site's page", body = ApiError)
    )
)]
async fn events(state: State<Arc<ApiState>>, request: axum::extract::Request) -> Response {
    events::stream(state, request).await
}

/// The finalised episodes in the data directory, newest first. Folders
/// that aren't finalised, or whose summary can't be read, are left out.
#[utoipa::path(
    get,
    path = "/api/episodes",
    params(
        ("limit" = Option<usize>, Query, description = "At most this many, newest first; 100 if not given."),
        ("accepted" = Option<bool>, Query, description = "Only those the quality checks accepted (`true`) or rejected (`false`), which leaves out those not checked yet; every episode if not given."),
        ("event" = Option<String>, Query, description = "Only those with an event of this kind, e.g. `intervention`, which leaves out those whose events weren't read yet; every episode if not given.")
    ),
    responses(
        (status = 200, body = Vec<EpisodeSummary>),
        (status = 500, description = "The episode index can't be read", body = ApiError)
    )
)]
async fn episodes(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<EpisodesQuery>,
) -> Result<Json<Vec<EpisodeSummary>>, Failure> {
    let store = state.store.clone();
    let limit = query.limit.unwrap_or(100);
    tokio::task::spawn_blocking(move || {
        let reader = store.reader()?;
        let deleted: std::collections::BTreeSet<String> = reader
            .deletions()?
            .into_iter()
            .map(|deletion| deletion.episode)
            .collect();
        let listed = reader.list(limit, query.accepted, query.event.as_deref())?;
        Ok::<_, KaptoError>(
            listed
                .into_iter()
                .map(|listed| {
                    let stored_here = !deleted.contains(&listed.episode.id);
                    EpisodeSummary {
                        stored_here,
                        ..EpisodeSummary::from(listed)
                    }
                })
                .collect(),
        )
    })
    .await
    .map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
    })?
    .map(Json)
    .map_err(Failure::from)
}

/// One finalised episode in full: its streams' counts, files, buffers, how
/// quickly it reached the disk, and the wall clock while it was recorded.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = EpisodeDetail),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No episode, here or deleted here, has this ID", body = ApiError)
    )
)]
async fn episode(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<EpisodeDetail>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || match episode_detail::read(&data_dir, &id) {
        // Deleted here once verified elsewhere: from the summary kept.
        Err(error) if error.code() == ErrorCode::ApiEpisodeNotFound => {
            let reader = store.reader()?;
            let Some(deletion) = reader.deletion(&id)? else {
                return Err(error);
            };
            let files = reader
                .uploads(&id)?
                .into_iter()
                .find(|upload| !upload.objects.is_empty())
                .map(|upload| {
                    upload
                        .objects
                        .into_iter()
                        .map(|object| EpisodeFile {
                            path: object.path,
                            bytes: object.bytes,
                        })
                        .collect()
                })
                .unwrap_or_default();
            episode_detail::from_summary(&data_dir, &deletion.summary, files)
        }
        read => read,
    })
    .await
    .map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::EpisodeUnreadable).detail(error.to_string()))
    })?
    .map(Json)
    .map_err(Failure::from)
}

/// Where uploads are: each destination's counts and every upload going or
/// failed.
#[utoipa::path(
    get,
    path = "/api/uploads",
    responses(
        (status = 200, body = UploadsOverview),
        (status = 500, description = "The runtime state database can't be read", body = ApiError)
    )
)]
async fn upload_overview(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<UploadsOverview>, Failure> {
    let store = state.store.clone();
    let destinations: Vec<UploadDestination> = state
        .destinations
        .iter()
        .map(|destination| UploadDestination {
            name: destination.name.clone(),
            url: destination.url.clone(),
        })
        .collect();
    tokio::task::spawn_blocking(move || uploads::overview(&store.reader()?, &destinations))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// One episode's uploads, where every object of it went, and whether it is
/// still stored here.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/uploads",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = EpisodeUploads),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No episode, here or deleted here, has this ID", body = ApiError)
    )
)]
async fn episode_uploads(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<EpisodeUploads>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || {
        let reader = store.reader()?;
        let here = episode_detail::stored_here(&data_dir, &reader, &id)?;
        uploads::of_episode(&reader, &id, here)
    })
    .await
    .map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
    })?
    .map(Json)
    .map_err(Failure::from)
}

/// Tests a destination as an upload would use it, as `kapto upload check`
/// does: a small probe object written under its prefix, read back and
/// removed.
#[utoipa::path(
    post,
    path = "/api/uploads/destinations/{name}/test",
    params(("name" = String, Path, description = "The destination's `name`, as `/api/uploads` lists it.")),
    responses(
        (status = 200, description = "Written, read back and, if the credentials allow, removed", body = DestinationTested),
        (status = 404, description = "No destination has this name", body = ApiError),
        (status = 409, description = "Its configuration or credentials can't be used", body = ApiError),
        (status = 502, description = "It couldn't be reached, refused the probe, or gave back another one", body = ApiError)
    )
)]
async fn test_destination(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Json<DestinationTested>, Failure> {
    let destination = state
        .destinations
        .iter()
        .find(|destination| destination.name == name)
        .ok_or_else(|| {
            Failure::from(
                KaptoError::new(ErrorCode::UploadNoDestination)
                    .detail(format!("no `[[upload.destination]]` is named `{name}`")),
            )
        })?;
    let started = std::time::Instant::now();
    let probed = kapto_upload::probe::check(destination, &|name| std::env::var(name).ok())
        .await
        .map_err(|error| Failure::from(error.context("destination", name.clone())))?;
    Ok(Json(DestinationTested {
        key: probed.key,
        removed: probed.removed,
        took_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }))
}

/// Tries an episode's failed uploads again now, without waiting for
/// `upload.retry_failed_after`.
#[utoipa::path(
    post,
    path = "/api/episodes/{id}/uploads/retry",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 202, description = "Queued: the upload lane takes them next", body = UploadsRetried),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 409, description = "None of its uploads failed", body = ApiError)
    )
)]
async fn retry_uploads(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<(StatusCode, Json<UploadsRetried>), Failure> {
    if kapto_store::episode_folder(&state.data_dir, &id).is_none() {
        return Err(Failure::from(
            KaptoError::new(ErrorCode::ApiInvalidRequest)
                .detail(format!("`{id}` isn't an episode ID")),
        ));
    }
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || crate::jobs::retry_uploads(&store, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(|destinations| (StatusCode::ACCEPTED, Json(UploadsRetried { destinations })))
        .map_err(Failure::from)
}

impl From<ListedEpisode> for EpisodeSummary {
    fn from(
        ListedEpisode {
            episode,
            accepted,
            events,
        }: ListedEpisode,
    ) -> Self {
        Self {
            accepted,
            events,
            id: episode.id,
            started_utc_ns: episode.started_utc_ns,
            ended_utc_ns: episode.ended_utc_ns,
            kapto_version: episode.kapto_version,
            package: episode.package.map(EpisodePackage::from),
            profile: episode.profile,
            mode: episode.mode.map(|mode| mode.as_str().to_owned()),
            task: episode.task,
            task_name: episode.task_name,
            recovered: episode.recovered.map(EpisodeRecovered::from),
            stored_here: true,
            streams: episode
                .streams
                .into_iter()
                .map(|stream| StreamCounts {
                    stream: stream.stream,
                    received: stream.received,
                    written: stream.written,
                    dropped_by_us: stream.dropped_by_us,
                    lost_upstream: stream.lost_upstream,
                    // What the live view showed isn't kept with an episode.
                    live_view: None,
                })
                .collect(),
        }
    }
}

/// What the quality checks found on one finalised episode: whether it was
/// accepted, the settings it was checked with, and each check's result.
/// Before the worker has checked it, `accepted` is `null` and there are no
/// results.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/qa",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = EpisodeQuality),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No episode, here or deleted here, has this ID", body = ApiError),
        (status = 500, description = "The results can't be read", body = ApiError)
    )
)]
async fn quality(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<EpisodeQuality>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || quality::read(&data_dir, &store.reader()?, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// What happened in one finalised episode: its events, in the order its
/// recording holds them, and the stretches it began but never ended. Before
/// the worker has read them, `read` is `false` and there are none.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/events",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = EpisodeEvents),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No episode, here or deleted here, has this ID", body = ApiError),
        (status = 500, description = "The events can't be read", body = ApiError)
    )
)]
async fn episode_events(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<EpisodeEvents>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || episode_events::read(&data_dir, &store.reader()?, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// One finalised episode's statistics, worked out with its quality checks:
/// its duration, each stream's rate and loss, each joint's position, speed,
/// acceleration, jerk and effort (count, extremes, mean and percentiles),
/// each gripper's cycles. Before
/// the worker has checked it, `computed_utc_ns` is `null` and there are
/// none.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/statistics",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = EpisodeStatistics),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No episode, here or deleted here, has this ID", body = ApiError),
        (status = 500, description = "The statistics can't be read", body = ApiError)
    )
)]
async fn statistics(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<EpisodeStatistics>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || quality::statistics(&data_dir, &store.reader()?, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// Where one finalised episode's replay is: not made, queued, being made,
/// ready with what it lacks, or failed with why. It asks for nothing:
/// `POST` does.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/replay",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = ReplayStatus),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No finalised episode has this ID", body = ApiError),
        (status = 500, description = "The job queue can't be read", body = ApiError)
    )
)]
async fn replay(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ReplayStatus>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || replays::status(&data_dir, &store.reader()?, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// Ask for one finalised episode's replay, its replay view to watch it
/// again in the web UI.
/// The worker makes it once, at low priority and not while an episode is
/// recorded: asking again while it is queued, being made or made changes
/// nothing; one that failed, or has since left the replay cache, is made
/// again. Answers where it is, `202` until it is ready.
#[utoipa::path(
    post,
    path = "/api/episodes/{id}/replay",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    request_body = ReplayRequest,
    responses(
        (status = 200, description = "Ready", body = ReplayStatus),
        (status = 202, description = "Queued or being made", body = ReplayStatus),
        (status = 400, description = "The ID isn't an episode ID, or the body isn't an empty object", body = ApiError),
        (status = 404, description = "No finalised episode has this ID", body = ApiError),
        (status = 500, description = "The job queue can't be read or written", body = ApiError)
    )
)]
async fn ask_replay(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    request: Result<Json<ReplayRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ReplayStatus>), Failure> {
    let Json(ReplayRequest {}) = request.map_err(|rejection| invalid(&rejection))?;
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    let status = tokio::task::spawn_blocking(move || replays::ask(&data_dir, &store, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map_err(Failure::from)?;
    let code = if status.state == ReplayState::Ready {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((code, Json(status)))
}

/// What a finalised episode's replay shows, once
/// `/api/episodes/{id}/replay` says it is ready: its span, and the links
/// and values its stretches give, in their order, as the live stream's
/// opening message names them for the live view.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/replay/opening",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it.")),
    responses(
        (status = 200, body = ReplayOpening),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No finalised episode has this ID, or its replay isn't made", body = ApiError),
        (status = 500, description = "Its replay view can't be read", body = ApiError)
    )
)]
async fn replay_opening(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<ReplayOpening>, Failure> {
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || replays::opening(&data_dir, &id))
        .await
        .map_err(|error| {
            Failure::from(
                KaptoError::new(ErrorCode::ViewerReplayUnreadable).detail(error.to_string()),
            )
        })?
        .map(Json)
        .map_err(Failure::from)
}

/// A stretch of a finalised episode's replay, once
/// `/api/episodes/{id}/replay` says it is ready: the robot's newest pose
/// and each value's minimum and maximum in each of `points` equal parts of
/// it, read from the replay view at the coarsest step that still has that
/// many. In the binary form the viewer stream answers a look back with
/// (`KVHI`, the question's number 0), its span the episode's. Reading it
/// counts as opening the replay.
#[utoipa::path(
    get,
    path = "/api/episodes/{id}/replay/window",
    params(
        ("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it."),
        ("from" = i64, Query, description = "Where the stretch begins, UTC nanoseconds."),
        ("to" = i64, Query, description = "Where it ends, UTC nanoseconds, included."),
        ("points" = u32, Query, description = "How many parts to merge it into: 1 to 10000.")
    ),
    responses(
        (status = 200, description = "The stretch, merged", content_type = "application/octet-stream"),
        (status = 400, description = "The ID isn't an episode ID, or the stretch or points are missing, out of range or outside the episode", body = ApiError),
        (status = 404, description = "No finalised episode has this ID, or its replay isn't made", body = ApiError),
        (status = 500, description = "Its replay view can't be read", body = ApiError)
    )
)]
async fn replay_window(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    query: Result<Query<WindowQuery>, QueryRejection>,
) -> Result<Response, Failure> {
    let Query(window) = query.map_err(|rejection| {
        Failure::from(KaptoError::new(ErrorCode::ApiInvalidRequest).detail(rejection.body_text()))
    })?;
    let data_dir = state.data_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        replays::window(&data_dir, &id, window.from, window.to, window.points)
    })
    .await
    .map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::ViewerReplayUnreadable).detail(error.to_string()))
    })?
    .map_err(Failure::from)?;
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    // Made again from the recording once it leaves the cache.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Start recording an episode. Answers once it is being recorded.
#[utoipa::path(
    post,
    path = "/api/episodes/start",
    request_body = StartRequest,
    responses(
        (status = 201, body = Started),
        (status = 400, description = "The body isn't a start request", body = ApiError),
        (status = 404, description = "No such profile or task", body = ApiError),
        (status = 409, description = "An episode is already being recorded, a check of pre-flight's latest run failed, the profile is for another robot package, or the profile or task file can't be read", body = ApiError),
        (status = 503, description = "The capture process doesn't answer", body = ApiError)
    )
)]
async fn start(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<StartRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<Started>), Failure> {
    let Json(request) = request.map_err(|rejection| invalid(&rejection))?;
    let folder = state.config_folder.clone();
    let (profile, task) = (request.profile.clone(), request.task.clone());
    let (profile, task) = tokio::task::spawn_blocking(move || {
        sessions::resolve(&folder, profile.as_deref(), task.as_deref())
    })
    .await
    .map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::ConfigUnreadable).detail(error.to_string()))
    })?
    .map_err(Failure::from)?;
    let request = Request::Start {
        duration_s: request.duration_s,
        profile: profile.map(Box::new),
        task: task.map(Box::new),
        waive: request.waive,
    };
    match ask(&state, request).await? {
        control::Response::Started { episode } => {
            Ok((StatusCode::CREATED, Json(Started { episode })))
        }
        other => Err(unexpected(&other)),
    }
}

/// Stop recording. Answers once the episode is finalised, and listed.
#[utoipa::path(
    post,
    path = "/api/episodes/stop",
    request_body = StopRequest,
    responses(
        (status = 200, body = Stopped),
        (status = 400, description = "The body isn't a stop request", body = ApiError),
        (status = 409, description = "No episode is being recorded", body = ApiError),
        (status = 503, description = "The capture process doesn't answer", body = ApiError)
    )
)]
async fn stop(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<StopRequest>, JsonRejection>,
) -> Result<Json<Stopped>, Failure> {
    let Json(StopRequest {}) = request.map_err(|rejection| invalid(&rejection))?;
    match ask(&state, Request::Stop).await? {
        control::Response::Stopped { episode, .. } => {
            // Indexed before answering, so listing right after stopping
            // shows it; capture's announcement would come a moment later.
            let (store, data_dir) = (state.store.clone(), state.data_dir.clone());
            let id = episode.clone();
            let indexed =
                tokio::task::spawn_blocking(move || crate::index::add(&store, &data_dir, &id))
                    .await;
            if let Ok(Err(error)) = indexed {
                eprintln!("[serve] {episode} isn't listed yet: {error}");
            }
            Ok(Json(Stopped { episode }))
        }
        other => Err(unexpected(&other)),
    }
}

/// Stop recording and discard the episode: nothing of it is kept. Answers
/// once it is deleted.
#[utoipa::path(
    post,
    path = "/api/episodes/abort",
    request_body = AbortRequest,
    responses(
        (status = 200, body = Aborted),
        (status = 400, description = "The body isn't an abort request", body = ApiError),
        (status = 409, description = "No episode is being recorded", body = ApiError),
        (status = 503, description = "The capture process doesn't answer", body = ApiError)
    )
)]
async fn abort(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<AbortRequest>, JsonRejection>,
) -> Result<Json<Aborted>, Failure> {
    let Json(AbortRequest {}) = request.map_err(|rejection| invalid(&rejection))?;
    match ask(&state, Request::Abort).await? {
        control::Response::Aborted { episode } => Ok(Json(Aborted { episode })),
        other => Err(unexpected(&other)),
    }
}

/// Send an event. During an episode it is recorded in it; between episodes
/// it is only told to whoever watches. Either way every watcher is told.
/// Its kind must be one of `/api/event-kinds`.
#[utoipa::path(
    post,
    path = "/api/episodes/events",
    request_body = SendEvent,
    responses(
        (status = 201, body = EventSent),
        (status = 400, description = "The body isn't an event, or the event isn't well formed", body = ApiError),
        (status = 409, description = "Its kind isn't one that may be sent", body = ApiError),
        (status = 503, description = "The capture process doesn't answer", body = ApiError)
    )
)]
async fn send_event(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<SendEvent>, JsonRejection>,
) -> Result<(StatusCode, Json<EventSent>), Failure> {
    let Json(request) = request.map_err(|rejection| invalid(&rejection))?;
    let source = request.source.unwrap_or_else(|| "api".to_owned());
    if source != "api" && source != "page" {
        return Err(Failure::from(
            KaptoError::new(ErrorCode::ApiInvalidRequest)
                .detail(format!("the source `{source}` isn't `api` or `page`")),
        ));
    }
    let event = control::NewEvent {
        kind: request.kind,
        at_utc_ns: request.at_utc_ns,
        until_utc_ns: request.until_utc_ns,
        phase: request.phase.into(),
        id: request.id,
        source,
        text: request.text,
        attributes: request.attributes,
    };
    let folder = state.config_folder.clone();
    let checked = event.clone();
    tokio::task::spawn_blocking(move || vocabulary::check(&folder, &checked))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::ConfigUnreadable).detail(error.to_string()))
        })?
        .map_err(Failure::from)?;
    match ask(&state, Request::Event(Box::new(event))).await? {
        control::Response::Noted { event, episode } => Ok((
            StatusCode::CREATED,
            Json(EventSent {
                event: SentEvent::from(*event),
                episode,
            }),
        )),
        other => Err(unexpected(&other)),
    }
}
