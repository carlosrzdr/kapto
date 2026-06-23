// SPDX-License-Identifier: Apache-2.0

//! The writer: stores every received message, exactly as received, in the
//! episode's MCAP recording.
//!
//! Each stream gets one channel, named after the stream. A message's
//! `log_time` is its receive time mapped to UTC through the episode's
//! [`ClockMap`], so it never jumps within an episode. The clock map's samples
//! are stored at the end in a `kapto.clock_map` metadata record, so the exact
//! mapping can be recovered. Before the first message, the recording gets
//! the records that say what it is ([`Recording::write_record`]), so even
//! one cut off part-way identifies itself.
//!
//! Messages are written into compressed chunks. On a timer, the writer closes
//! the current chunk and asks the operating system to put the file on disk,
//! which bounds how much a power cut can lose.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use kapto_core::clock::monotonic_now;
use kapto_core::{ClockMap, ErrorCode, KaptoError, RecvTime, StreamId};
use kapto_store::records::flatten;
use kapto_store::{EVENTS_CHANNEL, Event, event_schema};
use mcap::McapError;
use mcap::records::{MessageHeader, Metadata};
use serde::Serialize;

use crate::batch::{Batch, StreamIndex};
use crate::durability::{DurabilityStats, DurabilityTracker};
use crate::kept::Kept;
use crate::loss::LossCounters;
use crate::post_roll::PostRoll;
use crate::pre_roll::PreRoll;
use crate::ring::{Drainer, RingConfig};

/// How messages are compressed in the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// Not compressed.
    None,
    /// LZ4: fast, moderate size.
    Lz4,
    /// Zstandard: smaller, slower as the level rises.
    Zstd,
}

/// How a recording is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriterConfig {
    /// How chunks are compressed.
    pub compression: Compression,
    /// The compressor's level; 0 is its own default.
    pub compression_level: u32,
    /// Uncompressed bytes of messages per chunk before a new one starts.
    pub chunk_bytes: u64,
    /// How often written data is put on disk. A power cut loses at most
    /// about this much.
    pub sync_interval: Duration,
    /// How long the writer thread waits when no batch is waiting.
    pub idle_wait: Duration,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            compression: Compression::Lz4,
            compression_level: 0,
            chunk_bytes: 1024 * 1024,
            sync_interval: Duration::from_millis(500),
            idle_wait: Duration::from_millis(1),
        }
    }
}

/// Decides when written data is due to be put on disk.
#[derive(Clone, Copy, Debug)]
pub struct SyncTimer {
    interval: Duration,
    last: RecvTime,
}

impl SyncTimer {
    /// A timer that first falls due `interval` after `start`.
    #[must_use]
    pub fn new(interval: Duration, start: RecvTime) -> Self {
        Self {
            interval,
            last: start,
        }
    }

    /// Whether a sync is due at `now`. When it is, the next one falls due
    /// `interval` after `now`.
    pub fn due(&mut self, now: RecvTime) -> bool {
        let due = now.saturating_since(self.last) >= self.interval;
        if due {
            self.last = now;
        }
        due
    }
}

/// The message encoding of channels whose payloads are stored as received,
/// without a schema.
pub const RAW_ENCODING: &str = "kapto.raw";

/// How a stream's payloads can be decoded, stored with its channel so the
/// recording can be read without the configuration it was made with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSchema {
    /// The channel's message encoding: how its payloads are laid out.
    pub message_encoding: String,
    /// The schema's name.
    pub name: String,
    /// The schema's encoding: the format of `data`.
    pub encoding: String,
    /// The schema itself.
    pub data: Vec<u8>,
    /// What the channel's metadata holds besides, e.g. a ROS 2 stream's
    /// fields.
    pub metadata: BTreeMap<String, String>,
}

/// One stream's channel in a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSpec {
    /// The stream the channel records.
    pub stream: StreamIndex,
    /// The stream's name, used as the channel's topic.
    pub id: StreamId,
    /// How its payloads can be decoded; `None` stores them as `kapto.raw`.
    pub schema: Option<ChannelSchema>,
}

