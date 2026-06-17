// SPDX-License-Identifier: Apache-2.0

//! The replay cache: each recorded episode's replay, made the first time
//! someone asks for it, kept in the data directory's `.cache/replay` as
//! `<episode ID>.view`, the replay view the web UI's player reads. It is
//! outside every episode's folder, so an episode stays exactly what its
//! manifest lists. A replay is made once its view is there, of the version
//! this Kapto writes: one of another version is made again when asked for.
//!
//! Anything in the cache can go at any time: a replay is made again when
//! next asked for. Kapto empties it itself ([`trim`]), so it never grows
//! without bound and never takes the room a recording needs. A replay's
//! modification time is when it was last opened: making it sets it, and so
//! does opening it.

use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kapto_core::{ErrorCode, KaptoError};
use kapto_store::manifest::FINALISED_FILE;
use kapto_store::replay_view::{VIEW_VERSION, view_version};
use kapto_store::{Job, JobFailure, JobState, Reader, Writer, episode_folder};
use kapto_viz::history::MAX_POINTS;
use kapto_viz::page_layout::{RobotRole, ViewLayout};
use kapto_viz::page_scene::RobotScene;
use kapto_viz::view::View;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::ApiError;
use crate::episode::finalised_folder;
use crate::jobs::REPLAY;

/// The cache's folder in the data directory.
pub const REPLAY_CACHE: &str = ".cache/replay";

/// A replay's file's name after its episode's ID.
const EXTENSION: &str = ".view";

/// What a replay's file being written ends with until it is complete.
const UNFINISHED: &str = ".partial";

/// Where the replay view of episode `id` is kept in `data_dir`'s cache, or
/// `None` if `id` isn't an episode ID: a request can never name a file
/// elsewhere.
#[must_use]
pub fn view_file(data_dir: &Path, id: &str) -> Option<PathBuf> {
    episode_folder(data_dir, id)?;
    Some(data_dir.join(REPLAY_CACHE).join(format!("{id}{EXTENSION}")))
}

/// Where an episode's replay is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplayState {
    /// Never asked for, or asked for and since removed from the cache.
    NotMade,
    /// Asked for, waiting for the worker.
    Queued,
    /// Being made.
    Making,
    /// Made: its stretches can be asked for.
    Ready,
    /// Making it failed; asking again tries again.
    Failed,
}

/// An episode's replay: whether it is made, and what it lacks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReplayStatus {
    /// The episode's ID.
    pub episode: String,
    /// Where it is.
    pub state: ReplayState,
    /// Its size in bytes once ready.
    pub bytes: Option<u64>,
    /// Why making it failed.
    pub failure: Option<ApiError>,
    /// What it lacks, and why, once ready: the robot, or some of its meshes.
    pub lacks: Vec<ApiError>,
}

/// Episode `id`'s replay in `data_dir`, as far as the worker got with it,
/// from `store`'s job queue.
///
/// # Errors
///
/// `api.invalid_request` if `id` isn't an episode ID; `api.episode_not_found`
/// if no finalised episode has it; `store.query_failed` if the queue can't
/// be read.
pub fn status(data_dir: &Path, store: &Reader, id: &str) -> Result<ReplayStatus, KaptoError> {
    finalised_folder(data_dir, id)?;
    Ok(described(data_dir, id, store.job(REPLAY, id)?))
}

/// Asks for episode `id`'s replay: queues the worker's `replay` job unless
/// the replay is made, queued or being made. One that failed, or was made
/// and has since left the cache, is made again.
///
/// # Errors
///
/// As [`status`].
pub fn ask(data_dir: &Path, store: &Writer, id: &str) -> Result<ReplayStatus, KaptoError> {
    let status = status(data_dir, &store.reader()?, id)?;
    if matches!(status.state, ReplayState::NotMade | ReplayState::Failed) {
        let job = store.enqueue_again(REPLAY, id)?;
        return Ok(described(data_dir, id, Some(job)));
    }
    Ok(status)
}

