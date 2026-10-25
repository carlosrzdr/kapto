// SPDX-License-Identifier: Apache-2.0

//! `cap-hash`: the recording hashed behind the writer gives the digest of
//! every byte of the file, or none.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::{
    Batch, ChannelSpec, Compression, HashBehind, LossCounters, Recording, WriterConfig,
};
use kapto_core::{ClockMap, ClockSample, RecvTime};
use sha2::{Digest, Sha256};

/// A file path unique to this test, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kapto-{}-hash-{name}", std::process::id()));
        let _ = fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The file's SHA-256, read on its own, apart from `cap-hash`.
fn sha256_of(path: &Path) -> String {
    kapto_core::hex(&Sha256::digest(fs::read(path).unwrap()))
}

/// Bytes that don't compress, different for each `seed`.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "{what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_recording_hashed_behind_the_writer_has_the_digest_of_the_whole_file_read_apart() {
    let scratch = Scratch::new("recording");
    let clock = ClockMap::new(ClockSample::now());
    let streams = [(0, "camera".parse().unwrap())].map(ChannelSpec::from);
    let config = WriterConfig {
        compression: Compression::None,
        ..WriterConfig::default()
    };
    let mut recording = Recording::create(&scratch.0, &streams, &clock, &config).unwrap();
    let (written, hash) = HashBehind::start(&scratch.0, || {}).unwrap();
    recording.set_hash_behind(written);
    let counters = LossCounters::new(1);
    let mut batch = Batch::new(4, 64 * 1024);
    let mut sync = 0;
    for round in 0..64_u64 {
        batch.clear();
        for n in 0..4 {
            batch.push(0, &noise(round * 4 + n, 64 * 1024));
        }
        batch.set_recv_time(RecvTime::from_nanos(round * 1_000_000));
        recording.write(&batch, &counters).unwrap();
        if round % 8 == 7 {
            sync += 1;
            // Past the interval each time, so each one syncs.
            let due = RecvTime::from_nanos(u64::MAX / 128 * sync);
            assert!(recording.sync_if_due(due).unwrap());
        }
    }
    // Every byte synced is hashed before the file is finished.
    let synced = fs::metadata(&scratch.0).unwrap().len();
    wait_until("cap-hash caught up with the writer", || {
        hash.hashed() == synced
    });
    recording.finish(&clock).unwrap();
    let hashed = hash.finish().unwrap();

    let size = fs::metadata(&scratch.0).unwrap().len();
    assert!(size > 16 * 1024 * 1024, "{size} bytes");
    assert_eq!(hashed.bytes, size);
    assert_eq!(hashed.sha256, sha256_of(&scratch.0));
    // Closing read only what was written after the last sync: the summary
    // and index, far less than the file.
    assert_eq!(hashed.read_at_close, size - synced);
    assert!(hashed.read_at_close < 64 * 1024, "{}", hashed.read_at_close);
}

#[test]
fn a_writer_far_ahead_of_cap_hash_never_waits_and_the_digest_is_still_whole() {
    let scratch = Scratch::new("ahead");
    fs::write(&scratch.0, b"").unwrap();
    let (go, held) = mpsc::channel::<()>();
    // `cap-hash` doesn't start reading until the writer is done.
    let (written, hash) = HashBehind::start(&scratch.0, move || {
        let _ = held.recv();
    })
    .unwrap();
    let mut file = OpenOptions::new().append(true).open(&scratch.0).unwrap();
    let mut size = 0;
    for round in 0..40 {
        let chunk = noise(round, 100 * 1024);
        file.write_all(&chunk).unwrap();
        size += chunk.len() as u64;
        written.publish(size);
    }
    written.finish(size);
    assert_eq!(hash.hashed(), 0);
    go.send(()).unwrap();
    let hashed = hash.finish().unwrap();
    assert_eq!(
        (hashed.bytes, hashed.read_at_close),
        (size, size),
        "all of it read at close, as it fell behind"
    );
    assert_eq!(hashed.sha256, sha256_of(&scratch.0));
}

