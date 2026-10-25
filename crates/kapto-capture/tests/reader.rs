// SPDX-License-Identifier: Apache-2.0

//! Reading recordings back: whole, stopped early, cut off, and not a
//! recording at all, or damaged; their streams and their events apart.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kapto_capture::reader::{read_events, read_recording, read_start, recorded_channels};
use kapto_capture::{
    Batch, ChannelSchema, ChannelSpec, LossCounters, Recording, StreamIndex, WriterConfig,
};
use kapto_core::{ClockMap, ClockSample, ErrorCode, RecvTime};
use kapto_store::{Event, Phase};

const SECOND: u64 = 1_000_000_000;

/// A file path unique to this test, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("kapto-reader-{}-{name}.mcap", std::process::id())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn clock() -> ClockMap {
    ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(5 * SECOND),
        utc_ns: 1_000_005 * 1_000_000_000,
        uncertainty: Duration::ZERO,
    })
}

/// Two streams: `arm` with a schema, `status` without.
fn channels() -> Vec<ChannelSpec> {
    let mut arm = ChannelSpec::from((0, "arm".parse().unwrap()));
    arm.schema = Some(ChannelSchema {
        message_encoding: "kapto.struct/v1".to_owned(),
        name: "arm".to_owned(),
        encoding: "kapto.struct/v1".to_owned(),
        data: b"{}".to_vec(),
        metadata: BTreeMap::new(),
    });
    vec![arm, ChannelSpec::from((1, "status".parse().unwrap()))]
}

fn batch(messages: &[(StreamIndex, &[u8])], mono_ns: u64) -> Batch {
    let mut batch = Batch::new(messages.len(), 16);
    for (stream, payload) in messages {
        batch.push(*stream, payload);
    }
    batch.set_recv_time(RecvTime::from_nanos(mono_ns));
    batch
}

/// An event of `kind` at `utc_ns`, from the API.
fn event(kind: &str, utc_ns: i64, phase: Phase) -> Event {
    Event {
        kind: kind.to_owned(),
        at_utc_ns: utc_ns,
        until_utc_ns: None,
        phase,
        id: (phase != Phase::Moment).then(|| "g1".to_owned()),
        source: "api".to_owned(),
        text: None,
        attributes: BTreeMap::new(),
    }
}

/// The events [`record`] writes: a grasp beginning before the batch and
/// ending after it, and a note.
fn events() -> Vec<Event> {
    let at = |seconds: u64| i64::try_from(1_000_000 * SECOND + seconds * SECOND / 2).unwrap();
    vec![
        event("grasp", at(11), Phase::Begin),
        event("note", at(13), Phase::Moment),
        event("grasp", at(13), Phase::End),
    ]
}

/// Writes `arm`: a0, a1 and `status`: s0, with [`events`] around them,
/// then finishes the file unless `crash` is set, in which case it is
/// synced and abandoned.
fn record(file: &TempFile, crash: bool) {
    let config = WriterConfig {
        sync_interval: Duration::ZERO,
        ..WriterConfig::default()
    };
    let mut recording = Recording::create(&file.0, &channels(), &clock(), &config).unwrap();
    let counters = LossCounters::new(2);
    let [begin, note, end] = <[Event; 3]>::try_from(events()).unwrap();
    recording.write_event(&begin).unwrap();
    recording
        .write(
            &batch(&[(0, b"a0"), (1, b"s0"), (0, b"a1")], 6 * SECOND),
            &counters,
        )
        .unwrap();
    recording.write_event(&note).unwrap();
    recording.write_event(&end).unwrap();
    if crash {
        recording
            .sync_if_due(RecvTime::from_nanos(u64::MAX))
            .unwrap();
        // A crash: never finished, so the file has no summary or footer.
        std::mem::forget(recording);
    } else {
        recording.finish(&clock()).unwrap();
    }
}

/// A message as (topic, sequence, log time, payload).
type Message = (String, u32, u64, Vec<u8>);

