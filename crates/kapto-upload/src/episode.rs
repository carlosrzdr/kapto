// SPDX-License-Identifier: Apache-2.0

//! One finalised episode up to one store: every file its manifest lists,
//! and the manifest, then the completeness marker `_SUCCESS` last.
//!
//! - **Never twice, never over.** An episode whose marker is already there
//!   with the same files is left as it is; one whose marker lists other
//!   files is a conflict, reported and never overwritten.
//! - **Carries on after a restart.** Each step is written into a
//!   [`Progress`] the caller saves: a large file's upload and part IDs as
//!   each part goes up, each file once verified. Given back, the upload goes
//!   on from there, sending no part twice.
//! - **Tries again, then gives up.** A request that fails because the store
//!   couldn't be reached is tried again after a [`Backoff`] wait, up to
//!   `max_attempts` times; one refused (credentials, permissions) isn't.
//!   One the store throttled is tried again after a wait without counting,
//!   and halves the requests in flight, which grow back as requests
//!   succeed ([`Window`]).
//! - **Checked before it counts.** Each file is verified as [`Verify`]
//!   says; the marker is written only once every file is.
//! - **Kept under the bandwidth cap.** Each request that sends data waits
//!   its turn at the [`Pace`] the cap sets, if one is set.
//! - **Paused or stopped between files.** The caller is asked before each
//!   file and before the marker, and may wait there or stop the upload,
//!   which carries on from that file when given its progress back.

use std::collections::BTreeMap;
use std::fs::File;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use kapto_core::{ErrorCode, KaptoError, hex};
use kapto_store::manifest::{FileEntry, MANIFEST_FILE, Manifest};
use kapto_store::upload_marker::{SUCCESS_FILE, UploadMarker};
use object_store::multipart::{MultipartStore, PartId};
use object_store::path::Path;
use object_store::{Error, ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backoff::Backoff;
use crate::pace::Pace;
use crate::throttle::{Window, throttled};

/// How uploaded files are checked to be the ones sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verify {
    /// The store checked each file's SHA-256 as it took it; a store that
    /// doesn't has the file read back instead.
    ServerChecksum,
    /// Each file is read back and hashed again.
    ReadBack,
    /// Only the size is compared.
    SizeOnly,
}

/// What an episode's upload goes by, from `[upload]` and its destination.
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Files up to this size go up whole; larger ones in parts this size,
    /// or larger for a file that would need more than `most_parts`.
    pub part_bytes: usize,
    /// The most parts the store takes in one file.
    pub most_parts: u64,
    /// How many parts of a file go up at once to begin with.
    pub parallel_parts: usize,
    /// The most requests in flight at once, which the parts grow to while
    /// the store keeps up.
    pub max_in_flight: usize,
    /// How many times a request is tried before the upload fails.
    pub max_attempts: u32,
    /// How files are checked.
    pub verify: Verify,
    /// Whether the store checks a file's SHA-256 as it takes it.
    pub store_checks_sha256: bool,
    /// Whether the completeness marker is written.
    pub marker: bool,
    /// The most sent per second on average; no cap if `None`.
    pub bandwidth: Option<NonZeroU64>,
}

/// The store an episode goes to, and its parts API if it has one.
#[derive(Clone, Copy)]
pub struct Store<'a> {
    /// The store.
    pub objects: &'a dyn ObjectStore,
    /// Putting a large file in parts that survive a restart; without it a
    /// large file goes up in one stream and starts again if interrupted.
    pub multipart: Option<&'a dyn MultipartStore>,
}

/// Waiting between tries and to keep under the cap: the runtime's timer
/// and the monotonic clock, or a test's.
pub trait Sleep: Send + Sync {
    /// Waits `time`.
    fn sleep(&self, time: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    /// The time now, from any fixed point.
    fn now(&self) -> Duration;
}

/// Waits on the runtime's timer.
pub struct RuntimeSleep;

impl Sleep for RuntimeSleep {
    fn sleep(&self, time: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(time))
    }

    fn now(&self) -> Duration {
        Duration::from_nanos(kapto_core::clock::monotonic_now().as_nanos())
    }
}

/// How far an episode's upload got, saved by the caller after each step.
/// Nothing saved yet (`{}`) is an upload not started.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    /// Each file started, by its path in the episode.
    pub files: BTreeMap<String, FileProgress>,
    /// Whether the episode is uploaded: every file verified and, unless
    /// turned off, its marker written.
    pub done: bool,
    /// How many requests the store throttled, tried again without counting
    /// as attempts, over every try of this upload.
    pub throttled: u64,
}

impl Progress {
    /// How many bytes of `files`, an episode's, are up: each file verified
    /// whole, and each part sent of one going up in parts.
    #[must_use]
    pub fn sent_bytes(&self, files: &[FileEntry]) -> u64 {
        files
            .iter()
            .map(|file| match self.files.get(&file.path) {
                Some(FileProgress::Verified { .. }) => file.bytes,
                Some(FileProgress::Parts {
                    part_bytes, parts, ..
                }) => {
                    let count = u64::try_from(parts.len()).unwrap_or(u64::MAX);
                    count.saturating_mul(*part_bytes).min(file.bytes)
                }
                None => 0,
            })
            .sum()
    }
}

