// SPDX-License-Identifier: Apache-2.0

//! An episode's values by role: read from its recording, decoded with the
//! layouts stored in it, and named with the `mapping.toml` it carries, so
//! checks and statistics never see a stream's or field's name and read the
//! episode as it was recorded, whatever its package says now.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;

use kapto_capture::reader::{RecordedChannel, is_kapto_channel, read_channels};
use kapto_config::{FieldReference, MAX_MESSAGE_BYTES, parse_mapping};
use kapto_core::{KaptoError, Role};
use kapto_decode::{Decoder, DecoderDeclaration, Value};
use kapto_store::{EVENTS_CHANNEL, Event};

use crate::cameras::{CameraReader, CameraTimeline, declared_streams};
use crate::carried::carried;
use crate::timing::{StreamTiming, TimingReader};

/// The name of the robot package's mapping, as recordings carry it.
const MAPPING_FILE: &str = "mapping.toml";

/// How many messages are read between two calls to `between`.
const STEP: u64 = 10_000;

/// What checks read of a recording, by role and by stream name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recorded {
    /// Every role's values.
    pub roles: BTreeMap<Role, RoleColumn>,
    /// Every camera's frames.
    pub cameras: BTreeMap<String, CameraTimeline>,
    /// When every stream's messages arrived.
    pub timing: BTreeMap<String, StreamTiming>,
    /// Every event the episode holds, in the order written; one that
    /// can't be read is left out.
    pub events: Vec<Event>,
}

/// One role's values through an episode: a row for each message of its
/// stream that could be decoded, in the order received.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoleColumn {
    /// When each row's message was received, in UTC nanoseconds since the
    /// Unix epoch.
    pub times_utc_ns: Vec<i64>,
    /// Values in a row: 1, or an array field's length.
    pub width: usize,
    /// The values, row after row; true/false as 1 and 0.
    pub values: Vec<f64>,
}

impl RoleColumn {
    /// How many rows it has.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.times_utc_ns.len()
    }

    /// Row `index`'s values, if it has that many rows.
    #[must_use]
    pub fn row(&self, index: usize) -> Option<&[f64]> {
        let start = index.checked_mul(self.width)?;
        self.values.get(start..start + self.width)
    }
}

/// Where one role's values are in its stream's decoded values.
struct Slot {
    role: Role,
    /// The first of its values among the stream's decoded values.
    first: usize,
    width: usize,
}

/// A stream's decoder, and the roles its values play.
struct StreamRoles {
    decoder: Box<dyn Decoder>,
    slots: Vec<Slot>,
    values: Vec<Value>,
}

