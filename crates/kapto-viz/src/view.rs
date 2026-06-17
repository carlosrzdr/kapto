// SPDX-License-Identifier: Apache-2.0

//! An episode's replay view: what the page's player needs to show any
//! stretch of a recorded episode at any zoom without reading the recording.
//! Specified in `docs/reference/replay-view.md`.
//!
//! It holds the robot's poses and each decoded value's minimum and maximum
//! at the display rate, then at steps four times as long, and so on until
//! one step spans the episode: each step's records are those of the step
//! below merged, so a spike shows however far out the player zooms. A
//! stretch is read at the coarsest step that still gives the points asked
//! for, a few blocks of records, whatever the episode's length. Records
//! are the live view's ([`Record`]), so the player draws a replay as it
//! draws the last minutes. It also carries the robot the episode was
//! recorded with, as the page draws it, so a replay shows that robot
//! whatever its package is now.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};
use kapto_robot::Transform;
use kapto_store::events::Event;
pub use kapto_store::replay_view::{VIEW_MAGIC, VIEW_VERSION};
use serde::{Deserialize, Serialize};

use crate::history::{Record, SampleRecord, Stretch, pose, stretch};
use crate::page_layout::{RobotRole, ViewLayout};
use crate::page_scene::RobotScene;
use crate::stream::OpeningStream;

/// How many times longer each step is than the one below.
const STEP_FACTOR: i64 = 4;
/// The most steps: at the display rate, the last spans days.
const MAX_STEPS: usize = 16;
/// Records between two entries of a step's index.
const BLOCK_RECORDS: u64 = 64;
/// The trailer: the footer's offset and length, then the magic.
const TRAILER: i64 = 16;

/// What a view says about itself, in its footer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewAbout {
    /// The view's version.
    pub version: u16,
    /// When the first message was received, UTC nanoseconds.
    pub start_utc_ns: i64,
    /// When the last was.
    pub end_utc_ns: i64,
    /// Each link's name, in the order a record gives its pose.
    pub links: Vec<String>,
    /// Each stream, by its number in a record, with its values.
    pub streams: Vec<OpeningStream>,
    /// Messages in it.
    pub messages: u64,
    /// The events the recording carries.
    pub events: Vec<Event>,
    /// Each step, finest first.
    pub steps: Vec<Step>,
    /// The robot the episode was recorded with, as the page draws it;
    /// absent if the recording carries none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<RobotScene>,
    /// Where each of the scene's meshes is in the file and how long it
    /// is, in the scene's numbering; empty for one that can't be read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meshes: Vec<(u64, u64)>,
    /// How the page arranges the replay: the layout the recording carries,
    /// or one made from what it declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<ViewLayout>,
    /// Each role the recording's mapping gives a value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RobotRole>,
}

/// One step of a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// How long each of its records spans, in nanoseconds.
    pub step_ns: i64,
    /// How many records it has.
    pub records: u64,
    /// Where its records begin in the file.
    pub offset: u64,
    /// Every 64th record's time and offset, to start a stretch from.
    pub index: Vec<(i64, u64)>,
}

/// What a view is about besides its records: given when it is finished.
#[derive(Clone, Debug, Default)]
pub struct ViewContents {
    /// Each link's name, in the order [`ViewWriter::pose`] gives them.
    pub links: Vec<String>,
    /// Each stream with its values.
    pub streams: Vec<OpeningStream>,
    /// The events the recording carries.
    pub events: Vec<Event>,
    /// The robot the episode was recorded with, as the page draws it.
    pub scene: Option<RobotScene>,
    /// Each of the scene's meshes in the page's binary form, in its
    /// numbering; empty for one that can't be read.
    pub meshes: Vec<Vec<u8>>,
    /// How the page arranges the replay.
    pub layout: Option<ViewLayout>,
    /// Each role the recording's mapping gives a value.
    pub roles: Vec<RobotRole>,
}

/// Writes a view as an episode is read: messages and poses in the order
/// they were received.
pub struct ViewWriter {
    file: BufWriter<File>,
    written: u64,
    steps: Vec<Merging>,
    origin: Option<i64>,
    end: i64,
    messages: u64,
}

