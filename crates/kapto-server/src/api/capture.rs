// SPDX-License-Identifier: Apache-2.0

//! Asking the capture process: running pre-flight, starting, stopping and
//! aborting episodes, and sending events.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use kapto_capture::control::{self, Request};
use kapto_core::{ErrorCode, KaptoError};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::preflight::{PreflightRequest, PreflightRun};
use crate::sessions;
use crate::vocabulary::{self, EventSent, SendEvent, SentEvent};

use super::{ApiError, ApiState, Failure, ask, invalid, unexpected};

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
pub(super) async fn preflight(
    State(state): State<Arc<ApiState>>,
    request: Result<Json<PreflightRequest>, JsonRejection>,
) -> Result<Json<PreflightRun>, Failure> {
    let Json(PreflightRequest {}) = request.map_err(|rejection| invalid(&rejection))?;
    match ask(&state, Request::Preflight).await? {
        control::Response::Preflight(run) => Ok(Json(PreflightRun::from(run))),
        other => Err(unexpected(&other)),
    }
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
pub(super) async fn start(
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
pub(super) async fn stop(
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
pub(super) async fn abort(
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
pub(super) async fn send_event(
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
