// SPDX-License-Identifier: Apache-2.0

//! `wrk-upload`: the worker's upload lane. It takes only upload jobs from
//! serve, on a connection of its own, and puts each episode up to every
//! destination in turn, on a small runtime whose helper threads
//! (`wrk-upload-io`) share its low priority: uploads use the CPU and the
//! disk only when nothing else wants them.
//!
//! Each destination's upload is started with serve, which answers with
//! the key it was first given and how far it got, so an upload carries on
//! after a restart; each step is reported back, and the upload counts as
//! done only once its marker is written.
//!
//! It waits before each file while an episode is recorded, if
//! `upload.pause_while_recording` says to, and once the worker is stopping
//! it stops there too, giving the job back: the upload carries on from
//! that file next time, without counting as an attempt.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use kapto_config::UploadDestination;
use kapto_core::clock::utc_now_ns;
use kapto_core::{ErrorCode, KaptoError};
use kapto_server::jobs::{Found, JOB_SOCKET, JobClient, JobOrder, JobRequest, UPLOAD};
use kapto_store::manifest::MANIFEST_FILE;
use kapto_store::{Episode, JobFailure, UploadCounts, UploadedObject, episode_folder};
use kapto_upload::backoff::Backoff;
use kapto_upload::episode::{
    FileProgress, Progress, RuntimeSleep, Uploaded, marker_of, upload_episode,
};
use kapto_upload::key::{KeyValues, episode_key};
use kapto_upload::stores;
use sha2::{Digest, Sha256};

use super::{POLL, WAIT, step, wait};

/// Takes upload jobs from serve and does them until `shutdown` is set,
/// waiting while `recording` is set if `pause` says to, before a job and
/// before each file of it.
pub(super) fn lane(
    data_dir: &Path,
    destinations: &[UploadDestination],
    pause: bool,
    (shutdown, recording): (&AtomicBool, &super::Recording),
) {
    // Timers for the waits between tries, and the network for the stores.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .thread_name("wrk-upload-io")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[worker] uploads stopped: no runtime for them: {error}");
            return;
        }
    };
    // Never heard to be recording, so never waited for.
    let never = super::Recording::new();
    let pausing = if pause { recording } else { &never };
    let mut between = || step(pausing, shutdown);
    let socket = data_dir.join(JOB_SOCKET);
    let mut serve: Option<JobClient> = None;
    while !shutdown.load(Ordering::Relaxed) {
        let Some(client) = serve.as_mut() else {
            serve = JobClient::connect(&socket).ok();
            if serve.is_none() {
                wait(WAIT, shutdown);
            }
            continue;
        };
        if pause && recording.now() {
            wait(POLL, shutdown);
            continue;
        }
        let order = match client.next_job_of(&[UPLOAD]) {
            Ok(Some(order)) => order,
            Ok(None) => {
                wait(WAIT, shutdown);
                continue;
            }
            Err(error) => {
                eprintln!("[worker] {error}");
                serve = None;
                continue;
            }
        };
        let failure = runtime
            .block_on(upload(data_dir, destinations, &order, client, &mut between))
            .err();
        if let Some(error) = &failure
            && error.code() == ErrorCode::WorkerJobStopped
        {
            match client.released(order.id) {
                Ok(()) => eprintln!(
                    "[worker] upload {}: stopped part-way, given back",
                    order.episode
                ),
                Err(error) => eprintln!("[worker] {error}"),
            }
            return;
        }
        if let Some(error) = &failure {
            eprintln!("[worker] {} not uploaded: {error}", order.episode);
        }
        let failure = failure.as_ref().map(JobFailure::from);
        if let Err(error) = client.finished(order.id, failure, Found::default()) {
            eprintln!("[worker] {error}");
            serve = None;
        }
    }
}