/// A file of `len` bytes with `cap-hash` started on it and every byte
/// published as written.
fn started(scratch: &Scratch, len: usize) -> (kapto_capture::Written, HashBehind) {
    fs::write(&scratch.0, noise(7, len)).unwrap();
    let (written, hash) = HashBehind::start(&scratch.0, || {}).unwrap();
    written.publish(len as u64);
    (written, hash)
}

#[test]
fn a_recording_the_writer_never_finished_has_no_digest() {
    let scratch = Scratch::new("unfinished");
    let (written, hash) = started(&scratch, 1000);
    drop(written);
    let reason = hash.finish().unwrap_err();
    assert_eq!(reason, "the recording wasn't finished");
}

#[test]
fn a_recording_shorter_than_the_writer_said_has_no_digest() {
    let scratch = Scratch::new("shorter");
    let (written, hash) = started(&scratch, 1000);
    written.finish(2000);
    let reason = hash.finish().unwrap_err();
    assert!(reason.contains("ended at 1000 bytes"), "{reason}");
}

#[test]
fn a_recording_longer_than_the_writer_said_has_no_digest() {
    let scratch = Scratch::new("longer");
    let (written, hash) = started(&scratch, 1000);
    written.finish(999);
    let reason = hash.finish().unwrap_err();
    assert!(reason.contains("longer than the 999"), "{reason}");
}

#[test]
fn a_recording_replaced_under_cap_hash_has_no_digest() {
    let scratch = Scratch::new("replaced");
    let other = Scratch::new("replacement");
    let (written, hash) = started(&scratch, 1000);
    written.finish(1000);
    wait_until("cap-hash read the file", || hash.hashed() == 1000);
    // The same size and contents, but another file.
    fs::copy(&scratch.0, &other.0).unwrap();
    fs::rename(&other.0, &scratch.0).unwrap();
    let reason = hash.finish().unwrap_err();
    assert_eq!(reason, "the recording is not the file cap-hash read");
}

#[test]
fn a_recording_that_cant_be_opened_has_no_cap_hash() {
    let scratch = Scratch::new("missing");
    let error = HashBehind::start(&scratch.0, || {}).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn an_episode_finalised_with_the_digest_has_the_manifest_hashing_every_file_gives() {
    use kapto_capture::finalise::{RECORDING_FILE, Staged};
    use kapto_store::manifest::Manifest;

    let out = std::env::temp_dir().join(format!("kapto-{}-hash-episode", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let episode = "20260305T192345.123Z-1a2b3c4d".parse().unwrap();
    let staged = Staged::create(&out, &episode).unwrap();
    let clock = ClockMap::new(ClockSample::now());
    let streams = [(0, "arm".parse().unwrap())].map(ChannelSpec::from);
    let path = staged.recording_path();
    let mut recording =
        Recording::create(&path, &streams, &clock, &WriterConfig::default()).unwrap();
    let (written, hash) = HashBehind::start(&path, || {}).unwrap();
    recording.set_hash_behind(written);
    let mut batch = Batch::new(8, 1024);
    for n in 0..8 {
        batch.push(0, &noise(n, 1024));
    }
    recording.write(&batch, &LossCounters::new(1)).unwrap();
    assert!(
        recording
            .sync_if_due(RecvTime::from_nanos(u64::MAX))
            .unwrap()
    );
    recording.finish(&clock).unwrap();
    fs::write(staged.dir().join("episode.json"), b"{}\n").unwrap();
    let hashed = hash.finish().unwrap();

    let finalised = staged
        .finalise_with(&[hashed.entry(RECORDING_FILE)])
        .unwrap();
    let listed = Manifest::read(finalised.dir()).unwrap();
    assert_eq!(
        listed,
        Manifest::of_directory(finalised.dir(), &episode).unwrap()
    );
    kapto_store::verify_episode(finalised.dir()).unwrap();
    fs::remove_dir_all(&out).unwrap();
}
