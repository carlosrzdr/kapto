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

use kapto_capture::HashBehind;
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