impl From<(StreamIndex, StreamId)> for ChannelSpec {
    /// A channel whose payloads are stored without a schema.
    fn from((stream, id): (StreamIndex, StreamId)) -> Self {
        Self {
            stream,
            id,
            schema: None,
        }
    }
}

/// The name of the metadata record holding the clock map.
pub const CLOCK_MAP_RECORD: &str = "kapto.clock_map";

/// The name of the metadata record saying when each message an episode
/// starts with, kept from before it, was received.
pub const CARRIED_RECORD: &str = "kapto.carried";

/// The `kapto.carried` record's entries: under each stream's name, the
/// receive times in UTC nanoseconds of the kept messages it starts with,
/// as a JSON list in the order they were written.
#[must_use]
pub fn carried_record(carried: &[(&str, i64)]) -> BTreeMap<String, String> {
    let mut times: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for (stream, received_utc_ns) in carried {
        times
            .entry((*stream).to_owned())
            .or_default()
            .push(*received_utc_ns);
    }
    times
        .into_iter()
        .map(|(stream, times)| {
            let list = times.iter().map(i64::to_string).collect::<Vec<_>>();
            (stream, format!("[{}]", list.join(",")))
        })
        .collect()
}

/// The `kapto.capture` record's entries: how the recording is written
/// (`writer.*`), the buffers of each input, under the name of the thread
/// that receives it (`buffers.<thread>.*`), whether it carries the
/// package's meshes (`attach_meshes`), and the SHA-256 of each package file
/// it doesn't carry, under its path (`left_out.<path>`).
#[must_use]
pub fn capture_record(
    config: &WriterConfig,
    buffers: &[(&str, &RingConfig)],
    attach_meshes: bool,
    left_out: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    #[derive(Serialize)]
    struct Settings<'a> {
        writer: WriterSettings,
        buffers: BTreeMap<&'a str, BufferSettings>,
        attach_meshes: bool,
    }
    #[derive(Serialize)]
    struct WriterSettings {
        compression: &'static str,
        compression_level: u32,
        chunk_bytes: u64,
        sync_interval_ns: u128,
    }
    #[derive(Serialize)]
    struct BufferSettings {
        batches: usize,
        batch_capacity: usize,
        max_message_len: usize,
        memory_bytes: usize,
    }
    let mut entries = flatten(&Settings {
        writer: WriterSettings {
            compression: match config.compression {
                Compression::None => "none",
                Compression::Lz4 => "lz4",
                Compression::Zstd => "zstd",
            },
            compression_level: config.compression_level,
            chunk_bytes: config.chunk_bytes,
            sync_interval_ns: config.sync_interval.as_nanos(),
        },
        buffers: buffers
            .iter()
            .map(|(thread, ring)| {
                let settings = BufferSettings {
                    batches: ring.batches,
                    batch_capacity: ring.batch_capacity,
                    max_message_len: ring.max_message_len,
                    memory_bytes: ring.memory_bytes(),
                };
                (*thread, settings)
            })
            .collect(),
        attach_meshes,
    });
    // Added by hand: a path's own dots would read as nesting to `flatten`.
    for (path, sha256) in left_out {
        entries.insert(format!("left_out.{path}"), sha256.clone());
    }
    entries
}

/// One episode's MCAP file, being written.
pub struct Recording {
    /// `None` once a write has failed: see [`abandon`].
    mcap: Option<McapWriter>,
    /// The cause code of the first failure, repeated by later calls.
    failure: Option<ErrorCode>,
    /// A second handle on the same file, because the MCAP writer doesn't
    /// give its own back until it is finished.
    file: File,
    path: PathBuf,
    channels: Box<[Option<Channel>]>,
    /// The channel events are written on.
    events: Channel,
    clock: ClockMap,
    sync: SyncTimer,
    durability: DurabilityTracker,
    idle_wait: Duration,
    /// When the episode was stopped, after which each stream's messages
    /// past its post-roll are left out.
    post_roll: PostRoll,
    /// Where each batch's newest messages are offered for the viewer.
    #[cfg(unix)]
    tap: Option<crate::tap::TapFeed>,
}

type McapWriter = mcap::Writer<BufWriter<File>>;

