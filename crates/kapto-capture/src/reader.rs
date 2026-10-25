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
//! [`read_events`] the events, by the recording's indexes where it has
//! them.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;

use kapto_core::{ErrorCode, KaptoError};
use kapto_store::{EVENTS_CHANNEL, Event};
use mcap::records::Record;
use mcap::sans_io::linear_reader::{LinearReadEvent, LinearReader, LinearReaderOptions};

/// The longest record a reader sets memory aside for whatever the file's
/// length: none Kapto writes is longer than its largest message (32 MiB, a
/// raw 4K frame) and a chunk of them, so twice that is plenty.
const LONGEST_RECORD: u64 = 64 << 20;

/// The longest record a reader of a file of `file_length` bytes trusts a
/// length for. A record's length comes from the file, and a damaged one's
/// would have the reader set aside terabytes: one stored as it is lies
/// inside the file, and one in a compressed chunk, which may be longer than
/// the file, is no longer than [`LONGEST_RECORD`].
pub(crate) fn record_length_limit(file_length: u64) -> usize {
    usize::try_from(file_length.max(LONGEST_RECORD)).unwrap_or(usize::MAX)
}

/// A recording read for an MCAP reader, which never sets aside room for
/// more than is left of the file: a reader asking for more is reading a
/// record whose length runs past the file's end.
pub(crate) struct Source {
    file: File,
    position: u64,
    length: u64,
}

impl Source {
    pub(crate) fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        Ok(Self {
            file,
            position: 0,
            length,
        })
    }

    /// The file's length when opened.
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// Reads up to `need` bytes into `reader`; `None` if fewer are left.
    fn read_into(
        &mut self,
        reader: &mut LinearReader,
        need: usize,
    ) -> Option<std::io::Result<usize>> {
        let need_bytes = u64::try_from(need).unwrap_or(u64::MAX);
        if self.length.saturating_sub(self.position) < need_bytes {
            // A recording still being written grows while it is read.
            self.length = self.file.metadata().map_or(self.length, |m| m.len());
            if self.length.saturating_sub(self.position) < need_bytes {
                return None;
            }
        }
        let read = self.file.read(reader.insert(need));
        if let Ok(read) = read {
            self.position += read as u64;
            reader.notify_read(read);
        }
        Some(read)
    }
}

/// Why a recording whose first record runs past the end of the file can't
/// be read at all.
const PAST_THE_END: &str = "it is empty or too short to be an MCAP recording, or its first record runs past the end of the file";

/// A linear reader for a file of `length` bytes, every record's length held
/// to what such a file can hold.
fn linear_reader(length: u64) -> LinearReader {
    LinearReader::new_with_options(
        LinearReaderOptions::default().with_record_length_limit(record_length_limit(length)),
    )
}

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
/// A finalised recording's events are found by its indexes, reading only
/// the chunks that hold them; one whose indexes can't be trusted to find
/// them all (cut off, salvaged, damaged) is read through instead, with the
/// same events either way.
///
/// # Errors
///
/// `recording.unreadable`, as for [`read_recording`].
pub fn read_events(path: &Path) -> Result<Vec<Event>, KaptoError> {
    if let Some(messages) = crate::seek::messages_by_index(path, EVENTS_CHANNEL) {
        return Ok(messages
            .iter()
            .filter_map(|data| Event::from_json(data))
            .collect());
    }
    read_events_through(path)
}

/// Every event the recording at `path` holds, read from start to end, as
/// [`read_events`] does for a recording without usable indexes.
///
/// # Errors
///
/// `recording.unreadable`, as for [`read_recording`].
pub fn read_events_through(path: &Path) -> Result<Vec<Event>, KaptoError> {
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
    let mut file = Source::open(path).map_err(|error| unreadable(error.to_string()))?;
    let mut reader = linear_reader(file.length());
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
            LinearReadEvent::ReadRequest(need) => match file.read_into(&mut reader, need) {
                Some(read) => {
                    read.map_err(|error| unreadable(error.to_string()))?;
                }
                // A record running past the end: what came before is kept,
                // as for a recording cut off part-way.
                None if read_anything => {
                    summary.cut_off = true;
                    break;
                }
                None => return Err(unreadable(PAST_THE_END.to_owned())),
            },
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
    let mut file = Source::open(path).map_err(|error| unreadable(error.to_string()))?;
    let mut reader = linear_reader(file.length());
    let mut start = RecordingStart::default();
    let mut read_anything = false;
    while let Some(event) = reader.next_event() {
        match event {
            Ok(LinearReadEvent::ReadRequest(need)) => match file.read_into(&mut reader, need) {
                Some(read) => {
                    read.map_err(|error| unreadable(error.to_string()))?;
                }
                None if read_anything => break,
                None => return Err(unreadable(PAST_THE_END.to_owned())),
            },
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

    use mcap::sans_io::summary_reader::{SummaryReadEvent, SummaryReader, SummaryReaderOptions};

    let mut file = File::open(path).ok()?;
    // Its size holds every record's length to what is left of the file.
    let size = file.metadata().ok()?.len();
    let mut reader =
        SummaryReader::new_with_options(SummaryReaderOptions::default().with_file_size(size));
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
