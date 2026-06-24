// SPDX-License-Identifier: Apache-2.0

//! An episode's replay: its replay view ([`crate::view`]), from which the
//! web UI's player shows it, worked out from what the recording carries,
//! so it shows the episode as it was recorded whatever its package says
//! now: the robot it was recorded with, its poses at the display rate,
//! which is all a screen shows, and every message's decoded values.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kapto_capture::reader::{RecordingStart, read_events, read_recording, read_start};
use kapto_capture::tap::wire::TapSample;
use kapto_config::carried::{carried_file, carried_manifest, carried_streams, slash_path};
use kapto_config::{
    MappingDeclaration, PackageManifest, StreamDeclaration, parse_layout, parse_mapping,
};
use kapto_core::{ClockSample, ErrorCode, KaptoError, RecvTime};
use kapto_decode::{Decoder, Value};
use kapto_robot::{Declared, JointMap, Kinematics, RobotModel, discover, load_urdf};
use kapto_store::records::EPISODE_RECORD;

use crate::live::LiveRobot;
use crate::page_layout::{RobotRole, ViewLayout, arranged, roles_of};
use crate::page_scene::{self, RobotScene};
use crate::picture::CameraKind;
use crate::plots::{Plots, StreamFields};
use crate::stream::OpeningStream;
use crate::view::{ViewContents, ViewWriter};

/// Messages read between two calls to the caller, where it may wait or
/// stop the replay.
const BETWEEN_MESSAGES: usize = 8192;

/// How a replay is made.
#[derive(Clone, Debug)]
pub struct ReplaySettings {
    /// How many times a second the robot's pose is kept.
    pub display_rate: u32,
    /// Where the local robot packages are: a recording that doesn't carry
    /// its meshes takes them from the package with the same ID and content
    /// hash.
    pub packages: PathBuf,
    /// The robot the last replay drew, kept for the next; shared by the
    /// clones of these settings.
    pub robots: DrawnRobots,
}

/// The robot a replay last drew, kept so the next replay of an episode
/// recorded with the same package doesn't read its meshes again. A
/// package's content hash covers every file in it, meshes included, so the
/// same hash draws the same robot. One robot is kept, and only one whose
/// meshes were all read: a process replaying episodes holds one robot's
/// shapes at most.
#[derive(Clone, Debug, Default)]
pub struct DrawnRobots(Arc<Mutex<Kept>>);

/// The content hash of the package last drawn whole, and its shapes.
type Kept = Option<(String, Arc<Drawn>)>;

/// A robot as the page draws it, with each mesh in its binary form.
#[derive(Debug)]
pub struct Drawn {
    scene: RobotScene,
    meshes: Vec<Vec<u8>>,
}

impl DrawnRobots {
    /// The shapes of `model`, kept from the last replay if it drew the
    /// package with content hash `hash`, and what of them can't be shown:
    /// `missing`, the mesh files the model lacks, then those that can't be
    /// read.
    fn draw(
        &self,
        hash: Option<&str>,
        model: &RobotModel,
        missing: Vec<KaptoError>,
    ) -> (Arc<Drawn>, Vec<KaptoError>) {
        let kept = self.0.lock().ok().and_then(|kept| kept.clone());
        if let (Some(hash), Some((kept_hash, drawn))) = (hash, kept)
            && *hash == kept_hash
        {
            // Meshes the recording lacks were read when it was kept.
            return (drawn, Vec::new());
        }
        let (scene, meshes, problems) = page_scene::Scene::with_meshes(model);
        let drawn = Arc::new(Drawn {
            scene: scene.robot,
            meshes,
        });
        if let Some(hash) = hash
            && missing.is_empty()
            && problems.is_empty()
            && let Ok(mut kept) = self.0.lock()
        {
            *kept = Some((hash.to_owned(), Arc::clone(&drawn)));
        }
        (drawn, missing.into_iter().chain(problems).collect())
    }
}

/// What went into a replay.
#[derive(Debug, Default)]
pub struct Replayed {
    /// Messages decoded.
    pub messages: u64,
    /// Poses of the robot kept.
    pub poses: u64,
    /// What the replay lacks, and why: the robot, or some of its meshes.
    pub problems: Vec<KaptoError>,
}

