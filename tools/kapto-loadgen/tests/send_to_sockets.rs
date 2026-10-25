// SPDX-License-Identifier: Apache-2.0

//! Runs the generator against real UDP sockets and TCP connections on this
//! machine and checks what arrives.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::io::Read;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use kapto_loadgen::payload::{GRIPPER_OPEN, READY_POSE, SIZE, Sample};
use kapto_loadgen::{Config, Transport, run};

/// Binds `count` sockets on consecutive loopback ports, retrying from another
/// starting port if one is taken.
fn consecutive_sockets(count: u16) -> Vec<UdpSocket> {
    for base in (20_000..60_000).step_by(97) {
        let sockets: Vec<UdpSocket> = (0..count)
            .filter_map(|offset| UdpSocket::bind(("127.0.0.1", base + offset)).ok())
            .collect();
        if sockets.len() == usize::from(count) {
            return sockets;
        }
    }
    panic!("no {count} consecutive free UDP ports on loopback");
}

/// Reads one socket until it stays quiet, returning every decoded message.
fn receive_all(socket: &UdpSocket) -> Vec<Sample> {
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut buffer = [0; 1_500];
    let mut samples = Vec::new();
    while let Ok(length) = socket.recv(&mut buffer) {
        samples.push(Sample::decode(&buffer[..length]).unwrap());
    }
    samples
}

