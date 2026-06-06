// SPDX-License-Identifier: Apache-2.0

//! Recovering episodes recording left unfinished: salvaged from staging,
//! finalised after stopping part-way through finalising, or left where they
//! are when nothing can be recovered.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

use kapto_capture::finalise::{EpisodeState, STAGING_DIR, Staged, inspect};
use kapto_capture::recover::REASON;
use kapto_capture::{
    Batch, ChannelSpec, LossCounters, Recording, WriterConfig, recover_interrupted,
};
use kapto_core::{ClockMap, ClockSample, EpisodeId, ErrorCode, RecvTime};
use kapto_store::records::{CAPTURE_RECORD, EPISODE_RECORD, EpisodeStart};
use kapto_store::{Episode, Event, PackageIdentity, Phase, SessionProfile, verify_episode};
use kapto_testkit::TempDir;

const STARTED_UTC_NS: i64 = 1_800_000_000_000_000_000;
const NOW_UTC_NS: i64 = STARTED_UTC_NS + 3_600_000_000_000;

fn id(n: u32) -> EpisodeId {
    format!("20260315T1{n:05}.000Z-0000000{n}").parse().unwrap()
}

fn clock() -> ClockMap {
    ClockMap::new(ClockSample {
        mono: RecvTime::from_nanos(0),
        utc_ns: STARTED_UTC_NS,
        uncertainty: Duration::ZERO,
    })
}

/// Episode `episode` in `data`'s staging, as recording leaves it when it
/// stops without finishing: its start records, `arm` messages 0 to 29 and
/// `gripper` 0 to 9, a millisecond apart, on disk, then nothing more.
fn crashed(data: &std::path::Path, episode: &EpisodeId) {
    let staged = Staged::create(data, episode).unwrap();
    let streams = [
        ChannelSpec::from((0, "arm".parse().unwrap())),
        ChannelSpec::from((1, "gripper".parse().unwrap())),
    ];
    let config = WriterConfig {
        sync_interval: Duration::ZERO,
        ..WriterConfig::default()
    };
    let mut recording =
        Recording::create(&staged.recording_path(), &streams, &clock(), &config).unwrap();
    let start = EpisodeStart::new(
        episode,
        STARTED_UTC_NS,
        Some(PackageIdentity {
            id: "arm7".into(),
            version: "0.1.0".into(),
            hash: "ab12".into(),
        }),
        (
            Some(SessionProfile {
                id: "lab_ana".into(),
                hash: None,
                operator: Some("Ana".into()),
                environment: None,
                mode: None,
                tags: Vec::new(),
                notes: None,
            }),
            None,
        ),
    );
    recording
        .write_record(EPISODE_RECORD, start.record())
        .unwrap();
    let capture = BTreeMap::from([("writer.sync_interval_ns".to_owned(), "500000000".to_owned())]);
    recording.write_record(CAPTURE_RECORD, capture).unwrap();
    // An event too, which is no stream: recovery mustn't count it as one.
    recording
        .write_event(&Event {
            kind: "note".to_owned(),
            at_utc_ns: STARTED_UTC_NS,
            until_utc_ns: None,
            phase: Phase::Moment,
            id: None,
            source: "api".to_owned(),
            text: None,
            attributes: BTreeMap::new(),
        })
        .unwrap();
    let counters = LossCounters::new(2);
    for n in 0..30_u32 {
        let mut batch = Batch::new(2, 8);
        batch.push(0, &n.to_le_bytes());
        if n < 10 {
            batch.push(1, &n.to_le_bytes());
        }
        batch.set_recv_time(RecvTime::from_nanos(u64::from(n) * 1_000_000 + 1));
        recording.write(&batch, &counters).unwrap();
    }
    recording
        .sync_if_due(RecvTime::from_nanos(u64::MAX))
        .unwrap();
    std::mem::forget(recording);
}

#[test]
fn an_episode_left_in_staging_is_salvaged_described_from_its_recording_and_finalised() {
    let data = TempDir::new("recover-staged");
    let episode = id(1);
    crashed(&data.0, &episode);
    // Torn after its last chunk.
    let recording = data
        .0
        .join(STAGING_DIR)
        .join(episode.to_string())
        .join("recording.mcap");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&recording)
        .unwrap()
        .write_all(&[0x06, 0xFF, 0x13])
        .unwrap();

    let recovered = recover_interrupted(&data.0, NOW_UTC_NS);
    assert_eq!(recovered.len(), 1);
    let recovered = recovered.into_iter().next().unwrap().unwrap();
    assert_eq!(recovered.episode, episode);
    let salvaged = recovered.salvaged.unwrap();
    // Every message the file keeps, the event among them.
    assert_eq!((salvaged.messages, salvaged.cut_bytes), (41, 3));
    assert_eq!(inspect(&data.0, &episode), EpisodeState::Finalised);
    assert!(verify_episode(&recovered.folder).is_ok());

    let described = Episode::read(&recovered.folder).unwrap();
    let recovery = described.recovered.unwrap();
    assert_eq!(recovery.at_utc_ns, NOW_UTC_NS);
    assert_eq!(recovery.bytes_cut, 3);
    assert_eq!(recovery.reason, REASON);
    let last = STARTED_UTC_NS + 29_000_001;
    assert_eq!(recovery.last_message_utc_ns, Some(last));
    assert_eq!(
        (described.started_utc_ns, described.ended_utc_ns),
        (STARTED_UTC_NS, last)
    );
    let counts: Vec<(&str, u64, u64)> = described
        .streams
        .iter()
        .map(|stream| (stream.id.as_str(), stream.written, stream.dropped_by_us))
        .collect();
    assert_eq!(counts, [("arm", 30, 0), ("gripper", 10, 0)]);
    assert_eq!(described.package.unwrap().id, "arm7");
    assert_eq!(described.profile.unwrap().operator.as_deref(), Some("Ana"));
    assert_eq!(described.durability.sync_interval_ns, 500_000_000);
    assert_eq!(described.clock.samples, 0);

    // Recovered, there is nothing left to recover.
    assert!(recover_interrupted(&data.0, NOW_UTC_NS).is_empty());
}