/// Writes the replay view of the recording at `recording` to `view`,
/// calling `between` every few thousand messages, where it may wait or
/// stop the replay with its error. It is written beside `view` first and
/// renamed once complete, so a file at `view` is always whole.
///
/// # Errors
///
/// `viewer.replay_write_failed` if the file can't be written;
/// `recording.unreadable` if the recording can't be read; the first problem
/// of the `package.toml` or `streams.toml` it carries; `between`'s.
pub fn write_replay(
    recording: &Path,
    view: &Path,
    settings: &ReplaySettings,
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<Replayed, KaptoError> {
    let mut partial = view.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let written = ViewWriter::create(
        &partial,
        1_000_000_000 / i64::from(settings.display_rate.max(1)),
    )
    .map_err(|error| write_failed(&partial, &error))
    .and_then(|writer| replay(recording, (writer, &partial), settings, between))
    .and_then(|replayed| {
        std::fs::rename(&partial, view).map_err(|error| write_failed(view, &error))?;
        Ok(replayed)
    });
    if written.is_err() {
        // Nothing to do about a failure: a partial file is never served.
        let _ = std::fs::remove_file(&partial);
    }
    written
}

fn write_failed(file: &Path, error: &dyn std::error::Error) -> KaptoError {
    KaptoError::new(ErrorCode::ViewerReplayWriteFailed)
        .detail(error.to_string())
        .context("file", file.display().to_string())
}

/// Writes the replay view of the recording at `recording` with `view`,
/// writing the file it names: the robot's poses at the display rate, every
/// decoded value, its events and the robot. Calls `between` as
/// [`write_replay`] does.
fn replay(
    recording: &Path,
    view: (ViewWriter, &Path),
    settings: &ReplaySettings,
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<Replayed, KaptoError> {
    let start = read_start(recording)?;
    let mut links = Vec::new();
    let mut drawn = None;
    let mut replayed = Replayed::default();
    let manifest = carried_manifest(&start.attachments, recording)?;
    let streams = carried_streams(&start.attachments, recording, manifest.as_ref())?;
    // Receive times in a recording are already UTC.
    let utc = ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: 0,
        uncertainty: Duration::ZERO,
    };
    let carried = carried_robot(&start, recording, manifest.as_ref(), &streams, settings);
    let (layout, roles) = carried_layout(
        &start,
        recording,
        manifest.as_ref(),
        &streams,
        carried.as_ref().ok(),
    );
    replayed.problems.extend(layout.1);
    let robot = match carried {
        Ok(mut carried) => {
            links = carried
                .model
                .links
                .iter()
                .map(|link| link.name.clone())
                .collect();
            // Reads the meshes, before the folder they were unpacked to goes.
            let missing = std::mem::take(&mut carried.problems);
            let (robot, problems) =
                settings
                    .robots
                    .draw(carried.hash.as_deref(), &carried.model, missing);
            replayed.problems.extend(problems);
            drawn = Some(robot);
            Some(LiveRobot::new(
                &carried.model,
                carried.kinematics,
                &carried.map,
                streams.len(),
                utc,
            ))
        }
        Err(problems) => {
            replayed.problems.extend(problems);
            None
        }
    };
    let plots = Plots::new(&StreamFields::of(&streams));
    let contents = ViewContents {
        links,
        streams: OpeningStream::of(&plots),
        events: read_events(recording)?,
        scene: drawn.as_ref().map(|drawn| drawn.scene.clone()),
        meshes: drawn.map(|drawn| drawn.meshes.clone()).unwrap_or_default(),
        layout: Some(layout.0),
        roles,
    };
    let mut player = Player::new(&streams, robot, view, settings.display_rate);
    player.replayed = replayed;
    let mut failed = None;
    let mut messages = 0_usize;
    read_recording(recording, |channel, message| {
        messages += 1;
        let step = if messages.is_multiple_of(BETWEEN_MESSAGES) {
            between()
        } else {
            Ok(())
        };
        match step.and_then(|()| player.play(&channel.topic, message.log_time, message.data)) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                failed = Some(error);
                ControlFlow::Break(())
            }
        }
    })?;
    if let Some(error) = failed {
        return Err(error);
    }
    player.finish(contents)
}

/// The recording's messages played through each stream's decoder into the
/// view and the robot.
struct Player {
    /// Each stream's place in the declarations, by its ID.
    by_id: BTreeMap<String, usize>,
    /// Each declared stream's decoder and its decoded values; `None` for
    /// one without a decoder.
    streams: Vec<Option<Decoding>>,
    /// The robot, if there is one to show.
    robot: Option<LiveRobot>,
    interval_ns: i64,
    next_pose: i64,
    /// The message being played, as the robot takes it.
    sample: TapSample,
    replayed: Replayed,
    view: ViewWriter,
    /// The file `view` writes, named by its problems.
    file: PathBuf,
}

/// A stream's decoder and the values it decoded last.
type Decoding = (Box<dyn Decoder>, Vec<Value>);

