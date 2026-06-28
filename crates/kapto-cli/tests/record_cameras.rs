// SPDX-License-Identifier: Apache-2.0

//! A camera recorded by `kapto record` reaches the live view's frame
//! socket at `viewer.camera_max_fps`, each frame as recorded, and with no
//! viewer listening is recorded just the same.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

mod common;

use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::free_tcp_ports;
use kapto_capture::tap::frames::{HEADER, read_header};
use kapto_loadgen::{Config, Transport, run};
use kapto_testkit::TempDir;

/// Seconds the camera sends for.
const SECONDS: u64 = 3;

/// A streams.toml with one JPEG camera listening on `port`.
fn streams_file(dir: &Path, port: &str) -> PathBuf {
    let path = dir.join("streams.toml");
    std::fs::write(
        &path,
        format!(
            "[streams.front_camera]\nrate_hz = 60\nmax_message_bytes = 1048576\n\
             tcp = {{ listen = \"127.0.0.1:{port}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}\n\
             camera = {{ format = \"jpeg\" }}\n"
        ),
    )
    .unwrap();
    path
}

/// Starts `kapto record` with frames sent to `frame_socket` at most
/// `max_fps` a second, returning once it has started recording.
fn start_recording(streams: &Path, data: &Path, frame_socket: &Path, max_fps: &str) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kapto"))
        .args(["record", "--duration", &(SECONDS + 6).to_string()])
        .arg("--streams")
        .arg(streams)
        .arg("--data-dir")
        .arg(data)
        .arg("--viewer-tap-socket")
        .arg(data.with_file_name("tap.sock"))
        .arg("--viewer-frame-socket")
        .arg(frame_socket)
        .args(["--viewer-camera-max-fps", max_fps])
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut seen = Vec::new();
    loop {
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("kapto record exited early: {seen:#?}"))
            .unwrap();
        let started = line.ends_with("Started.");
        seen.push(line);
        if started {
            break;
        }
    }
    std::thread::spawn(move || lines.for_each(drop));
    child
}

/// Sends `SECONDS` of 320x240 JPEG frames at 60 a second to `port`. The
/// recording lasts long enough for the generator's encoding them first,
/// however busy the machine is.
fn send_camera(port: &str) {
    let camera = Config {
        target: SocketAddr::from(([127, 0, 0, 1], port.parse().unwrap())),
        streams: 1,
        rate_hz: 60,
        duration: Some(Duration::from_secs(SECONDS)),
        spin: Duration::from_micros(100),
        transport: Transport::TcpConnect,
        wait_for_subscribers: None,
        camera: Some((320, 240)),
    };
    let (sent, _) = run(&camera, |_, _| {}).unwrap();
    assert_eq!(sent.send_errors, 0);
}

/// What `kapto record` printed at the end, and every frame recorded.
fn finish(child: Child, data: &Path) -> (String, Vec<Vec<u8>>) {
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let episode = std::fs::read_dir(data)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("_FINALISED").exists())
        .unwrap();
    let bytes = std::fs::read(episode.join("recording.mcap")).unwrap();
    let frames = mcap::MessageStream::new(&bytes)
        .unwrap()
        .map(Result::unwrap)
        .filter(|message| message.channel.topic == "front_camera")
        .map(|message| message.data.into_owned())
        .collect();
    (String::from_utf8(output.stdout).unwrap(), frames)
}

#[test]
fn a_cameras_frames_reach_the_viewer_at_the_cap_each_as_recorded() {
    let dir = TempDir::new("record-camera-viewer");
    let [port] = free_tcp_ports::<1>();
    let streams = streams_file(&dir.0, &port);
    let data = dir.0.join("data");
    let socket = dir.0.join("frames.sock");
    let viewer = UnixListener::bind(&socket).unwrap();
    let reading = std::thread::spawn(move || {
        let (mut stream, _) = viewer.accept().unwrap();
        let mut frames = Vec::new();
        let mut header = [0; HEADER];
        while stream.read_exact(&mut header).is_ok() {
            let header = read_header(&header).unwrap();
            let mut frame = vec![0; header.len];
            stream.read_exact(&mut frame).unwrap();
            frames.push((header.stream, frame));
        }
        frames
    });
    let recording = start_recording(&streams, &data, &socket, "15");
    send_camera(&port);
    let (summary, recorded) = finish(recording, &data);
    let shown = reading.join().unwrap();

    assert_eq!(recorded.len(), 60 * 3, "every frame recorded");
    // What the summary says became of the frames offered: each sent (and
    // read here), replaced by a newer one while this reader was slow, or
    // skipped; a slow viewer losing frames is how the live view works.
    let line = summary
        .lines()
        .find(|line| line.starts_with("Live view of front_camera: "))
        .unwrap_or_else(|| panic!("{summary}"));
    let numbers: Vec<usize> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|word| !word.is_empty())
        .map(|word| word.parse().unwrap())
        .collect();
    let [sent, offered, replaced, skipped, ..] = numbers[..] else {
        panic!("{line}");
    };
    assert_eq!(sent, shown.len(), "{line}");
    assert_eq!(sent + replaced + skipped, offered, "{line}");
    // Never more offered than fifteen a second of the sixty sent. How close
    // to fifteen depends on the machine: a writer held up longer than two
    // frames' time starts the cap's schedule again, as it must rather than
    // catch up in a burst. That rate is checked with simulated times in
    // `tap::frames`; here, only that frames keep coming.
    let cap = 15 * usize::try_from(SECONDS).unwrap();
    let seconds = usize::try_from(SECONDS).unwrap();
    assert!((seconds..=cap + 1).contains(&offered), "{line}");
    for (stream, frame) in &shown {
        assert_eq!(*stream, 0);
        assert!(recorded.contains(frame), "a frame shown wasn't recorded");
    }
}

#[test]
fn with_no_viewer_listening_a_camera_is_recorded_whole_and_its_frames_counted_as_not_sent() {
    let dir = TempDir::new("record-camera-alone");
    let [port] = free_tcp_ports::<1>();
    let streams = streams_file(&dir.0, &port);
    let data = dir.0.join("data");
    let recording = start_recording(&streams, &data, &dir.0.join("frames.sock"), "30");
    send_camera(&port);
    let (summary, recorded) = finish(recording, &data);

    assert_eq!(recorded.len(), 60 * 3, "every frame recorded");
    let line = summary
        .lines()
        .find(|line| line.starts_with("Live view of front_camera: 0 of "))
        .unwrap_or_else(|| panic!("{summary}"));
    assert!(line.ends_with("while no viewer listened"), "{line}");
}
