// SPDX-License-Identifier: Apache-2.0

//! Decimation to the display rate that keeps spikes: between two sends, the
//! tap thread folds every message it decoded into one bucket per stream
//! holding the newest values and each value's minimum and maximum. Each send
//! then carries at most one summary per stream however fast the stream is,
//! and a plot of the minimums and maximums still reaches every extreme the
//! tap saw.

use super::wire::Summary;

/// One stream's messages since the last send.
#[derive(Debug, Default)]
struct Bucket {
    first_recv_time_ns: u64,
    recv_time_ns: u64,
    messages: u32,
    values: Vec<f64>,
    minimums: Vec<f64>,
    maximums: Vec<f64>,
}

/// A bucket per stream. Each stream's buffers are reused, so once every
/// stream has been seen, adding and taking don't allocate.
#[derive(Debug)]
pub struct Decimator {
    buckets: Box<[Bucket]>,
}

impl Decimator {
    /// Empty buckets for `streams` streams.
    #[must_use]
    pub fn new(streams: usize) -> Self {
        Self {
            buckets: (0..streams).map(|_| Bucket::default()).collect(),
        }
    }

    /// Folds one message's values into its stream's bucket. A stream that
    /// doesn't exist is ignored. If the number of values changed, the bucket
    /// starts again from this message, since the old extremes describe other
    /// fields.
    pub fn add(&mut self, stream: u16, recv_time_ns: u64, values: &[f64]) {
        let Some(bucket) = self.buckets.get_mut(usize::from(stream)) else {
            return;
        };
        if bucket.messages == 0 || bucket.values.len() != values.len() {
            bucket.first_recv_time_ns = recv_time_ns;
            bucket.messages = 0;
            for column in [&mut bucket.minimums, &mut bucket.maximums] {
                column.clear();
                column.extend_from_slice(values);
            }
        } else {
            // `f64::min` and `max` skip a NaN, so one unreadable value
            // doesn't hide the extremes of the others.
            for ((minimum, maximum), value) in bucket
                .minimums
                .iter_mut()
                .zip(&mut bucket.maximums)
                .zip(values)
            {
                *minimum = minimum.min(*value);
                *maximum = maximum.max(*value);
            }
        }
        // The newest value of each, kept from an earlier message when this
        // one doesn't have it (NaN): a topic two publishers share sends some
        // values in one message and the rest in the next.
        if bucket.values.len() == values.len() {
            for (newest, value) in bucket.values.iter_mut().zip(values) {
                if !value.is_nan() {
                    *newest = *value;
                }
            }
        } else {
            bucket.values.clear();
            bucket.values.extend_from_slice(values);
        }
        bucket.recv_time_ns = recv_time_ns;
        bucket.messages = bucket.messages.saturating_add(1);
    }

