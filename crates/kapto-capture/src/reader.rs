// SPDX-License-Identifier: Apache-2.0

//! Reading a recording back, message by message, and what it says about
//! itself at its start.
//!
//! The file is read in pieces, so recordings of any size can be read without
//! loading them into memory. Compressed chunks are decompressed as they come.
//! A recording cut off by a crash is read up to where it stops.
//!
//! A recording's streams and Kapto's own channels (named `kapto.…`, such as
//! its events) are read apart: [`read_recording`] hands over only streams,
//! as everything that decodes, checks or replays them expects, and
//! [`read_events`] the events.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{EVENTS_CHANNEL, Event};
use mcap::records::Record;
use mcap::sans_io::linear_reader::{LinearReadEvent, LinearReader};

/// A channel of the recording: one stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedChannel {
    /// The stream's name.
    pub topic: String,
    /// How its payloads are laid out, e.g. `kapto.struct/v1` or `kapto.raw`.
    pub message_encoding: String,
    /// The schema stored with it, if any.
    pub schema: Option<RecordedSchema>,
    /// The channel's metadata, e.g. a ROS 2 stream's fields.
    pub metadata: BTreeMap<String, String>,
}

/// A schema stored in the recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedSchema {
    /// Its name.
    pub name: String,
    /// The format of `data`.
    pub encoding: String,
    /// The schema itself.
    pub data: Vec<u8>,
}

/// One recorded message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedMessage<'a> {
    /// Its position in its channel, counting from 0.
    pub sequence: u32,
    /// When it was received, in UTC nanoseconds since the Unix epoch.
    pub log_time: u64,
    /// When its source sent it, as the source had it; its receive time if
    /// the source gave none.
    pub publish_time: u64,
    /// The payload exactly as received.
    pub data: &'a [u8],
}

/// How much of the file was read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadSummary {
    /// Messages passed to the callback.
    pub messages: u64,
    /// Whether the file ended before its closing records, as a recording
    /// cut off by a crash does. Every complete message before that point was
    /// read.
    pub cut_off: bool,
}