/// How far one file got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileProgress {
    /// Going up in parts: the upload's ID, the parts' size and each part
    /// sent so far, by its number, which may come back out of order.
    Parts {
        /// The store's ID of the upload.
        upload_id: String,
        /// The size of each part but the last, kept so an upload carried on
        /// cuts the file as it began; 0 in progress saved without it.
        #[serde(default)]
        part_bytes: u64,
        /// The store's ID of each part sent, by its number from 0.
        parts: BTreeMap<u64, String>,
    },
    /// Up and checked, with the store's tag of it.
    Verified {
        /// The store's version tag (an S3 `ETag`), if it has one.
        e_tag: Option<String>,
    },
}

/// How an episode's upload ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uploaded {
    /// Its files and marker were put there now.
    Now(UploadMarker),
    /// Its marker, with the same files, was there already.
    AlreadyThere(UploadMarker),
}

/// The episode in `folder` up to `store` under `key`, carrying on from
/// `progress` and saving it with `save` after each step. `between` is
/// called before each file and before the marker: it may wait, or stop the
/// upload with an error of its own, which is returned as it is.
///
/// # Errors
///
/// What `between` stopped the upload with, and:
///
/// - `upload.episode_unreadable`: the episode's manifest or a file can't be read.
/// - `upload.unreachable`: the store couldn't be reached `max_attempts` times running.
/// - `upload.denied`: the store refused the credentials or the request.
/// - `upload.marker_conflict`: a marker listing other files is there.
/// - `upload.verify_failed`: a file there isn't the one sent.
/// - `upload.marker_unreadable`, `upload.failed`: as they say.
#[allow(
    clippy::too_many_arguments,
    reason = "each is one thing an upload needs; grouping them would only rename them"
)]
pub async fn upload_episode(
    store: Store<'_>,
    folder: &std::path::Path,
    key: &Path,
    marker: UploadMarker,
    policy: &Policy,
    backoff: &mut Backoff,
    sleep: &dyn Sleep,
    progress: &mut Progress,
    save: &mut (dyn FnMut(&Progress) + Send),
    between: &mut (dyn FnMut() -> Result<(), KaptoError> + Send),
) -> Result<Uploaded, KaptoError> {
    let tries = Tries {
        throttled: AtomicU64::new(progress.throttled),
        policy,
        backoff: Mutex::new(backoff),
        sleep,
        pace: Mutex::new(Pace::new(policy.bandwidth)),
        window: Mutex::new(Window::new(policy.parallel_parts, policy.max_in_flight)),
    };
    let marker_key = key.clone().join(SUCCESS_FILE);
    if policy.marker
        && let Some(there) = existing_marker(store, &marker_key, &tries).await?
    {
        return compare(there, &marker);
    }
    for file in &marker.files {
        if !matches!(
            progress.files.get(&file.path),
            Some(FileProgress::Verified { .. })
        ) {
            between()?;
            let at = key.clone().join(file.path.as_str());
            upload_file(store, folder, file, &at, &tries, progress, save).await?;
            let e_tag = verify(store, file, &at, &tries).await?;
            progress
                .files
                .insert(file.path.clone(), FileProgress::Verified { e_tag });
            tries.count(progress);
            save(progress);
        }
    }
    if policy.marker {
        between()?;
        let json = PutPayload::from(marker.to_json().into_bytes());
        tries.paced(bytes(json.content_length())).await;
        let create = PutOptions::from(PutMode::Create);
        let put = tries
            .run(|| {
                store
                    .objects
                    .put_opts(&marker_key, json.clone(), create.clone())
            })
            .await;
        match put {
            Ok(_) => {}
            // Another upload of it finished first.
            Err(Error::AlreadyExists { .. }) => {
                let there = existing_marker(store, &marker_key, &tries).await?;
                return there.map_or_else(
                    || Err(failed(&marker_key, "the marker vanished")),
                    |there| compare(there, &marker),
                );
            }
            Err(error) => return Err(store_error(&marker_key, &error)),
        }
    }
    progress.done = true;
    tries.count(progress);
    save(progress);
    Ok(Uploaded::Now(marker))
}

/// The marker of the episode in `folder` as it would be uploaded now.
///
/// # Errors
///
/// `upload.episode_unreadable` if its manifest can't be read.
pub fn marker_of(
    folder: &std::path::Path,
    robot: Option<String>,
    uploaded_utc_ns: i64,
) -> Result<UploadMarker, KaptoError> {
    let bytes =
        std::fs::read(folder.join(MANIFEST_FILE)).map_err(|error| unreadable(folder, &error))?;
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|error| unreadable(folder, &error))?;
    Ok(UploadMarker::new(
        &manifest,
        &bytes,
        robot,
        env!("CARGO_PKG_VERSION"),
        uploaded_utc_ns,
    ))
}

fn compare(there: UploadMarker, ours: &UploadMarker) -> Result<Uploaded, KaptoError> {
    if there.same_files(ours) {
        Ok(Uploaded::AlreadyThere(there))
    } else {
        Err(
            KaptoError::new(ErrorCode::UploadMarkerConflict).detail(format!(
                "episode `{}` is there already with other files",
                ours.episode
            )),
        )
    }
}

async fn existing_marker(
    store: Store<'_>,
    at: &Path,
    tries: &Tries<'_>,
) -> Result<Option<UploadMarker>, KaptoError> {
    let got = tries
        .run(|| async { store.objects.get(at).await?.bytes().await })
        .await;
    match got {
        Ok(bytes) => UploadMarker::parse(&bytes).map(Some),
        Err(Error::NotFound { .. }) => Ok(None),
        Err(error) => Err(store_error(at, &error)),
    }
}

