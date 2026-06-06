// SPDX-License-Identifier: Apache-2.0

//! The TCP transport over loopback, in both directions: what it receives,
//! how it survives a robot that disconnects or isn't there yet, and what it
//! counts.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::Batch;
use kapto_core::ErrorCode;
use kapto_transport::{Framing, LengthOrder, TcpConfig, TcpEndpoint, TcpTransport, Transport};

mod common;

use common::{Sender, WAIT, conformance};

/// A two-byte big-endian length before each message.
const LENGTH_PREFIX: Framing = Framing::LengthPrefix {
    header_bytes: 2,
    length_offset: 0,
    length_bytes: 2,
    order: LengthOrder::Big,
    includes_header: false,
};

fn newline() -> Framing {
    Framing::Delimiter(b"\n".to_vec())
}

fn listening(framing: Framing, max_message_bytes: usize) -> TcpTransport {
    let endpoint = TcpEndpoint::Listen("127.0.0.1:0".parse().unwrap());
    TcpTransport::open(&TcpConfig::new(0, endpoint, framing, max_message_bytes)).unwrap()
}

fn connect_to(transport: &TcpTransport) -> TcpStream {
    let robot = TcpStream::connect(transport.local_addr().unwrap()).unwrap();
    robot.set_nodelay(true).unwrap();
    robot
}

/// Receives until `batch` holds `count` messages or `WAIT` has passed,
/// carrying on through receives that return nothing.
fn receive_for(transport: &mut TcpTransport, batch: &mut Batch, count: usize) {
    let deadline = Instant::now() + WAIT;
    while batch.len() < count && Instant::now() < deadline {
        transport.receive(batch, Duration::from_millis(20)).unwrap();
    }
}

fn messages(batch: &Batch) -> Vec<&[u8]> {
    batch.messages().map(|(_, message)| message).collect()
}

/// An address on loopback that nothing listens on.
fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[test]
fn listening_for_the_robot_meets_the_transport_contract() {
    conformance(|| {
        let transport = listening(Framing::Delimiter(b"\r\n".to_vec()), 64);
        let robot = connect_to(&transport);
        let send: Sender = Box::new(move |payload| {
            (&robot).write_all(&[payload, b"\r\n"].concat()).unwrap();
        });
        (transport, send)
    });
}

#[test]
fn connecting_to_the_robot_meets_the_transport_contract() {
    conformance(|| {
        let robot = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = TcpEndpoint::Connect(robot.local_addr().unwrap());
        let mut transport =
            TcpTransport::open(&TcpConfig::new(0, endpoint, newline(), 64)).unwrap();
        transport
            .receive(&mut Batch::new(1, 64), Duration::from_millis(10))
            .unwrap();
        assert!(transport.is_connected());
        let (robot, _) = robot.accept().unwrap();
        let send: Sender = Box::new(move |payload| {
            (&robot).write_all(&[payload, b"\n"].concat()).unwrap();
        });
        (transport, send)
    });
}

