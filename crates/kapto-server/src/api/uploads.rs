// SPDX-License-Identifier: Apache-2.0

//! Where uploads are: each destination's counts, each upload going, failed
//! or done, and one episode's uploads with where every object of it went
//! and whether it is still stored here.

use kapto_store::{Deletion, JobState, Reader, Upload, UploadState};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::jobs::UPLOAD;

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
) -> Result<UploadsOverview, kapto_core::KaptoError> {
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
) -> Result<EpisodeUploads, kapto_core::KaptoError> {
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