/// Gives up on an MCAP writer after an error without calling it again.
///
/// An I/O error while the writer starts a chunk leaves it in a state where
/// every later call panics, including its own `Drop`, which tries to finish
/// the file. A panic there would stop the writer thread, so the writer is
/// never used or dropped after an error. Its file handle and buffers are
/// leaked instead, once per failed recording.
fn abandon(writer: McapWriter) {
    std::mem::forget(writer);
}

#[derive(Clone, Copy, Debug)]
struct Channel {
    id: u16,
    next_sequence: u32,
}

impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Recording {
    /// Creates the file at `path` with one channel per stream, each with its
    /// schema if it has one. Receive times are mapped to UTC through `clock`.
    ///
    /// # Errors
    ///
    /// `capture.write_failed` if the file can't be created,
    /// `capture.disk_full` if the disk is full.
    pub fn create(
        path: &Path,
        streams: &[ChannelSpec],
        clock: &ClockMap,
        config: &WriterConfig,
    ) -> Result<Self, KaptoError> {
        let failed = |error| storage_error(error, path);
        let file = File::create(path).map_err(|error| failed(McapError::Io(error)))?;
        let sync_handle = file
            .try_clone()
            .map_err(|error| failed(McapError::Io(error)))?;
        let compression = match config.compression {
            Compression::None => None,
            Compression::Lz4 => Some(mcap::Compression::Lz4),
            Compression::Zstd => Some(mcap::Compression::Zstd),
        };
        let mut mcap = mcap::WriteOptions::new()
            .library(concat!("kapto ", env!("CARGO_PKG_VERSION")))
            .compression(compression)
            .compression_level(config.compression_level)
            // Compress on this thread: the capture process runs only the
            // threads it names, and zstd would otherwise start one per core.
            .compression_threads(0)
            .chunk_size(Some(config.chunk_bytes))
            .create(BufWriter::new(file))
            .map_err(failed)?;

        // Every recording has the channel, even with no event on it, so a reader
        // always finds it where it looks.
        let events = mcap
            .add_schema(EVENTS_CHANNEL, "jsonschema", event_schema().as_bytes())
            .and_then(|schema| mcap.add_channel(schema, EVENTS_CHANNEL, "json", &BTreeMap::new()));
        let events = match events {
            Ok(id) => Channel {
                id,
                next_sequence: 0,
            },
            Err(error) => {
                abandon(mcap);
                return Err(failed(error));
            }
        };
        let count = streams.iter().map(|spec| usize::from(spec.stream) + 1);
        let mut channels = vec![None; count.max().unwrap_or(0)];
        for spec in streams {
            // Schema 0 means "no schema" in MCAP.
            let added = match &spec.schema {
                Some(schema) => mcap
                    .add_schema(&schema.name, &schema.encoding, &schema.data)
                    .and_then(|schema_id| {
                        mcap.add_channel(
                            schema_id,
                            spec.id.as_str(),
                            &schema.message_encoding,
                            &schema.metadata,
                        )
                    }),
                None => mcap.add_channel(0, spec.id.as_str(), RAW_ENCODING, &BTreeMap::new()),
            };
            match added {
                Ok(channel) => {
                    channels[usize::from(spec.stream)] = Some(Channel {
                        id: channel,
                        next_sequence: 0,
                    });
                }
                Err(error) => {
                    abandon(mcap);
                    return Err(failed(error));
                }
            }
        }
        Ok(Self {
            mcap: Some(mcap),
            failure: None,
            file: sync_handle,
            path: path.to_owned(),
            channels: channels.into_boxed_slice(),
            events,
            clock: clock.clone(),
            sync: SyncTimer::new(config.sync_interval, monotonic_now()),
            durability: DurabilityTracker::new(config.sync_interval, monotonic_now()),
            idle_wait: config.idle_wait,
            post_roll: PostRoll::default(),
            #[cfg(unix)]
            tap: None,
        })
    }

    /// Writes the metadata record `name` with `entries`: once per episode,
    /// before the first message, never per message.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`; the recording can't be
    /// written to afterwards.
    pub fn write_record(
        &mut self,
        name: &str,
        entries: BTreeMap<String, String>,
    ) -> Result<(), KaptoError> {
        let Some(mcap) = self.mcap.as_mut() else {
            return Err(self.earlier_failure());
        };
        let record = Metadata {
            name: name.to_owned(),
            metadata: entries,
        };
        mcap.write_metadata(&record)
            .map_err(|error| self.fail(error))
    }

    /// Attaches the file `name`, of media type `media_type`, holding
    /// `data`, as it was at `utc_ns`: once per episode, before the first
    /// message.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`; the recording can't be
    /// written to afterwards.
    pub fn attach(
        &mut self,
        name: &str,
        media_type: &str,
        utc_ns: i64,
        data: &[u8],
    ) -> Result<(), KaptoError> {
        let Some(mcap) = self.mcap.as_mut() else {
            return Err(self.earlier_failure());
        };
        let time = u64::try_from(utc_ns).unwrap_or(0);
        let attachment = mcap::Attachment {
            log_time: time,
            create_time: time,
            name: name.to_owned(),
            media_type: media_type.to_owned(),
            data: std::borrow::Cow::Borrowed(data),
        };
        mcap.attach(&attachment).map_err(|error| self.fail(error))
    }

    /// Writes `event` on the events channel, at its moment: between batches,
    /// never in one, so a stream's messages are never held up by it.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`; the recording can't be
    /// written to afterwards.
    pub fn write_event(&mut self, event: &Event) -> Result<(), KaptoError> {
        let Some(mcap) = self.mcap.as_mut() else {
            return Err(self.earlier_failure());
        };
        let at = u64::try_from(event.at_utc_ns).unwrap_or(0);
        let header = MessageHeader {
            channel_id: self.events.id,
            sequence: self.events.next_sequence,
            log_time: at,
            publish_time: at,
        };
        mcap.write_to_known_channel(&header, &event.to_json())
            .map_err(|error| self.fail(error))?;
        self.events.next_sequence = self.events.next_sequence.wrapping_add(1);
        Ok(())
    }

    /// Writes every message `kept` holds, as the first of its stream's,
    /// at `at_utc_ns`: the episode's start, so the episode's messages start
    /// with it. Each keeps its payload and the time its source gave it,
    /// or, without one, the time it was received. They aren't counted as
    /// received or written again. Returns each one's stream and receive
    /// time in UTC, in the order written, for [`carried_record`].
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`; the recording can't be
    /// written to afterwards.
    pub fn write_kept(
        &mut self,
        kept: &Kept,
        at_utc_ns: i64,
    ) -> Result<Vec<(StreamIndex, i64)>, KaptoError> {
        let log_time = u64::try_from(at_utc_ns).unwrap_or(0);
        let mut carried = Vec::new();
        for message in kept.messages() {
            let Some(Some(channel)) = self.channels.get(usize::from(message.stream)).copied()
            else {
                continue;
            };
            let received_utc_ns = self.clock.to_utc_ns(message.received);
            let header = MessageHeader {
                channel_id: channel.id,
                sequence: channel.next_sequence,
                log_time,
                publish_time: message
                    .source_time
                    .unwrap_or_else(|| u64::try_from(received_utc_ns).unwrap_or(0)),
            };
            let written = match self.mcap.as_mut() {
                Some(mcap) => mcap.write_to_known_channel(&header, message.payload),
                None => return Err(self.earlier_failure()),
            };
            written.map_err(|error| self.fail(error))?;
            if let Some(Some(written)) = self.channels.get_mut(usize::from(message.stream)) {
                written.next_sequence = channel.next_sequence.wrapping_add(1);
            }
            carried.push((message.stream, received_utc_ns));
        }
        Ok(carried)
    }

    /// Writes the messages `pre_roll` holds for an episode asked for at
    /// `asked` and begun at `begun` (see [`PreRoll::messages`]), at the
    /// times they were received, and counts them as received and written,
    /// and the gaps before them as lost from `from`, the episode's start,
    /// on. They were held on purpose, so how long they waited to be on disk
    /// isn't counted.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`; the recording can't be
    /// written to afterwards.
    pub fn write_pre_roll(
        &mut self,
        pre_roll: &PreRoll,
        (asked, begun, from): (RecvTime, RecvTime, RecvTime),
        counters: &LossCounters,
    ) -> Result<(), KaptoError> {
        for message in pre_roll.messages(asked, begun) {
            if let Some(gap) = &message.gap {
                counters.add_gap(gap, message.received, from);
            }
            let Some(Some(channel)) = self.channels.get(usize::from(message.stream)).copied()
            else {
                continue;
            };
            let log_time = u64::try_from(self.clock.to_utc_ns(message.received)).unwrap_or(0);
            let header = MessageHeader {
                channel_id: channel.id,
                sequence: channel.next_sequence,
                log_time,
                publish_time: message.source_time.unwrap_or(log_time),
            };
            let written = match self.mcap.as_mut() {
                Some(mcap) => mcap.write_to_known_channel(&header, message.payload),
                None => return Err(self.earlier_failure()),
            };
            written.map_err(|error| self.fail(error))?;
            if let Some(Some(written)) = self.channels.get_mut(usize::from(message.stream)) {
                written.next_sequence = channel.next_sequence.wrapping_add(1);
            }
            counters.add_received(message.stream, 1);
            counters.add_written(message.stream, 1);
        }
        Ok(())
    }

    /// Writes every batch left in `handed` once its receiver has stopped,
    /// each message counted as received and written, its gaps as lost from
    /// `from`, the episode's start, on, and the messages each ring dropped
    /// after it was handed over as received and dropped. Each
    /// batch is also offered to `kept` and held in `pre_roll`, as the writer
    /// thread does.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`; the recording can't be
    /// written to afterwards.
    pub fn write_handed_over(
        &mut self,
        (handed, from): (Vec<HandedOver>, RecvTime),
        counters: &LossCounters,
        (kept, pre_roll): (&mut Kept, &mut PreRoll),
    ) -> Result<(), KaptoError> {
        for mut handed in handed {
            while let Some(batch) = handed.drainer.take() {
                for (stream, _) in batch.messages() {
                    counters.add_received(stream, 1);
                }
                for gap in batch.gaps() {
                    counters.add_gap(gap, batch.recv_time(), from);
                }
                self.write(&batch, counters)?;
                kept.keep(&batch);
                pre_roll.hold(&batch);
            }
            let stats = handed.drainer.stats();
            for (stream, before) in (0..=StreamIndex::MAX).zip(handed.dropped.iter()) {
                let dropped = stats.dropped_messages(stream).saturating_sub(*before);
                counters.add_received(stream, dropped);
                counters.add_dropped_by_us(stream, dropped);
            }
        }
        Ok(())
    }

    /// Writes every message of `batch` and counts it as written, its log
    /// time the batch's receive time and its publish time the time its
    /// source gave it, or the receive time without one. A message of a
    /// stream without a channel is counted as dropped. A message past its
    /// stream's post-roll, as the batch's receiver saw the stop, is left
    /// out, as its receiver didn't count it.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`. The messages that
    /// weren't written are counted as dropped before returning.
    pub fn write(&mut self, batch: &Batch, counters: &LossCounters) -> Result<(), KaptoError> {
        let utc_ns = self.clock.to_utc_ns(batch.recv_time());
        let log_time = u64::try_from(utc_ns).unwrap_or(0);
        let source_times = batch.has_source_times();
        let mut written_messages = 0;
        for (index, (stream, payload)) in batch.messages().enumerate() {
            if self
                .post_roll
                .ended(batch.stopped(), stream, batch.recv_time())
            {
                continue;
            }
            let Some(Some(channel)) = self.channels.get_mut(usize::from(stream)) else {
                counters.add_dropped_by_us(stream, 1);
                continue;
            };
            let header = MessageHeader {
                channel_id: channel.id,
                sequence: channel.next_sequence,
                log_time,
                publish_time: if source_times {
                    batch.source_time(index).unwrap_or(log_time)
                } else {
                    log_time
                },
            };
            let written = match self.mcap.as_mut() {
                Some(mcap) => mcap.write_to_known_channel(&header, payload).map_err(Some),
                None => Err(None),
            };
            if let Err(error) = written {
                for (stream, _) in batch.messages().skip(index) {
                    counters.add_dropped_by_us(stream, 1);
                }
                return Err(match error {
                    Some(error) => self.fail(error),
                    None => self.earlier_failure(),
                });
            }
            channel.next_sequence = channel.next_sequence.wrapping_add(1);
            counters.add_written(stream, 1);
            written_messages += 1;
        }
        self.durability.written(batch.recv_time(), written_messages);
        Ok(())
    }

    /// If the sync interval has passed at `now`, closes the current chunk
    /// and puts everything written so far on disk. Returns whether it did.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`.
    pub fn sync_if_due(&mut self, now: RecvTime) -> Result<bool, KaptoError> {
        if !self.sync.due(now) {
            return Ok(false);
        }
        let flushed = match self.mcap.as_mut() {
            Some(mcap) => mcap.flush(),
            None => return Err(self.earlier_failure()),
        };
        flushed.map_err(|error| self.fail(error))?;
        // A failed sync means written data may not be on disk; nothing
        // later can be trusted to be either, so the recording stops here.
        self.file
            .sync_data()
            .map_err(|error| self.fail(McapError::Io(error)))?;
        self.durability.synced(now, monotonic_now());
        Ok(true)
    }

    /// Offers each written batch's newest message per stream to `feed`, for
    /// the viewer. Offering never waits; see [`crate::tap`].
    #[cfg(unix)]
    pub fn set_tap(&mut self, feed: crate::tap::TapFeed) {
        self.tap = Some(feed);
    }

    /// Leaves out each stream's messages past its post-roll once `post_roll`
    /// says the episode was stopped.
    pub fn set_post_roll(&mut self, post_roll: PostRoll) {
        self.post_roll = post_roll;
    }

    /// Gives the viewer's feed back, for the next episode or to end the
    /// tap: its reader finishes once the feed is dropped.
    #[cfg(unix)]
    pub fn take_tap(&mut self) -> Option<crate::tap::TapFeed> {
        self.tap.take()
    }

    /// The writer thread's loop: takes batches from every ring in turn,
    /// writes them, and syncs on the timer, until every receiver is gone and
    /// every batch is taken. It never stops taking batches, so receivers
    /// never wait: after a failure, the rest is counted as dropped.
    ///
    /// # Errors
    ///
    /// The first failure, after every batch has been taken and counted.
    pub fn drain(
        &mut self,
        drainers: &mut [Drainer],
        counters: &LossCounters,
    ) -> Result<(), KaptoError> {
        self.drain_with_events(
            drainers,
            counters,
            &std::sync::mpsc::channel().1,
            &mut Kept::default(),
            &mut PreRoll::default(),
        )
    }

    /// [`Recording::drain`], also writing each event `events` hands over,
    /// between batches, until every receiver is gone; events still waiting
    /// then are written last. Each batch written is also offered to
    /// `kept` and held in `pre_roll`, for the episodes after this one.
    ///
    /// # Errors
    ///
    /// As [`Recording::drain`].
    pub fn drain_with_events(
        &mut self,
        drainers: &mut [Drainer],
        counters: &LossCounters,
        events: &std::sync::mpsc::Receiver<Event>,
        kept: &mut Kept,
        pre_roll: &mut PreRoll,
    ) -> Result<(), KaptoError> {
        let mut failure = None;
        loop {
            let mut idle = true;
            for drainer in drainers.iter_mut() {
                while let Some(batch) = drainer.take() {
                    idle = false;
                    if failure.is_some() {
                        for (stream, _) in batch.messages() {
                            counters.add_dropped_by_us(stream, 1);
                        }
                    } else if let Err(error) = self.write(&batch, counters) {
                        failure = Some(error);
                    } else {
                        kept.keep(&batch);
                        pre_roll.hold(&batch);
                        #[cfg(unix)]
                        if let Some(tap) = &mut self.tap {
                            tap.offer(&batch);
                        }
                    }
                }
            }
            for event in events.try_iter() {
                idle = false;
                if failure.is_none()
                    && let Err(error) = self.write_event(&event)
                {
                    failure = Some(error);
                }
            }
            if failure.is_none()
                && let Err(error) = self.sync_if_due(monotonic_now())
            {
                failure = Some(error);
            }
            if drainers.iter().all(Drainer::is_finished) {
                for event in events.try_iter() {
                    if failure.is_none()
                        && let Err(error) = self.write_event(&event)
                    {
                        failure = Some(error);
                    }
                }
                return failure.map_or(Ok(()), Err);
            }
            if idle {
                thread::park_timeout(self.idle_wait);
            }
        }
    }

    /// Stores `clock`'s samples, writes the file's index and puts the file on
    /// disk. Returns how long the recording's messages waited to be on disk.
    ///
    /// # Errors
    ///
    /// `capture.disk_full` or `capture.write_failed`.
    pub fn finish(mut self, clock: &ClockMap) -> Result<DurabilityStats, KaptoError> {
        let started = monotonic_now();
        let Some(mut mcap) = self.mcap.take() else {
            return Err(self.earlier_failure());
        };
        let failed = |error| storage_error(error, &self.path);
        let finished = mcap
            .write_metadata(&clock_map_record(clock))
            .and_then(|()| mcap.finish().map(|_| ()));
        if let Err(error) = finished {
            abandon(mcap);
            return Err(failed(error));
        }
        mcap.into_inner()
            .flush()
            .map_err(|error| failed(McapError::Io(error)))?;
        self.file
            .sync_data()
            .map_err(|error| failed(McapError::Io(error)))?;
        self.durability.synced(started, monotonic_now());
        Ok(self.durability.stats())
    }

    /// Records the first failure and abandons the MCAP writer.
    fn fail(&mut self, error: McapError) -> KaptoError {
        if let Some(mcap) = self.mcap.take() {
            abandon(mcap);
        }
        let error = storage_error(error, &self.path);
        self.failure.get_or_insert(error.code());
        error
    }

    /// The error every call returns once the recording has failed.
    fn earlier_failure(&self) -> KaptoError {
        KaptoError::new(self.failure.unwrap_or(ErrorCode::CaptureWriteFailed))
            .detail("an earlier write to this recording failed")
            .context("file", self.path.display().to_string())
    }
}

