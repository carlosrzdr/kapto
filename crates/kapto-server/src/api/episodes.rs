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
use crate::episode_bin;
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
    /// When it was deleted by hand into the bin, in UTC nanoseconds since
    /// the Unix epoch; `null` unless it is in the bin.
    #[serde(default)]
    pub binned_utc_ns: Option<i64>,
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
    /// Only those in the bin (`true`), deleted by hand; none of them if
    /// not given.
    pub bin: Option<bool>,
}

/// The finalised episodes in the data directory, newest first. Folders
/// that aren't finalised, or whose summary can't be read, are left out.
#[utoipa::path(
    get,
    path = "/api/episodes",
    params(
        ("limit" = Option<usize>, Query, description = "At most this many, newest first; 100 if not given."),
        ("accepted" = Option<bool>, Query, description = "Only those the quality checks accepted (`true`) or rejected (`false`), which leaves out those not checked yet; every episode if not given."),
        ("event" = Option<String>, Query, description = "Only those with an event of this kind, e.g. `intervention`, which leaves out those whose events weren't read yet; every episode if not given."),
        ("bin" = Option<bool>, Query, description = "Only those in the bin, deleted by hand and not emptied yet (`true`); none of them if not given.")
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
        let bin: BTreeMap<String, i64> = reader
            .bin()?
            .into_iter()
            .map(|binned| (binned.episode, binned.binned_utc_ns))
            .collect();
        let in_bin = query.bin.unwrap_or(false);
        let listed = reader.list(limit, query.accepted, query.event.as_deref(), in_bin)?;
        Ok::<_, KaptoError>(
            listed
                .into_iter()
                .map(|listed| {
                    let stored_here = !deleted.contains(&listed.episode.id);
                    let binned_utc_ns = bin.get(&listed.episode.id).copied();
                    EpisodeSummary {
                        stored_here,
                        binned_utc_ns,
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
        Ok(detail) => Ok(EpisodeDetail {
            binned_utc_ns: store
                .reader()?
                .binned(&id)?
                .map(|binned| binned.binned_utc_ns),
            ..detail
        }),
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
            binned_utc_ns: None,
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

/// An episode put in the bin.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BinnedEpisode {
    /// Its ID.
    pub episode: String,
    /// When it went into the bin, in UTC nanoseconds since the Unix epoch.
    pub binned_utc_ns: i64,
}

/// The header a client names itself in, recorded with what it deletes:
/// `web UI` or `command line`; anything else, or none, is the API.
pub const CLIENT_HEADER: &str = "kapto-client";

/// Deletes a finalised episode by hand: its folder moved into the bin, as
/// it is, until the bin is emptied or it is put back. Its replay is dropped
/// from the cache, and its jobs wait until it is put back.
#[utoipa::path(
    delete,
    path = "/api/episodes/{id}",
    params(
        ("id" = String, Path, description = "The episode's ID, as `/api/episodes` lists it."),
        ("kapto-client" = Option<String>, Header, description = "What deletes it, recorded with it: `web UI` or `command line`; the API if not given.")
    ),
    responses(
        (status = 200, body = BinnedEpisode),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 404, description = "No finalised episode here has this ID", body = ApiError),
        (status = 409, description = "The episode is in the bin already", body = ApiError),
        (status = 500, description = "Its folder can't be moved, or the deletion recorded", body = ApiError)
    )
)]
pub(super) async fn delete_episode(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<BinnedEpisode>, Failure> {
    let from = match headers
        .get(CLIENT_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some(client @ ("web UI" | "command line")) => client.to_owned(),
        _ => "API".to_owned(),
    };
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || {
        let now = kapto_core::ClockSample::now().utc_ns;
        episode_bin::put_in_bin(&data_dir, &store, &id, (&from, now))
    })
    .await
    .map_err(|error| {
        Failure::from(KaptoError::new(ErrorCode::StoreBinFailed).detail(error.to_string()))
    })?
    .map(|binned| {
        Json(BinnedEpisode {
            episode: binned.episode,
            binned_utc_ns: binned.binned_utc_ns,
        })
    })
    .map_err(Failure::from)
}

/// Puts an episode deleted by hand back from the bin, as it was.
#[utoipa::path(
    post,
    path = "/api/episodes/{id}/restore",
    params(("id" = String, Path, description = "The episode's ID, as `/api/episodes?bin=true` lists it.")),
    responses(
        (status = 200, description = "Put back; an empty object", body = Object),
        (status = 400, description = "The ID isn't an episode ID", body = ApiError),
        (status = 409, description = "The episode isn't in the bin", body = ApiError),
        (status = 500, description = "Its folder can't be moved back, or a folder of its name is in the way", body = ApiError)
    )
)]
pub(super) async fn restore_episode(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || episode_bin::put_back(&data_dir, &store, &id))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreBinFailed).detail(error.to_string()))
        })?
        .map(|()| Json(serde_json::json!({})))
        .map_err(Failure::from)
}

/// What emptying the bin by hand did.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmptiedBin {
    /// How many episodes in it were queued to be deleted for good, by the
    /// worker's retention executor, which records each as it goes.
    pub queued: usize,
}

/// Empties the bin by hand: every episode in it is deleted for good, by the
/// worker as it gets to them, and can no longer be put back once it has.
#[utoipa::path(
    post,
    path = "/api/bin/empty",
    responses(
        (status = 200, body = EmptiedBin),
        (status = 500, description = "The bin can't be read, or the deletions queued", body = ApiError)
    )
)]
pub(super) async fn empty_bin(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<EmptiedBin>, Failure> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || episode_bin::empty(&store))
        .await
        .map_err(|error| {
            Failure::from(KaptoError::new(ErrorCode::StoreQueryFailed).detail(error.to_string()))
        })?
        .map(|queued| Json(EmptiedBin { queued }))
        .map_err(Failure::from)
}