/// What a replay shows, for the page to name what each stretch of it
/// holds, as the live stream's opening message does for the live view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReplayOpening {
    /// When its first message was received, UTC nanoseconds.
    pub start_utc_ns: i64,
    /// When its last was.
    pub end_utc_ns: i64,
    /// Each link's name, in the order a stretch gives its pose.
    pub links: Vec<String>,
    /// Each stream, by its number in a stretch, with its values.
    pub streams: Vec<ReplayStream>,
    /// The streams that are cameras, by name, in declaration order: their
    /// frames come from `/api/episodes/{id}/cameras/{camera}/frames`. Empty
    /// if the files the recording carries can't be read.
    pub cameras: Vec<String>,
    /// How the page arranges it: the layout the recording carries, or one
    /// made from what it declares, each panel with what the recording
    /// lacks; absent for a replay made before replays had one.
    pub layout: Option<ViewLayout>,
    /// Each role the recording's mapping gives a value.
    pub roles: Vec<RobotRole>,
}

/// One stream of a replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReplayStream {
    /// Its ID.
    pub name: String,
    /// Each decoded value, in the order a stretch gives their extremes.
    pub values: Vec<ReplayValue>,
}

/// One decoded value of a replay's stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReplayValue {
    /// Its name: the field's, or `field[index]` for an array's.
    pub name: String,
    /// Whether it is plotted; counters and clocks aren't.
    pub plotted: bool,
}

/// What episode `id`'s replay shows, from its replay view.
///
/// # Errors
///
/// As [`status`]; `viewer.replay_not_ready` if it isn't made;
/// `viewer.replay_unreadable` if its view can't be read.
pub fn opening(data_dir: &Path, id: &str) -> Result<ReplayOpening, KaptoError> {
    finalised_folder(data_dir, id)?;
    let view = View::open(&current_view(data_dir, id)?)?;
    let about = view.about();
    Ok(ReplayOpening {
        start_utc_ns: about.start_utc_ns,
        end_utc_ns: about.end_utc_ns,
        links: about.links.clone(),
        streams: about
            .streams
            .iter()
            .map(|stream| ReplayStream {
                name: stream.name.clone(),
                values: stream
                    .values
                    .iter()
                    .map(|value| ReplayValue {
                        name: value.name.clone(),
                        plotted: value.plotted,
                    })
                    .collect(),
            })
            .collect(),
        // A recording whose cameras can't be listed still replays its
        // robot; asking for a camera's frames says what is wrong.
        cameras: crate::cameras::cameras(data_dir, id).unwrap_or_default(),
        layout: about.layout.clone(),
        roles: about.roles.clone(),
    })
}

/// The robot episode `id` was recorded with, as the page draws it, from its
/// replay view.
///
/// # Errors
///
/// As [`opening`], and `viewer.replay_without_robot` if the recording
/// carries no robot.
pub fn scene(data_dir: &Path, id: &str) -> Result<RobotScene, KaptoError> {
    finalised_folder(data_dir, id)?;
    let view = View::open(&current_view(data_dir, id)?)?;
    view.about().scene.clone().ok_or_else(|| {
        KaptoError::new(ErrorCode::ViewerReplayWithoutRobot)
            .detail("the recording carries no robot description".to_owned())
            .context("episode", id.to_owned())
    })
}

/// Mesh `n` of the robot episode `id` was recorded with, in the binary form
/// the page reads, from its replay view.
///
/// # Errors
///
/// As [`opening`]; `viewer.mesh_not_found` if its scene has no mesh `n`,
/// `robot.mesh_unreadable` if it couldn't be read when the replay was made.
pub fn mesh(data_dir: &Path, id: &str, n: usize) -> Result<Vec<u8>, KaptoError> {
    finalised_folder(data_dir, id)?;
    View::open(&current_view(data_dir, id)?)?.mesh(n)
}

/// Episode `id`'s replay view, if it is made and of the version this Kapto
/// reads.
fn current_view(data_dir: &Path, id: &str) -> Result<PathBuf, KaptoError> {
    view_file(data_dir, id)
        .filter(|path| view_version(path) == Some(VIEW_VERSION))
        .ok_or_else(|| {
            KaptoError::new(ErrorCode::ViewerReplayNotReady)
                .detail("its replay isn't made".to_owned())
                .context("episode", id.to_owned())
        })
}

