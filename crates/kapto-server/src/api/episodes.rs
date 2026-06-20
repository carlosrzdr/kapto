// SPDX-License-Identifier: Apache-2.0

//! The finalised episodes: listing them, one's details, and what the worker
//! found in it (quality checks, events, statistics).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::ListedEpisode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::episode::{
    self as episode_detail, EpisodeDetail, EpisodeFile, EpisodePackage, EpisodeRecovered,
};
use crate::episode_events::{self, EpisodeEvents};
use crate::quality::{self, EpisodeQuality, EpisodeStatistics};

use super::{ApiError, ApiState, Failure, StreamCounts};

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
pub(super) async fn episodes(
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
pub(super) async fn episode(
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
pub(super) async fn quality(
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
pub(super) async fn episode_events(
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
pub(super) async fn statistics(
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