/// While no episode is being recorded: takes every batch from every ring,
/// so receivers never wait, offers it to the viewer's `tap`, to `kept` and
/// to `pre_roll`, for the next episode, and discards it. Nothing is counted
/// as written or dropped: these messages belong to no episode.
///
/// Once `stop` is set it takes no more and hands every ring over, with
/// what is left in it: the phase ends because an episode is starting, and
/// what arrived since belongs to it. It also returns, with nothing to hand
/// over, once every receiver is gone and every batch is taken.
pub fn discard(
    mut drainers: Vec<Drainer>,
    #[cfg(unix)] mut tap: Option<&mut crate::tap::TapFeed>,
    (kept, pre_roll): (&mut Kept, &mut PreRoll),
    stop: &AtomicBool,
    idle_wait: Duration,
) -> Vec<HandedOver> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return drainers.into_iter().map(HandedOver::new).collect();
        }
        let mut idle = true;
        for drainer in &mut drainers {
            while let Some(batch) = drainer.take() {
                idle = false;
                kept.keep(&batch);
                pre_roll.hold(&batch);
                #[cfg(unix)]
                if let Some(tap) = tap.as_deref_mut() {
                    tap.offer(&batch);
                }
            }
        }
        if drainers.iter().all(Drainer::is_finished) {
            return Vec::new();
        }
        if idle {
            thread::park_timeout(idle_wait);
        }
    }
}

