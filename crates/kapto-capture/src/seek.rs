// SPDX-License-Identifier: Apache-2.0

//! Reading one channel of a finalised recording by its MCAP indexes rather
//! than from start to end: every message's receive time without reading a
//! message, and any one message by its position, reading only the chunk
//! that holds it. A camera's frames are read this way to replay them.
//!
//! The file is read in pieces: its summary at the end, each chunk's small
//! index of the channel's messages, and one chunk per message asked for,
//! so a recording of any size is read in bounded memory. Every length and
//! offset the indexes give is checked against the file before anything is
//! set aside for it: a damaged recording is refused, never read into
//! memory it doesn't hold.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use mcap::records::{ChunkIndex, MessageIndexEntry, Record, op};
use mcap::sans_io::linear_reader::{LinearReadEvent, LinearReader, LinearReaderOptions};
use mcap::sans_io::summary_reader::{SummaryReadEvent, SummaryReader, SummaryReaderOptions};
use mcap::{Summary, parse_record};

use crate::reader::record_length_limit;

/// One channel's messages, found by the recording's indexes.
#[derive(Debug)]
pub struct ChannelIndex {
    path: PathBuf,
    /// The file's length when its indexes were read.
    length: u64,
    summary: Summary,
    /// Every message, in receive-time order.
    messages: Vec<Indexed>,
}

/// Where one message is.
#[derive(Clone, Copy, Debug)]
struct Indexed {
    /// When it was received, in UTC nanoseconds.
    log_time: u64,
    /// The position of its chunk in the summary's chunk indexes.
    chunk: usize,
    /// Where it starts in its chunk, decompressed.
    offset: u64,
}

impl ChannelIndex {
    /// Finds every message of the channel named `topic` in the recording
    /// at `path`, reading only the recording's indexes.
    ///
    /// # Errors
    ///
    /// `recording.unreadable` if the file can't be read, isn't an MCAP
    /// recording, has no indexes (as a recording cut off before it was
    /// finished doesn't), or has no channel named `topic`.
    pub fn open(path: &Path, topic: &str) -> Result<Self, KaptoError> {
        let unreadable = |detail: String| {
            KaptoError::new(ErrorCode::RecordingUnreadable)
                .detail(detail)
                .context("file", path.display().to_string())
        };
        let mut file = File::open(path).map_err(|error| unreadable(error.to_string()))?;
        let length = file
            .metadata()
            .map_err(|error| unreadable(error.to_string()))?
            .len();
        let cut_off = || {
            unreadable(
                "it has no index to find messages by: it was cut off before it was finished"
                    .to_owned(),
            )
        };
        let summary = match read_summary(&mut file) {
            Ok(Some(summary)) => summary,
            // Without the closing magic, the file ends where it was cut.
            Ok(None) | Err(mcap::McapError::BadMagic) => return Err(cut_off()),
            Err(error) => return Err(unreadable(error.to_string())),
        };
        let channel = summary
            .channels
            .values()
            .find(|channel| channel.topic == topic)
            .map(|channel| channel.id)
            .ok_or_else(|| unreadable(format!("it has no channel named `{topic}`")))?;
        let mut messages = Vec::new();
        for (chunk, index) in summary.chunk_indexes.iter().enumerate() {
            let Some(&offset) = index.message_index_offsets.get(&channel) else {
                continue;
            };
            let entries = read_message_index(&mut file, length, offset, channel)
                .map_err(|error| unreadable(format!("its index of a chunk: {error}")))?;
            messages.extend(entries.into_iter().map(|entry| Indexed {
                log_time: entry.log_time,
                chunk,
                offset: entry.offset,
            }));
        }
        // Chunks are in file order; a message received a little before one
        // written ahead of it sorts by when it was received.
        messages.sort_by_key(|message| (message.log_time, message.chunk, message.offset));
        Ok(Self {
            path: path.to_path_buf(),
            length,
            summary,
            messages,
        })
    }