#[test]
fn four_streams_at_2_khz_for_2_seconds_deliver_every_message_in_order() {
    let sockets = consecutive_sockets(4);
    let target: SocketAddr = sockets[0].local_addr().unwrap();
    let readers: Vec<_> = sockets
        .into_iter()
        .map(|socket| thread::spawn(move || receive_all(&socket)))
        .collect();

    let config = Config {
        target,
        streams: 4,
        rate_hz: 2_000,
        duration: Some(Duration::from_secs(2)),
        spin: Duration::from_micros(100),
        transport: Transport::Udp,
        wait_for_subscribers: None,
        camera: None,
        raw_camera: false,
    };
    let (stats, _elapsed) = run(&config, |_, _| {}).unwrap();
    assert_eq!(stats.ticks + stats.skipped_ticks, 4_000);
    assert_eq!(stats.messages, stats.ticks * 4);
    assert_eq!(stats.send_errors, 0);

    for (stream, reader) in readers.into_iter().enumerate() {
        let samples = reader.join().unwrap();
        // Loopback can drop a datagram under heavy load; allow 0.5 %.
        let sent = usize::try_from(stats.ticks).unwrap();
        assert!(
            samples.len() * 1_000 >= sent * 995,
            "stream {stream}: received {} of {sent}",
            samples.len()
        );
        // Sequence numbers only increase, so nothing arrived out of order.
        assert!(samples.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        assert_eq!(samples[0].seq, 0);
        assert!(samples.iter().all(|sample| {
            sample
                .joints
                .iter()
                .zip(READY_POSE)
                .all(|(joint, ready)| (joint - ready).abs() <= 1.0)
                && (0.0..=GRIPPER_OPEN).contains(&sample.gripper)
        }));
    }
}

#[test]
fn every_interval_is_reported_once_per_second_of_schedule() {
    let sockets = consecutive_sockets(1);
    let config = Config {
        target: sockets[0].local_addr().unwrap(),
        streams: 1,
        rate_hz: 1_000,
        duration: Some(Duration::from_millis(2_500)),
        spin: Duration::from_micros(100),
        transport: Transport::Udp,
        wait_for_subscribers: None,
        camera: None,
        raw_camera: false,
    };
    let mut reported = Vec::new();
    let (stats, _) = run(&config, |at, interval| reported.push((at, interval.ticks))).unwrap();
    assert_eq!(
        reported
            .iter()
            .map(|(at, _)| at.as_secs())
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(stats.ticks + stats.skipped_ticks, 2_500);
}

/// Reads length-prefixed frames until the sender closes the connection.
fn read_frames(mut stream: TcpStream) -> Vec<Sample> {
    let mut samples = Vec::new();
    let mut header = [0; 4];
    while stream.read_exact(&mut header).is_ok() {
        let length = usize::try_from(u32::from_le_bytes(header)).unwrap();
        assert_eq!(length, SIZE, "every frame holds one message");
        let mut message = vec![0; length];
        stream.read_exact(&mut message).unwrap();
        samples.push(Sample::decode(&message).unwrap());
    }
    samples
}

fn tcp_config(target: SocketAddr, streams: u16, transport: Transport) -> Config {
    Config {
        target,
        streams,
        rate_hz: 1_000,
        duration: Some(Duration::from_millis(1_500)),
        spin: Duration::from_micros(100),
        transport,
        wait_for_subscribers: None,
        camera: None,
        raw_camera: false,
    }
}

/// Binds `count` TCP listeners on consecutive loopback ports.
fn consecutive_listeners(count: u16) -> Vec<TcpListener> {
    for base in (20_011..60_000).step_by(89) {
        let listeners: Vec<TcpListener> = (0..count)
            .filter_map(|offset| TcpListener::bind(("127.0.0.1", base + offset)).ok())
            .collect();
        if listeners.len() == usize::from(count) {
            return listeners;
        }
    }
    panic!("no {count} consecutive free TCP ports on loopback");
}

#[test]
fn connecting_over_tcp_every_message_arrives_framed_by_its_length_in_order() {
    let listeners = consecutive_listeners(2);
    let target = listeners[0].local_addr().unwrap();
    let readers: Vec<_> = listeners
        .into_iter()
        .map(|listener| thread::spawn(move || read_frames(listener.accept().unwrap().0)))
        .collect();
    let (stats, _) = run(&tcp_config(target, 2, Transport::TcpConnect), |_, _| {}).unwrap();
    assert_eq!(stats.send_errors, 0);
    for reader in readers {
        let samples = reader.join().unwrap();
        // TCP loses nothing: every tick's message, numbered from 0.
        assert_eq!(samples.len(), usize::try_from(stats.ticks).unwrap());
        assert!(
            samples
                .iter()
                .zip(0..)
                .all(|(sample, seq)| sample.seq == seq)
        );
    }
}

/// Reads a camera's length-prefixed frames until the sender closes the
/// connection, each with when it arrived.
fn read_camera(mut stream: TcpStream) -> Vec<(Instant, Vec<u8>)> {
    let mut frames = Vec::new();
    let mut header = [0; 4];
    while stream.read_exact(&mut header).is_ok() {
        let mut frame = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
        stream.read_exact(&mut frame).unwrap();
        frames.push((Instant::now(), frame));
    }
    frames
}

#[test]
fn a_camera_over_tcp_sends_every_frame_a_jpeg_of_its_size_at_its_rate() {
    let listeners = consecutive_listeners(2);
    let target = listeners[0].local_addr().unwrap();
    let readers: Vec<_> = listeners
        .into_iter()
        .map(|listener| thread::spawn(move || read_camera(listener.accept().unwrap().0)))
        .collect();
    let config = Config {
        rate_hz: 30,
        duration: Some(Duration::from_secs(4)),
        camera: Some((320, 240)),
        raw_camera: false,
        ..tcp_config(target, 2, Transport::TcpConnect)
    };
    let (stats, _) = run(&config, |_, _| {}).unwrap();
    assert_eq!(stats.send_errors, 0);
    for reader in readers {
        let frames = reader.join().unwrap();
        assert_eq!(frames.len(), 120);
        for (_, frame) in &frames {
            let mut decoder = jpeg_decoder::Decoder::new(frame.as_slice());
            decoder.decode().unwrap();
            let info = decoder.info().unwrap();
            assert_eq!((info.width, info.height), (320, 240));
        }
        let span = frames[frames.len() - 1].0 - frames[0].0;
        #[allow(clippy::cast_precision_loss, reason = "120 frames")]
        let rate = (frames.len() - 1) as f64 / span.as_secs_f64();
        assert!((rate - 30.0).abs() <= 0.3, "{rate:.2} frames a second");
    }
}

#[test]
fn listening_over_tcp_messages_flow_once_kapto_connects_and_until_then_are_counted() {
    // Find a free port, then let the generator listen on it.
    let target = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = tcp_config(target, 1, Transport::TcpListen);
    let reader = thread::spawn(move || {
        // Connect a moment after the generator started, as Kapto would.
        thread::sleep(Duration::from_millis(300));
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match TcpStream::connect(target) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Err(error) => panic!("can't connect: {error}"),
            }
        };
        read_frames(stream)
    });
    let (stats, _) = run(&config, |_, _| {}).unwrap();
    let samples = reader.join().unwrap();
    assert_ne!(samples.len(), 0);
    assert!(stats.send_errors > 0, "messages before the connection");
    assert_eq!(
        u64::try_from(samples.len()).unwrap() + stats.send_errors,
        stats.messages
    );
    // In order and without gaps from the first one sent.
    let first = samples[0].seq;
    assert!(
        samples
            .iter()
            .zip(first..)
            .all(|(sample, seq)| sample.seq == seq)
    );
}

#[test]
fn connecting_before_the_receiver_listens_it_connects_as_soon_as_it_does() {
    // A port free now, listened on only after the generator has started.
    let target = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = Config {
        rate_hz: 100,
        ..tcp_config(target, 1, Transport::TcpConnect)
    };
    let sending = thread::spawn(move || run(&config, |_, _| {}).unwrap().0);
    thread::sleep(Duration::from_millis(300));
    let listener = TcpListener::bind(target).unwrap();
    let reader = thread::spawn(move || read_frames(listener.accept().unwrap().0));
    let stats = sending.join().unwrap();
    let received = reader.join().unwrap().len();
    // Refused until then, and connected within a few messages of it: not
    // a second later.
    assert!(
        stats.send_errors <= 40,
        "{} messages unsent",
        stats.send_errors
    );
    assert_eq!(
        u64::try_from(received).unwrap() + stats.send_errors,
        stats.ticks
    );
}
