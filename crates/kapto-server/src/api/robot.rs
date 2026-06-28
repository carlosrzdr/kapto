// SPDX-License-Identifier: Apache-2.0

//! The robot as serve loaded it: its overview, the layout the live view is
//! arranged by, and its scene and meshes for drawing it.

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use kapto_core::{ErrorCode, KaptoError};

use crate::robot::{LayoutInput, RobotOverview, ViewLayout};
use crate::scene::RobotScene;

use super::{ApiError, ApiState, Failure, invalid};

/// The robot and its streams as Kapto runs them: each joint with its limits
/// and the decoded value that drives it, each stream with its input, rate
/// and decoded fields, and problems that don't stop Kapto.
#[utoipa::path(get, path = "/api/robot", responses((status = 200, body = RobotOverview)))]
pub(super) async fn robot(State(state): State<Arc<ApiState>>) -> Json<RobotOverview> {
    let mut robot = RobotOverview::clone(&state.robot);
    if let Some(layout) = state.layouts.current() {
        robot.layout = layout;
    }
    Json(robot)
}

/// Save a layout as the default for the robot package Kapto runs, read by
/// every browser: written to `layouts/<package id>.toml` in the
/// configuration folder, checked as `layout.toml` is. Answers with it as
/// `/api/robot` now gives it, each panel with what the package lacks.
#[utoipa::path(
    put,
    path = "/api/layout",
    request_body = LayoutInput,
    responses(
        (status = 200, body = ViewLayout),
        (status = 400, description = "The body isn't a layout", body = ApiError),
        (status = 409, description = "The layout breaks a rule of layout.toml, or Kapto runs no robot package", body = ApiError),
        (status = 500, description = "The file can't be written", body = ApiError)
    )
)]
pub(super) async fn save_layout(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<LayoutInput>, JsonRejection>,
) -> Result<Json<ViewLayout>, Failure> {
    let Json(input) = request.map_err(|rejection| invalid(&rejection))?;
    let layouts = state.layouts.clone();
    on_disk(move || layouts.save(&input)).await
}

/// What a layout lacks: the layout as the live view draws it, each panel
/// with what the robot's streams lack of it, by the rules every layout is
/// held to. For one the operator keeps in their browser; nothing is saved.
#[utoipa::path(
    post,
    path = "/api/layout/check",
    request_body = LayoutInput,
    responses(
        (status = 200, body = ViewLayout),
        (status = 400, description = "The body isn't a layout", body = ApiError),
        (status = 409, description = "The layout breaks a rule of layout.toml", body = ApiError)
    )
)]
pub(super) async fn check_layout(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<LayoutInput>, JsonRejection>,
) -> Result<Json<ViewLayout>, Failure> {
    let Json(input) = request.map_err(|rejection| invalid(&rejection))?;
    state.layouts.check(&input).map(Json).map_err(|problems| {
        Failure::from(problems.into_iter().next().unwrap_or_else(|| {
            KaptoError::new(ErrorCode::LayoutWithoutPackage).detail("no reason given".to_owned())
        }))
    })
}

/// Remove the default layout saved for the robot package Kapto runs: the
/// live view is arranged by the package's own layout again, or the one
/// made from it. Answers with it.
#[utoipa::path(
    delete,
    path = "/api/layout",
    responses(
        (status = 200, body = ViewLayout),
        (status = 409, description = "Kapto runs no robot package", body = ApiError),
        (status = 500, description = "The file can't be removed", body = ApiError)
    )
)]
pub(super) async fn remove_layout(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<ViewLayout>, Failure> {
    let layouts = state.layouts.clone();
    on_disk(move || layouts.remove()).await
}

/// Runs `work`, which reads or writes files, off the API's threads; its
/// first problem is the answer if it has any.
async fn on_disk(
    work: impl FnOnce() -> Result<ViewLayout, Vec<KaptoError>> + Send + 'static,
) -> Result<Json<ViewLayout>, Failure> {
    let done = tokio::task::spawn_blocking(work).await.map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::LayoutWriteFailed).detail(error.to_string()))
    })?;
    done.map(Json).map_err(|problems| {
        Failure::from(problems.into_iter().next().unwrap_or_else(|| {
            KaptoError::new(ErrorCode::LayoutWriteFailed).detail("no reason given".to_owned())
        }))
    })
}

/// The robot as the web UI draws it: each link, in the order its poses are
/// sent as the robot moves, with its visual shapes, their colours and where
/// they sit. Meshes are numbered, each at `/api/robot/meshes/{n}`.
#[utoipa::path(get, path = "/api/robot/scene", responses((status = 200, body = RobotScene)))]
pub(super) async fn scene(State(state): State<Arc<ApiState>>) -> Json<RobotScene> {
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
pub(super) async fn mesh(
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