/// A stretch of episode `id`'s replay, from `from` to `to` (UTC
/// nanoseconds, both included) in at most `points` parts, from its replay
/// view: in the binary form the viewer stream answers a look back with.
/// Reading it counts as opening the replay.
///
/// # Errors
///
/// As [`status`]; `viewer.replay_not_ready` if it isn't made;
/// `viewer.replay_unreadable` if its view can't be read;
/// `api.invalid_request` for no points or more than 10000, a stretch
/// that ends before it begins, or one outside the episode.
pub fn window(
    data_dir: &Path,
    id: &str,
    from: i64,
    to: i64,
    points: u32,
) -> Result<Vec<u8>, KaptoError> {
    finalised_folder(data_dir, id)?;
    let invalid = |detail: String| KaptoError::new(ErrorCode::ApiInvalidRequest).detail(detail);
    if !(1..=MAX_POINTS).contains(&points) {
        return Err(invalid(format!(
            "`points` is {points}; ask for 1 to {MAX_POINTS}"
        )));
    }
    if to < from {
        return Err(invalid(format!(
            "the stretch ends ({to}) before it begins ({from})"
        )));
    }
    let path = current_view(data_dir, id)?;
    let mut view = View::open(&path)?;
    let (start, end) = (view.about().start_utc_ns, view.about().end_utc_ns);
    if to < start || from > end {
        return Err(invalid(format!(
            "the stretch from {from} to {to} is outside the episode, from {start} to {end}"
        )));
    }
    let stretch = view.window(from, to, points)?;
    // Kept even if it can't be marked: the cache then sees it as opened
    // when it was made.
    let _ = File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(SystemTime::now()));
    Ok(stretch.reply(0).to_vec())
}

/// The replay's status from its view and its job, if it has one. The view
/// in the cache, of the current version, is a replay made; otherwise the
/// job says how far it got.
fn described(data_dir: &Path, id: &str, job: Option<Job>) -> ReplayStatus {
    let bytes = view_file(data_dir, id)
        .filter(|path| view_version(path) == Some(VIEW_VERSION))
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len());
    let explained = |failures: &[JobFailure]| failures.iter().map(explain).collect();
    let (state, failure, lacks) = match (bytes, job) {
        (Some(_), job) => (
            ReplayState::Ready,
            None,
            job.map_or_else(Vec::new, |job| explained(&job.warnings)),
        ),
        (None, Some(job)) => match job.state {
            JobState::Queued => (ReplayState::Queued, None, Vec::new()),
            JobState::Running => (ReplayState::Making, None, Vec::new()),
            JobState::Failed => (
                ReplayState::Failed,
                job.failure.as_ref().map(explain),
                Vec::new(),
            ),
            JobState::Done => (ReplayState::NotMade, None, Vec::new()),
        },
        (None, None) => (ReplayState::NotMade, None, Vec::new()),
    };
    ReplayStatus {
        episode: id.to_owned(),
        state,
        bytes,
        failure,
        lacks,
    }
}

/// A failure the worker reported, with its code's remedy.
fn explain(failure: &JobFailure) -> ApiError {
    let code = ErrorCode::ALL.iter().find(|code| code.id() == failure.code);
    ApiError {
        code: failure.code.clone(),
        message: failure.message.clone(),
        remedy: code.map_or_else(String::new, |code| code.remedy().to_owned()),
    }
}

/// How far the cache may grow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheLimits {
    /// The most bytes its replays may take together
    /// (`viewer.replay_cache_size`).
    pub size: u64,
    /// How long a replay may go unopened (`viewer.replay_cache_age`).
    pub age: Duration,
    /// Below this many free bytes on the disk, every replay goes
    /// (`capture.disk_warn_below`).
    pub free_below: u64,
}

/// Why a file left the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A replay left unfinished, by a worker that stopped while writing it.
    Unfinished,
    /// Its episode is gone, or was never finalised.
    EpisodeGone,
    /// The disk had less free space than recording warns at.
    DiskLow,
    /// Nobody had opened it for longer than the cache keeps replays.
    Unopened,
    /// The cache was over its size, and others were opened more recently.
    OverSize,
}

impl Reason {
    /// Why, in words, for the worker's log.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Unfinished => "left unfinished",
            Self::EpisodeGone => "its episode is gone",
            Self::DiskLow => "the disk is low (capture.disk_warn_below)",
            Self::Unopened => "unopened for longer than viewer.replay_cache_age",
            Self::OverSize => "the cache is over viewer.replay_cache_size",
        }
    }
}

/// What trimming the cache did.
#[derive(Debug, Default)]
pub struct Trimmed {
    /// Each file removed, by name, and why.
    pub removed: Vec<(String, Reason)>,
    /// Files that should have gone but couldn't be removed, and why not.
    pub failed: Vec<(String, std::io::Error)>,
}