/// One step's record being merged, and the records it has finished.
struct Merging {
    step_ns: i64,
    at: i64,
    record: Option<Record>,
    records: u64,
    index: Vec<(i64, u64)>,
    /// Its finished records; the finest step's go straight to the file.
    bytes: Vec<u8>,
}

impl ViewWriter {
    /// Starts a view at `path` whose finest step is `step_ns` long, the
    /// time between two poses on screen.
    ///
    /// # Errors
    ///
    /// The operating system's, if the file can't be created.
    pub fn create(path: &Path, step_ns: i64) -> io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(VIEW_MAGIC)?;
        file.write_all(&VIEW_VERSION.to_le_bytes())?;
        file.write_all(&0_u16.to_le_bytes())?;
        let mut step_ns = step_ns.max(1);
        let steps = (0..MAX_STEPS)
            .map(|_| {
                let merging = Merging {
                    step_ns,
                    at: i64::MIN,
                    record: None,
                    records: 0,
                    index: Vec::new(),
                    bytes: Vec::new(),
                };
                step_ns = step_ns.saturating_mul(STEP_FACTOR);
                merging
            })
            .collect();
        Ok(Self {
            file,
            written: 8,
            steps,
            origin: None,
            end: i64::MIN,
            messages: 0,
        })
    }

    /// Adds a message of stream `stream` received at `time` with its
    /// decoded `values`.
    ///
    /// # Errors
    ///
    /// The operating system's, if the file can't be written.
    pub fn message(&mut self, stream: u16, time: i64, values: &[f64]) -> io::Result<()> {
        self.messages += 1;
        let record = Record {
            received_utc_ns: time,
            poses: None,
            samples: vec![SampleRecord {
                stream,
                messages: 1,
                first_utc_ns: time,
                newest_utc_ns: time,
                extremes: values.iter().map(|&value| (value, value)).collect(),
            }],
        };
        self.add(record)
    }

    /// Notes a message received at `time` that has nothing to show, such
    /// as a camera's frame: the view's span still covers it, so it runs
    /// from the episode's first message to its last.
    pub fn received(&mut self, time: i64) {
        self.origin.get_or_insert(time);
        self.end = self.end.max(time);
    }

    /// Adds the robot's pose at `time`: every link's, in link order.
    ///
    /// # Errors
    ///
    /// As [`ViewWriter::message`].
    pub fn pose(&mut self, time: i64, poses: &[Transform]) -> io::Result<()> {
        self.add(Record {
            received_utc_ns: time,
            poses: Some(poses.iter().map(pose).collect()),
            samples: Vec::new(),
        })
    }

    fn add(&mut self, record: Record) -> io::Result<()> {
        let origin = *self.origin.get_or_insert(record.received_utc_ns);
        self.end = self.end.max(record.received_utc_ns);
        self.merge(0, origin, record)
    }

    /// Merges `record` into step `level`, finishing the record before it
    /// if it falls in a later part, which is merged into the next step.
    fn merge(&mut self, level: usize, origin: i64, record: Record) -> io::Result<()> {
        let step = &mut self.steps[level];
        // A message received a little before the last, as streams written
        // in batches can be, joins the part being merged.
        let at = (record.received_utc_ns.saturating_sub(origin) / step.step_ns).max(step.at);
        if at != step.at
            && let Some(done) = step.record.take()
        {
            self.finish_record(level, origin, done)?;
        }
        let step = &mut self.steps[level];
        step.at = at;
        match step.record.as_mut() {
            Some(kept) => absorb(kept, record),
            None => step.record = Some(record),
        }
        Ok(())
    }

    fn finish_record(&mut self, level: usize, origin: i64, record: Record) -> io::Result<()> {
        let mut bytes = Vec::new();
        encode(&record, &mut bytes);
        let step = &mut self.steps[level];
        let offset = if level == 0 {
            self.written
        } else {
            step.bytes.len() as u64
        };
        if step.records.is_multiple_of(BLOCK_RECORDS) {
            step.index.push((record.received_utc_ns, offset));
        }
        step.records += 1;
        if level == 0 {
            self.file.write_all(&bytes)?;
            self.written += bytes.len() as u64;
        } else {
            step.bytes.extend(bytes);
        }
        if level + 1 < self.steps.len() {
            self.merge(level + 1, origin, record)?;
        }
        Ok(())
    }

    /// Finishes the last records, writes every step and the footer.
    ///
    /// # Errors
    ///
    /// As [`ViewWriter::message`].
    pub fn finish(mut self, contents: ViewContents) -> io::Result<ViewAbout> {
        let origin = self.origin.unwrap_or(0);
        for level in 0..self.steps.len() {
            if let Some(done) = self.steps[level].record.take() {
                self.finish_record(level, origin, done)?;
            }
        }
        // Steps past the first that holds the whole episode in one record
        // add nothing.
        let wanted = self
            .steps
            .iter()
            .position(|step| step.records <= 1)
            .map_or(self.steps.len(), |last| last + 1);
        let mut steps = Vec::new();
        for (level, mut step) in self.steps.drain(..).enumerate().take(wanted) {
            let offset = if level == 0 { 8 } else { self.written };
            if level > 0 {
                self.file.write_all(&step.bytes)?;
                self.written += step.bytes.len() as u64;
                for entry in &mut step.index {
                    entry.1 += offset;
                }
            }
            steps.push(Step {
                step_ns: step.step_ns,
                records: step.records,
                offset,
                index: step.index,
            });
        }
        let mut meshes = Vec::new();
        for mesh in &contents.meshes {
            self.file.write_all(mesh)?;
            meshes.push((self.written, mesh.len() as u64));
            self.written += mesh.len() as u64;
        }
        let about = ViewAbout {
            version: VIEW_VERSION,
            start_utc_ns: self.origin.unwrap_or(0),
            end_utc_ns: self.end.max(self.origin.unwrap_or(0)),
            links: contents.links,
            streams: contents.streams,
            messages: self.messages,
            events: contents.events,
            steps,
            scene: contents.scene,
            meshes,
            layout: contents.layout,
            roles: contents.roles,
        };
        let footer = serde_json::to_vec(&about).map_err(io::Error::other)?;
        self.file.write_all(&footer)?;
        self.file.write_all(&self.written.to_le_bytes())?;
        self.file.write_all(
            &u32::try_from(footer.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        )?;
        self.file.write_all(VIEW_MAGIC)?;
        self.file.flush()?;
        Ok(about)
    }
}

