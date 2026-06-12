// SPDX-License-Identifier: Apache-2.0

//! Reading one channel of a finalised recording by its MCAP indexes rather
//! than from start to end: every message's receive time without reading a
//! message, and any one message by its position, reading only the chunk
//! that holds it. A camera's frames are read this way to replay them.
//!
//! The file is read in pieces: its summary at the end, each chunk's small
//! index of the channel's messages, and one chunk per message asked for,
//! so a recording of any size is read in bounded memory.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use kapto_core::{ErrorCode, KaptoError};
use mcap::records::{ChunkIndex, MessageIndexEntry, Record, op};
use mcap::sans_io::summary_reader::{SummaryReadEvent, SummaryReader, SummaryReaderOptions};
use mcap::{Summary, parse_record};

/// One channel's messages, found by the recording's indexes.
#[derive(Debug)]
pub struct ChannelIndex {
    path: PathBuf,
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
            let entries = read_message_index(&mut file, offset, channel)
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
    /// `recording.unreadable` if its chunk can't be read.
    pub fn read(&self, n: usize) -> Result<Option<(u64, Vec<u8>)>, KaptoError> {
        let Some(message) = self.messages.get(n) else {
            return Ok(None);
        };
        let unreadable = |detail: String| {
            KaptoError::new(ErrorCode::RecordingUnreadable)
                .detail(detail)
                .context("file", self.path.display().to_string())
        };
        let index = &self.summary.chunk_indexes[message.chunk];
        let mut chunk = vec![0; to_usize(index.chunk_length)];
        let mut file = File::open(&self.path).map_err(|error| unreadable(error.to_string()))?;
        file.seek(SeekFrom::Start(index.chunk_start_offset))
            .and_then(|_| file.read_exact(&mut chunk))
            .map_err(|error| unreadable(format!("its chunk: {error}")))?;
        // The chunk read on its own: its offsets count from its start.
        let local = ChunkIndex {
            chunk_start_offset: 0,
            ..index.clone()
        };
        let entry = MessageIndexEntry {
            log_time: message.log_time,
            offset: message.offset,
        };
        let found = self
            .summary
            .seek_message(&chunk, &local, &entry)
            .map_err(|error| unreadable(format!("a message in its chunk: {error}")))?;
        Ok(Some((found.log_time, found.data.into_owned())))
    }
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

/// The message index record at `offset`, of `channel`.
fn read_message_index(
    file: &mut File,
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
    let mut length = [0; 8];
    length.copy_from_slice(&header[1..]);
    let mut body = vec![0; to_usize(u64::from_le_bytes(length))];
    file.read_exact(&mut body)?;
    match parse_record(op::MESSAGE_INDEX, &body) {
        Ok(Record::MessageIndex(index)) if index.channel_id == channel => Ok(index.records),
        Ok(_) => Err(bad("a message index of another channel")),
        Err(error) => Err(bad(&error.to_string())),
    }
}