/// One replay in the cache.
struct Cached {
    name: String,
    path: PathBuf,
    /// When it was last opened.
    opened: SystemTime,
    bytes: u64,
}

/// Empties `data_dir`'s replay cache as `limits` say, at `now` with `free`
/// bytes left on the disk (`None` when that can't be read). In order:
/// unfinished replays, those whose episode is gone, every one when the disk
/// is low, those unopened for too long, then, while the cache is too big,
/// the least recently opened. The one opened last always stays for size,
/// even alone over it, so a long episode can still be replayed.
///
/// Only to be called while no replay is being made, since one being written
/// looks unfinished. Files the cache doesn't name are left alone.
#[must_use]
pub fn trim(data_dir: &Path, limits: &CacheLimits, free: Option<u64>, now: SystemTime) -> Trimmed {
    let mut trimmed = Trimmed::default();
    let Ok(entries) = std::fs::read_dir(data_dir.join(REPLAY_CACHE)) else {
        return trimmed;
    };
    let mut kept: Vec<Cached> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(OsStr::to_str).map(str::to_owned) else {
            continue;
        };
        let unfinished = name.strip_suffix(UNFINISHED).unwrap_or(&name);
        let Some(id) = unfinished.strip_suffix(EXTENSION) else {
            continue;
        };
        let finalised = || {
            episode_folder(data_dir, id).is_some_and(|folder| folder.join(FINALISED_FILE).exists())
        };
        if unfinished.len() < name.len() {
            remove(&mut trimmed, name, &path, Reason::Unfinished);
        } else if !finalised() {
            remove(&mut trimmed, name, &path, Reason::EpisodeGone);
        } else if let Ok(metadata) = entry.metadata() {
            kept.push(Cached {
                name,
                path,
                opened: metadata.modified().unwrap_or(now),
                bytes: metadata.len(),
            });
        }
    }
    if free.is_some_and(|free| free < limits.free_below) {
        for replay in kept.drain(..) {
            remove(&mut trimmed, replay.name, &replay.path, Reason::DiskLow);
        }
    }
    // The most recently opened first.
    kept.sort_by_key(|replay| std::cmp::Reverse(replay.opened));
    let mut bytes = 0_u64;
    for (place, replay) in kept.into_iter().enumerate() {
        let unopened = now
            .duration_since(replay.opened)
            .is_ok_and(|unopened| unopened > limits.age);
        bytes = bytes.saturating_add(replay.bytes);
        if unopened {
            bytes -= replay.bytes;
            remove(&mut trimmed, replay.name, &replay.path, Reason::Unopened);
        } else if place > 0 && bytes > limits.size {
            bytes -= replay.bytes;
            remove(&mut trimmed, replay.name, &replay.path, Reason::OverSize);
        }
    }
    trimmed
}