/// Merges `record` into `kept`: the newer pose, each stream's extremes.
fn absorb(kept: &mut Record, record: Record) {
    if record.poses.is_some()
        && (record.received_utc_ns >= kept.received_utc_ns || kept.poses.is_none())
    {
        kept.poses = record.poses;
    }
    kept.received_utc_ns = kept.received_utc_ns.max(record.received_utc_ns);
    for sample in record.samples {
        match kept
            .samples
            .iter_mut()
            .find(|kept| kept.stream == sample.stream)
        {
            Some(kept) => {
                kept.messages = kept.messages.saturating_add(sample.messages);
                kept.first_utc_ns = kept.first_utc_ns.min(sample.first_utc_ns);
                kept.newest_utc_ns = kept.newest_utc_ns.max(sample.newest_utc_ns);
                for (extremes, (minimum, maximum)) in kept.extremes.iter_mut().zip(sample.extremes)
                {
                    *extremes = (extremes.0.min(minimum), extremes.1.max(maximum));
                }
            }
            None => kept.samples.push(sample),
        }
    }
}

fn count(n: usize) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes()
}

/// A record as the view's specification lays it out: as a frame of the
/// live stream lays out its poses and samples.
fn encode(record: &Record, bytes: &mut Vec<u8>) {
    let poses = record.poses.as_deref().unwrap_or_default();
    bytes.extend(record.received_utc_ns.to_le_bytes());
    bytes.extend(count(poses.len()));
    bytes.extend(count(record.samples.len()));
    for value in poses.iter().flatten() {
        bytes.extend(value.to_le_bytes());
    }
    for sample in &record.samples {
        bytes.extend(sample.stream.to_le_bytes());
        bytes.extend(0_u16.to_le_bytes());
        bytes.extend(sample.messages.to_le_bytes());
        bytes.extend(count(sample.extremes.len()));
        bytes.extend(sample.first_utc_ns.to_le_bytes());
        bytes.extend(sample.newest_utc_ns.to_le_bytes());
        for (minimum, maximum) in &sample.extremes {
            bytes.extend(minimum.to_le_bytes());
            bytes.extend(maximum.to_le_bytes());
        }
    }
}