impl Player {
    fn new(
        streams: &[StreamDeclaration],
        robot: Option<LiveRobot>,
        (view, file): (ViewWriter, &Path),
        display_rate: u32,
    ) -> Self {
        let played = streams
            .iter()
            .map(|declared| {
                let decoder = declared.decoder.as_ref()?.compile().ok()?;
                let values = vec![Value::default(); decoder.value_count()];
                Some((decoder, values))
            })
            .collect();
        Self {
            by_id: streams
                .iter()
                .enumerate()
                .map(|(index, declared)| (declared.id.to_string(), index))
                .collect(),
            streams: played,
            robot,
            interval_ns: 1_000_000_000 / i64::from(display_rate.max(1)),
            next_pose: i64::MIN,
            sample: TapSample::single(0, 0, Vec::new()),
            replayed: Replayed::default(),
            view,
            file: file.to_owned(),
        }
    }

    /// Adds the values of a message of stream `topic` received at
    /// `log_time`, and the robot's pose if it is time to. A message of a
    /// stream without a decoder, or that doesn't decode, only counts in
    /// the span.
    fn play(&mut self, topic: &str, log_time: u64, data: &[u8]) -> Result<(), KaptoError> {
        // Every message is in the episode's span, shown or not: a camera's
        // frames are played from the recording beside the view.
        self.view
            .received(i64::try_from(log_time).unwrap_or(i64::MAX));
        let Some(&index) = self.by_id.get(topic) else {
            return Ok(());
        };
        let Some((decoder, values)) = self.streams[index].as_mut() else {
            return Ok(());
        };
        if decoder.decode(data, values).is_err() {
            return Ok(());
        }
        self.replayed.messages += 1;
        let time = i64::try_from(log_time).unwrap_or(i64::MAX);
        let sample = &mut self.sample;
        sample.stream = u16::try_from(index).unwrap_or(u16::MAX);
        sample.recv_time_ns = log_time;
        sample.values.clear();
        sample
            .values
            .extend(values.iter().map(|value| value.as_f64()));
        self.view
            .message(sample.stream, time, &sample.values)
            .map_err(|error| write_failed(&self.file, &error))?;
        if let Some(live) = self.robot.as_mut()
            && live.apply(sample)
            && time >= self.next_pose
            && live.advance().is_some()
        {
            // On a grid from the first pose, so a pose falls in every
            // display interval however the messages fall.
            if self.next_pose == i64::MIN {
                self.next_pose = time;
            }
            while self.next_pose <= time {
                self.next_pose = self.next_pose.saturating_add(self.interval_ns);
            }
            self.replayed.poses += 1;
            self.view
                .pose(time, live.poses())
                .map_err(|error| write_failed(&self.file, &error))?;
        }
        Ok(())
    }

    /// Finishes the view with `contents`.
    fn finish(self, contents: ViewContents) -> Result<Replayed, KaptoError> {
        self.view
            .finish(contents)
            .map_err(|error| write_failed(&self.file, &error))?;
        Ok(self.replayed)
    }
}

/// The streams of the recording at `recording` that are cameras, by the
/// `streams.toml` it carries: each one's name and how its frames are laid
/// out, in declaration order.
///
/// # Errors
///
/// `recording.unreadable` if the recording can't be read, or a carried
/// file's error if it can't be parsed.
pub fn recorded_cameras(recording: &Path) -> Result<Vec<(String, CameraKind)>, KaptoError> {
    let start = read_start(recording)?;
    let manifest = carried_manifest(&start.attachments, recording)?;
    let streams = carried_streams(&start.attachments, recording, manifest.as_ref())?;
    Ok(streams
        .iter()
        .filter_map(|stream| Some((stream.id.to_string(), stream.camera_kind()?)))
        .collect())
}

