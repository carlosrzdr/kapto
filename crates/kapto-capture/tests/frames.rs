// SPDX-License-Identifier: Apache-2.0

//! Sending cameras' frames over a real Unix stream socket: each received
//! whole by a bridge that reads, counted as not sent with none listening,
//! and never waited on by the writer when the bridge stops reading or goes
//! away part-way through a frame.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kapto_capture::Batch;
use kapto_capture::tap::frames::{FrameConfig, HEADER, frames, read_header};
use kapto_core::RecvTime;

fn socket_path(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("kapto-frames-{}-{name}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Two cameras on streams 1 and 2, frames up to `max_len`, at most 30 a
/// second each.
fn config(max_len: usize) -> FrameConfig {
    FrameConfig {
        cameras: vec![(1, max_len), (2, max_len)],
        min_interval: Duration::from_nanos(1_000_000_000 / 30),
    }
}

/// A batch of one frame of `len` bytes, every byte `fill`.
fn batch(stream: u16, frame: u64, len: usize, fill: u8) -> Batch {
    let mut batch = Batch::new(1, len);
    assert!(batch.push(stream, &vec![fill; len]));
    // A frame every tenth of a second: each is due.
    batch.set_recv_time(RecvTime::from_nanos(frame * 100_000_000));
    batch
}

/// The next frame on `stream`: its stream, receive time and message.
fn read_frame(stream: &mut UnixStream) -> (u16, u64, Vec<u8>) {
    let mut header = [0; HEADER];
    stream.read_exact(&mut header).unwrap();
    let header = read_header(&header).unwrap();
    let mut message = vec![0; header.len];
    stream.read_exact(&mut message).unwrap();
    (header.stream, header.recv_time.as_nanos(), message)
}

#[test]
fn a_listening_bridge_receives_each_cameras_frames_whole() {
    let path = socket_path("listening");
    let bridge = UnixListener::bind(&path).unwrap();
    let (mut feed, mut sender, stats) = frames(&config(1 << 20), &path);
    feed.offer(&batch(1, 0, 1 << 20, 7));
    feed.offer(&batch(2, 1, 300_000, 9));
    let reading = std::thread::spawn(move || {
        let (mut stream, _) = bridge.accept().unwrap();
        [read_frame(&mut stream), read_frame(&mut stream)]
    });
    sender.send_until(Instant::now() + Duration::from_secs(10));
    let [first, second] = reading.join().unwrap();
    assert_eq!(first, (1, 0, vec![7; 1 << 20]));
    assert_eq!(second, (2, 100_000_000, vec![9; 300_000]));
    let totals = stats.totals();
    assert_eq!((totals[0].sent, totals[1].sent), (1, 1));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn with_no_bridge_frames_are_counted_as_not_sent_until_one_listens() {
    let path = socket_path("absent");
    let (mut feed, mut sender, stats) = frames(&config(1024), &path);
    feed.offer(&batch(1, 0, 1024, 1));
    let started = Instant::now();
    sender.send_until(started + Duration::from_secs(10));
    // Nothing to wait for: it returned at once.
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(stats.totals()[0].not_sent, 1);

    // A bridge that starts listening is found on a later try.
    let bridge = UnixListener::bind(&path).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    feed.offer(&batch(1, 1, 1024, 2));
    sender.send_until(Instant::now() + Duration::from_secs(10));
    let (mut stream, _) = bridge.accept().unwrap();
    assert_eq!(read_frame(&mut stream), (1, 100_000_000, vec![2; 1024]));
    assert_eq!((stats.totals()[0].sent, stats.totals()[0].not_sent), (1, 1));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_bridge_that_stops_reading_never_holds_up_the_writer_and_gets_the_newest_frame_when_it_resumes()
{
    let path = socket_path("stalled");
    let bridge = UnixListener::bind(&path).unwrap();
    let (mut feed, mut sender, stats) = frames(&config(1 << 20), &path);
    let started = Instant::now();
    for frame in 0..200 {
        feed.offer(&batch(
            1,
            frame,
            1 << 20,
            u8::try_from(frame % 251).unwrap(),
        ));
        let deadline = Instant::now() + Duration::from_millis(5);
        sender.send_until(deadline);
        // The sender gives up at its deadline.
        assert!(Instant::now() < deadline + Duration::from_millis(500));
    }
    // Two hundred offers of a mebibyte while nothing read: none waited.
    assert!(started.elapsed() < Duration::from_secs(10));
    // The tap thread holds the frame it was cut off in and the newest one;
    // each newer frame took the waiting one's place, so the writer always
    // had a slot.
    let totals = stats.totals()[0];
    assert_eq!(
        (totals.offered, totals.skipped, totals.superseded),
        (200, 0, 198),
        "{totals:?}"
    );

    // Reading again: the frame cut off is finished first, then the newest.
    let reading = std::thread::spawn(move || {
        let (mut stream, _) = bridge.accept().unwrap();
        let mut seen = Vec::new();
        let mut header = [0; HEADER];
        while stream.read_exact(&mut header).is_ok() {
            let header = read_header(&header).unwrap();
            let mut message = vec![0; header.len];
            stream.read_exact(&mut message).unwrap();
            let fill = message[0];
            assert!(
                message.iter().all(|&byte| byte == fill),
                "a frame mixed with another"
            );
            seen.push(header.recv_time.as_nanos());
        }
        seen
    });
    drop(feed);
    while !sender.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(30));
        sender.send_until(Instant::now() + Duration::from_millis(100));
    }
    // Closing the connection ends the bridge's reading.
    drop(sender);
    let seen = reading.join().unwrap();
    assert_eq!(seen, [0, 199 * 100_000_000]);
    let totals = stats.totals()[0];
    assert_eq!((totals.sent, totals.not_sent), (2, 0), "{totals:?}");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_bridge_that_goes_away_part_way_through_a_frame_counts_it_as_not_sent() {
    let path = socket_path("gone");
    let bridge = UnixListener::bind(&path).unwrap();
    let (mut feed, mut sender, stats) = frames(&config(4 << 20), &path);
    feed.offer(&batch(1, 0, 4 << 20, 3));
    // Far more than the socket holds: the frame is cut off at the deadline.
    sender.send_until(Instant::now() + Duration::from_millis(50));
    let (stream, _) = bridge.accept().unwrap();
    drop((stream, bridge));
    sender.send_until(Instant::now() + Duration::from_secs(10));
    let totals = stats.totals()[0];
    assert_eq!((totals.sent, totals.not_sent), (0, 1));
    std::fs::remove_file(&path).unwrap();
}
