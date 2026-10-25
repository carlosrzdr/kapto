// SPDX-License-Identifier: Apache-2.0

//! What Kapto is doing and runs with: the versions, the capture process's
//! status, Kapto's processes, the data directory's storage and the
//! settings.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use kapto_capture::control;
use kapto_core::{ErrorCode, KaptoError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::preflight::PreflightRun;
use crate::processes::ProcessStatus;
use crate::settings::SettingsOverview;
use crate::storage::{self, Storage};

use super::{API_VERSION, ApiError, ApiState, Failure};

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
    /// The episode being closed, if one is: stopped, its stop answered and
    /// its end announced once it is finalised, while capture receives and
    /// may record the next.
    #[serde(default)]
    pub closing: Option<String>,
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

/// The versions of Kapto and of this API. The web UI checks it when it
/// loads.
#[utoipa::path(get, path = "/api/version", responses((status = 200, body = Version)))]
pub(super) async fn version() -> Json<Version> {
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
pub(super) async fn status(State(state): State<Arc<ApiState>>) -> Result<Json<Status>, Failure> {
    current_status(&state).await.map(Json)
}

/// What the capture process is doing, as it last answered.
pub(super) async fn current_status(state: &ApiState) -> Result<Status, Failure> {
    let status = state
        .capture_status
        .latest(&state.control_socket)
        .await
        .map_err(Failure::from)?;
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
            closing: status.closing,
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
pub(super) async fn processes(State(state): State<Arc<ApiState>>) -> Json<Vec<ProcessStatus>> {
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
pub(super) async fn storage(State(state): State<Arc<ApiState>>) -> Result<Json<Storage>, Failure> {
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

/// The settings Kapto runs with, each with its value and where it came
/// from. They are read when serve starts.
#[utoipa::path(get, path = "/api/settings", responses((status = 200, body = SettingsOverview)))]
pub(super) async fn settings(State(state): State<Arc<ApiState>>) -> Json<SettingsOverview> {
    Json(SettingsOverview::clone(&state.settings))
}