/// The robot the recording was made with and how its joints are driven,
/// with its meshes that can't be shown; or why there is no robot to show.
fn carried_robot(
    start: &RecordingStart,
    recording: &Path,
    manifest: Option<&PackageManifest>,
    streams: &[StreamDeclaration],
    settings: &ReplaySettings,
) -> Result<CarriedRobot, Vec<KaptoError>> {
    let Some(manifest) = manifest else {
        return Err(
            vec![KaptoError::new(ErrorCode::ViewerReplayWithoutRobot).detail(
            "the recording was made without a robot package, so it carries no robot description"
                .to_owned(),
        )],
        );
    };
    let mapping_name = slash_path(&manifest.mapping);
    let mapping = start.attachments.get(mapping_name.as_str()).map_or_else(
        || Ok(MappingDeclaration::default()),
        |data| parse_mapping(&carried_file(recording, &mapping_name, data)),
    )?;
    let search = std::slice::from_ref(&settings.packages);
    // A local package with the same ID and content hash has the same
    // files, meshes included; otherwise the recording's own are used.
    let (model, unpacked) = if let Some(urdf) = local_package(start, manifest, &settings.packages) {
        (load_urdf(&urdf, search)?, None)
    } else {
        let unpacked = Unpacked::of(start)?;
        (
            load_urdf(&unpacked.0.join(&manifest.urdf), search)?,
            Some(unpacked),
        )
    };
    let problems = model
        .missing_files()
        .map(|(link, file)| {
            KaptoError::new(ErrorCode::RobotMeshUnreadable)
                .detail(format!(
                    "{} isn't in the recording or in a package with the same ID and content, so it isn't shown",
                    file.uri
                ))
                .context("link", link.name.clone())
        })
        .collect();
    let kinematics = Kinematics::new(&model).map_err(|error| vec![error])?;
    let map = JointMap::new(
        &model,
        &kinematics,
        streams,
        &mapping,
        &recording.join(&mapping_name),
    )?;
    Ok(CarriedRobot {
        model,
        mapping,
        kinematics,
        map,
        problems,
        hash: start
            .records
            .get(EPISODE_RECORD)
            .and_then(|record| record.get("package.hash"))
            .cloned(),
        _unpacked: unpacked,
    })
}

/// How the replay is arranged: by the layout the recording carries if it
/// reads, else one made from what it declares; with the problems of one
/// that doesn't read, and the roles its plots may name.
fn carried_layout(
    start: &RecordingStart,
    recording: &Path,
    manifest: Option<&PackageManifest>,
    streams: &[StreamDeclaration],
    robot: Option<&CarriedRobot>,
) -> ((ViewLayout, Vec<KaptoError>), Vec<RobotRole>) {
    let no_mapping = MappingDeclaration::default();
    let mapping = robot.map_or(&no_mapping, |robot| &robot.mapping);
    let declared = Declared::new(robot.map(|robot| &robot.model), streams, mapping);
    let own = manifest.map(|manifest| {
        let name = slash_path(&manifest.layout);
        start
            .attachments
            .get(name.as_str())
            .map_or(Ok(None), |data| {
                parse_layout(&carried_file(recording, &name, data)).map(Some)
            })
    });
    (arranged(own, &declared), roles_of(mapping))
}

/// The robot a recording was made with, ready to show.
struct CarriedRobot {
    model: RobotModel,
    /// Which decoded value drives which joint, and plays which role.
    mapping: MappingDeclaration,
    kinematics: Kinematics,
    map: JointMap,
    /// Its meshes that can't be shown.
    problems: Vec<KaptoError>,
    /// The content hash of the package it was recorded with.
    hash: Option<String>,
    /// The folder its meshes are read from, kept until they are.
    _unpacked: Option<Unpacked>,
}

/// The robot description of the local package the episode was recorded
/// with, if it is still there unchanged.
fn local_package(
    start: &RecordingStart,
    manifest: &PackageManifest,
    packages: &Path,
) -> Option<PathBuf> {
    let record = start.records.get(EPISODE_RECORD)?;
    let hash = record.get("package.hash")?;
    let (found, _) = discover(packages);
    found
        .into_iter()
        .find(|package| package.manifest.id == manifest.id && &package.hash == hash)
        .map(|package| package.urdf())
}

/// The files the recording carries, written out to a temporary folder so
/// the robot description finds its meshes beside it as in its package;
/// removed when dropped. The meshes are read before it is.
struct Unpacked(PathBuf);

impl Unpacked {
    fn of(start: &RecordingStart) -> Result<Self, Vec<KaptoError>> {
        let folder = std::env::temp_dir().join(format!(
            "kapto-replay-{}-{}",
            std::process::id(),
            ClockSample::now().utc_ns
        ));
        let unpacked = Self(folder);
        for (name, data) in &start.attachments {
            // A name that would leave the folder isn't a package's file.
            let inside = Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
            if !inside {
                continue;
            }
            let path = unpacked.0.join(name);
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, data));
            if let Err(error) = written {
                return Err(vec![
                    KaptoError::new(ErrorCode::ViewerReplayWriteFailed)
                        .detail(format!("the robot's files can't be unpacked: {error}"))
                        .context("folder", unpacked.0.display().to_string()),
                ]);
            }
        }
        Ok(unpacked)
    }
}

impl Drop for Unpacked {
    fn drop(&mut self) {
        // Nothing to do about a failure: it is in the temporary folder.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
