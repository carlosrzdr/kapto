// SPDX-License-Identifier: Apache-2.0

//! Replaying a finalised episode: asking for its replay, its opening and
//! the stretches of it to draw, the robot it was recorded with, and its
//! cameras' frames.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use kapto_core::{ErrorCode, KaptoError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::replays::{self, ReplayOpening, ReplayState, ReplayStatus};
use crate::scene::RobotScene;

use super::{ApiError, ApiState, Failure, invalid};

/// Ask for an episode's replay; an empty object.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayRequest {}

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
pub(super) async fn replay_scene(
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
pub(super) async fn replay_mesh(
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
pub(super) async fn camera_frames(
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
pub(super) async fn camera_picture(
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
pub(super) async fn replay(
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
pub(super) async fn ask_replay(
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
pub(super) async fn replay_opening(
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
pub(super) async fn replay_window(
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