    /// Calls `each` with the summary of every stream that had messages since
    /// the last call, in stream order, and empties the buckets.
    pub fn take(&mut self, mut each: impl FnMut(&Summary<'_>)) {
        for (stream, bucket) in self.buckets.iter_mut().enumerate() {
            if bucket.messages == 0 {
                continue;
            }
            each(&Summary {
                stream: u16::try_from(stream).unwrap_or(u16::MAX),
                first_recv_time_ns: bucket.first_recv_time_ns,
                recv_time_ns: bucket.recv_time_ns,
                messages: bucket.messages,
                values: &bucket.values,
                minimums: &bucket.minimums,
                maximums: &bucket.maximums,
            });
            bucket.messages = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::super::wire::TapSample;
    use super::*;

    fn taken(decimator: &mut Decimator) -> Vec<TapSample> {
        let mut seen = Vec::new();
        decimator.take(|summary| {
            seen.push(TapSample {
                stream: summary.stream,
                first_recv_time_ns: summary.first_recv_time_ns,
                recv_time_ns: summary.recv_time_ns,
                messages: summary.messages,
                values: summary.values.to_vec(),
                minimums: summary.minimums.to_vec(),
                maximums: summary.maximums.to_vec(),
            });
        });
        seen
    }

    #[test]
    fn a_spike_between_sends_is_kept_as_an_extreme_while_the_newest_value_is_sent() {
        let mut decimator = Decimator::new(2);
        decimator.add(1, 10, &[0.0, 5.0]);
        decimator.add(1, 20, &[9.0, 5.0]);
        decimator.add(1, 30, &[1.0, -3.0]);
        assert_eq!(
            taken(&mut decimator),
            [TapSample {
                stream: 1,
                first_recv_time_ns: 10,
                recv_time_ns: 30,
                messages: 3,
                values: vec![1.0, -3.0],
                minimums: vec![0.0, -3.0],
                maximums: vec![9.0, 5.0],
            }]
        );
        // The next span starts afresh.
        assert_eq!(taken(&mut decimator), []);
        decimator.add(1, 40, &[2.0, 2.0]);
        assert_eq!(
            taken(&mut decimator),
            [TapSample::single(1, 40, vec![2.0, 2.0])]
        );
    }

    #[test]
    fn a_missing_reading_does_not_hide_the_extremes_and_unknown_streams_are_ignored() {
        let mut decimator = Decimator::new(1);
        decimator.add(0, 1, &[1.0]);
        decimator.add(0, 2, &[f64::NAN]);
        decimator.add(0, 3, &[-1.0]);
        decimator.add(4, 3, &[100.0]);
        let sample = &taken(&mut decimator)[0];
        assert_eq!((sample.minimums[0], sample.maximums[0]), (-1.0, 1.0));
    }

    #[test]
    fn a_value_a_message_lacks_keeps_the_last_one_sent_even_across_sends() {
        let mut decimator = Decimator::new(1);
        decimator.add(0, 1, &[1.0, 5.0]);
        decimator.add(0, 2, &[f64::NAN, 6.0]);
        assert_eq!(taken(&mut decimator)[0].values, [1.0, 6.0]);
        decimator.add(0, 3, &[2.0, f64::NAN]);
        assert_eq!(taken(&mut decimator)[0].values, [2.0, 6.0]);
    }

    #[test]
    fn a_message_with_another_number_of_values_starts_the_bucket_again() {
        let mut decimator = Decimator::new(1);
        decimator.add(0, 1, &[50.0]);
        decimator.add(0, 2, &[1.0, 2.0]);
        assert_eq!(
            taken(&mut decimator),
            [TapSample::single(0, 2, vec![1.0, 2.0])]
        );
    }

    proptest! {
        /// However many messages arrive between sends, each send carries at
        /// most one summary per stream, and across all sends every value's
        /// smallest and largest reading are still there.
        #[test]
        fn extremes_survive_and_each_send_is_bounded_by_the_stream_count(
            steps in prop::collection::vec(
                prop_oneof![
                    (0..3_u16, prop::array::uniform2(-1e6..1e6_f64)).prop_map(Some),
                    Just(None),
                ],
                0..300,
            ),
        ) {
            let mut decimator = Decimator::new(3);
            let mut sent = Vec::new();
            for (time, step) in (0_u64..).zip(&steps) {
                if let Some((stream, values)) = step {
                    decimator.add(*stream, time, values);
                } else {
                    let summaries = taken(&mut decimator);
                    prop_assert!(summaries.len() <= 3);
                    sent.extend(summaries);
                }
            }
            sent.extend(taken(&mut decimator));
            for stream in 0..3 {
                let added: Vec<&[f64; 2]> = steps
                    .iter()
                    .flatten()
                    .filter(|(s, _)| *s == stream)
                    .map(|(_, values)| values)
                    .collect();
                let summaries: Vec<&TapSample> = sent.iter().filter(|s| s.stream == stream).collect();
                let messages: u32 = summaries.iter().map(|s| s.messages).sum();
                prop_assert_eq!(usize::try_from(messages).unwrap(), added.len());
                for field in 0..2 {
                    let lowest = added.iter().map(|v| v[field]).fold(f64::INFINITY, f64::min);
                    let highest = added.iter().map(|v| v[field]).fold(f64::NEG_INFINITY, f64::max);
                    let low = summaries.iter().map(|s| s.minimums[field]).fold(f64::INFINITY, f64::min);
                    let high = summaries.iter().map(|s| s.maximums[field]).fold(f64::NEG_INFINITY, f64::max);
                    prop_assert_eq!((low, high), (lowest, highest));
                }
                if let (Some(last), Some(newest)) = (summaries.last(), added.last()) {
                    prop_assert_eq!(&last.values, &newest.to_vec());
                }
            }
        }
    }
}
