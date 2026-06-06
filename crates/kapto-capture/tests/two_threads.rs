// SPDX-License-Identifier: Apache-2.0

//! The ring used the way capture uses it: a receiver thread and a writer
//! thread running at once.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::thread;
use std::time::Duration;

use kapto_capture::{RingConfig, ring};

const MESSAGES: u64 = 200_000;

/// Sends `MESSAGES` numbered messages through a small ring while the writer
/// pauses now and then, so both delivery and drops happen.
#[test]
fn every_message_is_delivered_in_order_or_counted_as_dropped() {
    let (mut filler, mut drainer, stats) = ring(&RingConfig {
        batches: 4,
        batch_capacity: 32,
        max_message_len: 8,
        streams: 1,
    });

    let receiver = thread::spawn(move || {
        for number in 0..MESSAGES {
            if !filler.batch().push(0, &number.to_le_bytes()) {
                filler.send();
                filler.batch().push(0, &number.to_le_bytes());
            }
        }
        filler.send();
    });

    let writer = thread::spawn(move || {
        let mut delivered = Vec::new();
        let mut batches = 0_u64;
        while !drainer.is_finished() {
            let Some(batch) = drainer.take() else {
                thread::yield_now();
                continue;
            };
            for (_, bytes) in batch.messages() {
                delivered.push(u64::from_le_bytes(bytes.try_into().unwrap()));
            }
            batches += 1;
            if batches.is_multiple_of(500) {
                // A writer stall long enough to fill the ring.
                thread::sleep(Duration::from_millis(2));
            }
        }
        delivered
    });

    receiver.join().unwrap();
    let delivered = writer.join().unwrap();
    assert!(delivered.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(delivered.len() as u64 + stats.dropped_messages(0), MESSAGES);
    assert!(
        stats.dropped_messages(0) > 0,
        "the stalls should have caused drops"
    );
    assert!(stats.max_queued() <= stats.batches());
}