/// A ring [`discard`] stopped taking from because an episode is starting:
/// the batches left in it, and those its receiver sends before it stops,
/// are the episode's first.
#[derive(Debug)]
pub struct HandedOver {
    drainer: Drainer,
    /// The messages the ring had dropped per stream when it was handed
    /// over; those it drops after are the episode's.
    dropped: Box<[u64]>,
}

impl HandedOver {
    fn new(drainer: Drainer) -> Self {
        let stats = drainer.stats();
        let dropped = (0..=StreamIndex::MAX)
            .take(stats.streams())
            .map(|stream| stats.dropped_messages(stream))
            .collect();
        Self { drainer, dropped }
    }

    /// When the oldest batch left was received, if one is.
    #[must_use]
    pub fn earliest(&self) -> Option<RecvTime> {
        self.drainer.next_recv_time()
    }
}

/// One entry per sample, keyed by its zero-padded position so the entries
/// sort in order: `"mono_ns utc_ns uncertainty_ns"`.
fn clock_map_record(clock: &ClockMap) -> Metadata {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "format".to_owned(),
        "mono_ns utc_ns uncertainty_ns".to_owned(),
    );
    for (index, sample) in clock.samples().iter().enumerate() {
        metadata.insert(
            format!("{index:06}"),
            format!(
                "{} {} {}",
                sample.mono.as_nanos(),
                sample.utc_ns,
                sample.uncertainty.as_nanos()
            ),
        );
    }
    Metadata {
        name: CLOCK_MAP_RECORD.to_owned(),
        metadata,
    }
}

