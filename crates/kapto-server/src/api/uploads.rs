// SPDX-License-Identifier: Apache-2.0

//! Where uploads are: each destination's counts, each upload going, failed
//! or done, and one episode's uploads with where every object of it went
//! and whether it is still stored here.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{Deletion, JobState, Reader, Upload, UploadState};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::episode;
use crate::jobs::UPLOAD;

use super::{ApiError, ApiState, Failure};

/// A destination episodes are uploaded to, as `[[upload.destination]]`
/// names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadDestination {
    /// Its name.
    pub name: String,
    /// Where it is, e.g. `s3://robot-data`; never its credentials.
    pub url: String,
}

/// Every destination's counts, and every upload not done yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadsOverview {
    /// Episodes waiting for the upload lane, to every destination.
    pub waiting: u64,
    /// Each destination, in the order the configuration lists them.
    pub destinations: Vec<DestinationStatus>,
    /// Each upload going or failed, newest first.
    pub uploads: Vec<UploadStatus>,
}

/// How uploads to one destination are going.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DestinationStatus {
    /// The destination.
    #[serde(flatten)]
    pub destination: UploadDestination,
    /// Episodes going up to it now.
    pub uploading: u64,
    /// Episodes uploaded and verified there.
    pub uploaded: u64,
    /// Episodes whose upload failed, tried again by themselves later.
    pub failed: u64,
    /// The bytes sent of the episodes going up to it.
    pub bytes_sent: u64,
    /// The bytes in all of those episodes.
    pub bytes_total: u64,
    /// How many requests it throttled, over every upload to it.
    pub throttled: u64,
}

/// One episode's upload to one destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadStatus {
    /// The episode.
    pub episode: String,
    /// The destination's name.
    pub destination: String,
    /// `uploading`, `uploaded` or `failed`.
    pub state: String,
    /// The episode's key at the destination.
    pub key: String,
    /// The bytes of its files up so far.
    pub bytes_sent: u64,
    /// The bytes of all its files.
    pub bytes_total: u64,
    /// How many requests the destination throttled.
    pub throttled: u64,
    /// When it was first started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// When its progress was last reported, the same way; for throughput.
    pub reported_utc_ns: Option<i64>,
    /// When it was uploaded, the same way.
    pub uploaded_utc_ns: Option<i64>,
    /// When it last failed, the same way.
    pub failed_utc_ns: Option<i64>,
    /// Why it failed last: its cause code.
    pub failure_code: Option<String>,
    /// Why it failed last, in words.
    pub failure_message: Option<String>,
}

/// One episode's uploads, and whether it is still stored here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeUploads {
    /// Whether its folder is here; `false` once deleted here, verified
    /// elsewhere.
    pub stored_here: bool,
    /// How it was deleted here, if it was.
    pub deletion: Option<EpisodeDeletion>,
    /// Its upload to each destination it was started for.
    pub uploads: Vec<EpisodeUpload>,
}

/// An episode's upload to one destination, with every object of it there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeUpload {
    /// How it went.
    #[serde(flatten)]
    pub status: UploadStatus,
    /// Every object of it there, once uploaded.
    pub objects: Vec<UploadedFile>,
}

/// One object of an uploaded episode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadedFile {
    /// Its path in the episode, e.g. `recording.mcap`.
    pub path: String,
    /// Its key at the destination.
    pub key: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its SHA-256, as 64 lowercase hex digits.
    pub sha256: String,
}

/// How an episode was deleted here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EpisodeDeletion {
    /// When, in UTC nanoseconds since the Unix epoch.
    pub deleted_utc_ns: i64,
    /// Why: `after_upload` or `disk_low`.
    pub reason: String,
    /// What deleted it, e.g. `worker`.
    pub deleted_by: String,
    /// Each destination holding a verified copy, by name.
    pub destinations: Vec<String>,
}

/// A destination tested with a probe object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DestinationTested {
    /// Where the probe was written.
    pub key: String,
    /// Whether it could be removed again; if not, it is still there: the
    /// credentials may write but not delete, which uploads don't need.
    pub removed: bool,
    /// How long writing, reading back and removing it took, in milliseconds.
    pub took_ms: u64,
}

/// An episode's failed uploads, queued to be tried again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadsRetried {
    /// The destinations they are to.
    pub destinations: Vec<String>,
}

