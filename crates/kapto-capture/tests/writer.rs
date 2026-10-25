// SPDX-License-Identifier: Apache-2.0

//! Recordings written to real files and read back with an independent MCAP
//! reader: every message exactly as received, on its stream's channel, at
//! its receive time in UTC.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use std::collections::BTreeMap;

use kapto_capture::loss::{ByteOrder, SequenceField, SequenceWidth};
use kapto_capture::reader::read_start;
use kapto_capture::writer::{CARRIED_RECORD, CLOCK_MAP_RECORD, capture_record, carried_record};
use kapto_capture::{
    Accounting, Batch, ChannelSchema, ChannelSpec, Compression, Kept, KeptStream, LossCounters,
    LossDetection, PostRoll, PreRoll, PreRollStream, Recording, RingConfig, StreamIndex,
    WriterConfig, discard, ring,
};
use kapto_core::{ClockMap, ClockSample, ErrorCode, RecvTime};

const SECOND: u64 = 1_000_000_000;

/// A file path unique to this test, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("kapto-{}-{name}.mcap", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A clock map whose UTC is 1 000 000 s ahead of the monotonic clock.
fn clock() -> ClockMap {
    let mut clock = ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(5 * SECOND),
        utc_ns: 1_000_005 * 1_000_000_000,
        uncertainty: Duration::from_nanos(300),
    });
    clock.record(ClockSample {
        mono: RecvTime::from_nanos(6 * SECOND),
        utc_ns: 1_000_006 * 1_000_000_000 + 7,
        uncertainty: Duration::from_nanos(250),
    });
    clock
}

fn streams(names: &[&str]) -> Vec<ChannelSpec> {
    (0..)
        .zip(names)
        .map(|(index, name): (StreamIndex, _)| ChannelSpec::from((index, name.parse().unwrap())))
        .collect()
}

fn batch(messages: &[(StreamIndex, &[u8])], mono_ns: u64) -> Batch {
    let mut batch = Batch::new(messages.len().max(1), 64);
    for (stream, payload) in messages {
        assert!(batch.push(*stream, payload));
    }
    batch.set_recv_time(RecvTime::from_nanos(mono_ns));
    batch
}

/// Every message in the file as (topic, sequence, log time, payload).
fn read_back(path: &PathBuf) -> Vec<(String, u32, u64, Vec<u8>)> {
    let bytes = std::fs::read(path).unwrap();
    mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(|message| {
            let message = message.unwrap();
            let topic = message.channel.topic.clone();
            (
                topic,
                message.sequence,
                message.log_time,
                message.data.to_vec(),
            )
        })
        .collect()
}