#[test]
fn length_prefixed_messages_sent_a_byte_at_a_time_arrive_whole_with_their_header() {
    let mut transport = listening(LENGTH_PREFIX, 64);
    let mut robot = connect_to(&transport);
    let writer = thread::spawn(move || {
        for byte in b"\x00\x05hello\x00\x05world" {
            robot.write_all(&[*byte]).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        robot
    });
    let mut batch = Batch::new(4, 64);
    receive_for(&mut transport, &mut batch, 2);
    assert_eq!(
        messages(&batch),
        [&b"\x00\x05hello"[..], &b"\x00\x05world"[..]]
    );
    drop(writer.join().unwrap());
}

#[test]
fn a_lost_connection_is_counted_and_its_partial_message_never_stored() {
    let mut transport = listening(newline(), 64);
    let mut first = connect_to(&transport);
    first.write_all(b"before\nhalf a mess").unwrap();
    let mut batch = Batch::new(4, 64);
    receive_for(&mut transport, &mut batch, 1);
    drop(first);

    let mut second = connect_to(&transport);
    second.write_all(b"after\n").unwrap();
    receive_for(&mut transport, &mut batch, 2);

    assert_eq!(messages(&batch), [&b"before"[..], &b"after"[..]]);
    assert_eq!(transport.stats().disconnects(0), 1);
    assert_eq!(transport.stats().truncated(0), 0);
}

#[test]
fn a_robot_reconnecting_while_its_old_connection_looks_open_replaces_it() {
    let mut transport = listening(newline(), 64);
    let mut old = connect_to(&transport);
    old.write_all(b"old\n").unwrap();
    let mut batch = Batch::new(4, 64);
    receive_for(&mut transport, &mut batch, 1);

    // The old connection stays open, as one does when the robot restarts
    // without closing it.
    let mut new = connect_to(&transport);
    new.write_all(b"new\n").unwrap();
    receive_for(&mut transport, &mut batch, 2);

    assert_eq!(messages(&batch), [&b"old"[..], &b"new"[..]]);
    assert_eq!(transport.stats().disconnects(0), 1);
    drop(old);
}

#[test]
fn connecting_keeps_trying_until_the_robot_is_there() {
    let address = free_address();
    let endpoint = TcpEndpoint::Connect(address);
    let mut transport = TcpTransport::open(&TcpConfig::new(0, endpoint, newline(), 64)).unwrap();
    let mut batch = Batch::new(4, 64);
    assert_eq!(
        transport
            .receive(&mut batch, Duration::from_millis(50))
            .unwrap(),
        0
    );
    assert!(!transport.is_connected());

    let robot = TcpListener::bind(address).unwrap();
    let robot = thread::spawn(move || {
        let (mut connection, _) = robot.accept().unwrap();
        connection.write_all(b"here\n").unwrap();
        connection
    });
    receive_for(&mut transport, &mut batch, 1);
    assert_eq!(messages(&batch), [&b"here"[..]]);
    drop(robot.join().unwrap());
}

#[test]
fn a_receive_returns_after_its_timeout_while_the_robot_is_absent() {
    let endpoint = TcpEndpoint::Connect(free_address());
    let mut transport = TcpTransport::open(&TcpConfig::new(0, endpoint, newline(), 64)).unwrap();
    let started = Instant::now();
    for _ in 0..5 {
        transport
            .receive(&mut Batch::new(4, 64), Duration::from_millis(20))
            .unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn messages_longer_than_the_stream_allows_are_skipped_and_counted() {
    let mut transport = listening(newline(), 8);
    let mut robot = connect_to(&transport);
    robot.write_all(b"twenty bytes long...\nfits\n").unwrap();
    let mut batch = Batch::new(4, 64);
    receive_for(&mut transport, &mut batch, 1);
    assert_eq!(messages(&batch), [&b"fits"[..]]);
    assert_eq!(transport.stats().truncated(0), 1);
}

#[test]
fn messages_longer_than_a_batch_slot_are_skipped_and_counted() {
    let mut transport = listening(newline(), 64);
    let mut robot = connect_to(&transport);
    robot.write_all(b"too long for a slot\nfits\n").unwrap();
    let mut batch = Batch::new(4, 8);
    receive_for(&mut transport, &mut batch, 1);
    assert_eq!(messages(&batch), [&b"fits"[..]]);
    assert_eq!(transport.stats().truncated(0), 1);
}

#[test]
fn many_messages_arrive_in_order_across_batches() {
    let mut transport = listening(Framing::Fixed(4), 4);
    let mut robot = connect_to(&transport);
    let bytes: Vec<u8> = (0_u32..1_000).flat_map(u32::to_le_bytes).collect();
    robot.write_all(&bytes).unwrap();
    let mut batch = Batch::new(64, 4);
    let mut numbers = Vec::new();
    while numbers.len() < 1_000 && transport.receive(&mut batch, WAIT).unwrap() > 0 {
        numbers.extend(
            batch
                .messages()
                .map(|(_, bytes)| u32::from_le_bytes(bytes.try_into().unwrap())),
        );
        batch.clear();
    }
    assert_eq!(numbers, (0..1_000).collect::<Vec<_>>());
}

#[test]
fn a_port_already_in_use_fails_with_listen_failed() {
    let holder = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = TcpEndpoint::Listen(holder.local_addr().unwrap());
    let error = TcpTransport::open(&TcpConfig::new(0, endpoint, newline(), 64)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::TransportTcpListenFailed);
}
