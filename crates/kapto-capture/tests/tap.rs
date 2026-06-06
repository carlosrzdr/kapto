// SPDX-License-Identifier: Apache-2.0

//! Sending tap samples over a real Unix datagram socket: received when a
//! viewer reads, counted as dropped when it stalls or isn't there, and never
//! waiting either way.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kapto_capture::tap::wire::{Summary, TapSample, decode};
use kapto_capture::tap::{TapSocket, TapStats};

fn socket_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("kapto-tap-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn a_listening_viewer_receives_each_sample() {
    let path = socket_path("listening");
    let viewer = UnixDatagram::bind(&path).unwrap();
    let stats = Arc::new(TapStats::default());
    let mut tap = TapSocket::new(&path, Arc::clone(&stats)).unwrap();
    tap.send(&Summary::single(2, 123, &[0.5, -0.25]));
    let mut buffer = [0; 256];
    let received = viewer.recv(&mut buffer).unwrap();
    assert_eq!(
        decode(&buffer[..received]).unwrap(),
        TapSample::single(2, 123, vec![0.5, -0.25])
    );
    assert_eq!(stats.sent(), 1);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_stalled_viewer_drops_samples_without_ever_making_the_sender_wait() {
    let path = socket_path("stalled");
    // Bound but never read, as a viewer that hangs.
    let _viewer = UnixDatagram::bind(&path).unwrap();
    let stats = Arc::new(TapStats::default());
    let mut tap = TapSocket::new(&path, Arc::clone(&stats)).unwrap();
    let values = [0.0; 64];
    let started = Instant::now();
    for n in 0..20_000 {
        tap.send(&Summary::single(0, n, &values));
    }
    // Twenty thousand sends finish quickly: none of them waited.
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(stats.sent() > 0);
    assert!(stats.viewer_slow() > 0, "the socket never filled up");
    assert_eq!(stats.sent() + stats.viewer_slow(), 20_000);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn with_no_viewer_listening_samples_are_counted_as_dropped() {
    let path = socket_path("absent");
    let stats = Arc::new(TapStats::default());
    let mut tap = TapSocket::new(&path, Arc::clone(&stats)).unwrap();
    tap.send(&Summary::single(0, 1, &[1.0]));
    tap.send(&Summary::single(0, 2, &[1.0]));
    assert_eq!((stats.sent(), stats.no_viewer()), (0, 2));
}

#[test]
fn a_socket_path_too_long_for_unix_sockets_is_refused_with_the_reason() {
    let path = PathBuf::from(format!("/tmp/{}/tap.sock", "x".repeat(120)));
    let error = TapSocket::new(&path, Arc::new(TapStats::default())).unwrap_err();
    assert!(error.to_string().contains("at most 107"), "{error}");
    assert!(error.to_string().contains("viewer.tap_socket"), "{error}");
}