/// A view opened to read stretches of it.
pub struct View {
    file: BufReader<File>,
    about: ViewAbout,
}

impl View {
    /// Opens the view at `path`.
    ///
    /// # Errors
    ///
    /// `viewer.replay_unreadable` if it can't be read, isn't a view or is
    /// of another version.
    pub fn open(path: &Path) -> Result<Self, KaptoError> {
        let unreadable = |detail: String| {
            KaptoError::new(ErrorCode::ViewerReplayUnreadable)
                .detail(detail)
                .context("file", path.display().to_string())
        };
        let mut file =
            BufReader::new(File::open(path).map_err(|error| unreadable(error.to_string()))?);
        let mut start = [0_u8; 8];
        file.read_exact(&mut start)
            .map_err(|error| unreadable(error.to_string()))?;
        if &start[..4] != VIEW_MAGIC {
            return Err(unreadable("it isn't a replay view".into()));
        }
        let version = u16::from_le_bytes([start[4], start[5]]);
        if version != VIEW_VERSION {
            return Err(unreadable(format!(
                "it is of version {version}, this Kapto reads {VIEW_VERSION}"
            )));
        }
        let mut trailer = [0_u8; 16];
        file.seek(SeekFrom::End(-TRAILER))
            .and_then(|_| file.read_exact(&mut trailer))
            .map_err(|error| unreadable(error.to_string()))?;
        let [o0, o1, o2, o3, o4, o5, o6, o7, l0, l1, l2, l3, ..] = trailer;
        if &trailer[12..] != VIEW_MAGIC {
            return Err(unreadable("it ends before its footer".into()));
        }
        let offset = u64::from_le_bytes([o0, o1, o2, o3, o4, o5, o6, o7]);
        let mut footer = vec![0_u8; u32::from_le_bytes([l0, l1, l2, l3]) as usize];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut footer))
            .map_err(|error| unreadable(error.to_string()))?;
        let about =
            serde_json::from_slice(&footer).map_err(|error| unreadable(error.to_string()))?;
        Ok(Self { file, about })
    }

    /// What it says about itself.
    #[must_use]
    pub fn about(&self) -> &ViewAbout {
        &self.about
    }

    /// Mesh `n` of its scene, in the page's binary form.
    ///
    /// # Errors
    ///
    /// `viewer.mesh_not_found` if the scene has no mesh `n`;
    /// `robot.mesh_unreadable` if it couldn't be read when the view was
    /// made; `viewer.replay_unreadable` if it can't be read from the view.
    pub fn mesh(&mut self, n: usize) -> Result<Vec<u8>, KaptoError> {
        let &(offset, length) = self.about.meshes.get(n).ok_or_else(|| {
            KaptoError::new(ErrorCode::ViewerMeshNotFound).detail(format!(
                "there is no mesh {n}: the replay's robot uses {}",
                self.about.meshes.len()
            ))
        })?;
        if length == 0 {
            return Err(
                KaptoError::new(ErrorCode::RobotMeshUnreadable).detail(format!(
                    "mesh {n} couldn't be read when the replay was made"
                )),
            );
        }
        let unreadable =
            |detail: String| KaptoError::new(ErrorCode::ViewerReplayUnreadable).detail(detail);
        // Read up to its length rather than allocated at once, so a
        // damaged footer can't ask for more memory than the file holds.
        let mut mesh = Vec::new();
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| (&mut self.file).take(length).read_to_end(&mut mesh))
            .map_err(|error| unreadable(error.to_string()))?;
        if mesh.len() as u64 != length {
            return Err(unreadable(format!("it ends inside mesh {n}")));
        }
        Ok(mesh)
    }

    /// The stretch from `from` to `to` (UTC nanoseconds, both included)
    /// merged into `points` parts, as the live view's history merges one:
    /// from the coarsest step that still has `points` parts in it, or the
    /// finest if none has.
    ///
    /// # Errors
    ///
    /// As [`View::records`].
    pub fn window(&mut self, from: i64, to: i64, points: u32) -> Result<Stretch, KaptoError> {
        let level = self.step_for(from, to, points);
        let records = self.records(level, from, to)?;
        let span = Some((self.about.start_utc_ns, self.about.end_utc_ns));
        Ok(stretch(&records, span, from, to, points))
    }

    /// The coarsest step with at least `points` parts from `from` to `to`,
    /// or the finest if none has.
    #[must_use]
    pub fn step_for(&self, from: i64, to: i64, points: u32) -> usize {
        let width = to.saturating_sub(from) / i64::from(points.max(1));
        self.about
            .steps
            .iter()
            .rposition(|step| step.step_ns <= width)
            .unwrap_or(0)
    }

    /// The records of step `level` from `from` to `to` (UTC nanoseconds,
    /// both included), in time order; none for a step it doesn't have.
    ///
    /// # Errors
    ///
    /// `viewer.replay_unreadable` if they can't be read.
    pub fn records(&mut self, level: usize, from: i64, to: i64) -> Result<Vec<Record>, KaptoError> {
        let Some(step) = self.about.steps.get(level) else {
            return Ok(Vec::new());
        };
        // From the last indexed record at or before `from`.
        let start = step
            .index
            .iter()
            .rev()
            .find(|(time, _)| *time <= from)
            .or(step.index.first())
            .map_or(step.offset, |&(_, offset)| offset);
        let before = step
            .index
            .iter()
            .filter(|&&(_, offset)| offset < start)
            .count() as u64
            * BLOCK_RECORDS;
        let unreadable = |error: io::Error| {
            KaptoError::new(ErrorCode::ViewerReplayUnreadable).detail(error.to_string())
        };
        self.file.seek(SeekFrom::Start(start)).map_err(unreadable)?;
        let mut records = Vec::new();
        for _ in before..step.records {
            let record = decode(&mut self.file).map_err(unreadable)?;
            if record.received_utc_ns > to {
                break;
            }
            if record.received_utc_ns >= from {
                records.push(record);
            }
        }
        Ok(records)
    }
}

