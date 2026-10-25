// SPDX-License-Identifier: Apache-2.0

//! The UDP transport over loopback: what it receives, what it discards and
//! counts, and how each failure is reported.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::net::{IpAddr, SocketAddr, UdpSocket};

use kapto_capture::Batch;
use kapto_core::{ErrorCode, KaptoError};
use kapto_transport::{Transport, UdpOptions, UdpSocketConfig, UdpTransport};

mod common;

use common::{Sender, WAIT, conformance, receive_until};

fn open(sockets: &[UdpSocketConfig]) -> UdpTransport {
    UdpTransport::open(sockets, UdpOptions::default()).unwrap()
}

/// Sends `payload` from `from` (an IP with a free port) to `to`.
fn send(from: &str, to: SocketAddr, payload: &[u8]) {
    UdpSocket::bind(from).unwrap().send_to(payload, to).unwrap();
}

fn udp_on(ip: &'static str) -> (UdpTransport, Sender) {
    let transport = open(&[UdpSocketConfig::new(
        0,
        SocketAddr::new(ip.parse().unwrap(), 0),
    )]);
    let to = transport.local_addrs().next().unwrap();
    let from = format!("{}", SocketAddr::new(to.ip(), 0));
    (transport, Box::new(move |payload| send(&from, to, payload)))
}

#[test]
fn udp_over_ipv4_meets_the_transport_contract() {
    conformance(|| udp_on("127.0.0.1"));
}

#[test]
fn udp_over_ipv6_meets_the_transport_contract() {
    conformance(|| udp_on("::1"));
}

#[test]
fn datagrams_on_several_ports_are_tagged_with_each_ports_stream() {
    let mut transport = open(&[
        UdpSocketConfig::new(3, "127.0.0.1:0".parse().unwrap()),
        UdpSocketConfig::new(5, "127.0.0.1:0".parse().unwrap()),
    ]);
    let ports: Vec<_> = transport.local_addrs().collect();
    send("127.0.0.1:0", ports[1], b"b");
    send("127.0.0.1:0", ports[0], b"a");
    let mut batch = Batch::new(8, 16);
    receive_until(&mut transport, &mut batch, 2);
    let mut messages: Vec<_> = batch.messages().collect();
    messages.sort_unstable();
    assert_eq!(messages, [(3, &b"a"[..]), (5, &b"b"[..])]);
}

#[test]
fn every_port_is_read_when_more_are_ready_at_once_than_one_wait_reports() {
    // More than one wait reports (64): the rest are read after the next.
    let configs: Vec<_> = (0_u16..100)
        .map(|stream| UdpSocketConfig::new(stream, "127.0.0.1:0".parse().unwrap()))
        .collect();
    let mut transport = open(&configs);
    let ports: Vec<_> = transport.local_addrs().collect();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    for (number, port) in (0_u16..).zip(&ports) {
        sender.send_to(&number.to_le_bytes(), port).unwrap();
    }
    let mut batch = Batch::new(256, 2);
    receive_until(&mut transport, &mut batch, 100);
    let mut received: Vec<(u16, u16)> = batch
        .messages()
        .map(|(stream, bytes)| (stream, u16::from_le_bytes(bytes.try_into().unwrap())))
        .collect();
    received.sort_unstable();
    assert_eq!(received, (0..100).map(|n| (n, n)).collect::<Vec<_>>());
}

#[test]
fn many_waiting_datagrams_arrive_in_order_across_batches() {
    let (mut transport, send) = udp_on("127.0.0.1");
    for number in 0_u32..150 {
        send(&number.to_le_bytes());
    }
    let mut batch = Batch::new(64, 4);
    let mut numbers = Vec::new();
    while numbers.len() < 150 && transport.receive(&mut batch, WAIT).unwrap() > 0 {
        numbers.extend(
            batch
                .messages()
                .map(|(_, bytes)| u32::from_le_bytes(bytes.try_into().unwrap())),
        );
        batch.clear();
    }
    assert_eq!(numbers, (0..150).collect::<Vec<_>>());
}

#[test]
fn datagrams_from_sources_outside_the_allow_list_are_discarded_and_counted() {
    let mut config = UdpSocketConfig::new(0, "127.0.0.1:0".parse().unwrap());
    config.allowed_sources = vec!["127.0.0.2".parse().unwrap()];
    let mut transport = open(&[config]);
    let to = transport.local_addrs().next().unwrap();
    send("127.0.0.1:0", to, b"not allowed");
    send("127.0.0.2:0", to, b"allowed");
    let mut batch = Batch::new(4, 16);
    receive_until(&mut transport, &mut batch, 1);
    assert_eq!(batch.messages().collect::<Vec<_>>(), [(0, &b"allowed"[..])]);
    assert_eq!(transport.stats().rejected(0), 1);
}

#[test]
fn datagrams_longer_than_a_slot_are_discarded_and_counted() {
    let (mut transport, send) = udp_on("127.0.0.1");
    send(b"twenty bytes long...");
    send(b"fits");
    let mut batch = Batch::new(4, 8);
    receive_until(&mut transport, &mut batch, 1);
    assert_eq!(batch.messages().collect::<Vec<_>>(), [(0, &b"fits"[..])]);
    assert_eq!(transport.stats().truncated(0), 1);
}

#[test]
fn a_port_already_in_use_fails_with_bind_failed() {
    let holder = UdpSocket::bind("127.0.0.1:0").unwrap();
    let taken = holder.local_addr().unwrap();
    let error = UdpTransport::open(&[UdpSocketConfig::new(0, taken)], UdpOptions::default());
    assert_eq!(error.unwrap_err().code(), ErrorCode::TransportUdpBindFailed);
}

#[test]
fn joining_an_address_that_is_not_a_multicast_group_fails_with_its_code() {
    let mut config = UdpSocketConfig::new(0, "0.0.0.0:0".parse().unwrap());
    config.multicast_group = Some("127.0.0.1".parse().unwrap());
    let error = UdpTransport::open(&[config], UdpOptions::default()).unwrap_err();
    assert_eq!(error.code(), ErrorCode::TransportUdpMulticastJoinFailed);
}

#[test]
fn a_receive_buffer_larger_than_the_host_allows_is_reported_once_as_a_warning() {
    let options = UdpOptions {
        receive_buffer_bytes: 1 << 30,
    };
    let sockets: Vec<_> = (0..3)
        .map(|stream| UdpSocketConfig::new(stream, "127.0.0.1:0".parse().unwrap()))
        .collect();
    let transport = UdpTransport::open(&sockets, options).unwrap();
    let codes: Vec<_> = transport.warnings().iter().map(KaptoError::code).collect();
    assert_eq!(codes, [ErrorCode::TransportUdpReceiveBufferSmall]);
    let message = transport.warnings()[0].message();
    assert!(message.contains("asked for 1024 MiB"), "{message}");
    assert!(message.contains("on 3 sockets"), "{message}");
}

#[test]
#[ignore = "needs a network interface with multicast; shared CI runners often have none"]
fn multicast_datagrams_are_received_after_joining_the_group() {
    let group: IpAddr = "239.255.42.1".parse().unwrap();
    let mut config = UdpSocketConfig::new(0, "0.0.0.0:0".parse().unwrap());
    config.multicast_group = Some(group);
    let mut transport = open(&[config]);
    let port = transport.local_addrs().next().unwrap().port();
    send("0.0.0.0:0", SocketAddr::new(group, port), b"to the group");
    let mut batch = Batch::new(4, 16);
    receive_until(&mut transport, &mut batch, 1);
    assert_eq!(batch.messages().next(), Some((0, &b"to the group"[..])));
}
