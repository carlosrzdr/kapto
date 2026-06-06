// SPDX-License-Identifier: Apache-2.0

//! Finalisation stopped after every step: until `_FINALISED` is written, the
//! episode is recognisably incomplete to anything reading the output folder.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::fs;
use std::path::{Path, PathBuf};

use kapto_capture::finalise::{EpisodeState, RECORDING_FILE, Staged, inspect};
use kapto_capture::{Batch, LossCounters, Recording, WriterConfig};
use kapto_core::{ClockMap, ClockSample, EpisodeId, ErrorCode, RecvTime};
use kapto_store::verify_episode;

/// An output folder unique to this test, removed when dropped.
struct Out(PathBuf);

impl Out {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kapto-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn episode() -> EpisodeId {
    "20260305T192345.123Z-1a2b3c4d".parse().unwrap()
}

/// Stages an episode and writes a short recording into it.
fn record(out: &Path) -> Staged {
    let staged = Staged::create(out, &episode()).unwrap();
    let clock = ClockMap::new(ClockSample::now());
    let streams = [(0, "arm".parse().unwrap())].map(kapto_capture::ChannelSpec::from);
    let mut recording = Recording::create(
        &staged.recording_path(),
        &streams,
        &clock,
        &WriterConfig::default(),
    )
    .unwrap();
    let mut batch = Batch::new(1, 8);
    batch.push(0, b"joints");
    batch.set_recv_time(RecvTime::from_nanos(1));
    recording.write(&batch, &LossCounters::new(1)).unwrap();
    recording.finish(&clock).unwrap();
    staged
}

fn final_dir(out: &Path) -> PathBuf {
    out.join(episode().to_string())
}

/// Anything reading the output folder refuses the episode.
fn assert_incomplete(out: &Path) {
    assert!(verify_episode(&final_dir(out)).is_err());
}

#[test]
fn a_recording_is_complete_only_after_every_step() {
    let out = Out::new("steps");
    let staged = record(&out.0);
    assert_eq!(inspect(&out.0, &episode()), EpisodeState::Staged);
    assert!(
        out.0
            .join(".recording")
            .join(episode().to_string())
            .join(RECORDING_FILE)
            .is_file()
    );
    assert_incomplete(&out.0);

    let renamed = staged.rename().unwrap();
    assert_eq!(inspect(&out.0, &episode()), EpisodeState::Renamed);
    assert!(
        fs::read_dir(out.0.join(".recording"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_incomplete(&out.0);

    let manifested = renamed.write_manifest().unwrap();
    assert_eq!(inspect(&out.0, &episode()), EpisodeState::Manifested);
    assert_incomplete(&out.0);

    let finalised = manifested.mark_finalised().unwrap();
    assert_eq!(inspect(&out.0, &episode()), EpisodeState::Finalised);
    let manifest = verify_episode(finalised.dir()).unwrap();
    let listed: Vec<_> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(listed, [RECORDING_FILE]);
    assert_eq!(manifest.episode, episode().to_string());
}

#[test]
fn finalising_in_one_call_gives_a_verified_episode() {
    let out = Out::new("one-call");
    let finalised = record(&out.0).finalise().unwrap();
    assert_eq!(finalised.dir(), final_dir(&out.0));
    verify_episode(finalised.dir()).unwrap();
}

#[test]
fn the_episode_summary_written_while_staged_is_listed_and_reads_back() {
    use kapto_capture::LossDetection;
    use kapto_store::{Episode, LossDetectionKind};

    let out = Out::new("summary");
    let staged = record(&out.0);
    let counters = LossCounters::new(1);
    counters.add_written(0, 1);
    let arm = counters
        .totals(0)
        .summary(&"arm".parse().unwrap(), &LossDetection::Undetectable);
    let clock = ClockMap::new(ClockSample::now());
    let ring = kapto_capture::RingConfig::for_rate(
        10,
        std::time::Duration::from_secs(1),
        std::time::Duration::from_millis(2),
        4,
        8,
        1,
    );
    let (_, _, ring_stats) = kapto_capture::ring(&ring);
    let durability =
        kapto_capture::DurabilityStats::default().summary(std::time::Duration::from_secs(1));
    let episode = Episode::new(
        &episode(),
        (1, 2),
        RECORDING_FILE,
        &clock,
        (ring_stats.summary(&ring), durability),
        vec![arm],
    );
    episode.write(&staged.dir()).unwrap();

    let finalised = staged.finalise().unwrap();
    let manifest = verify_episode(finalised.dir()).unwrap();
    let listed: Vec<_> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(listed, ["episode.json", RECORDING_FILE]);
    let read = Episode::read(finalised.dir()).unwrap();
    assert_eq!(read, episode);
    assert_eq!(
        read.streams[0].loss_detection,
        LossDetectionKind::Undetectable
    );
    assert_eq!(read.streams[0].written, 1);
}

#[test]
fn an_existing_episode_folder_is_never_overwritten() {
    let out = Out::new("exists");
    let staged = record(&out.0);
    fs::create_dir(final_dir(&out.0)).unwrap();
    fs::write(final_dir(&out.0).join("keep.txt"), b"keep").unwrap();

    let error = staged.rename().unwrap_err();
    assert_eq!(error.code(), ErrorCode::EpisodeAlreadyExists);
    assert_eq!(
        fs::read(final_dir(&out.0).join("keep.txt")).unwrap(),
        b"keep"
    );
    let staged_recording = out
        .0
        .join(".recording")
        .join(episode().to_string())
        .join(RECORDING_FILE);
    assert!(
        staged_recording.is_file(),
        "the staged recording is left in place"
    );

    let again = Staged::create(&out.0, &episode()).unwrap_err();
    assert_eq!(again.code(), ErrorCode::EpisodeAlreadyExists);
}

#[test]
fn an_episode_already_being_staged_is_not_staged_twice() {
    let out = Out::new("staged-twice");
    let _first = Staged::create(&out.0, &episode()).unwrap();
    let error = Staged::create(&out.0, &episode()).unwrap_err();
    assert_eq!(error.code(), ErrorCode::EpisodeAlreadyExists);
}

#[test]
fn an_output_folder_that_cannot_be_created_fails_with_write_failed() {
    let out = Out::new("blocked");
    // A file where the output folder should be.
    let blocked = out.0.join("data");
    fs::write(&blocked, b"").unwrap();
    let error = Staged::create(&blocked, &episode()).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureWriteFailed);
    assert!(
        error
            .message()
            .contains("couldn't create the staging folder")
    );
}

#[test]
fn nothing_there_is_absent() {
    let out = Out::new("absent");
    assert_eq!(inspect(&out.0, &episode()), EpisodeState::Absent);
}

#[test]
fn an_aborted_episode_leaves_nothing_behind() {
    let out = Out::new("discarded");
    let staged = record(&out.0);
    staged.discard().unwrap();
    assert_eq!(inspect(&out.0, &episode()), EpisodeState::Absent);
    assert!(!final_dir(&out.0).exists());
}

#[test]
fn an_aborted_episode_that_cannot_be_deleted_fails_with_write_failed() {
    let out = Out::new("discard-failed");
    let staged = record(&out.0);
    // Taken away by something else, so there is nothing to delete.
    fs::remove_dir_all(staged.dir()).unwrap();
    let error = staged.discard().unwrap_err();
    assert_eq!(error.code(), ErrorCode::CaptureWriteFailed);
    assert!(error.message().contains("delete the aborted episode"));
}
