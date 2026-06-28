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

mod capture;
mod episodes;
mod events;
mod host;
mod profiles;
mod replay;
mod robot;
mod status;
mod uploads;

pub use capture::{AbortRequest, Aborted, StartRequest, Started, StopRequest, Stopped};
pub use episodes::{EpisodeSummary, EpisodesQuery};
pub use events::{CaptureAnnouncements, Event, EventKind};
pub use replay::{ReplayRequest, WindowQuery};
pub use status::{
    CaptureStatus, LiveViewCounts, Status, StreamCounts, StreamValues, Version, Viewer,
};
pub use uploads::{
    DestinationStatus, DestinationTested, EpisodeDeletion, EpisodeUpload, EpisodeUploads,
    UploadDestination, UploadStatus, UploadedFile, UploadsOverview, UploadsRetried,
    overview as upload_status,
};

use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use kapto_capture::control::{self, ANSWER_TIMEOUT, ControlClient, Request};
use kapto_capture::disk::DiskLimits;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::Writer;
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};

use crate::episode::{
    EpisodeBuffers, EpisodeClock, EpisodeDetail, EpisodeDurability, EpisodeFile, EpisodePackage,
    EpisodeProfile, EpisodeRecovered, EpisodeStream, EpisodeTask,
};
use crate::episode_events::EpisodeEvents;
use crate::layouts::Layouts;
use crate::preflight::{PreflightCheck, PreflightRequest, PreflightRun};
use crate::processes::{ProcessState, ProcessStatus, Processes};
use crate::quality::{
    CheckRange, CheckResult, EpisodeQuality, EpisodeStatistics, StatisticSummary,
};
use crate::replays::{self, ReplayOpening, ReplayState, ReplayStatus};
use crate::robot::{
    ColumnInput, LayoutInput, PanelInput, RobotField, RobotFiles, RobotJoint, RobotOverview,
    RobotRole, RobotStream, ViewColumn, ViewLayout, ViewPanel,
};
use crate::scene::{Placement, RobotScene, Scene, SceneLink, SceneReach, SceneShape, SceneVisual};
use crate::sessions::{ProfileSummary, ProfilesOverview, TaskSummary, TasksOverview};
use crate::settings::{Setting, SettingsOverview};
use crate::storage::Storage;
use crate::vocabulary::{
    EventPhase, EventSent, EventVocabulary, SendEvent, SentEvent, VocabularyKind,
};

// The handlers by name: the OpenAPI document tags an operation by its
// handler's module when a path names one.
use capture::{
    __path_abort, __path_preflight, __path_send_event, __path_start, __path_stop, abort, preflight,
    send_event, start, stop,
};
use episodes::{
    __path_episode, __path_episode_events, __path_episodes, __path_quality, __path_statistics,
    episode, episode_events, episodes, quality, statistics,
};
use events::{__path_events, events};
use profiles::{__path_event_kinds, __path_profiles, __path_tasks, event_kinds, profiles, tasks};
use replay::{
    __path_ask_replay, __path_camera_frames, __path_camera_picture, __path_replay,
    __path_replay_mesh, __path_replay_opening, __path_replay_scene, __path_replay_window,
    ask_replay, camera_frames, camera_picture, replay, replay_mesh, replay_opening, replay_scene,
    replay_window,
};
use robot::{
    __path_check_layout, __path_mesh, __path_remove_layout, __path_robot, __path_save_layout,
    __path_scene, check_layout, mesh, remove_layout, robot, save_layout, scene,
};
use status::{
    __path_processes, __path_settings, __path_status, __path_storage, __path_version, processes,
    settings, status, storage, version,
};
use uploads::{
    __path_episode_uploads, __path_retry_uploads, __path_test_destination, __path_upload_overview,
    episode_uploads, retry_uploads, test_destination, upload_overview,
};

/// The API's version: it only ever gains endpoints and fields within one.
pub const API_VERSION: u32 = 1;

/// What the API needs to know about this machine.
#[derive(Clone, Debug)]
pub struct ApiState {
    /// Kapto's processes: what each is doing and what it costs.
    pub processes: Arc<Processes>,
    /// The robot and its streams, as serve loaded them when it started.
    pub robot: Arc<RobotOverview>,
    /// The robot as the web UI draws it, and the mesh files it uses.
    pub scene: Arc<Scene>,
    /// The layouts the live view may be arranged by, as they change.
    pub layouts: Layouts,
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
        .route("/api/layout", put(save_layout).delete(remove_layout))
        .route("/api/layout/check", post(check_layout))
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
        save_layout,
        check_layout,
        remove_layout,
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
        RobotRole,
        ViewLayout,
        ViewColumn,
        ViewPanel,
        LayoutInput,
        ColumnInput,
        PanelInput,
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
///
/// # Errors
///
/// The document couldn't be written as JSON.
pub fn openapi_json() -> Result<String, serde_json::Error> {
    let mut document = ApiDoc::openapi();
    document.info.version = API_VERSION.to_string();
    document.to_pretty_json()
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
            | ErrorCode::LayoutUnknownKey
            | ErrorCode::LayoutWithoutPackage
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
            // Another party answered, but not as expected: capture, or
            // an upload destination.
            ErrorCode::CaptureControlUnexpectedAnswer
            | ErrorCode::UploadUnreachable
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
        ControlClient::connect(&socket, ANSWER_TIMEOUT)?.ask(&request)
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
        KaptoError::new(ErrorCode::CaptureControlUnexpectedAnswer)
            .detail(format!("unexpected answer: {response:?}")),
    )
}