#[test]
fn a_stream_with_a_schema_gets_it_on_its_channel_and_one_without_stays_raw() {
    let file = TempFile::new("schema");
    let mut specs = streams(&["arm", "status"]);
    specs[0].schema = Some(ChannelSchema {
        message_encoding: "kapto.struct/v1".to_owned(),
        name: "arm".to_owned(),
        encoding: "kapto.struct/v1".to_owned(),
        data: br#"{"fields":[]}"#.to_vec(),
        metadata: BTreeMap::new(),
    });
    let mut recording =
        Recording::create(&file.0, &specs, &clock(), &WriterConfig::default()).unwrap();
    let counters = LossCounters::new(2);
    recording
        .write(&batch(&[(0, b"a"), (1, b"s")], 5 * SECOND), &counters)
        .unwrap();
    recording.finish(&clock()).unwrap();

    let bytes = std::fs::read(&file.0).unwrap();
    let channels: Vec<_> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(|message| message.unwrap().channel)
        .collect();
    let arm = &channels[0];
    assert_eq!(arm.topic, "arm");
    assert_eq!(arm.message_encoding, "kapto.struct/v1");
    let schema = arm.schema.as_ref().unwrap();
    assert_eq!(schema.name, "arm");
    assert_eq!(schema.encoding, "kapto.struct/v1");
    assert_eq!(&schema.data[..], br#"{"fields":[]}"#);
    let status = &channels[1];
    assert_eq!(status.message_encoding, "kapto.raw");
    assert!(status.schema.is_none());
}

#[test]
fn every_message_is_stored_as_received_on_its_streams_channel_at_its_utc_time() {
    for compression in [Compression::None, Compression::Lz4, Compression::Zstd] {
        let file = TempFile::new(&format!("{compression:?}"));
        let counters = LossCounters::new(2);
        let config = WriterConfig {
            compression,
            // Small chunks, so messages span several of them.
            chunk_bytes: 64,
            ..WriterConfig::default()
        };
        let mut recording =
            Recording::create(&file.0, &streams(&["arm", "gripper"]), &clock(), &config).unwrap();
        recording
            .write(
                &batch(&[(0, b"a0"), (1, b"g0"), (0, b"a1")], 5 * SECOND),
                &counters,
            )
            .unwrap();
        for n in 0_u8..50 {
            recording
                .write(
                    &batch(&[(1, &[n; 40])], 6 * SECOND + u64::from(n)),
                    &counters,
                )
                .unwrap();
        }
        recording.finish(&clock()).unwrap();

        let messages = read_back(&file.0);
        assert_eq!(messages.len(), 53, "{compression:?}");
        let utc = |mono_ns: u64| mono_ns + 1_000_000 * SECOND;
        assert_eq!(
            messages[..3],
            [
                ("arm".to_owned(), 0, utc(5 * SECOND), b"a0".to_vec()),
                ("gripper".to_owned(), 0, utc(5 * SECOND), b"g0".to_vec()),
                ("arm".to_owned(), 1, utc(5 * SECOND), b"a1".to_vec()),
            ]
        );
        let last = messages.last().unwrap();
        assert_eq!(
            last,
            &("gripper".to_owned(), 50, utc(6 * SECOND + 49), vec![49; 40])
        );
        assert_eq!(counters.totals(0).written, 2);
        assert_eq!(counters.totals(1).written, 51);
    }
}

#[test]
fn a_message_is_published_at_the_time_its_source_gave_it_or_else_when_received() {
    let file = TempFile::new("publish-time");
    let mut recording = Recording::create(
        &file.0,
        &streams(&["arm"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    let mut batch = Batch::new(2, 8);
    assert!(batch.push_with_source_time(0, b"a", 1_700_000_000_123_456_789));
    assert!(batch.push(0, b"b"));
    batch.set_recv_time(RecvTime::from_nanos(5 * SECOND));
    recording.write(&batch, &LossCounters::new(1)).unwrap();
    recording.finish(&clock()).unwrap();

    let bytes = std::fs::read(&file.0).unwrap();
    let times: Vec<(u64, u64)> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(|message| {
            let message = message.unwrap();
            (message.log_time, message.publish_time)
        })
        .collect();
    let received = 1_000_005 * SECOND;
    assert_eq!(
        times,
        [(received, 1_700_000_000_123_456_789), (received, received)]
    );
}

#[test]
fn messages_kept_while_idle_start_the_next_recording_at_its_start_as_received() {
    let file = TempFile::new("kept");
    let config = RingConfig {
        batches: 4,
        batch_capacity: 8,
        max_message_len: 64,
        streams: 2,
    };
    let mut kept = Kept::new(&[KeptStream {
        stream: 1,
        depth: 2,
        max_message_len: 64,
    }]);
    // Idle: the fixed transforms arrive, and nothing is recorded.
    let (mut filler, drainer, _) = ring(&config);
    filler.batch().push(0, b"moving");
    assert!(
        filler
            .batch()
            .push_with_source_time(1, b"camera_link", 1_700_000_000_000_000_001)
    );
    filler.send();
    drop(filler);
    let handed = discard(
        vec![drainer],
        None,
        (&mut kept, &mut PreRoll::default()),
        &AtomicBool::new(false),
        Duration::from_millis(1),
    );
    assert!(handed.is_empty());

    let counters = LossCounters::new(2);
    let mut recording = Recording::create(
        &file.0,
        &streams(&["joints", "tf_static"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    let start = 1_000_009 * SECOND;
    let carried = recording
        .write_kept(&kept, i64::try_from(start).unwrap())
        .unwrap();
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].0, 1);
    recording
        .write_record(
            CARRIED_RECORD,
            carried_record(&[("tf_static", carried[0].1)]),
        )
        .unwrap();
    // During the episode another one arrives, written as every message is.
    let (mut filler, drainer, _) = ring(&config);
    filler.batch().push(1, b"gripper_link");
    filler.send();
    drop(filler);
    recording
        .drain_with_events(
            &mut [drainer],
            &counters,
            &std::sync::mpsc::channel().1,
            &mut kept,
            &mut PreRoll::default(),
        )
        .unwrap();
    recording.finish(&clock()).unwrap();

    let bytes = std::fs::read(&file.0).unwrap();
    let messages: Vec<(u64, u64, Vec<u8>)> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .filter(|message| message.channel.topic == "tf_static")
        .map(|message| {
            (
                message.log_time,
                message.publish_time,
                message.data.to_vec(),
            )
        })
        .collect();
    assert_eq!(
        messages[0],
        (start, 1_700_000_000_000_000_001, b"camera_link".to_vec())
    );
    assert_eq!(messages[1].2, b"gripper_link");
    // Counted once, when the episode received it.
    assert_eq!(counters.totals(1).written, 1);
    let (_, record) = records(&bytes)
        .into_iter()
        .find(|(name, _)| name == CARRIED_RECORD)
        .unwrap();
    assert_eq!(record["tf_static"], format!("[{}]", carried[0].1));
    // Both are kept for the episode after.
    let next: Vec<&[u8]> = kept.messages().map(|message| message.payload).collect();
    assert_eq!(next, [b"camera_link".as_slice(), b"gripper_link"]);
}

#[test]
fn what_the_idle_writer_left_when_told_to_stop_starts_the_episode_with_its_drops() {
    let file = TempFile::new("handed");
    let config = RingConfig {
        batches: 3,
        batch_capacity: 2,
        max_message_len: 64,
        streams: 2,
    };
    let (mut filler, drainer, _) = ring(&config);
    let mut next = 0_u8;
    let mut send = |filler: &mut kapto_capture::Filler, at: u64| {
        filler
            .batch()
            .set_recv_time(RecvTime::from_nanos(at * SECOND));
        filler.batch().push(0, &[next]);
        filler.batch().push(1, &[next]);
        next += 1;
        filler.send()
    };
    // Two batches arrive after Start: the idle writer, told to stop, takes
    // neither and hands the ring over.
    send(&mut filler, 7);
    send(&mut filler, 8);
    let mut kept = Kept::default();
    let mut pre_roll = PreRoll::default();
    let handed = discard(
        vec![drainer],
        None,
        (&mut kept, &mut pre_roll),
        &AtomicBool::new(true),
        Duration::from_millis(1),
    );
    assert_eq!(handed.len(), 1);
    assert_eq!(handed[0].earliest(), Some(RecvTime::from_nanos(7 * SECOND)));
    // Its receiver goes on until it stops, dropping one batch for want of
    // room: those messages are the episode's too.
    assert_eq!(
        send(&mut filler, 9),
        kapto_capture::Sent::Dropped { messages: 2 }
    );
    drop(filler);

    let counters = LossCounters::new(2);
    let mut recording = Recording::create(
        &file.0,
        &streams(&["joints", "camera"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    recording
        .write_handed_over(
            (handed, RecvTime::from_nanos(0)),
            &counters,
            (&mut kept, &mut pre_roll),
        )
        .unwrap();
    recording.finish(&clock()).unwrap();

    let read: Vec<(String, Vec<u8>)> = read_back(&file.0)
        .into_iter()
        .map(|(topic, _, _, payload)| (topic, payload))
        .collect();
    assert_eq!(
        read,
        [
            ("joints".to_owned(), vec![0]),
            ("camera".to_owned(), vec![0]),
            ("joints".to_owned(), vec![1]),
            ("camera".to_owned(), vec![1]),
        ]
    );
    for stream in 0..2 {
        let totals = counters.totals(stream);
        assert_eq!(
            (totals.received, totals.written, totals.dropped_by_us),
            (3, 2, 1)
        );
    }
}

#[test]
fn messages_from_before_the_episode_count_as_decoded_unless_their_receiver_could_not() {
    let file = TempFile::new("decoded");
    let sequence = LossDetection::Sequence(SequenceField {
        offset: 0,
        width: SequenceWidth::U32,
        byte_order: ByteOrder::Little,
    });
    let config = RingConfig {
        batches: 4,
        batch_capacity: 4,
        max_message_len: 64,
        streams: 2,
    };
    // Idle: the receiver counts each batch as it arrives, stream 0 by its
    // sequence number, which one message is too short to hold.
    let idle = Arc::new(LossCounters::new(2));
    let mut accounting = Accounting::new(Arc::clone(&idle), &[(0, sequence)]);
    let (mut filler, drainer, _) = ring(&config);
    let mut send = |filler: &mut kapto_capture::Filler, at: u64, first: u32, short: bool| {
        let batch = filler.batch();
        batch.set_recv_time(RecvTime::from_nanos(at * SECOND));
        batch.push(0, &first.to_le_bytes());
        let second = (first + 1).to_le_bytes();
        batch.push(0, if short { &second[..1] } else { &second });
        batch.push(1, b"no number");
        accounting.count(batch);
        filler.send();
    };
    // The pre-roll holds the first batch while idle; Start is asked, and
    // the idle writer, told to stop, hands over the two after it.
    send(&mut filler, 1, 1, true);
    drop(filler);
    let hold = |stream| PreRollStream {
        stream,
        seconds: Duration::from_secs(5),
        bytes: 1024,
        messages: 8,
    };
    let mut kept = Kept::default();
    let mut pre_roll = PreRoll::new(&[hold(0), hold(1)]);
    let idle = discard(
        vec![drainer],
        None,
        (&mut kept, &mut pre_roll),
        &AtomicBool::new(false),
        Duration::from_millis(1),
    );
    assert!(idle.is_empty());
    let (mut filler, drainer, _) = ring(&config);
    send(&mut filler, 3, 3, false);
    send(&mut filler, 4, 5, true);
    drop(filler);
    let handed = discard(
        vec![drainer],
        None,
        (&mut kept, &mut PreRoll::default()),
        &AtomicBool::new(true),
        Duration::from_millis(1),
    );
    let (asked, begun) = (
        RecvTime::from_nanos(2 * SECOND),
        RecvTime::from_nanos(2 * SECOND),
    );

    let counters = LossCounters::new(2);
    let mut recording = Recording::create(
        &file.0,
        &streams(&["joints", "status"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    let from = RecvTime::from_nanos(0);
    recording
        .write_pre_roll(&pre_roll, (asked, begun, from), &counters)
        .unwrap();
    recording
        .write_handed_over(
            (handed, from),
            &counters,
            (&mut kept, &mut PreRoll::default()),
        )
        .unwrap();
    recording.finish(&clock()).unwrap();

    // Three batches of two messages each: one too short in the first and
    // the last; every message of the stream with no number decoded.
    let joints = counters.totals(0);
    assert_eq!((joints.received, joints.decoded, joints.written), (6, 4, 6));
    let status = counters.totals(1);
    assert_eq!((status.received, status.decoded, status.written), (3, 3, 3));
}

#[test]
fn after_a_stop_the_writer_leaves_out_what_its_receiver_did_not_count() {
    let file = TempFile::new("post-roll");
    let post_roll = PostRoll::new(&[Duration::ZERO, Duration::from_secs(5)]);
    let counters = Arc::new(LossCounters::new(2));
    let mut accounting = Accounting::new(Arc::clone(&counters), &[]);
    accounting.set_post_roll(post_roll.clone());
    let mut recording = Recording::create(
        &file.0,
        &streams(&["joints", "voice"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    recording.set_post_roll(post_roll.clone());
    let mut receive = |at: u64| {
        let mut received = batch(&[(0, b"joints"), (1, b"voice")], at * SECOND);
        accounting.count(&mut received);
        recording.write(&received, &counters).unwrap();
    };
    receive(6);
    post_roll.stop(Some(RecvTime::from_nanos(7 * SECOND)));
    receive(8);
    // Seen by its receiver before the stop, the batch is written whole.
    post_roll.stop(None);
    let mut early = batch(&[(0, b"joints"), (1, b"voice")], 9 * SECOND);
    accounting.count(&mut early);
    post_roll.stop(Some(RecvTime::from_nanos(7 * SECOND)));
    recording.write(&early, &counters).unwrap();
    recording.finish(&clock()).unwrap();

    let topics: Vec<String> = read_back(&file.0)
        .into_iter()
        .map(|(topic, ..)| topic)
        .collect();
    assert_eq!(topics, ["joints", "voice", "voice", "joints", "voice"]);
    for (stream, messages) in [(0, 2), (1, 3)] {
        let totals = counters.totals(stream);
        assert_eq!((totals.received, totals.written), (messages, messages));
    }
}

#[test]
fn the_clock_map_is_stored_as_a_metadata_record() {
    let file = TempFile::new("clock");
    let recording = Recording::create(
        &file.0,
        &streams(&["arm"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    recording.finish(&clock()).unwrap();

    let bytes = std::fs::read(&file.0).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let index = &summary.metadata_indexes[0];
    let record = mcap::read::metadata(&bytes, index).unwrap();
    assert_eq!(record.name, CLOCK_MAP_RECORD);
    let entries: Vec<_> = record
        .metadata
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        entries,
        [
            ("000000", "5000000000 1000005000000000 300"),
            ("000001", "6000000000 1000006000000007 250"),
            ("format", "mono_ns utc_ns uncertainty_ns"),
        ]
    );
}

/// Every metadata record in `bytes`, in the order written, read straight
/// through, so a file cut off part-way is read up to where it stops.
fn records(bytes: &[u8]) -> Vec<(String, BTreeMap<String, String>)> {
    mcap::read::LinearReader::new(bytes)
        .unwrap()
        .map_while(Result::ok)
        .filter_map(|record| match record {
            mcap::records::Record::Metadata(metadata) => Some((metadata.name, metadata.metadata)),
            _ => None,
        })
        .collect()
}

#[test]
fn records_written_before_the_first_message_survive_a_crash_and_the_capture_record_names_every_setting()
 {
    let file = TempFile::new("records");
    let counters = LossCounters::new(1);
    let config = WriterConfig {
        sync_interval: Duration::ZERO,
        ..WriterConfig::default()
    };
    let ring = RingConfig {
        batches: 64,
        batch_capacity: 256,
        max_message_len: 1500,
        streams: 1,
    };
    let settings = capture_record(&config, &[("cap-rx-udp-0", &ring)], false, &BTreeMap::new());
    let mut recording = Recording::create(&file.0, &streams(&["arm"]), &clock(), &config).unwrap();
    let episode = BTreeMap::from([("id".to_owned(), "20260305T192345.123Z-1a2b3c4d".to_owned())]);
    recording
        .write_record("kapto.episode", episode.clone())
        .unwrap();
    recording
        .write_record("kapto.capture", settings.clone())
        .unwrap();
    recording
        .write(&batch(&[(0, b"kept")], 5 * SECOND), &counters)
        .unwrap();
    assert!(
        recording
            .sync_if_due(RecvTime::from_nanos(u64::MAX))
            .unwrap()
    );
    std::mem::forget(recording);

    let bytes = std::fs::read(&file.0).unwrap();
    assert_eq!(
        records(&bytes),
        [
            ("kapto.episode".to_owned(), episode),
            ("kapto.capture".to_owned(), settings.clone()),
        ]
    );
    let names: Vec<(&str, &str)> = settings
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("attach_meshes", "false"),
            ("buffers.cap-rx-udp-0.batch_capacity", "256"),
            ("buffers.cap-rx-udp-0.batches", "64"),
            ("buffers.cap-rx-udp-0.max_message_len", "1500"),
            (
                "buffers.cap-rx-udp-0.memory_bytes",
                ring.memory_bytes().to_string().as_str()
            ),
            ("writer.chunk_bytes", "1048576"),
            ("writer.compression", "lz4"),
            ("writer.compression_level", "0"),
            ("writer.sync_interval_ns", "0"),
        ]
    );
}

#[test]
fn attached_files_are_read_back_by_name_with_their_media_type_and_bytes() {
    let file = TempFile::new("attached");
    let mut recording = Recording::create(
        &file.0,
        &streams(&["arm"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    recording
        .attach(
            "package.toml",
            "application/toml",
            1_000_005_000_000_000,
            b"id = \"arm7\"\n",
        )
        .unwrap();
    recording
        .attach(
            "urdf/arm7.urdf",
            "application/xml",
            1_000_005_000_000_000,
            b"<robot/>",
        )
        .unwrap();
    recording.finish(&clock()).unwrap();

    let bytes = std::fs::read(&file.0).unwrap();
    let summary = mcap::Summary::read(&bytes).unwrap().unwrap();
    let attached: Vec<(String, String, Vec<u8>)> = summary
        .attachment_indexes
        .iter()
        .map(|index| {
            let attachment = mcap::read::attachment(&bytes, index).unwrap();
            (
                attachment.name,
                attachment.media_type,
                attachment.data.into_owned(),
            )
        })
        .collect();
    assert_eq!(
        attached,
        [
            (
                "package.toml".to_owned(),
                "application/toml".to_owned(),
                b"id = \"arm7\"\n".to_vec()
            ),
            (
                "urdf/arm7.urdf".to_owned(),
                "application/xml".to_owned(),
                b"<robot/>".to_vec()
            ),
        ]
    );
}

#[test]
fn the_start_of_a_recording_gives_the_records_and_attachments_written_before_its_first_message() {
    let file = TempFile::new("start");
    let counters = LossCounters::new(1);
    let mut recording = Recording::create(
        &file.0,
        &streams(&["arm"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    let episode = BTreeMap::from([("id".to_owned(), "20260305T192345.123Z-1a2b3c4d".to_owned())]);
    recording
        .write_record("kapto.episode", episode.clone())
        .unwrap();
    recording
        .attach(
            "qa.toml",
            "application/toml",
            1_000_005_000_000_000,
            b"qa_params_version = \"7\"\n",
        )
        .unwrap();
    recording
        .write(&batch(&[(0, b"later")], 5 * SECOND), &counters)
        .unwrap();
    recording.finish(&clock()).unwrap();

    let start = read_start(&file.0).unwrap();
    assert_eq!(
        start.records,
        BTreeMap::from([("kapto.episode".to_owned(), episode)])
    );
    assert_eq!(
        start.attachments,
        BTreeMap::from([(
            "qa.toml".to_owned(),
            b"qa_params_version = \"7\"\n".to_vec()
        )])
    );
    // A recording from before the records were written has none.
    let bare = TempFile::new("bare");
    Recording::create(
        &bare.0,
        &streams(&["arm"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap()
    .finish(&clock())
    .unwrap();
    let start = read_start(&bare.0).unwrap();
    assert!(start.attachments.is_empty());
}

#[test]
fn messages_synced_before_a_crash_can_be_read_from_the_unfinished_file() {
    let file = TempFile::new("crash");
    let counters = LossCounters::new(1);
    let config = WriterConfig {
        sync_interval: Duration::ZERO,
        ..WriterConfig::default()
    };
    let mut recording = Recording::create(&file.0, &streams(&["arm"]), &clock(), &config).unwrap();
    recording
        .write(&batch(&[(0, b"kept")], 5 * SECOND), &counters)
        .unwrap();
    assert!(
        recording
            .sync_if_due(RecvTime::from_nanos(u64::MAX))
            .unwrap()
    );
    // Simulate a crash: the writer is never finished, so the file has no
    // index or footer.
    std::mem::forget(recording);

    let bytes = std::fs::read(&file.0).unwrap();
    let readable: Vec<_> = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map_while(Result::ok)
        .map(|message| message.data.to_vec())
        .collect();
    assert_eq!(readable, [b"kept".to_vec()]);
}

#[test]
fn every_message_sent_by_the_receivers_is_written_or_counted_as_dropped() {
    let file = TempFile::new("drain");
    let counters = LossCounters::new(2);
    let config = RingConfig {
        batches: 4,
        batch_capacity: 8,
        max_message_len: 16,
        streams: 2,
    };
    let (first, first_drainer, first_stats) = ring(&config);
    let (second, second_drainer, second_stats) = ring(&config);
    let receivers = [(first, 0, 2_000), (second, 1, 3_000)].map(|(mut filler, stream, count)| {
        thread::spawn(move || {
            for n in 0_u32..count {
                filler.batch().push(stream, &n.to_le_bytes());
                if filler.batch().is_full() {
                    filler.send();
                }
            }
            filler.send();
        })
    });
    let mut recording = Recording::create(
        &file.0,
        &streams(&["a", "b"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap();
    let mut drainers = [first_drainer, second_drainer];
    recording.drain(&mut drainers, &counters).unwrap();
    for receiver in receivers {
        receiver.join().unwrap();
    }
    recording.finish(&clock()).unwrap();

    let (a, b) = (counters.totals(0), counters.totals(1));
    assert_eq!(a.written + first_stats.dropped_messages(0), 2_000);
    assert_eq!(b.written + second_stats.dropped_messages(1), 3_000);
    assert!(a.written > 0 && b.written > 0);
    assert_eq!(read_back(&file.0).len() as u64, a.written + b.written);
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_disk_is_reported_with_its_own_cause_code() {
    // Every write to /dev/full fails as if the disk were full.
    let error = Recording::create(
        "/dev/full".as_ref(),
        &streams(&["arm"]),
        &clock(),
        &WriterConfig::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureDiskFull);
}

#[test]
fn a_file_that_cannot_be_created_fails_with_write_failed() {
    let path = std::env::temp_dir().join("kapto-no-such-directory/recording.mcap");
    let error = Recording::create(&path, &[], &clock(), &WriterConfig::default()).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureWriteFailed);
}

#[test]
fn finishing_reports_how_long_messages_waited_to_be_on_disk() {
    let file = TempFile::new("durability");
    let counters = LossCounters::new(1);
    let config = WriterConfig {
        sync_interval: Duration::ZERO,
        ..WriterConfig::default()
    };
    let mut recording = Recording::create(&file.0, &streams(&["arm"]), &clock(), &config).unwrap();
    // Received 200 ms ago, then written and synced now.
    let now = kapto_core::clock::monotonic_now().as_nanos();
    recording
        .write(
            &batch(&[(0, b"a"), (0, b"b")], now - 200_000_000),
            &counters,
        )
        .unwrap();
    assert!(
        recording
            .sync_if_due(kapto_core::clock::monotonic_now())
            .unwrap()
    );
    let stats = recording.finish(&clock()).unwrap();
    assert_eq!(stats.syncs, 2, "one sync on the timer, one when finishing");
    assert!(
        stats.max_unsynced >= Duration::from_millis(200),
        "{stats:?}"
    );
    assert!(
        stats.p99_unsynced >= Duration::from_millis(200),
        "{stats:?}"
    );
}
