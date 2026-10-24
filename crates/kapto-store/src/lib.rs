// SPDX-License-Identifier: Apache-2.0

//! Runtime state in SQLite, episode sidecar files, manifests and their schemas.

mod binned;
pub mod db;
mod deletions;
pub mod episode;
pub mod events;
mod indexed_events;
pub mod jobs;
pub mod manifest;
pub mod models;
pub mod qa;
pub mod records;
pub mod replay_view;
mod sidecar;
pub mod upload_marker;
mod uploads;

pub use binned::{BIN_DIR, Binned};
pub use db::{IndexedEpisode, IndexedStream, ListedEpisode, Reader, Writer};
pub use deletions::{Deletion, DeletionReason, RemoteCopy};
pub use episode::{
    BufferSummary, ClockSummary, DurabilitySummary, Episode, LossDetectionKind, OperationMode,
    PackageIdentity, Preflight, PreflightCheck, Recovered, SessionProfile, SessionTask,
    StreamSummary, episode_folder,
};
pub use events::{EVENTS_CHANNEL, Event, Phase, event_schema, unended};
pub use jobs::{Job, JobFailure, JobId, JobState, Kinds, Requeued};
pub use manifest::{Discrepancy, FileEntry, Manifest, verify_episode, verify_episode_with};
pub use models::AnomalyModel;
pub use qa::{CheckKind, QaResult, QaRun, TimeRange, Verdict};
pub use uploads::{Upload, UploadCounts, UploadState, UploadedObject};