async fn upload_file(
    store: Store<'_>,
    folder: &std::path::Path,
    file: &FileEntry,
    at: &Path,
    tries: &Tries<'_>,
    progress: &mut Progress,
    save: &mut (dyn FnMut(&Progress) + Send),
) -> Result<(), KaptoError> {
    let local = folder.join(&file.path);
    let carried = match progress.files.remove(&file.path) {
        Some(FileProgress::Parts {
            upload_id,
            part_bytes,
            parts,
        }) => Some((upload_id, part_bytes, parts)),
        _ => None,
    };
    let part = carried
        .as_ref()
        .map(|(_, part_bytes, _)| *part_bytes)
        .filter(|part_bytes| *part_bytes > 0)
        .unwrap_or_else(|| part_size(tries.policy, file.bytes));
    let read = |index: u64| read_part(&local, index * part, part, file.bytes);
    let Some(multipart) = store.multipart.filter(|_| file.bytes > part) else {
        if file.bytes <= part {
            let payload = PutPayload::from(read(0)?);
            tries.paced(file.bytes).await;
            return tries
                .run(|| store.objects.put(at, payload.clone()))
                .await
                .map(|_| ())
                .map_err(|error| store_error(at, &error));
        }
        return stream_whole(store.objects, at, file, part, &read, tries).await;
    };

    let count = file.bytes.div_ceil(part);
    let (upload_id, mut parts) = if let Some((upload_id, _, parts)) = carried {
        (upload_id, parts)
    } else {
        let id = tries
            .run(|| multipart.create_multipart(at))
            .await
            .map_err(|error| store_error(at, &error))?;
        (id, BTreeMap::new())
    };
    let saved = |parts: &BTreeMap<u64, String>, progress: &mut Progress| {
        progress.files.insert(
            file.path.clone(),
            FileProgress::Parts {
                upload_id: upload_id.clone(),
                part_bytes: part,
                parts: parts.clone(),
            },
        );
    };
    saved(&parts, progress);
    tries.count(progress);
    save(progress);
    // As many at once as the window allows, each read only once it may go,
    // so memory stays at that many parts; each saved as it comes back.
    let missing: Vec<u64> = (0..count)
        .filter(|index| !parts.contains_key(index))
        .collect();
    let put = |index: u64| {
        let (read, upload_id) = (&read, &upload_id);
        async move {
            let payload = PutPayload::from(read(index)?);
            tries.paced(bytes(payload.content_length())).await;
            let place = usize::try_from(index).unwrap_or(usize::MAX);
            let id = tries
                .run(|| multipart.put_part(at, upload_id, place, payload.clone()))
                .await
                .map_err(|error| store_error(at, &error))?;
            Ok::<_, KaptoError>((index, id.content_id))
        }
    };
    let mut missing = missing.into_iter();
    let mut in_flight = FuturesUnordered::new();
    loop {
        while in_flight.len() < lock(&tries.window).limit() {
            let Some(index) = missing.next() else { break };
            in_flight.push(put(index));
        }
        let Some(done) = in_flight.next().await else {
            break;
        };
        let (index, content_id) = done?;
        parts.insert(index, content_id);
        saved(&parts, progress);
        tries.count(progress);
        save(progress);
    }
    drop(in_flight);
    let ids: Vec<PartId> = parts
        .values()
        .map(|content_id| PartId {
            content_id: content_id.clone(),
        })
        .collect();
    tries
        .run(|| multipart.complete_multipart(at, &upload_id, ids.clone()))
        .await
        .map(|_| ())
        .map_err(|error| store_error(at, &error))
}

/// The size of the parts a file of `bytes` goes up in: the policy's, or
/// larger if the store wouldn't take that many.
fn part_size(policy: &Policy, bytes: u64) -> u64 {
    let part = u64::try_from(policy.part_bytes).unwrap_or(u64::MAX).max(1);
    part.max(bytes.div_ceil(policy.most_parts.max(1)))
}

/// A large file to a store without parts that survive a restart: one
/// stream of parts, all sent again if any try fails.
async fn stream_whole(
    store: &dyn ObjectStore,
    at: &Path,
    file: &FileEntry,
    part: u64,
    read: &dyn Fn(u64) -> Result<Vec<u8>, KaptoError>,
    tries: &Tries<'_>,
) -> Result<(), KaptoError> {
    let (mut attempt, mut throttles) = (0, 0);
    loop {
        let sent_at = lock(&tries.window).generation();
        let sent: Result<(), Error> = async {
            let mut upload = store.put_multipart(at).await?;
            for index in 0..file.bytes.div_ceil(part) {
                let data = read(index).map_err(|error| Error::Generic {
                    store: "local",
                    source: error.message().into(),
                })?;
                tries.paced(bytes(data.len())).await;
                upload.put_part(PutPayload::from(data)).await?;
            }
            upload.complete().await.map(|_| ())
        }
        .await;
        match sent {
            Ok(()) => return Ok(()),
            Err(error) if throttled(&error) => {
                tries.throttled(sent_at, &mut throttles).await;
            }
            Err(error) if retryable(&error) && attempt + 1 < tries.policy.max_attempts => {
                tries.wait(attempt).await;
                attempt += 1;
            }
            Err(error) => return Err(store_error(at, &error)),
        }
    }
}

fn bytes(length: usize) -> u64 {
    u64::try_from(length).unwrap_or(u64::MAX)
}

