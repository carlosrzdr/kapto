// SPDX-License-Identifier: Apache-2.0

//! Passing batches through the ring never allocates: the hot path must not
//! allocate per message or per batch.
//!
//! A counting allocator wraps the system allocator for this test binary. It
//! counts per thread, so tests running in parallel (and the test harness
//! itself) don't count each other's allocations.

#![allow(
    unsafe_code,
    reason = "a global allocator can only be written with unsafe trait methods"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use kapto_capture::{RingConfig, Sent, ring};

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
fn filling_sending_writing_and_returning_batches_never_allocates() {
    let (mut filler, mut drainer, stats) = ring(&RingConfig {
        batches: 8,
        batch_capacity: 64,
        max_message_len: 128,
        streams: 16,
    });
    let message = [7_u8; 68];
    let mut cycle = |filler: &mut kapto_capture::Filler| {
        for stream in 0..64 {
            filler.batch().push(stream % 16, &message);
        }
        let sent = filler.send();
        let mut written = 0;
        if let Some(batch) = drainer.take() {
            written += batch
                .messages()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>();
        }
        (sent, written)
    };

    // Warm up: the first round can touch lazily initialised state.
    cycle(&mut filler);

    let before = allocations();
    let mut written = 0;
    for _ in 0..10_000 {
        let (sent, bytes) = cycle(&mut filler);
        assert_eq!(sent, Sent::Handed);
        written += bytes;
    }
    let allocations = allocations() - before;

    assert_eq!(written, 10_000 * 64 * 68);
    assert_eq!(stats.dropped_batches(), 0);
    assert_eq!(allocations, 0, "the ring allocated {allocations} times");
}

#[test]
fn camera_frames_of_a_mebibyte_go_through_their_ring_without_allocating() {
    // A camera's ring: a frame a batch, slots of its declared size.
    let (mut filler, mut drainer, stats) = ring(&RingConfig {
        batches: 62,
        batch_capacity: 1,
        max_message_len: 1 << 20,
        streams: 2,
    });
    let frame = vec![7_u8; 1 << 20];
    let mut cycle = |filler: &mut kapto_capture::Filler| {
        assert!(filler.batch().push(1, &frame));
        let sent = filler.send();
        let written = drainer.take().map_or(0, |batch| {
            batch.messages().map(|(_, bytes)| bytes.len()).sum()
        });
        (sent, written)
    };
    cycle(&mut filler);

    let before = allocations();
    let mut written = 0;
    for _ in 0..1_000 {
        let (sent, bytes) = cycle(&mut filler);
        assert_eq!(sent, Sent::Handed);
        written += bytes;
    }
    let allocations = allocations() - before;

    assert_eq!(written, 1_000 << 20);
    assert_eq!(stats.dropped_batches(), 0);
    assert_eq!(allocations, 0, "the ring allocated {allocations} times");
}

#[test]
fn dropping_messages_when_the_writer_is_stalled_never_allocates() {
    let (mut filler, _drainer, stats) = ring(&RingConfig {
        batches: 2,
        batch_capacity: 16,
        max_message_len: 8,
        streams: 4,
    });
    filler.batch().push(0, b"x");
    filler.send();

    let before = allocations();
    for _ in 0..10_000 {
        filler.batch().push(1, b"y");
        assert_eq!(filler.send(), Sent::Dropped { messages: 1 });
    }
    assert_eq!(allocations() - before, 0);
    assert_eq!(stats.dropped_messages(1), 10_000);
}

#[test]
fn counting_received_and_lost_messages_never_allocates() {
    use std::sync::Arc;

    use kapto_capture::loss::{ByteOrder, RateGap, SequenceField, SequenceWidth};
    use kapto_capture::{Accounting, Batch, LossCounters, LossDetection};
    use kapto_core::RecvTime;

    let counters = Arc::new(LossCounters::new(2));
    let sequence = LossDetection::Sequence(SequenceField {
        offset: 0,
        width: SequenceWidth::U32,
        byte_order: ByteOrder::Little,
    });
    let rate = LossDetection::RateGap(RateGap::for_rate(2_000));
    let mut accounting = Accounting::new(Arc::clone(&counters), &[(0, sequence), (1, rate)]);
    let mut batch = Batch::new(64, 68);
    let mut number = 0_u32;
    let mut fill = |batch: &mut Batch, round: u64| {
        batch.clear();
        for _ in 0..32 {
            // Every tenth number goes missing, so gaps are counted too.
            number += if number.is_multiple_of(10) { 2 } else { 1 };
            batch.push(0, &number.to_le_bytes());
            batch.push(1, &[0; 68]);
        }
        batch.set_recv_time(RecvTime::from_nanos(round * 16_000_000));
    };

    fill(&mut batch, 0);
    accounting.count(&batch);

    let before = allocations();
    for round in 1..10_000 {
        fill(&mut batch, round);
        accounting.count(&batch);
    }
    assert_eq!(allocations() - before, 0);
    assert_eq!(counters.totals(0).received, 10_000 * 32);
    assert!(counters.totals(0).lost_upstream > 0);
}