fn decode(file: &mut impl Read) -> io::Result<Record> {
    let received_utc_ns = read_i64(file)?;
    let links = read_u32(file)?;
    let streams = read_u32(file)?;
    let mut poses = Vec::new();
    for _ in 0..links {
        let mut pose = [0.0_f32; 7];
        for value in &mut pose {
            *value = f32::from_le_bytes(bytes(file)?);
        }
        poses.push(pose);
    }
    let mut samples = Vec::new();
    for _ in 0..streams {
        let [low, high, _, _] = bytes::<4>(file)?;
        let messages = read_u32(file)?;
        let values = read_u32(file)?;
        let first_utc_ns = read_i64(file)?;
        let newest_utc_ns = read_i64(file)?;
        let mut extremes = Vec::new();
        for _ in 0..values {
            let minimum = f64::from_le_bytes(bytes(file)?);
            let maximum = f64::from_le_bytes(bytes(file)?);
            extremes.push((minimum, maximum));
        }
        samples.push(SampleRecord {
            stream: u16::from_le_bytes([low, high]),
            messages,
            first_utc_ns,
            newest_utc_ns,
            extremes,
        });
    }
    Ok(Record {
        received_utc_ns,
        poses: (links > 0).then_some(poses),
        samples,
    })
}

fn bytes<const N: usize>(file: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_u32(file: &mut impl Read) -> io::Result<u32> {
    bytes(file).map(u32::from_le_bytes)
}

fn read_i64(file: &mut impl Read) -> io::Result<i64> {
    bytes(file).map(i64::from_le_bytes)
}