    /// How many messages the channel has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the channel has none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Each message's receive time, in UTC nanoseconds, in order.
    pub fn times(&self) -> impl Iterator<Item = u64> + '_ {
        self.messages.iter().map(|message| message.log_time)
    }

    /// The position of the newest message received at or before
    /// `utc_ns`; `None` if all came after.
    #[must_use]
    pub fn at(&self, utc_ns: u64) -> Option<usize> {
        self.messages
            .partition_point(|message| message.log_time <= utc_ns)
            .checked_sub(1)
    }

    /// Message `n`, as received, with its receive time; `None` if there are
    /// fewer.
    ///
    /// # Errors
    ///
    /// `recording.unreadable` if its chunk can't be read, or the recording's
    /// indexes put it where the file can't hold it.
    pub fn read(&self, n: usize) -> Result<Option<(u64, Vec<u8>)>, KaptoError> {
        let Some(message) = self.messages.get(n) else {
            return Ok(None);
        };
        let unreadable = |detail: String| {
            KaptoError::new(ErrorCode::RecordingUnreadable)
                .detail(detail)
                .context("file", self.path.display().to_string())
        };
        let index = self
            .summary
            .chunk_indexes
            .get(message.chunk)
            .ok_or_else(|| unreadable("its index of chunks has no such chunk".to_owned()))?;
        let chunk = read_chunk(&self.path, self.length, index).map_err(unreadable)?;
        let found = message_in_chunk(&chunk, message.offset, record_length_limit(self.length))
            .map_err(|error| unreadable(format!("a message in its chunk: {error}")))?;
        Ok(Some(found))
    }
}

/// A record's opcode and length before its content: one byte and eight.
const RECORD_HEADER: u64 = 9;

/// The message starting `offset` bytes into the records of `chunk`, a whole
/// chunk record, with its receive time. Each record's length inside it is
/// held to `limit` before anything is set aside for it.
fn message_in_chunk(chunk: &[u8], offset: u64, limit: usize) -> Result<(u64, Vec<u8>), String> {
    let mut reader = chunk_reader(limit);
    let mut remaining = chunk;
    // Where the next record starts among the chunk's records, decompressed.
    let mut at = 0_u64;
    while let Some(event) = reader.next_event() {
        match event.map_err(|error| error.to_string())? {
            LinearReadEvent::ReadRequest(need) => {
                // Never more than the chunk holds: past it, the reader
                // finds the chunk ends early.
                let len = need.min(remaining.len());
                reader.insert(len).copy_from_slice(&remaining[..len]);
                reader.notify_read(len);
                remaining = &remaining[len..];
            }
            LinearReadEvent::Record { opcode, data } => {
                if at == offset {
                    return match parse_record(opcode, data).map_err(|error| error.to_string())? {
                        Record::Message { header, data } => {
                            Ok((header.log_time, data.into_owned()))
                        }
                        _ => Err("its index points at a record that isn't a message".to_owned()),
                    };
                }
                at = at.saturating_add(RECORD_HEADER + data.len() as u64);
                if at > offset {
                    break;
                }
            }
        }
    }
    Err("no message starts where its index says".to_owned())
}

/// The chunk record `index` points at, read whole from the file at `path`
/// of `length` bytes.
fn read_chunk(path: &Path, length: u64, index: &ChunkIndex) -> Result<Vec<u8>, String> {
    // The summary's word for where the chunk is and how long: a damaged
    // recording's could ask for more than the disk holds.
    let start = index.chunk_start_offset;
    let fits = start
        .checked_add(index.chunk_length)
        .is_some_and(|end| end <= length);
    if !fits || index.chunk_length < RECORD_HEADER {
        return Err(format!(
            "its index puts a chunk of {} bytes at byte {start}, which a file of {length} bytes can't hold: the recording is damaged",
            index.chunk_length
        ));
    }
    let mut chunk = vec![0; to_usize(index.chunk_length)];
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut chunk))
        .map_err(|error| format!("its chunk: {error}"))?;
    Ok(chunk)
}