fn remove(trimmed: &mut Trimmed, name: String, path: &Path, reason: Reason) {
    match std::fs::remove_file(path) {
        Ok(()) => trimmed.removed.push((name, reason)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => trimmed.failed.push((name, error)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use kapto_testkit::TempDir;

    use super::*;

    const OLDEST: &str = "20260305T192345.123Z-1a2b3c4d";
    const MIDDLE: &str = "20260306T080000.000Z-5e6f7a8b";
    const NEWEST: &str = "20260307T080000.000Z-9c0d1e2f";

    const MIB: u64 = 1 << 20;
    const DAY: Duration = Duration::from_hours(24);

    /// A data directory with these finalised episodes, each with a replay
    /// of `bytes` last opened `ago` before [`now`].
    fn cache(test: &str, replays: &[(&str, u64, Duration)]) -> TempDir {
        let data = TempDir::new(test);
        for &(id, bytes, ago) in replays {
            data.write(&format!("{id}/{FINALISED_FILE}"), b"");
            let path = view_file(&data.0, id).unwrap();
            data.write(&format!("{REPLAY_CACHE}/{id}.view"), b"");
            let file = File::options().write(true).open(&path).unwrap();
            file.set_len(bytes).unwrap();
            file.set_modified(now() - ago).unwrap();
        }
        data
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
    }

    fn limits() -> CacheLimits {
        CacheLimits {
            size: 10 * MIB,
            age: 7 * DAY,
            free_below: 100 * MIB,
        }
    }

    /// What was removed, by episode ID for a replay.
    fn removed(trimmed: &Trimmed) -> Vec<(&str, Reason)> {
        assert!(trimmed.failed.is_empty(), "{:?}", trimmed.failed);
        let mut removed: Vec<(&str, Reason)> = trimmed
            .removed
            .iter()
            .map(|(name, reason)| (name.strip_suffix(".view").unwrap_or(name), *reason))
            .collect();
        removed.sort_by_key(|(name, _)| *name);
        removed
    }

    #[test]
    fn a_replay_is_kept_by_its_episodes_id_and_only_an_id_names_one() {
        let data = Path::new("/data");
        assert_eq!(
            view_file(data, OLDEST),
            Some(PathBuf::from(format!("/data/.cache/replay/{OLDEST}.view")))
        );
        for bad in ["../kapto.db", "", "20260305T192345.123Z-1a2b3c4d/x"] {
            assert_eq!(view_file(data, bad), None, "{bad:?}");
        }
    }

    #[test]
    fn past_its_size_the_least_recently_opened_replays_go_first() {
        let data = cache(
            "replay-cache-size",
            &[
                (OLDEST, 4 * MIB, DAY),
                (MIDDLE, 4 * MIB, 3 * DAY),
                (NEWEST, 4 * MIB, 2 * DAY),
            ],
        );
        let trimmed = trim(&data.0, &limits(), Some(u64::MAX), now());
        // Opened a day ago, OLDEST stays although its episode is older.
        assert_eq!(removed(&trimmed), [(MIDDLE, Reason::OverSize)]);
        assert!(view_file(&data.0, OLDEST).unwrap().exists());
        assert!(view_file(&data.0, NEWEST).unwrap().exists());
    }

    #[test]
    fn the_replay_opened_last_stays_even_alone_over_the_size() {
        let data = cache(
            "replay-cache-one",
            &[(OLDEST, 11 * MIB, DAY), (NEWEST, MIB, 2 * DAY)],
        );
        let trimmed = trim(&data.0, &limits(), Some(u64::MAX), now());
        assert_eq!(removed(&trimmed), [(NEWEST, Reason::OverSize)]);
        assert!(view_file(&data.0, OLDEST).unwrap().exists());
    }

    #[test]
    fn a_replay_unopened_for_longer_than_the_cache_keeps_them_goes_however_small_the_cache() {
        let data = cache(
            "replay-cache-age",
            &[(OLDEST, 1, 8 * DAY), (NEWEST, 1, 6 * DAY)],
        );
        let trimmed = trim(&data.0, &limits(), Some(u64::MAX), now());
        assert_eq!(removed(&trimmed), [(OLDEST, Reason::Unopened)]);
    }

    #[test]
    fn below_the_free_space_recording_warns_at_every_replay_goes() {
        let data = cache(
            "replay-cache-disk",
            &[(OLDEST, 1, DAY), (NEWEST, 1, Duration::ZERO)],
        );
        let trimmed = trim(&data.0, &limits(), Some(99 * MIB), now());
        assert_eq!(
            removed(&trimmed),
            [(OLDEST, Reason::DiskLow), (NEWEST, Reason::DiskLow)]
        );
        // Free space that can't be read is never a reason to remove them.
        let data = cache("replay-cache-disk-unknown", &[(OLDEST, 1, DAY)]);
        assert_eq!(removed(&trim(&data.0, &limits(), None, now())), []);
    }

    #[test]
    fn replays_of_episodes_that_are_gone_and_unfinished_ones_are_swept_and_other_files_left_alone()
    {
        let data = cache("replay-cache-gone", &[(OLDEST, 1, DAY), (NEWEST, 1, DAY)]);
        std::fs::remove_dir_all(data.0.join(NEWEST)).unwrap();
        data.write(&format!("{REPLAY_CACHE}/{MIDDLE}.view.partial"), b"");
        data.write(&format!("{REPLAY_CACHE}/notes.txt"), b"");
        let trimmed = trim(&data.0, &limits(), Some(u64::MAX), now());
        assert_eq!(
            removed(&trimmed),
            [
                (&*format!("{MIDDLE}.view.partial"), Reason::Unfinished),
                (NEWEST, Reason::EpisodeGone),
            ]
        );
        assert!(data.0.join(REPLAY_CACHE).join("notes.txt").exists());
    }

    #[test]
    fn a_data_directory_without_a_cache_has_nothing_to_trim() {
        let data = TempDir::new("replay-cache-none");
        let trimmed = trim(&data.0, &limits(), Some(0), now());
        assert_eq!(removed(&trimmed), []);
    }
}