#[test]
fn an_episode_stopped_part_way_through_finalising_is_finalised_and_not_marked_recovered() {
    let data = TempDir::new("recover-finalising");
    let (renamed, manifested) = (id(2), id(3));
    for (episode, manifest) in [(&renamed, false), (&manifested, true)] {
        let staged = Staged::create(&data.0, episode).unwrap();
        let streams = [ChannelSpec::from((0, "arm".parse().unwrap()))];
        let recording = Recording::create(
            &staged.recording_path(),
            &streams,
            &clock(),
            &WriterConfig::default(),
        )
        .unwrap();
        recording.finish(&clock()).unwrap();
        kapto_testkit::episode(
            &episode.to_string(),
            (STARTED_UTC_NS, STARTED_UTC_NS + 1_000_000_000),
            vec![kapto_testkit::stream("arm", 0)],
        )
        .write(&staged.dir())
        .unwrap();
        let moved = staged.rename().unwrap();
        if manifest {
            moved.write_manifest().unwrap();
        }
    }
    assert_eq!(inspect(&data.0, &renamed), EpisodeState::Renamed);
    assert_eq!(inspect(&data.0, &manifested), EpisodeState::Manifested);

    let recovered: Vec<_> = recover_interrupted(&data.0, NOW_UTC_NS)
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let episodes: Vec<EpisodeId> = recovered.iter().map(|done| done.episode).collect();
    assert_eq!(episodes, [renamed, manifested]);
    for done in &recovered {
        assert_eq!(done.salvaged, None);
        assert_eq!(inspect(&data.0, &done.episode), EpisodeState::Finalised);
        assert!(verify_episode(&done.folder).is_ok());
        assert_eq!(Episode::read(&done.folder).unwrap().recovered, None);
    }
}

#[test]
fn a_staged_folder_with_nothing_to_recover_is_left_where_it_is_saying_why() {
    let data = TempDir::new("recover-nothing");
    // A finished episode, and a folder that isn't an episode's, are left alone.
    crashed(&data.0, &id(6));
    assert!(recover_interrupted(&data.0, NOW_UTC_NS)[0].is_ok());
    std::fs::create_dir_all(data.0.join("notes")).unwrap();
    let (empty, garbage) = (id(4), id(5));
    Staged::create(&data.0, &empty).unwrap();
    let staged = Staged::create(&data.0, &garbage).unwrap();
    std::fs::write(staged.recording_path(), b"not a recording").unwrap();
    // Let go, as when its process ended.
    drop(staged);

    let recovered = recover_interrupted(&data.0, NOW_UTC_NS);
    let codes: Vec<ErrorCode> = recovered
        .iter()
        .map(|outcome| outcome.as_ref().unwrap_err().code())
        .collect();
    assert_eq!(
        codes,
        [
            ErrorCode::RecordingUnreadable,
            ErrorCode::RecordingUnreadable
        ]
    );
    for (outcome, episode) in recovered.iter().zip([&empty, &garbage]) {
        let message = outcome.as_ref().unwrap_err().report();
        assert!(message.contains(&episode.to_string()), "{message}");
        assert_eq!(inspect(&data.0, episode), EpisodeState::Staged);
    }
    assert_eq!(inspect(&data.0, &id(6)), EpisodeState::Finalised);
}

#[test]
fn an_episode_another_process_holds_is_left_to_it_and_recovered_once_it_lets_go() {
    let data = TempDir::new("recover-held");
    // Being recorded: its folder held from its creation.
    let recording = Staged::create(&data.0, &id(7)).unwrap();
    // Being finalised: held still once moved.
    let finalising = {
        let staged = Staged::create(&data.0, &id(8)).unwrap();
        let streams = [ChannelSpec::from((0, "arm".parse().unwrap()))];
        Recording::create(
            &staged.recording_path(),
            &streams,
            &clock(),
            &WriterConfig::default(),
        )
        .unwrap()
        .finish(&clock())
        .unwrap();
        kapto_testkit::episode(
            &id(8).to_string(),
            (STARTED_UTC_NS, STARTED_UTC_NS + 1_000_000_000),
            vec![kapto_testkit::stream("arm", 0)],
        )
        .write(&staged.dir())
        .unwrap();
        staged.rename().unwrap()
    };

    assert!(recover_interrupted(&data.0, NOW_UTC_NS).is_empty());
    assert_eq!(inspect(&data.0, &id(7)), EpisodeState::Staged);
    assert_eq!(inspect(&data.0, &id(8)), EpisodeState::Renamed);

    // Its process ended, however it ended.
    drop((recording, finalising));
    let recovered = recover_interrupted(&data.0, NOW_UTC_NS);
    let outcomes: Vec<Option<EpisodeId>> = recovered
        .iter()
        .map(|outcome| outcome.as_ref().ok().map(|done| done.episode))
        .collect();
    // The one stopped before its recording was created can't be.
    assert_eq!(outcomes, [None, Some(id(8))]);
}

#[test]
fn a_data_directory_without_staging_has_nothing_to_recover() {
    let data = TempDir::new("recover-none");
    assert!(recover_interrupted(&data.0, NOW_UTC_NS).is_empty());
    assert!(recover_interrupted(&data.0.join("missing"), NOW_UTC_NS).is_empty());
}