/// A reader of the records inside one whole chunk record, each record's
/// length held to `limit` before anything is set aside for it.
fn chunk_reader(limit: usize) -> LinearReader {
    LinearReader::new_with_options(
        LinearReaderOptions::default()
            .with_skip_start_magic(true)
            .with_skip_end_magic(true)
            .with_validate_chunk_crcs(true)
            .with_record_length_limit(limit),
    )
}

/// Every message of the channels named `topic` in the finalised recording
/// at `path`, as received, in file order, read from only the chunks its
/// indexes say hold them: a small channel among cameras' frames is read
/// without decompressing the frames.
///
/// `None` when the indexes can't be trusted to find all of them, so the
/// caller reads the file through instead: no summary (a recording cut off
/// or salvaged), no statistics counting every message by channel, no such
/// channel, a chunk that can't be read, or fewer or more messages found
/// than the statistics count.
pub(crate) fn messages_by_index(path: &Path, topic: &str) -> Option<Vec<Vec<u8>>> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    let summary = read_summary(&mut file).ok()??;
    let stats = summary.stats.as_ref()?;
    // Counts by channel that add up to every message: none can be outside
    // the channels counted, e.g. in no chunk at all.
    let counted = stats
        .channel_message_counts
        .values()
        .try_fold(0_u64, |sum, count| sum.checked_add(*count))?;
    if counted != stats.message_count {
        return None;
    }
    let channels: BTreeSet<u16> = summary
        .channels
        .values()
        .filter(|channel| channel.topic == topic)
        .map(|channel| channel.id)
        .collect();
    if channels.is_empty() {
        return None;
    }
    let expected = channels
        .iter()
        .filter_map(|id| stats.channel_message_counts.get(id))
        .try_fold(0_u64, |sum, count| sum.checked_add(*count))?;
    // A read through stops at a chunk whose own header doesn't match where
    // the summary puts it, losing every event after it; the indexes would
    // find those, so such a file is read through too, with the same result.
    // Chunks are listed in file order, none overlapping: a damaged summary
    // can't list one twice.
    let mut previous_end = 0;
    for index in &summary.chunk_indexes {
        if index.chunk_start_offset < previous_end {
            return None;
        }
        previous_end = index.chunk_start_offset.checked_add(index.chunk_length)?;
        let mut header = [0; 9];
        file.seek(SeekFrom::Start(index.chunk_start_offset))
            .and_then(|_| file.read_exact(&mut header))
            .ok()?;
        let mut size = [0; 8];
        size.copy_from_slice(&header[1..]);
        if header[0] != op::CHUNK
            || Some(u64::from_le_bytes(size)) != index.chunk_length.checked_sub(RECORD_HEADER)
        {
            return None;
        }
    }
    let chunks = summary.chunk_indexes.iter().filter(|index| {
        channels
            .iter()
            .any(|id| index.message_index_offsets.contains_key(id))
    });
    let limit = record_length_limit(length);
    let mut messages = Vec::new();
    for index in chunks {
        let chunk = read_chunk(path, length, index).ok()?;
        messages_in_chunk(&chunk, &channels, limit, &mut messages).ok()?;
        // Never more than counted: a damaged index can't grow this past
        // what the statistics promise.
        if messages.len() as u64 > expected {
            return None;
        }
    }
    (messages.len() as u64 == expected).then_some(messages)
}

/// Appends to `messages` every message of `channels` in `chunk`, a whole
/// chunk record, in the order they were written.
fn messages_in_chunk(
    chunk: &[u8],
    channels: &BTreeSet<u16>,
    limit: usize,
    messages: &mut Vec<Vec<u8>>,
) -> Result<(), String> {
    let mut reader = chunk_reader(limit);
    let mut remaining = chunk;
    while let Some(event) = reader.next_event() {
        match event.map_err(|error| error.to_string())? {
            LinearReadEvent::ReadRequest(need) => {
                // The whole chunk is in: its records have all been read.
                if remaining.is_empty() {
                    return Ok(());
                }
                let len = need.min(remaining.len());
                reader.insert(len).copy_from_slice(&remaining[..len]);
                reader.notify_read(len);
                remaining = &remaining[len..];
            }
            LinearReadEvent::Record { opcode, data } => {
                if opcode != op::MESSAGE {
                    continue;
                }
                if let Record::Message { header, data } =
                    parse_record(opcode, data).map_err(|error| error.to_string())?
                    && channels.contains(&header.channel_id)
                {
                    messages.push(data.into_owned());
                }
            }
        }
    }
    Ok(())
}