#[test]
fn writing_messages_allocates_per_chunk_not_per_message() {
    use kapto_capture::{Batch, LossCounters, Recording, WriterConfig};
    use kapto_core::{ClockMap, ClockSample, RecvTime};

    let path = std::env::temp_dir().join(format!("kapto-{}-alloc.mcap", std::process::id()));
    let clock = ClockMap::new(ClockSample::now());
    let streams = [(0, "arm".parse().unwrap()), (1, "gripper".parse().unwrap())]
        .map(kapto_capture::ChannelSpec::from);
    let config = WriterConfig::default();
    let mut recording = Recording::create(&path, &streams, &clock, &config).unwrap();
    let counters = LossCounters::new(2);
    let mut batch = Batch::new(64, 68);
    let fill = |batch: &mut Batch, round: u64| {
        batch.clear();
        for n in 0..64_u16 {
            batch.push(n % 2, &[u8::try_from(round % 251).unwrap(); 68]);
        }
        batch.set_recv_time(RecvTime::from_nanos(round * 1_000_000));
    };
    // Warm up through several chunks, so buffers reach their working size.
    for round in 0..2_000 {
        fill(&mut batch, round);
        recording.write(&batch, &counters).unwrap();
    }

    let messages = 10_000 * 64;
    let before = allocations();
    for round in 2_000..12_000 {
        fill(&mut batch, round);
        recording.write(&batch, &counters).unwrap();
    }
    let allocations = allocations() - before;
    recording.finish(&clock).unwrap();
    std::fs::remove_file(&path).unwrap();

    // The MCAP library allocates when it starts and finishes a chunk (its
    // compressor and message index), about 25 to 40 times per chunk. Measured
    // over these messages: 16 KiB chunks about 100 000 allocations, 64 KiB
    // about 31 000, 1 MiB (the default) about 2 600. Per chunk, then, never
    // per message.
    assert!(
        allocations * 100 < messages,
        "{allocations} allocations for {messages} messages"
    );
}

#[test]
fn writing_camera_frames_allocates_no_more_than_their_chunks_need() {
    use kapto_capture::{Batch, LossCounters, Recording, WriterConfig};
    use kapto_core::{ClockMap, ClockSample, RecvTime};

    let path = std::env::temp_dir().join(format!("kapto-{}-frames.mcap", std::process::id()));
    let clock = ClockMap::new(ClockSample::now());
    let streams = [(0, "camera".parse().unwrap())].map(kapto_capture::ChannelSpec::from);
    let config = WriterConfig::default();
    let mut recording = Recording::create(&path, &streams, &clock, &config).unwrap();
    let counters = LossCounters::new(1);
    let mut batch = Batch::new(1, 1 << 20);
    let frames: Vec<Vec<u8>> = (0..2_u8).map(|byte| vec![byte; 1 << 20]).collect();
    let fill = |batch: &mut Batch, round: u64| {
        batch.clear();
        batch.push(0, &frames[usize::from(round % 2 == 1)]);
        batch.set_recv_time(RecvTime::from_nanos(round * 33_000_000));
    };
    for round in 0..20 {
        fill(&mut batch, round);
        recording.write(&batch, &counters).unwrap();
    }

    let written = 200;
    let before = allocations();
    for round in 20..20 + written {
        fill(&mut batch, round);
        recording.write(&batch, &counters).unwrap();
    }
    let allocations = allocations() - before;
    recording.finish(&clock).unwrap();
    std::fs::remove_file(&path).unwrap();

    // A 1 MiB frame fills the default 1 MiB chunk, so the MCAP library's
    // allocations for each chunk (its compressor and message index) come
    // with each frame: about 9 measured, at a camera's 30 a second. The
    // receiver and the ring never allocate for one (above).
    assert!(
        allocations <= written * 16,
        "{allocations} allocations writing {written} frames"
    );
}

#[test]
fn offering_messages_to_the_viewer_tap_never_allocates() {
    use kapto_capture::Batch;
    use kapto_capture::tap::{TapConfig, tap};

    // Four streams per batch and two batches between drains need eight
    // slots; six make the tap run out every other round.
    let config = TapConfig {
        slots: 6,
        max_message_len: 68,
        streams: 4,
    };
    let (mut feed, mut drain, stats) = tap(&config);
    let mut batch = Batch::new(64, 68);
    for n in 0..64_u16 {
        batch.push(n % 4, &[7; 68]);
    }
    let before = allocations();
    for round in 0..1_000 {
        feed.offer(&batch);
        if round % 2 == 0 {
            drain.each(|_, _, _| {});
        }
    }
    let allocated = allocations() - before;
    assert!(stats.skipped() > 0, "the skip path wasn't exercised");
    assert_eq!(
        allocated, 0,
        "offering to the tap allocated {allocated} times"
    );
}

#[test]
fn keeping_messages_for_later_episodes_never_allocates() {
    use kapto_capture::{Batch, Kept, KeptStream};

    let mut kept = Kept::new(&[KeptStream {
        stream: 1,
        depth: 3,
        max_message_len: 68,
    }]);
    let mut batch = Batch::new(64, 68);
    for n in 0..64_u16 {
        batch.push(n % 4, &[7; 68]);
    }
    let before = allocations();
    for _ in 0..1_000 {
        kept.keep(&batch);
    }
    let allocated = allocations() - before;
    assert_eq!(kept.messages().count(), 3);
    assert_eq!(allocated, 0, "keeping messages allocated {allocated} times");
}