/// Reads every channel's messages from the recording at `path`, in file
/// order, calling `on_message` for each until it returns `Break`.
///
/// # Errors
///
/// `recording.unreadable` if the file can't be opened or isn't an MCAP
/// recording.
pub fn read_recording(
    path: &Path,
    on_message: impl FnMut(&RecordedChannel, RecordedMessage<'_>) -> ControlFlow<()>,
) -> Result<ReadSummary, KaptoError> {
    read_channels(path, |topic| !is_kapto_channel(topic), on_message)
}

/// Whether a channel named `topic` is one of Kapto's own rather than a
/// stream: stream IDs never hold a dot.
#[must_use]
pub fn is_kapto_channel(topic: &str) -> bool {
    topic.starts_with("kapto.")
}

/// Every event the recording at `path` holds, in the order they were
/// written; an event that can't be read is left out. A recording made
/// before events were recorded has none.
///
/// # Errors
///
/// `recording.unreadable`, as for [`read_recording`].
pub fn read_events(path: &Path) -> Result<Vec<Event>, KaptoError> {
    let mut events = Vec::new();
    read_channels(
        path,
        |topic| topic == EVENTS_CHANNEL,
        |_, message| {
            events.extend(Event::from_json(message.data));
            ControlFlow::Continue(())
        },
    )?;
    Ok(events)
}

/// Reads the messages of every channel `wanted` names, in file order,
/// calling `on_message` for each until it returns `Break`.
///
/// # Errors
///
/// `recording.unreadable`, as for [`read_recording`].
pub fn read_channels(
    path: &Path,
    wanted: impl Fn(&str) -> bool,
    mut on_message: impl FnMut(&RecordedChannel, RecordedMessage<'_>) -> ControlFlow<()>,
) -> Result<ReadSummary, KaptoError> {
    let unreadable = |detail: String| {
        KaptoError::new(ErrorCode::RecordingUnreadable)
            .detail(detail)
            .context("file", path.display().to_string())
    };
    let mut file = File::open(path).map_err(|error| unreadable(error.to_string()))?;
    let mut reader = LinearReader::new();
    let mut schemas: BTreeMap<u16, RecordedSchema> = BTreeMap::new();
    let mut channels: BTreeMap<u16, RecordedChannel> = BTreeMap::new();
    let mut summary = ReadSummary::default();
    let mut read_anything = false;

    while let Some(event) = reader.next_event() {
        let event = match event {
            Ok(event) => event,
            // A file that started as a valid recording and then stops is one
            // cut off part-way: keep what was read.
            Err(_) if read_anything => {
                summary.cut_off = true;
                break;
            }
            Err(error) => return Err(unreadable(error.to_string())),
        };
        match event {
            LinearReadEvent::ReadRequest(need) => {
                let read = file
                    .read(reader.insert(need))
                    .map_err(|error| unreadable(error.to_string()))?;
                reader.notify_read(read);
            }
            LinearReadEvent::Record { opcode, data } => {
                read_anything = true;
                let Ok(record) = mcap::parse_record(opcode, data) else {
                    summary.cut_off = true;
                    break;
                };
                match record {
                    Record::Schema { header, data } => {
                        schemas.entry(header.id).or_insert_with(|| RecordedSchema {
                            name: header.name,
                            encoding: header.encoding,
                            data: data.into_owned(),
                        });
                    }
                    Record::Channel(channel) => {
                        let schema = schemas.get(&channel.schema_id).cloned();
                        channels.entry(channel.id).or_insert(RecordedChannel {
                            topic: channel.topic,
                            message_encoding: channel.message_encoding,
                            schema,
                            metadata: channel.metadata,
                        });
                    }
                    Record::Message { header, data } => {
                        let Some(channel) = channels
                            .get(&header.channel_id)
                            .filter(|channel| wanted(&channel.topic))
                        else {
                            continue;
                        };
                        summary.messages += 1;
                        let message = RecordedMessage {
                            sequence: header.sequence,
                            log_time: header.log_time,
                            publish_time: header.publish_time,
                            data: &data,
                        };
                        if on_message(channel, message).is_break() {
                            return Ok(summary);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if !read_anything {
        return Err(unreadable("the file is empty".to_owned()));
    }
    Ok(summary)
}

/// What a recording says about itself from its start: its metadata records
/// and attached files, all written before its first message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordingStart {
    /// Each metadata record's entries, by the record's name.
    pub records: BTreeMap<String, BTreeMap<String, String>>,
    /// Each attached file's contents, by its name.
    pub attachments: BTreeMap<String, Vec<u8>>,
}

/// Reads the records and attachments at the start of the recording at
/// `path`, stopping at its first message, so only the start of a large
/// recording is read. A recording made before they were written has none.
///
/// # Errors
///
/// `recording.unreadable` if the file can't be opened or isn't an MCAP
/// recording.
pub fn read_start(path: &Path) -> Result<RecordingStart, KaptoError> {
    let unreadable = |detail: String| {
        KaptoError::new(ErrorCode::RecordingUnreadable)
            .detail(detail)
            .context("file", path.display().to_string())
    };
    let mut file = File::open(path).map_err(|error| unreadable(error.to_string()))?;
    let mut reader = LinearReader::new();
    let mut start = RecordingStart::default();
    let mut read_anything = false;
    while let Some(event) = reader.next_event() {
        match event {
            Ok(LinearReadEvent::ReadRequest(need)) => {
                let read = file
                    .read(reader.insert(need))
                    .map_err(|error| unreadable(error.to_string()))?;
                reader.notify_read(read);
            }
            Ok(LinearReadEvent::Record { opcode, data }) => {
                read_anything = true;
                match mcap::parse_record(opcode, data) {
                    Ok(Record::Metadata(metadata)) => {
                        start.records.insert(metadata.name, metadata.metadata);
                    }
                    Ok(Record::Attachment { header, data, .. }) => {
                        start.attachments.insert(header.name, data.into_owned());
                    }
                    Ok(Record::Message { .. } | Record::DataEnd(_) | Record::Footer(_))
                    | Err(_) => break,
                    Ok(_) => {}
                }
            }
            // Cut off before its first message: what was read is the start.
            Err(_) if read_anything => break,
            Err(error) => return Err(unreadable(error.to_string())),
        }
    }
    if !read_anything {
        return Err(unreadable("the file is empty".to_owned()));
    }
    Ok(start)
}

/// One channel and how many messages it holds, when the recording says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSummary {
    /// The channel.
    pub channel: RecordedChannel,
    /// Its message count from the recording's summary; `None` for a
    /// recording cut off before its summary was written.
    pub messages: Option<u64>,
}

/// Every channel of the recording, in the order they were created. Read from
/// the summary at the end of the file, without reading any message; a
/// recording cut off before its summary is scanned from the start instead.
///
/// # Errors
///
/// `recording.unreadable`, as for [`read_recording`].
pub fn recorded_channels(path: &Path) -> Result<Vec<ChannelSummary>, KaptoError> {
    if let Some(channels) = channels_from_summary(path) {
        return Ok(channels);
    }
    let mut channels: Vec<ChannelSummary> = Vec::new();
    read_recording(path, |channel, _| {
        if !channels
            .iter()
            .any(|known| known.channel.topic == channel.topic)
        {
            channels.push(ChannelSummary {
                channel: channel.clone(),
                messages: None,
            });
        }
        ControlFlow::Continue(())
    })?;
    Ok(channels)
}

/// The channels listed in the summary section, or `None` if there is none
/// (or it can't be read).
fn channels_from_summary(path: &Path) -> Option<Vec<ChannelSummary>> {
    use std::io::Seek;

    use mcap::sans_io::summary_reader::{SummaryReadEvent, SummaryReader};

    let mut file = File::open(path).ok()?;
    let mut reader = SummaryReader::new();
    while let Some(event) = reader.next_event() {
        match event.ok()? {
            SummaryReadEvent::ReadRequest(need) => {
                let read = file.read(reader.insert(need)).ok()?;
                reader.notify_read(read);
            }
            SummaryReadEvent::SeekRequest(to) => {
                let position = file.seek(to).ok()?;
                reader.notify_seeked(position);
            }
        }
    }
    let summary = reader.finish()?;
    let counts = summary
        .stats
        .as_ref()
        .map(|stats| stats.channel_message_counts.clone());
    let mut channels: Vec<_> = summary
        .channels
        .values()
        .filter(|channel| !is_kapto_channel(&channel.topic))
        .collect();
    channels.sort_by_key(|channel| channel.id);
    Some(
        channels
            .into_iter()
            .map(|channel| ChannelSummary {
                channel: RecordedChannel {
                    topic: channel.topic.clone(),
                    message_encoding: channel.message_encoding.clone(),
                    schema: channel.schema.as_ref().map(|schema| RecordedSchema {
                        name: schema.name.clone(),
                        encoding: schema.encoding.clone(),
                        data: schema.data.to_vec(),
                    }),
                    metadata: channel.metadata.clone(),
                },
                messages: counts
                    .as_ref()
                    .and_then(|counts| counts.get(&channel.id).copied()),
            })
            .collect(),
    )
}