/// The episode of `order` up to each destination, carrying on each upload
/// from where it was; the first failure, once every destination was tried,
/// or at once `worker.job_stopped` if `between` stopped it.
async fn upload(
    data_dir: &Path,
    destinations: &[UploadDestination],
    order: &JobOrder,
    client: &mut JobClient,
    between: &mut (dyn FnMut() -> Result<(), KaptoError> + Send),
) -> Result<(), KaptoError> {
    let folder = episode_folder(data_dir, &order.episode).ok_or_else(|| {
        KaptoError::new(ErrorCode::EpisodeUnreadable)
            .detail(format!("`{}` isn't an episode ID", order.episode))
    })?;
    let episode = Episode::read(&folder)?;
    let robot = episode.package.as_ref().map(|package| package.id.clone());
    let manifest = std::fs::read(folder.join(MANIFEST_FILE)).map_err(|error| {
        KaptoError::new(ErrorCode::UploadEpisodeUnreadable).detail(error.to_string())
    })?;
    let values = KeyValues {
        robot: robot.as_deref(),
        episode: &episode.id,
        started_utc_ns: episode.started_utc_ns,
        manifest_sha256: &hex(&Sha256::digest(&manifest)),
    };
    let mut first_failure = None;
    // Only those serve says the episode goes to now: each destination
    // takes it as its own `upload.when` says.
    let due = destinations
        .iter()
        .filter(|destination| order.destinations.contains(&destination.name));
    for destination in due {
        let done = to(
            destination,
            &folder,
            &values,
            robot.clone(),
            client,
            between,
        )
        .await;
        // Stopped, not failed: the upload stays as far as it got.
        if let Err(error) = &done
            && error.code() == ErrorCode::WorkerJobStopped
        {
            return done;
        }
        if let Err(error) = done {
            let failure = JobRequest::UploadFailed {
                episode: episode.id.clone(),
                destination: destination.name.clone(),
                failure: JobFailure::from(&error),
            };
            client.upload_report(&failure)?;
            first_failure.get_or_insert(error.context("destination", destination.name.clone()));
        }
    }
    first_failure.map_or(Ok(()), Err)
}

/// The episode in `folder` up to `destination`.
async fn to(
    destination: &UploadDestination,
    folder: &Path,
    values: &KeyValues<'_>,
    robot: Option<String>,
    client: &mut JobClient,
    between: &mut (dyn FnMut() -> Result<(), KaptoError> + Send),
) -> Result<(), KaptoError> {
    let (name, episode) = (&destination.name, values.episode);
    let key = episode_key(&destination.prefix, &destination.layout, values)?;
    // Started before the store is opened, so a destination that can't be
    // is recorded as failed, not as never tried.
    let (key, progress) = client.upload_start(episode, name, key.as_ref())?;
    let opened = stores::open(destination, &|name| std::env::var(name).ok())?;
    let key = object_store::path::Path::parse(&key).map_err(|error| {
        KaptoError::new(ErrorCode::UploadFailed).detail(format!("the key kept, `{key}`: {error}"))
    })?;
    let mut progress: Progress = serde_json::from_str(&progress).unwrap_or_default();
    let settings = &destination.settings;
    let marker = marker_of(folder, robot, utc_now_ns())?;
    let files = marker.files.clone();
    let bytes_total = files.iter().map(|file| file.bytes).sum();
    let mut backoff =
        Backoff::from_clock(settings.backoff_initial.value, settings.backoff_max.value);
    let mut reported = Ok(());
    let outcome = upload_episode(
        opened.store(),
        folder,
        &key,
        marker,
        &stores::policy(settings, &opened),
        &mut backoff,
        &RuntimeSleep,
        &mut progress,
        &mut |progress: &Progress| {
            let report = JobRequest::UploadProgress {
                episode: episode.to_owned(),
                destination: name.clone(),
                progress: serde_json::to_string(progress).unwrap_or_default(),
                counts: UploadCounts {
                    bytes_sent: progress.sent_bytes(&files),
                    bytes_total,
                    throttled: progress.throttled,
                },
            };
            if reported.is_ok() {
                reported = client.upload_report(&report);
            }
        },
        between,
    )
    .await;
    reported?;
    let marker = match outcome? {
        Uploaded::Now(marker) | Uploaded::AlreadyThere(marker) => marker,
    };
    let objects = marker
        .files
        .into_iter()
        .map(|file| {
            let e_tag = match progress.files.get(&file.path) {
                Some(FileProgress::Verified { e_tag }) => e_tag.clone(),
                _ => None,
            };
            UploadedObject {
                key: key.clone().join(file.path.as_str()).to_string(),
                path: file.path,
                bytes: file.bytes,
                sha256: file.sha256,
                e_tag,
            }
        })
        .collect();
    client.upload_report(&JobRequest::Uploaded {
        episode: episode.to_owned(),
        destination: name.clone(),
        objects,
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}