fn read_part(
    file: &std::path::Path,
    offset: u64,
    part: u64,
    size: u64,
) -> Result<Vec<u8>, KaptoError> {
    let length = usize::try_from(part.min(size.saturating_sub(offset))).unwrap_or(usize::MAX);
    let mut data = vec![0; length];
    let mut opened = File::open(file).map_err(|error| unreadable(file, &error))?;
    opened
        .seek(SeekFrom::Start(offset))
        .and_then(|_| opened.read_exact(&mut data))
        .map_err(|error| unreadable(file, &error))?;
    Ok(data)
}

/// Checks the file at `at` is `file`, returning the store's tag of it.
async fn verify(
    store: Store<'_>,
    file: &FileEntry,
    at: &Path,
    tries: &Tries<'_>,
) -> Result<Option<String>, KaptoError> {
    let meta = tries
        .run(|| store.objects.head(at))
        .await
        .map_err(|error| store_error(at, &error))?;
    if meta.size != file.bytes {
        return Err(mismatch(
            at,
            &format!("{} bytes, not {}", meta.size, file.bytes),
        ));
    }
    let read_back = match tries.policy.verify {
        Verify::SizeOnly => false,
        Verify::ReadBack => true,
        Verify::ServerChecksum => !tries.policy.store_checks_sha256,
    };
    if read_back {
        let sha256 = tries
            .run(|| async {
                let mut stream = store.objects.get(at).await?.into_stream();
                let mut hasher = Sha256::new();
                while let Some(chunk) = stream.next().await {
                    hasher.update(chunk?);
                }
                Ok(hex(&hasher.finalize()))
            })
            .await
            .map_err(|error| store_error(at, &error))?;
        if sha256 != file.sha256 {
            return Err(mismatch(at, "another SHA-256"));
        }
    }
    Ok(meta.e_tag)
}

/// Runs requests, trying each again after a wait while the store can't be
/// reached.
/// Shared by the parts in flight: each lock is held only to work out a
/// wait, never across one.
struct Tries<'a> {
    policy: &'a Policy,
    backoff: Mutex<&'a mut Backoff>,
    sleep: &'a dyn Sleep,
    pace: Mutex<Pace>,
    window: Mutex<Window>,
    /// Requests throttled, those of earlier tries included.
    throttled: AtomicU64,
}

impl Tries<'_> {
    async fn run<T, F, Fut>(&self, mut request: F) -> Result<T, Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        let (mut attempt, mut throttles) = (0, 0);
        loop {
            let sent_at = lock(&self.window).generation();
            match request().await {
                // Not a failure: the store asked for fewer requests.
                Err(error) if throttled(&error) => self.throttled(sent_at, &mut throttles).await,
                Err(error) if retryable(&error) && attempt + 1 < self.policy.max_attempts => {
                    self.wait(attempt).await;
                    attempt += 1;
                }
                result => {
                    if result.is_ok() {
                        lock(&self.window).succeeded();
                    }
                    return result;
                }
            }
        }
    }

    /// A request sent at `sent_at` was throttled, the `throttles`-th time
    /// running: fewer in flight, and a wait growing with each.
    async fn throttled(&self, sent_at: u64, throttles: &mut u32) {
        lock(&self.window).throttled(sent_at);
        self.throttled.fetch_add(1, Ordering::Relaxed);
        self.wait(*throttles).await;
        *throttles = throttles.saturating_add(1);
    }

    /// Writes how many requests were throttled into `progress`, to save.
    fn count(&self, progress: &mut Progress) {
        progress.throttled = self.throttled.load(Ordering::Relaxed);
    }

    async fn wait(&self, attempt: u32) {
        let time = lock(&self.backoff).wait(attempt);
        self.sleep.sleep(time).await;
    }

    /// Waits until `bytes` may be sent under the cap.
    async fn paced(&self, bytes: u64) {
        let wait = lock(&self.pace).wait(bytes, self.sleep.now());
        if !wait.is_zero() {
            self.sleep.sleep(wait).await;
        }
    }
}

/// A lock nothing panics while holding; one poisoned anyway is used as it
/// was left, a wait being all it guards.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether trying again may help: the store couldn't be reached or answered
/// that it couldn't serve, which `object_store` gives as its generic error.
fn retryable(error: &Error) -> bool {
    matches!(error, Error::Generic { .. } | Error::JoinError { .. })
}

pub(crate) fn store_error(at: &Path, error: &Error) -> KaptoError {
    let code = match &error {
        Error::Generic { .. } | Error::JoinError { .. } => ErrorCode::UploadUnreachable,
        Error::PermissionDenied { .. } | Error::Unauthenticated { .. } => ErrorCode::UploadDenied,
        _ => ErrorCode::UploadFailed,
    };
    KaptoError::new(code)
        .detail(error.to_string())
        .context("key", at.to_string())
}

fn failed(at: &Path, detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::UploadFailed)
        .detail(detail.to_owned())
        .context("key", at.to_string())
}

fn mismatch(at: &Path, detail: &str) -> KaptoError {
    KaptoError::new(ErrorCode::UploadVerifyFailed)
        .detail(format!("the file there has {detail}"))
        .context("key", at.to_string())
}

