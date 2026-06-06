// SPDX-License-Identifier: Apache-2.0

//! Receiving UDP datagrams or TCP messages, counting them and handing them
//! to the writer never allocates once warmed up: the hot path must not allocate per
//! message or per batch.
//!
//! A counting allocator wraps the system allocator for this test binary. It
//! counts per thread, and only the calls under test run between readings.

#![allow(
    unsafe_code,
    reason = "a global allocator can only be written with unsafe trait methods"
)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;

thread_local! {
    // A `const` initialiser: reading it never allocates, so the allocator can
    // use it without recursing into itself.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count_allocation() {
    ALLOCATIONS.with(|count| count.set(count.get() + 1));
}

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

// SAFETY: every method delegates to the system allocator with the arguments
// it was given, which upholds `GlobalAlloc`'s contract; counting has no effect
// on the memory handed out.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation();
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn receiving_counting_and_sending_udp_datagrams_never_allocates() {
    use std::net::UdpSocket;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use kapto_capture::loss::{ByteOrder, SequenceField, SequenceWidth};
    use kapto_capture::{Accounting, LossCounters, LossDetection, RingConfig, ring};
    use kapto_transport::{Receiver, ReceiverConfig, UdpOptions, UdpSocketConfig, UdpTransport};

    let config = UdpSocketConfig::new(0, "127.0.0.1:0".parse().unwrap());
    let transport = UdpTransport::open(&[config], UdpOptions::default()).unwrap();
    let to = transport.local_addrs().next().unwrap();
    let (filler, mut drainer, stats) = ring(&RingConfig {
        batches: 8,
        batch_capacity: 64,
        max_message_len: 68,
        streams: 1,
    });
    let counters = Arc::new(LossCounters::new(1));
    let detection = LossDetection::Sequence(SequenceField {
        offset: 0,
        width: SequenceWidth::U32,
        byte_order: ByteOrder::Little,
    });
    let accounting = Accounting::new(Arc::clone(&counters), &[(0, detection)]);
    let mut receiver = Receiver::new(
        transport,
        (filler, stats),
        accounting,
        Arc::clone(&counters),
        ReceiverConfig::default(),
    );
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    // With stop already set, each run reads what is waiting, sends it and
    // returns, so the whole round trip happens on this thread.
    let stop = AtomicBool::new(true);
    let mut payload = [0_u8; 68];
    let mut next = 0_u32;
    let mut round = |allocated: &mut u64| {
        for _ in 0..32 {
            payload[..4].copy_from_slice(&next.to_le_bytes());
            next += 1;
            sender.send_to(&payload, to).unwrap();
        }
        let before = allocations();
        while counters.totals(0).received < u64::from(next) {
            receiver.run(&stop).unwrap();
            while let Some(batch) = drainer.take() {
                drop(batch);
            }
        }
        *allocated += allocations() - before;
    };

    let mut warm_up = 0;
    round(&mut warm_up);
    let mut allocated = 0;
    for _ in 0..1_000 {
        round(&mut allocated);
    }
    assert_eq!(counters.totals(0).received, 1_001 * 32);
    assert_eq!(counters.totals(0).lost_upstream, 0);
    assert_eq!(allocated, 0, "the receive path allocated {allocated} times");
}

#[test]
fn cutting_frames_out_of_a_tcp_byte_stream_never_allocates() {
    use kapto_transport::tcp::{Deframer, Framing, LengthOrder};

    let framing = Framing::LengthPrefix {
        header_bytes: 4,
        length_offset: 0,
        length_bytes: 4,
        order: LengthOrder::Little,
        includes_header: false,
    };
    let mut deframer = Deframer::new(framing, 128, 4_096);
    // One message: a 4-byte length (64) and a 64-byte body; 40 per read,
    // with one over-long message to skip in each read as well.
    let mut message = 64_u32.to_le_bytes().to_vec();
    message.extend([7_u8; 64]);
    let mut read = Vec::new();
    for _ in 0..40 {
        read.extend(&message);
    }
    read.extend(1_000_u32.to_le_bytes());
    read.extend([0_u8; 1_000]);

    let before = allocations();
    let mut frames = 0;
    for _ in 0..1_000 {
        let mut rest = read.as_slice();
        while !rest.is_empty() {
            let space = deframer.space();
            let size = space.len().min(rest.len());
            space[..size].copy_from_slice(&rest[..size]);
            deframer.filled(size);
            rest = &rest[size..];
            while let Some(frame) = deframer.next_frame() {
                assert_eq!(frame.len(), 68);
                frames += 1;
            }
        }
    }
    let allocated = allocations() - before;
    assert_eq!(frames, 40_000);
    assert_eq!(deframer.too_long(), 1_000);
    assert_eq!(allocated, 0, "deframing allocated {allocated} times");
}

#[test]
fn receiving_messages_from_a_tcp_connection_never_allocates() {
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    use kapto_capture::Batch;
    use kapto_transport::{Framing, TcpConfig, TcpEndpoint, TcpTransport, Transport};

    let listen = TcpEndpoint::Listen("127.0.0.1:0".parse().unwrap());
    let mut transport =
        TcpTransport::open(&TcpConfig::new(0, listen, Framing::Fixed(68), 68)).unwrap();
    let mut robot = TcpStream::connect(transport.local_addr().unwrap()).unwrap();
    let mut batch = Batch::new(64, 68);
    let read = [7_u8; 68 * 32];
    let mut round = |allocated: &mut u64| {
        robot.write_all(&read).unwrap();
        let before = allocations();
        while batch.len() < 32 {
            transport
                .receive(&mut batch, Duration::from_secs(5))
                .unwrap();
        }
        *allocated += allocations() - before;
        batch.clear();
    };

    // The first round accepts the connection, which allocates.
    let mut warm_up = 0;
    round(&mut warm_up);
    let mut allocated = 0;
    for _ in 0..1_000 {
        round(&mut allocated);
    }
    assert_eq!(
        allocated, 0,
        "receiving over TCP allocated {allocated} times"
    );
}