fn read_all(path: &Path) -> (Vec<Message>, bool) {
    let mut messages = Vec::new();
    let summary = read_recording(path, |channel, message| {
        messages.push((
            channel.topic.clone(),
            message.sequence,
            message.log_time,
            message.data.to_vec(),
        ));
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(summary.messages, messages.len() as u64);
    (messages, summary.cut_off)
}

#[test]
fn every_message_is_read_back_with_its_channel_position_and_time() {
    let file = TempFile::new("whole");
    record(&file, false);
    let (messages, cut_off) = read_all(&file.0);
    let utc = 1_000_006 * SECOND;
    assert_eq!(
        messages,
        [
            ("arm".to_owned(), 0, utc, b"a0".to_vec()),
            ("status".to_owned(), 0, utc, b"s0".to_vec()),
            ("arm".to_owned(), 1, utc, b"a1".to_vec()),
        ]
    );
    assert!(!cut_off);
}

#[test]
fn events_are_read_back_apart_from_the_streams_in_the_order_they_were_written() {
    for crash in [false, true] {
        let file = TempFile::new(if crash { "events-crash" } else { "events" });
        record(&file, crash);
        // The streams read as they were, without the events among them.
        assert_eq!(read_all(&file.0).0.len(), 3);
        assert_eq!(read_events(&file.0).unwrap(), events(), "crash: {crash}");
    }
}

#[test]
fn a_recording_without_events_has_none() {
    let file = TempFile::new("no-events");
    let mut recording =
        Recording::create(&file.0, &channels(), &clock(), &WriterConfig::default()).unwrap();
    recording
        .write(&batch(&[(0, b"a0")], 6 * SECOND), &LossCounters::new(2))
        .unwrap();
    recording.finish(&clock()).unwrap();
    assert_eq!(read_events(&file.0).unwrap(), []);
}

#[test]
fn reading_stops_when_asked_to() {
    let file = TempFile::new("stop");
    record(&file, false);
    let mut seen = 0;
    let summary = read_recording(&file.0, |_, _| {
        seen += 1;
        ControlFlow::Break(())
    })
    .unwrap();
    assert_eq!((seen, summary.messages), (1, 1));
}

#[test]
fn channels_come_from_the_summary_with_their_schemas_and_message_counts() {
    let file = TempFile::new("summary");
    record(&file, false);
    let channels = recorded_channels(&file.0).unwrap();
    let topics: Vec<_> = channels
        .iter()
        .map(|summary| summary.channel.topic.as_str())
        .collect();
    assert_eq!(topics, ["arm", "status"]);
    assert_eq!(channels[0].messages, Some(2));
    assert_eq!(channels[1].messages, Some(1));
    let schema = channels[0].channel.schema.as_ref().unwrap();
    assert_eq!(schema.encoding, "kapto.struct/v1");
    assert_eq!(schema.data, b"{}");
    assert_eq!(channels[1].channel.message_encoding, "kapto.raw");
    assert!(channels[1].channel.schema.is_none());
}

#[test]
fn a_recording_cut_off_by_a_crash_is_read_up_to_where_it_stops() {
    let file = TempFile::new("crash");
    record(&file, true);
    let (messages, cut_off) = read_all(&file.0);
    assert_eq!(messages.len(), 3);
    assert!(cut_off);
    // No summary: channels are found by scanning, without counts.
    let channels = recorded_channels(&file.0).unwrap();
    assert_eq!(channels.len(), 2);
    assert!(channels.iter().all(|summary| summary.messages.is_none()));
}

#[test]
fn files_that_are_not_recordings_are_reported_as_unreadable() {
    let missing = std::env::temp_dir().join("kapto-reader-no-such-file.mcap");
    let empty = TempFile::new("empty");
    std::fs::write(&empty.0, b"").unwrap();
    let text = TempFile::new("text");
    std::fs::write(&text.0, b"this is not an mcap file at all").unwrap();
    for path in [&missing, &empty.0, &text.0] {
        let error = read_recording(path, |_, _| ControlFlow::Continue(())).unwrap_err();
        assert_eq!(
            error.code(),
            ErrorCode::RecordingUnreadable,
            "{}",
            path.display()
        );
    }
}

/// Where the first record with `opcode` starts, walked by each record's
/// opcode and length.
fn first_record(bytes: &[u8], opcode: u8) -> usize {
    let mut at = mcap::MAGIC.len();
    while bytes[at] != opcode {
        let length = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().unwrap());
        at += 9 + usize::try_from(length).unwrap();
    }
    at
}

/// Lengths a damaged file could hold for a record: one byte past the file's
/// end, and a petabyte, which no reader could set aside.
fn damaged_lengths(file_length: usize) -> [u64; 2] {
    [file_length as u64, 1 << 50]
}

#[test]
fn a_record_whose_length_runs_past_the_end_is_read_up_to_without_setting_room_aside_for_it() {
    let file = TempFile::new("damaged-chunk");
    record(&file, false);
    let whole = std::fs::read(&file.0).unwrap();
    let (all, _) = read_all(&file.0);
    let chunk = first_record(&whole, mcap::records::op::CHUNK);
    for length in damaged_lengths(whole.len()) {
        let mut bytes = whole.clone();
        bytes[chunk + 1..chunk + 9].copy_from_slice(&length.to_le_bytes());
        std::fs::write(&file.0, &bytes).unwrap();

        // What could be read is, as of a recording cut off there.
        let (messages, cut_off) = read_all(&file.0);
        assert!(all.starts_with(&messages), "{length}");
        assert!(cut_off, "{length}");
        assert!(read_start(&file.0).is_ok(), "{length}");
    }
}

#[test]
fn a_recording_whose_first_record_runs_past_the_end_is_unreadable() {
    let file = TempFile::new("damaged-header");
    record(&file, false);
    let whole = std::fs::read(&file.0).unwrap();
    let header = mcap::MAGIC.len();
    for length in damaged_lengths(whole.len()) {
        let mut bytes = whole.clone();
        bytes[header + 1..header + 9].copy_from_slice(&length.to_le_bytes());
        std::fs::write(&file.0, &bytes).unwrap();

        let read = read_recording(&file.0, |_, _| ControlFlow::Continue(())).unwrap_err();
        let start = read_start(&file.0).unwrap_err();
        for error in [read, start] {
            assert_eq!(error.code(), ErrorCode::RecordingUnreadable, "{length}");
        }
    }
}
