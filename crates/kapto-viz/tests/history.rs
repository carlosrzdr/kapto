// SPDX-License-Identifier: Apache-2.0

//! Looking back over the live view's last minutes: any stretch, merged to
//! any number of points, keeps every message's extremes and count.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use kapto_viz::history::{History, Record, SampleRecord};
use proptest::prelude::*;

/// A record per millisecond from `values`, each one sample of stream 0
/// holding one value, its own minimum and maximum.
fn history(values: &[(f64, f64)]) -> History {
    let mut history = History::new(i64::MAX, usize::MAX);
    for (index, &(low, high)) in values.iter().enumerate() {
        let time = 1_000_000 * i64::try_from(index).unwrap();
        history.push(Record {
            received_utc_ns: time,
            poses: None,
            samples: vec![SampleRecord {
                stream: 0,
                messages: 2,
                first_utc_ns: time,
                newest_utc_ns: time,
                extremes: vec![(low.min(high), low.max(high))],
            }],
        });
    }
    history
}

proptest! {
    #[test]
    fn each_part_holds_the_extremes_and_count_of_the_samples_it_covers(
        values in prop::collection::vec((-1e6..1e6_f64, -1e6..1e6_f64), 1..3000),
        from in 0..3000_i64,
        length in 0..3000_i64,
        points in 1..200_u32,
    ) {
        let (from, to) = (from * 1_000_000, (from + length) * 1_000_000);
        let stretch = history(&values).snapshot().stretch(from, to, points);
        let covered: Vec<(i64, (f64, f64))> = values
            .iter()
            .enumerate()
            .map(|(index, &(low, high))| {
                (1_000_000 * i64::try_from(index).unwrap(), (low.min(high), low.max(high)))
            })
            .filter(|(time, _)| (from..=to).contains(time))
            .collect();
        let parts = stretch.streams.first().map_or(&[][..], |(_, parts)| parts.as_slice());
        prop_assert!(parts.len() <= points as usize);
        prop_assert_eq!(parts.iter().map(|part| part.messages).sum::<u64>(), 2 * covered.len() as u64);
        for part in parts {
            let inside: Vec<(f64, f64)> = covered
                .iter()
                .filter(|(time, _)| (part.first_utc_ns..=part.newest_utc_ns).contains(time))
                .map(|(_, extremes)| *extremes)
                .collect();
            let low = inside.iter().map(|value| value.0).fold(f64::INFINITY, f64::min);
            let high = inside.iter().map(|value| value.1).fold(f64::NEG_INFINITY, f64::max);
            prop_assert_eq!(part.extremes[0], (low, high));
        }
    }
}