/// Every destination's counts and every upload not done, from `reader`.
///
/// # Errors
///
/// `store.query_failed` if reading failed.
pub fn overview(
    reader: &Reader,
    destinations: &[UploadDestination],
) -> Result<UploadsOverview, KaptoError> {
    let all = reader.all_uploads()?;
    let mut statuses: Vec<DestinationStatus> = destinations
        .iter()
        .map(|destination| DestinationStatus {
            destination: destination.clone(),
            uploading: 0,
            uploaded: 0,
            failed: 0,
            bytes_sent: 0,
            bytes_total: 0,
            throttled: 0,
        })
        .collect();
    for upload in &all {
        let Some(status) = statuses
            .iter_mut()
            .find(|status| status.destination.name == upload.destination)
        else {
            continue;
        };
        status.throttled += upload.counts.throttled;
        match upload.state {
            UploadState::Uploading => {
                status.uploading += 1;
                status.bytes_sent += upload.counts.bytes_sent;
                status.bytes_total += upload.counts.bytes_total;
            }
            UploadState::Uploaded => status.uploaded += 1,
            UploadState::Failed => status.failed += 1,
        }
    }
    let mut uploads: Vec<UploadStatus> = all
        .iter()
        .filter(|upload| upload.state != UploadState::Uploaded)
        .map(status)
        .collect();
    uploads.sort_by_key(|upload| std::cmp::Reverse(upload.started_utc_ns));
    Ok(UploadsOverview {
        waiting: reader.count_jobs(UPLOAD, JobState::Queued)?,
        destinations: statuses,
        uploads,
    })
}

/// `episode`'s uploads, from `reader`; `stored_here` says if its folder is.
pub(crate) fn of_episode(
    reader: &Reader,
    episode: &str,
    stored_here: bool,
) -> Result<EpisodeUploads, KaptoError> {
    let uploads = reader
        .uploads(episode)?
        .iter()
        .map(|upload| EpisodeUpload {
            status: status(upload),
            objects: upload
                .objects
                .iter()
                .map(|object| UploadedFile {
                    path: object.path.clone(),
                    key: object.key.clone(),
                    bytes: object.bytes,
                    sha256: object.sha256.clone(),
                })
                .collect(),
        })
        .collect();
    Ok(EpisodeUploads {
        stored_here,
        deletion: reader.deletion(episode)?.map(deletion),
        uploads,
    })
}

fn status(upload: &Upload) -> UploadStatus {
    UploadStatus {
        episode: upload.episode.clone(),
        destination: upload.destination.clone(),
        state: match upload.state {
            UploadState::Uploading => "uploading",
            UploadState::Uploaded => "uploaded",
            UploadState::Failed => "failed",
        }
        .to_owned(),
        key: upload.key.clone(),
        bytes_sent: upload.counts.bytes_sent,
        bytes_total: upload.counts.bytes_total,
        throttled: upload.counts.throttled,
        started_utc_ns: upload.started_utc_ns,
        reported_utc_ns: upload.reported_utc_ns,
        uploaded_utc_ns: upload.uploaded_utc_ns,
        failed_utc_ns: upload.failed_utc_ns,
        failure_code: upload.failure.as_ref().map(|failure| failure.code.clone()),
        failure_message: upload
            .failure
            .as_ref()
            .map(|failure| failure.message.clone()),
    }
}

fn deletion(deletion: Deletion) -> EpisodeDeletion {
    EpisodeDeletion {
        deleted_utc_ns: deletion.deleted_utc_ns,
        reason: deletion.reason.as_str().to_owned(),
        deleted_by: deletion.deleted_by,
        destinations: deletion
            .copies
            .into_iter()
            .map(|copy| copy.destination)
            .collect(),
    }
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
pub(super) async fn upload_overview(
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
    tokio::task::spawn_blocking(move || overview(&store.reader()?, &destinations))
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
pub(super) async fn episode_uploads(
    State(state): State<Arc<ApiState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<EpisodeUploads>, Failure> {
    let (data_dir, store) = (state.data_dir.clone(), state.store.clone());
    tokio::task::spawn_blocking(move || {
        let reader = store.reader()?;
        let here = episode::stored_here(&data_dir, &reader, &id)?;
        of_episode(&reader, &id, here)
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
pub(super) async fn test_destination(
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
pub(super) async fn retry_uploads(
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