/// Every role the recording at `recording` has, with its values. A role the
/// episode lacks (no `mapping.toml`, a stream not recorded, a layout not
/// stored) is absent, not an error. `between` is called every few thousand
/// messages, where reading may wait, and stops it with the error it gives.
///
/// # Errors
///
/// `recording.unreadable` if the recording can't be read; the first problem
/// of the `mapping.toml` it carries, with how many more; what `between`
/// gives.
pub fn read_roles(
    recording: &Path,
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<BTreeMap<Role, RoleColumn>, KaptoError> {
    read_recorded(recording, (CameraReader::default(), None), between)
        .map(|recorded| recorded.roles)
}

/// What checks read of the recording at `recording`, of an episode
/// spanning `span` in UTC nanoseconds: every role it has, with its values,
/// as [`read_roles`] reads them, every camera's frames, and when every
/// stream's messages arrived, all read in one pass.
///
/// # Errors
///
/// As [`read_roles`].
pub fn read_episode(
    recording: &Path,
    span: (i64, i64),
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<Recorded, KaptoError> {
    let declared = declared_streams(recording)?;
    let readers = (
        CameraReader::of(&declared),
        Some(TimingReader::new(span, &declared)),
    );
    read_recorded(recording, readers, between)
}

fn read_recorded(
    recording: &Path,
    (mut cameras, mut timing): (CameraReader, Option<TimingReader>),
    between: &mut dyn FnMut() -> Result<(), KaptoError>,
) -> Result<Recorded, KaptoError> {
    let wanted = carried(recording, MAPPING_FILE, parse_mapping)?
        .map(|mapping| mapping.all_roles())
        .unwrap_or_default();
    if wanted.is_empty() && cameras.is_empty() && timing.is_none() {
        return Ok(Recorded::default());
    }
    let mut streams: BTreeMap<String, Option<StreamRoles>> = BTreeMap::new();
    let mut columns: BTreeMap<Role, RoleColumn> = BTreeMap::new();
    let mut read = 0_u64;
    let mut stopped = None;
    let mut events = Vec::new();
    // The streams, and the events beside them for an episode's checks.
    let with_events = timing.is_some();
    let channels =
        move |topic: &str| !is_kapto_channel(topic) || (with_events && topic == EVENTS_CHANNEL);
    read_channels(recording, channels, |channel, message| {
        read += 1;
        if read.is_multiple_of(STEP)
            && let Err(error) = between()
        {
            stopped = Some(error);
            return ControlFlow::Break(());
        }
        if channel.topic == EVENTS_CHANNEL {
            events.extend(Event::from_json(message.data));
            return ControlFlow::Continue(());
        }
        cameras.take(&channel.topic, message.log_time, message.data);
        if let Some(timing) = &mut timing {
            timing.take(&channel.topic, message.log_time);
        }
        let stream = streams
            .entry(channel.topic.clone())
            .or_insert_with(|| stream_roles(channel, &wanted));
        let Some(stream) = stream else {
            return ControlFlow::Continue(());
        };
        if stream
            .decoder
            .decode(message.data, &mut stream.values)
            .is_err()
        {
            return ControlFlow::Continue(());
        }
        let time = i64::try_from(message.log_time).unwrap_or(i64::MAX);
        for slot in &stream.slots {
            let values = &stream.values[slot.first..slot.first + slot.width];
            // A role the message doesn't have isn't read as 0: it has no
            // value at this time.
            if values.contains(&Value::Missing) {
                continue;
            }
            let column = columns.entry(slot.role.clone()).or_default();
            column.width = slot.width;
            column.times_utc_ns.push(time);
            column
                .values
                .extend(values.iter().map(|value| value.as_f64()));
        }
        ControlFlow::Continue(())
    })?;
    match stopped {
        Some(error) => Err(error),
        None => Ok(Recorded {
            roles: columns,
            cameras: cameras.finish(),
            timing: timing.map(TimingReader::finish).unwrap_or_default(),
            events,
        }),
    }
}

/// The decoder of `channel`'s stored layout and where each of `wanted`'s
/// roles on it is among its values; `None` if it has no usable layout or
/// plays no role.
fn stream_roles(
    channel: &RecordedChannel,
    wanted: &[(Role, FieldReference)],
) -> Option<StreamRoles> {
    let schema = channel.schema.as_ref()?;
    let decoder = DecoderDeclaration::from_stored(
        &schema.name,
        &schema.encoding,
        &schema.data,
        &channel.metadata,
    )?
    .ok()?
    // A stored layout is bounded by the largest message any stream may
    // have: the recording's own stream sizes aren't read here.
    .compile(MAX_MESSAGE_BYTES as usize)
    .ok()?;
    let fields = decoder.fields();
    let slots: Vec<Slot> = wanted
        .iter()
        .filter(|(_, reference)| reference.stream == channel.topic)
        .filter_map(|(role, reference)| {
            let position = fields
                .iter()
                .position(|field| field.name == reference.field)?;
            let before: usize = fields[..position].iter().map(|field| field.count).sum();
            let count = fields[position].count;
            let (first, width) = match reference.index {
                Some(index) if index < count => (before + index, 1),
                Some(_) => return None,
                None => (before, count),
            };
            Some(Slot {
                role: role.clone(),
                first,
                width,
            })
        })
        .collect();
    if slots.is_empty() {
        return None;
    }
    let values = vec![Value::default(); decoder.value_count()];
    Some(StreamRoles {
        decoder,
        slots,
        values,
    })
}