fn storage_error(error: McapError, path: &Path) -> KaptoError {
    let code = match &error {
        McapError::Io(io) if io.kind() == io::ErrorKind::StorageFull => ErrorCode::CaptureDiskFull,
        _ => ErrorCode::CaptureWriteFailed,
    };
    KaptoError::new(code)
        .detail(error.to_string())
        .context("file", path.display().to_string())
        .source(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: u64 = 1_000_000_000;

    #[test]
    fn a_sync_falls_due_once_per_interval() {
        let mut timer = SyncTimer::new(Duration::from_secs(1), RecvTime::from_nanos(10 * SECOND));
        assert!(!timer.due(RecvTime::from_nanos(10 * SECOND + SECOND / 2)));
        assert!(timer.due(RecvTime::from_nanos(11 * SECOND)));
        assert!(!timer.due(RecvTime::from_nanos(11 * SECOND + 1)));
        assert!(timer.due(RecvTime::from_nanos(13 * SECOND)));
        assert!(!timer.due(RecvTime::from_nanos(13 * SECOND + SECOND - 1)));
    }

    #[test]
    fn after_a_failure_the_writer_thread_keeps_taking_batches_and_counts_them_as_dropped() {
        use kapto_core::ClockSample;

        use crate::ring::{RingConfig, ring};

        let path = std::env::temp_dir().join(format!("kapto-{}-failed.mcap", std::process::id()));
        let clock = ClockMap::new(ClockSample::now());
        let streams = [(0, "arm".parse().unwrap())].map(ChannelSpec::from);
        let mut recording =
            Recording::create(&path, &streams, &clock, &WriterConfig::default()).unwrap();
        // The same path a failed write takes.
        let full = McapError::Io(io::Error::from(io::ErrorKind::StorageFull));
        let _ = recording.fail(full);

        let counters = LossCounters::new(1);
        let (mut filler, drainer, _) = ring(&RingConfig {
            batches: 4,
            batch_capacity: 8,
            max_message_len: 8,
            streams: 1,
        });
        for _ in 0..3 {
            for _ in 0..8 {
                filler.batch().push(0, b"x");
            }
            filler.send();
        }
        drop(filler);
        let error = recording.drain(&mut [drainer], &counters).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CaptureDiskFull);
        assert_eq!(counters.totals(0).dropped_by_us, 24);
        assert_eq!(counters.totals(0).written, 0);

        let again = recording
            .sync_if_due(RecvTime::from_nanos(u64::MAX))
            .unwrap_err();
        assert_eq!(again.code(), ErrorCode::CaptureDiskFull);
        assert_eq!(
            recording.finish(&clock).unwrap_err().code(),
            ErrorCode::CaptureDiskFull
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_full_disk_has_its_own_cause_code() {
        let full = McapError::Io(io::Error::from(io::ErrorKind::StorageFull));
        assert_eq!(
            storage_error(full, Path::new("x")).code(),
            ErrorCode::CaptureDiskFull
        );
        let denied = McapError::Io(io::Error::from(io::ErrorKind::PermissionDenied));
        let error = storage_error(denied, Path::new("/data/rec.mcap"));
        assert_eq!(error.code(), ErrorCode::CaptureWriteFailed);
        assert_eq!(
            error.context_entries(),
            [("file", "/data/rec.mcap".to_owned())]
        );
    }
}