fn to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// The recording's summary, read from its end; `None` if it has none.
fn read_summary(file: &mut File) -> Result<Option<Summary>, mcap::McapError> {
    let size = file.metadata()?.len();
    let mut reader =
        SummaryReader::new_with_options(SummaryReaderOptions::default().with_file_size(size));
    while let Some(event) = reader.next_event() {
        match event? {
            SummaryReadEvent::ReadRequest(n) => {
                let read = file.read(reader.insert(n))?;
                reader.notify_read(read);
            }
            SummaryReadEvent::SeekRequest(to) => {
                let at = file.seek(to)?;
                reader.notify_seeked(at);
            }
        }
    }
    Ok(reader.finish())
}

/// The message index record at `offset`, of `channel`, in a file of
/// `length` bytes.
fn read_message_index(
    file: &mut File,
    length: u64,
    offset: u64,
    channel: u16,
) -> io::Result<Vec<MessageIndexEntry>> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut header = [0; 9];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut header)?;
    if header[0] != op::MESSAGE_INDEX {
        return Err(bad("not a message index where the summary says"));
    }
    let mut size = [0; 8];
    size.copy_from_slice(&header[1..]);
    let size = u64::from_le_bytes(size);
    let left = length.saturating_sub(offset.saturating_add(RECORD_HEADER));
    if size > left {
        return Err(bad("a message index running past the end of the file"));
    }
    let mut body = vec![0; to_usize(size)];
    file.read_exact(&mut body)?;
    match parse_record(op::MESSAGE_INDEX, &body) {
        Ok(Record::MessageIndex(index)) if index.channel_id == channel => Ok(index.records),
        Ok(_) => Err(bad("a message index of another channel")),
        Err(error) => Err(bad(&error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kapto_core::{ClockMap, ClockSample, RecvTime};
    use kapto_store::{EVENTS_CHANNEL, Event, Phase};
    use kapto_testkit::TempDir;

    use super::*;
    use crate::{Batch, ChannelSpec, LossCounters, Recording, WriterConfig};

    #[test]
    fn a_finished_recording_has_its_events_found_by_its_indexes_and_a_cut_one_not() {
        let folder = TempDir::new("seek-events-by-index");
        let path = folder.0.join("recording.mcap");
        let clock = ClockMap::new(ClockSample::now());
        let streams = [(0, "arm".parse().unwrap())].map(ChannelSpec::from);
        let config = WriterConfig {
            chunk_bytes: 256,
            ..WriterConfig::default()
        };
        let mut recording = Recording::create(&path, &streams, &clock, &config).unwrap();
        let counters = LossCounters::new(1);
        let mut marked = Vec::new();
        for step in 0..50_u64 {
            let mut batch = Batch::new(1, 64);
            batch.push(0, &[7; 64]);
            batch.set_recv_time(RecvTime::from_nanos(step + 1));
            recording.write(&batch, &counters).unwrap();
            if step % 10 == 0 {
                let event = Event {
                    kind: "note".to_owned(),
                    at_utc_ns: i64::try_from(step).unwrap(),
                    until_utc_ns: None,
                    phase: Phase::Moment,
                    id: None,
                    source: "page".to_owned(),
                    text: None,
                    attributes: BTreeMap::new(),
                };
                recording.write_event(&event).unwrap();
                marked.push(event.to_json());
            }
        }
        recording.finish(&clock).unwrap();
        assert_eq!(messages_by_index(&path, EVENTS_CHANNEL), Some(marked));

        let whole = std::fs::read(&path).unwrap();
        std::fs::write(&path, &whole[..whole.len() - 8]).unwrap();
        assert_eq!(messages_by_index(&path, EVENTS_CHANNEL), None);
    }
}