fn unreadable(path: &std::path::Path, error: &dyn std::fmt::Display) -> KaptoError {
    KaptoError::new(ErrorCode::UploadEpisodeUnreadable)
        .detail(error.to_string())
        .context("path", path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use kapto_core::EpisodeId;
    use object_store::MultipartId;
    use object_store::memory::InMemory;

    use super::*;
    use crate::faulty::Faulty;

    const EPISODE: &str = "20260323T073012.345Z-5f729d2c";

    /// An episode folder: a recording of three parts of 16 bytes (the last
    /// shorter), a small summary, and their manifest.
    fn episode(test: &str) -> std::path::PathBuf {
        let folder = kapto_testkit::scratch(test);
        let recording: Vec<u8> = (0..40).collect();
        std::fs::write(folder.join("recording.mcap"), recording).unwrap();
        std::fs::write(folder.join("episode.json"), b"{}\n").unwrap();
        let id: EpisodeId = EPISODE.parse().unwrap();
        Manifest::of_directory(&folder, &id)
            .unwrap()
            .write(&folder)
            .unwrap();
        folder
    }

    /// Gives the other futures a turn: wakes itself at once and is ready
    /// when polled again.
    async fn yield_once() {
        let mut yielded = false;
        std::future::poll_fn(|context| {
            if yielded {
                std::task::Poll::Ready(())
            } else {
                yielded = true;
                context.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        })
        .await;
    }

    /// Records each wait instead of waiting, its clock moved on only by
    /// the waits: everything else takes no time.
    #[derive(Default)]
    struct Waits(Mutex<Vec<Duration>>);

    impl Sleep for Waits {
        fn sleep(&self, time: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.0.lock().unwrap().push(time);
            // Gives the other requests in flight a turn per millisecond, as
            // a real wait does: a request through the test stores takes a
            // few.
            let turns = time.as_millis().max(1);
            Box::pin(async move {
                for _ in 0..turns {
                    yield_once().await;
                }
            })
        }

        fn now(&self) -> Duration {
            self.0.lock().unwrap().iter().sum()
        }
    }

    const POLICY: Policy = Policy {
        part_bytes: 16,
        most_parts: 10_000,
        parallel_parts: 3,
        max_in_flight: 3,
        max_attempts: 3,
        verify: Verify::ReadBack,
        store_checks_sha256: false,
        marker: true,
        bandwidth: None,
    };

    /// Uploads the episode in `folder` to `store` under `robot/episode`,
    /// with every progress saved: the outcome, the saves and the waits.
    async fn upload(
        store: Store<'_>,
        folder: &std::path::Path,
        policy: &Policy,
        progress: &mut Progress,
    ) -> (Result<Uploaded, KaptoError>, Vec<Progress>, Vec<Duration>) {
        upload_between(store, folder, policy, progress, &mut || Ok(())).await
    }

    /// As [`upload`], asking `between` before each file and the marker.
    async fn upload_between(
        store: Store<'_>,
        folder: &std::path::Path,
        policy: &Policy,
        progress: &mut Progress,
        between: &mut (dyn FnMut() -> Result<(), KaptoError> + Send),
    ) -> (Result<Uploaded, KaptoError>, Vec<Progress>, Vec<Duration>) {
        let marker = marker_of(folder, Some("arm7".into()), 7).unwrap();
        let waits = Waits::default();
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_secs(1), 1);
        let mut saves = Vec::new();
        let outcome = upload_episode(
            store,
            folder,
            &Path::from("robot/episode"),
            marker,
            policy,
            &mut backoff,
            &waits,
            progress,
            &mut |progress: &Progress| saves.push(progress.clone()),
            between,
        )
        .await;
        (outcome, saves, waits.0.into_inner().unwrap())
    }

    async fn object(store: &dyn ObjectStore, name: &str) -> Option<Vec<u8>> {
        let at = Path::from("robot/episode").join(name);
        Some(store.get(&at).await.ok()?.bytes().await.ok()?.to_vec())
    }

    #[tokio::test]
    async fn every_file_goes_up_and_is_checked_then_the_marker_last() {
        let folder = episode("upload-whole");
        let memory = InMemory::new();
        for multipart in [Some(&memory as &dyn MultipartStore), None] {
            let store = Store {
                objects: &memory,
                multipart,
            };
            memory
                .delete(&Path::from("robot/episode/_SUCCESS"))
                .await
                .unwrap();
            let mut progress = Progress::default();
            let (outcome, saves, waits) = upload(store, &folder, &POLICY, &mut progress).await;
            let Ok(Uploaded::Now(marker)) = outcome else {
                panic!("{outcome:?}");
            };
            for file in ["recording.mcap", "episode.json", "manifest.json"] {
                let sent = std::fs::read(folder.join(file)).unwrap();
                assert_eq!(object(&memory, file).await.unwrap(), sent, "{file}");
            }
            let there = object(&memory, SUCCESS_FILE).await.unwrap();
            assert_eq!(UploadMarker::parse(&there).unwrap(), marker);
            assert!(progress.done && waits.is_empty());
            assert!(
                saves.iter().rev().skip(1).all(|save| !save.done),
                "done only last"
            );
            // Parts that survive a restart only where the store has them.
            let in_parts = saves.iter().any(|save| {
                matches!(
                    save.files.get("recording.mcap"),
                    Some(FileProgress::Parts { .. })
                )
            });
            assert_eq!(in_parts, multipart.is_some());
        }
    }

    #[tokio::test]
    async fn an_episode_already_there_is_left_as_it_is_and_another_one_there_is_a_conflict() {
        let folder = episode("upload-twice");
        let memory = InMemory::new();
        let store = Store {
            objects: &memory,
            multipart: Some(&memory),
        };
        upload(store, &folder, &POLICY, &mut Progress::default())
            .await
            .0
            .unwrap();
        let first = object(&memory, SUCCESS_FILE).await.unwrap();
        let (again, _, _) = upload(store, &folder, &POLICY, &mut Progress::default()).await;
        assert!(matches!(again, Ok(Uploaded::AlreadyThere(_))), "{again:?}");
        assert_eq!(object(&memory, SUCCESS_FILE).await.unwrap(), first);

        std::fs::write(folder.join("episode.json"), b"{\"changed\":1}\n").unwrap();
        let id: EpisodeId = EPISODE.parse().unwrap();
        Manifest::of_directory(&folder, &id)
            .unwrap()
            .write(&folder)
            .unwrap();
        let (other, _, _) = upload(store, &folder, &POLICY, &mut Progress::default()).await;
        assert_eq!(other.unwrap_err().code(), ErrorCode::UploadMarkerConflict);
        assert_eq!(
            object(&memory, SUCCESS_FILE).await.unwrap(),
            first,
            "never overwritten"
        );
        assert_eq!(object(&memory, "episode.json").await.unwrap(), b"{}\n");
    }

    fn unreachable() -> Error {
        Error::Generic {
            store: "test",
            source: "connection reset".into(),
        }
    }

    #[tokio::test]
    async fn a_store_out_of_reach_is_tried_again_after_waits_then_given_up_on() {
        let folder = episode("upload-unreachable");
        let faulty = Faulty::new(InMemory::new());
        let store = Store {
            objects: &faulty,
            multipart: Some(&faulty),
        };
        faulty.fail_next(unreachable());
        faulty.fail_next(unreachable());
        let (outcome, _, waits) = upload(store, &folder, &POLICY, &mut Progress::default()).await;
        assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
        assert_eq!(waits.len(), 2);
        assert!(waits[0] <= Duration::from_millis(100) && waits[1] <= Duration::from_millis(200));

        let folder = episode("upload-gone");
        let faulty = Faulty::new(InMemory::new());
        let store = Store {
            objects: &faulty,
            multipart: Some(&faulty),
        };
        for _ in 0..3 {
            faulty.fail_next(unreachable());
        }
        let (outcome, _, waits) = upload(store, &folder, &POLICY, &mut Progress::default()).await;
        assert_eq!(outcome.unwrap_err().code(), ErrorCode::UploadUnreachable);
        assert_eq!(waits.len(), 2, "max_attempts tries, two waits between");
        assert_eq!(object(&faulty, SUCCESS_FILE).await, None);
    }

    #[tokio::test]
    async fn refused_credentials_are_not_tried_again() {
        let folder = episode("upload-denied");
        let faulty = Faulty::new(InMemory::new());
        let store = Store {
            objects: &faulty,
            multipart: Some(&faulty),
        };
        faulty.fail_next(Error::PermissionDenied {
            path: "robot".into(),
            source: "access denied".into(),
        });
        let (outcome, _, waits) = upload(store, &folder, &POLICY, &mut Progress::default()).await;
        assert_eq!(outcome.unwrap_err().code(), ErrorCode::UploadDenied);
        assert_eq!(waits, []);
    }

    #[tokio::test]
    async fn a_file_stored_corrupted_fails_verification_and_no_marker_is_written() {
        let folder = episode("upload-corrupted");
        for (verify, store_checks_sha256, caught) in [
            (Verify::ReadBack, false, true),
            (Verify::ServerChecksum, false, true),
            // The size is the same: only reading back notices.
            (Verify::SizeOnly, false, false),
        ] {
            let faulty = Faulty::new(InMemory::new());
            faulty.corrupt_puts_to(Path::from("robot/episode/episode.json"));
            let store = Store {
                objects: &faulty,
                multipart: Some(&faulty),
            };
            let policy = Policy {
                verify,
                store_checks_sha256,
                ..POLICY
            };
            let mut progress = Progress::default();
            let (outcome, _, _) = upload(store, &folder, &policy, &mut progress).await;
            if caught {
                let error = outcome.unwrap_err();
                assert_eq!(error.code(), ErrorCode::UploadVerifyFailed, "{verify:?}");
                assert!(
                    error.message().contains("another SHA-256"),
                    "{}",
                    error.message()
                );
                assert_eq!(object(&faulty, SUCCESS_FILE).await, None, "{verify:?}");
                assert!(!progress.done);
                assert!(!matches!(
                    progress.files.get("episode.json"),
                    Some(FileProgress::Verified { .. })
                ));
            } else {
                assert!(outcome.is_ok(), "{verify:?}: {outcome:?}");
            }
        }
    }

    #[tokio::test]
    async fn an_upload_given_its_progress_back_sends_no_part_twice() {
        let folder = episode("upload-resumed");
        let memory = InMemory::new();
        let at = Path::from("robot/episode/recording.mcap");
        // Part 0 went up before a restart; if it were sent again, the
        // recording would start with the file's own bytes instead.
        let upload_id = memory.create_multipart(&at).await.unwrap();
        let sent_before = PutPayload::from(vec![0xEE; 16]);
        let part = memory
            .put_part(&at, &upload_id, 0, sent_before)
            .await
            .unwrap();
        let mut progress = Progress::default();
        progress.files.insert(
            "recording.mcap".into(),
            FileProgress::Parts {
                upload_id,
                part_bytes: 16,
                parts: BTreeMap::from([(0, part.content_id)]),
            },
        );
        let store = Store {
            objects: &memory,
            multipart: Some(&memory),
        };
        let policy = Policy {
            verify: Verify::SizeOnly,
            ..POLICY
        };
        upload(store, &folder, &policy, &mut progress)
            .await
            .0
            .unwrap();
        let recording = object(&memory, "recording.mcap").await.unwrap();
        assert_eq!(recording[..16], [0xEE; 16]);
        assert_eq!(recording[16..], (16..40).collect::<Vec<u8>>());
    }

    #[tokio::test]
    async fn an_upload_stopped_between_files_carries_on_from_the_next_one() {
        let folder = episode("upload-stopped");
        let memory = InMemory::new();
        let store = Store {
            objects: &memory,
            multipart: Some(&memory),
        };
        let mut progress = Progress::default();
        let mut asked = 0;
        let mut stop_at_second = || {
            asked += 1;
            if asked == 2 {
                Err(KaptoError::new(ErrorCode::WorkerJobStopped))
            } else {
                Ok(())
            }
        };
        let (outcome, _, _) =
            upload_between(store, &folder, &POLICY, &mut progress, &mut stop_at_second).await;
        assert_eq!(outcome.unwrap_err().code(), ErrorCode::WorkerJobStopped);
        let verified: Vec<_> = progress
            .files
            .iter()
            .filter(|(_, file)| matches!(file, FileProgress::Verified { .. }))
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(
            verified.len(),
            1,
            "only the file before the stop: {verified:?}"
        );
        assert_eq!(object(&memory, SUCCESS_FILE).await, None);

        // Given its progress back, it is asked only for the files left and
        // the marker.
        let mut asked_again = 0;
        let (outcome, _, _) = upload_between(store, &folder, &POLICY, &mut progress, &mut || {
            asked_again += 1;
            Ok(())
        })
        .await;
        assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
        assert_eq!(asked_again, 3, "the two files left, then the marker");
    }

    #[tokio::test]
    async fn under_a_bandwidth_cap_each_request_waits_for_those_before_it_at_the_cap() {
        let folder = episode("upload-capped");
        let memory = InMemory::new();
        let store = Store {
            objects: &memory,
            multipart: Some(&memory),
        };
        let policy = Policy {
            bandwidth: NonZeroU64::new(16),
            ..POLICY
        };
        let (outcome, _, waits) = upload(store, &folder, &policy, &mut Progress::default()).await;
        assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
        // Everything sent: every object there, the marker last.
        let sent: u64 = futures::TryStreamExt::try_collect::<Vec<_>>(memory.list(None))
            .await
            .unwrap()
            .iter()
            .map(|object| object.size)
            .sum();
        let marker = u64::try_from(object(&memory, SUCCESS_FILE).await.unwrap().len()).unwrap();
        // On a clock moved only by the waits, the marker went once all
        // before it would have at 16 bytes a second.
        let waited: Duration = waits.iter().sum();
        assert_eq!(
            waited,
            Duration::from_nanos((sent - marker) * 1_000_000_000 / 16)
        );
        assert!(
            waits.len() > 3,
            "a wait before each request but the first: {waits:?}"
        );
    }

    /// Parts put through it come back late, the first ones latest, counting
    /// how many are in flight at most.
    struct Slow {
        store: InMemory,
        in_flight: std::sync::atomic::AtomicUsize,
        most: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MultipartStore for Slow {
        async fn create_multipart(&self, path: &Path) -> object_store::Result<MultipartId> {
            self.store.create_multipart(path).await
        }

        async fn put_part(
            &self,
            path: &Path,
            id: &MultipartId,
            part_idx: usize,
            data: PutPayload,
        ) -> object_store::Result<PartId> {
            use std::sync::atomic::Ordering::SeqCst;
            let now = self.in_flight.fetch_add(1, SeqCst) + 1;
            self.most.fetch_max(now, SeqCst);
            for _ in 0..(8usize.saturating_sub(part_idx)) {
                yield_once().await;
            }
            self.in_flight.fetch_sub(1, SeqCst);
            self.store.put_part(path, id, part_idx, data).await
        }

        async fn complete_multipart(
            &self,
            path: &Path,
            id: &MultipartId,
            parts: Vec<PartId>,
        ) -> object_store::Result<object_store::PutResult> {
            self.store.complete_multipart(path, id, parts).await
        }

        async fn abort_multipart(&self, path: &Path, id: &MultipartId) -> object_store::Result<()> {
            self.store.abort_multipart(path, id).await
        }
    }

    #[tokio::test]
    async fn parts_go_up_several_at_once_and_back_out_of_order_make_the_file_as_it_was() {
        let folder = episode("upload-parallel");
        // Ten parts of 4 bytes.
        let policy = Policy {
            part_bytes: 4,
            ..POLICY
        };
        let slow = Slow {
            store: InMemory::new(),
            in_flight: 0.into(),
            most: 0.into(),
        };
        let store = Store {
            objects: &slow.store,
            multipart: Some(&slow),
        };
        let mut progress = Progress::default();
        let (outcome, saves, _) = upload(store, &folder, &policy, &mut progress).await;
        assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
        let recording = object(&slow.store, "recording.mcap").await.unwrap();
        assert_eq!(recording, (0..40).collect::<Vec<u8>>());
        assert_eq!(
            slow.most.into_inner(),
            3,
            "parallel_parts at once, never more"
        );
        // Saved as each came back: a later part before an earlier one.
        let first_saved = saves
            .iter()
            .find_map(|save| match save.files.get("recording.mcap") {
                Some(FileProgress::Parts { parts, .. }) if !parts.is_empty() => {
                    parts.keys().next().copied()
                }
                _ => None,
            });
        assert_ne!(first_saved, Some(0), "{saves:?}");
    }

    #[tokio::test]
    async fn a_file_needing_more_parts_than_the_store_takes_goes_up_in_larger_ones() {
        let folder = episode("upload-part-limit");
        let memory = InMemory::new();
        let store = Store {
            objects: &memory,
            multipart: Some(&memory),
        };
        // 40 bytes in parts of 16 would be three; the store takes two.
        let policy = Policy {
            most_parts: 2,
            ..POLICY
        };
        let (outcome, saves, _) = upload(store, &folder, &policy, &mut Progress::default()).await;
        assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
        let parts = saves
            .iter()
            .rev()
            .find_map(|save| match save.files.get("recording.mcap") {
                Some(FileProgress::Parts {
                    part_bytes, parts, ..
                }) => Some((*part_bytes, parts.len())),
                _ => None,
            });
        assert_eq!(parts, Some((20, 2)));
        let recording = object(&memory, "recording.mcap").await.unwrap();
        assert_eq!(recording, (0..40).collect::<Vec<u8>>());
    }

    /// Throttles any part put through it beyond `most` in flight, as S3
    /// words it; counts what it took and what it throttled.
    struct Throttling {
        store: InMemory,
        most: usize,
        in_flight: std::sync::atomic::AtomicUsize,
        throttled: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MultipartStore for Throttling {
        async fn create_multipart(&self, path: &Path) -> object_store::Result<MultipartId> {
            self.store.create_multipart(path).await
        }

        async fn put_part(
            &self,
            path: &Path,
            id: &MultipartId,
            part_idx: usize,
            data: PutPayload,
        ) -> object_store::Result<PartId> {
            use std::sync::atomic::Ordering::SeqCst;
            if self.in_flight.fetch_add(1, SeqCst) >= self.most {
                self.in_flight.fetch_sub(1, SeqCst);
                self.throttled.fetch_add(1, SeqCst);
                return Err(Error::Generic {
                    store: "S3",
                    source: "Error performing PUT: Server returned non-2xx status code: \
                             503 Service Unavailable: <Error><Code>SlowDown</Code></Error>"
                        .into(),
                });
            }
            for _ in 0..4 {
                yield_once().await;
            }
            self.in_flight.fetch_sub(1, SeqCst);
            self.store.put_part(path, id, part_idx, data).await
        }

        async fn complete_multipart(
            &self,
            path: &Path,
            id: &MultipartId,
            parts: Vec<PartId>,
        ) -> object_store::Result<object_store::PutResult> {
            self.store.complete_multipart(path, id, parts).await
        }

        async fn abort_multipart(&self, path: &Path, id: &MultipartId) -> object_store::Result<()> {
            self.store.abort_multipart(path, id).await
        }
    }

    #[tokio::test]
    async fn a_store_throttling_above_some_requests_has_the_engine_settle_below_without_failing() {
        let folder = episode("upload-throttled");
        let throttling = Throttling {
            store: InMemory::new(),
            most: 4,
            in_flight: 0.into(),
            throttled: 0.into(),
        };
        let store = Store {
            objects: &throttling.store,
            multipart: Some(&throttling),
        };
        // Every file in parts of a byte, starting two at a time, allowed up
        // to 32; a single failed try would fail the upload.
        let policy = Policy {
            part_bytes: 1,
            parallel_parts: 2,
            max_in_flight: 32,
            max_attempts: 1,
            ..POLICY
        };
        let mut progress = Progress::default();
        let (outcome, _, waits) = upload(store, &folder, &policy, &mut progress).await;
        assert!(matches!(outcome, Ok(Uploaded::Now(_))), "{outcome:?}");
        let recording = object(&throttling.store, "recording.mcap").await.unwrap();
        assert_eq!(recording, (0..40).collect::<Vec<u8>>());
        // It grew past what the store takes, was throttled, and each time
        // fell back below it: throttled now and then, not part after part.
        let parts: u64 = futures::TryStreamExt::try_collect::<Vec<_>>(throttling.store.list(None))
            .await
            .unwrap()
            .iter()
            .filter(|object| object.location.filename() != Some(SUCCESS_FILE))
            .map(|object| object.size)
            .sum();
        let throttled = throttling.throttled.into_inner();
        assert!(throttled > 0, "it never grew past what the store takes");
        assert_eq!(
            progress.throttled,
            u64::try_from(throttled).unwrap(),
            "counted, to show"
        );
        assert!(
            u64::try_from(throttled).unwrap() * 5 <= parts,
            "throttled {throttled} times in {parts} parts"
        );
        assert_eq!(waits.len(), throttled, "a wait after each throttled try");
    }

    #[test]
    fn the_bytes_sent_count_each_file_verified_whole_and_each_part_sent() {
        let file = |path: &str, bytes| FileEntry {
            path: path.to_owned(),
            bytes,
            sha256: String::new(),
        };
        let files = [
            file("recording.mcap", 40),
            file("episode.json", 3),
            file("manifest.json", 9),
        ];
        let mut progress = Progress::default();
        assert_eq!(progress.sent_bytes(&files), 0);
        progress.files.insert(
            "episode.json".into(),
            FileProgress::Verified { e_tag: None },
        );
        // Two parts of 16 of the recording, then all three, the last short.
        let parts = |count: u64| FileProgress::Parts {
            upload_id: "u".into(),
            part_bytes: 16,
            parts: (0..count)
                .map(|index| (index, format!("p{index}")))
                .collect(),
        };
        progress.files.insert("recording.mcap".into(), parts(2));
        assert_eq!(progress.sent_bytes(&files), 3 + 32);
        progress.files.insert("recording.mcap".into(), parts(3));
        assert_eq!(progress.sent_bytes(&files), 3 + 40);
    }
}
