// SPDX-License-Identifier: Apache-2.0

//! Benchmark of receiving a batch of UDP datagrams: the system calls that move
//! waiting datagrams into a batch's slots.
//!
//! Datagrams are queued on a loopback socket first (untimed), then one
//! `receive` call takes them all (timed), across a grid of datagrams per
//! batch and datagram sizes. Run with
//! `cargo bench -p kapto-transport --bench receive`.

#![allow(
    clippy::unwrap_used,
    reason = "benchmark setup fails loudly when the setup itself breaks"
)]

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kapto_capture::Batch;
use kapto_transport::{Transport, UdpOptions, UdpSocketConfig, UdpTransport};

const MESSAGES: [usize; 4] = [1, 8, 64, 256];
const SIZES: [usize; 3] = [68, 256, 1024];

/// A transport on a free loopback port and a socket to send to it.
fn pair() -> (UdpTransport, UdpSocket, SocketAddr) {
    let config = UdpSocketConfig::new(0, "127.0.0.1:0".parse().unwrap());
    let transport = UdpTransport::open(&[config], UdpOptions::default()).unwrap();
    let target = transport.local_addrs().next().unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    (transport, sender, target)
}

/// Queues `messages` datagrams, then receives them; returns how many arrived.
fn queue_and_receive(
    transport: &mut UdpTransport,
    sender: &UdpSocket,
    target: SocketAddr,
    batch: &mut Batch,
    payload: &[u8],
    messages: usize,
) -> (usize, Duration) {
    for _ in 0..messages {
        sender.send_to(payload, target).unwrap();
    }
    batch.clear();
    let start = Instant::now();
    let received = transport.receive(batch, Duration::ZERO).unwrap();
    (received, start.elapsed())
}

fn receive(c: &mut Criterion) {
    let mut group = c.benchmark_group("udp_receive");
    for messages in MESSAGES {
        for size in SIZES {
            let (mut transport, sender, target) = pair();
            let mut batch = Batch::new(messages, size);
            let payload = vec![7_u8; size];
            // Larger grids may not fit the socket's receive buffer on this
            // machine; measuring them would time a partial batch, so they
            // are skipped instead.
            let (received, _) = queue_and_receive(
                &mut transport,
                &sender,
                target,
                &mut batch,
                &payload,
                messages,
            );
            if received != messages {
                eprintln!(
                    "skipping {messages}x{size}B: only {received} datagrams fit the receive buffer"
                );
                continue;
            }
            group.throughput(Throughput::Elements(messages as u64));
            group.bench_function(
                BenchmarkId::from_parameter(format!("{messages}x{size}B")),
                |b| {
                    b.iter_custom(|iterations| {
                        let mut timed = Duration::ZERO;
                        for _ in 0..iterations {
                            let (received, elapsed) = queue_and_receive(
                                &mut transport,
                                &sender,
                                target,
                                &mut batch,
                                &payload,
                                messages,
                            );
                            assert_eq!(received, messages, "datagrams were lost on loopback");
                            timed += elapsed;
                        }
                        timed
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, receive);
criterion_main!(benches);
