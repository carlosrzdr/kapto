// SPDX-License-Identifier: Apache-2.0

//! Statistics that pool: a [`Distribution`] of an episode's values can be
//! merged with others (of segments, episodes, a whole session) and gives the
//! same count, extremes and mean, and percentiles within the same error, as
//! one computed on all the values at once. So a session's statistics come
//! from its episodes', without reading their recordings again.
//!
//! Percentiles come from a sketch with a relative error: values are counted
//! in buckets whose bounds grow by a fixed ratio, so any percentile is
//! within [`RELATIVE_ERROR`] of a value at that rank, whatever the range of
//! the values, and merging two sketches is adding their counts.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The largest relative error of a percentile: 1 %.
pub const RELATIVE_ERROR: f64 = 0.01;

/// The ratio between consecutive bucket bounds.
const GAMMA: f64 = (1.0 + RELATIVE_ERROR) / (1.0 - RELATIVE_ERROR);

/// Values this close to zero are counted as zero: below any value a robot
/// sends that means something, and keeps the bucket count bounded.
const ZERO: f64 = 1e-12;

/// The distribution of a set of values: how many, the smallest and largest,
/// their sum, and a sketch of their percentiles. Values that aren't finite
/// are counted apart and leave the rest unchanged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Distribution {
    /// Finite values counted.
    pub count: u64,
    /// Values that weren't finite (NaN or infinite), left out of the rest.
    pub non_finite: u64,
    /// The smallest value; `None` before the first.
    pub min: Option<f64>,
    /// The largest value; `None` before the first.
    pub max: Option<f64>,
    /// The sum of the values.
    pub sum: f64,
    /// Positive values per bucket.
    positive: Buckets,
    /// Negative values per bucket of their magnitude.
    negative: Buckets,
    /// Values counted as zero.
    zeros: u64,
}

impl Distribution {
    /// The distribution of `values`.
    #[must_use]
    pub fn of(values: impl IntoIterator<Item = f64>) -> Self {
        let mut distribution = Self::default();
        for value in values {
            distribution.add(value);
        }
        distribution
    }

    /// Counts `value`.
    pub fn add(&mut self, value: f64) {
        if !value.is_finite() {
            self.non_finite += 1;
            return;
        }
        self.count += 1;
        self.sum += value;
        self.min = Some(self.min.map_or(value, |min| min.min(value)));
        self.max = Some(self.max.map_or(value, |max| max.max(value)));
        if value.abs() < ZERO {
            self.zeros += 1;
        } else if value > 0.0 {
            self.positive.add(bucket(value), 1);
        } else {
            self.negative.add(bucket(-value), 1);
        }
    }

    /// Adds `other`'s values to these, as if they had been counted here.
    pub fn merge(&mut self, other: &Self) {
        self.count += other.count;
        self.non_finite += other.non_finite;
        self.sum += other.sum;
        self.min = match (self.min, other.min) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.max = match (self.max, other.max) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.zeros += other.zeros;
        for (store, from) in [
            (&mut self.positive, &other.positive),
            (&mut self.negative, &other.negative),
        ] {
            for (bucket, count) in from.iter() {
                store.add(bucket, count);
            }
        }
    }

    /// The mean, if any value was counted.
    #[must_use]
    pub fn mean(&self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss, reason = "counts stay far below 2^53")]
        (self.count > 0).then(|| self.sum / self.count as f64)
    }

    /// The value at `fraction` of the way through the sorted values (0.5 is
    /// the median, 0.99 the 99th percentile), within [`RELATIVE_ERROR`] of
    /// a value at that rank; `None` if no value was counted. The ends are
    /// the exact smallest and largest values.
    #[must_use]
    pub fn quantile(&self, fraction: f64) -> Option<f64> {
        let (min, max) = (self.min?, self.max?);
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction <= 0.0 {
            return Some(min);
        }
        if fraction >= 1.0 {
            return Some(max);
        }
        // The rank of the value asked for, counting from 0.
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "counts stay far below 2^53, and the rank is within them"
        )]
        let rank = (fraction * (self.count - 1) as f64).round() as u64;
        let mut seen = 0;
        // From the most negative value up: negative buckets by falling
        // magnitude, then zeros, then positive buckets by rising magnitude.
        for (bucket, count) in self.negative.iter().rev() {
            seen += count;
            if rank < seen {
                return Some((-value(bucket)).clamp(min, max));
            }
        }
        seen += self.zeros;
        if rank < seen {
            return Some(0.0_f64.clamp(min, max));
        }
        for (bucket, count) in self.positive.iter() {
            seen += count;
            if rank < seen {
                return Some(value(bucket).clamp(min, max));
            }
        }
        Some(max)
    }
}

/// How many values fell in each bucket, kept in one array from the lowest
/// bucket counted to the highest. A series' values fall in neighbouring
/// buckets, so counting one is indexing an array rather than searching a
/// tree, which mattered with dozens of values per message. The span stays
/// bounded: buckets run from about -2800 (`ZERO`) to 35 500 (the largest
/// finite value), a few hundred KiB at the very worst. Written and read as
/// a map from bucket to count, so stored distributions are unchanged.
#[derive(Clone, Debug, Default)]
struct Buckets {
    /// The bucket `counts[0]` is for.
    first: i32,
    counts: Vec<u64>,
}

impl Buckets {
    /// Counts `count` more values in `bucket`.
    fn add(&mut self, bucket: i32, count: u64) {
        if self.counts.is_empty() {
            self.first = bucket;
        } else if bucket < self.first {
            let grow = usize::try_from(self.first.abs_diff(bucket)).unwrap_or(usize::MAX);
            self.counts.splice(0..0, std::iter::repeat_n(0, grow));
            self.first = bucket;
        }
        let index = usize::try_from(bucket.abs_diff(self.first)).unwrap_or(usize::MAX);
        if index >= self.counts.len() {
            self.counts.resize(index + 1, 0);
        }
        self.counts[index] += count;
    }

    /// Each bucket counted and its count, the lowest first.
    fn iter(&self) -> impl DoubleEndedIterator<Item = (i32, u64)> + '_ {
        self.counts
            .iter()
            .enumerate()
            .filter(|&(_, &count)| count > 0)
            .map(|(index, &count)| {
                let offset = i32::try_from(index).unwrap_or(i32::MAX);
                (self.first.saturating_add(offset), count)
            })
    }
}

impl PartialEq for Buckets {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}

impl Serialize for Buckets {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de> Deserialize<'de> for Buckets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut buckets = Self::default();
        for (bucket, count) in BTreeMap::<i32, u64>::deserialize(deserializer)? {
            buckets.add(bucket, count);
        }
        Ok(buckets)
    }
}

/// The bucket of a positive value: `k` such that `GAMMA^(k-1) < x <= GAMMA^k`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "finite values give logarithms well within an i32"
)]
fn bucket(magnitude: f64) -> i32 {
    (magnitude.ln() / GAMMA.ln()).ceil() as i32
}

/// The value a bucket stands for: the one within the relative error of
/// every value in it.
fn value(bucket: i32) -> f64 {
    2.0 * GAMMA.powi(bucket) / (GAMMA + 1.0)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// The value at `fraction`'s rank in `values`, sorted: what a sketch
    /// stands for.
    fn exact(values: &[f64], fraction: f64) -> f64 {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a short list's last index times a fraction from 0 to 1, rounded: a rank within it"
        )]
        let rank = (fraction * (sorted.len() - 1) as f64).round() as usize;
        sorted[rank]
    }

    fn close(estimate: f64, exact: f64) -> bool {
        (estimate - exact).abs() <= RELATIVE_ERROR * exact.abs() + ZERO
    }

    fn values() -> impl Strategy<Value = Vec<f64>> {
        prop::collection::vec(prop_oneof![-1e6..1e6_f64, -1.0..1.0_f64, Just(0.0)], 1..400)
    }

    #[test]
    fn the_median_and_tails_of_known_values_are_within_one_percent() {
        let values: Vec<f64> = (1..=1000).map(f64::from).collect();
        let distribution = Distribution::of(values.iter().copied());
        assert_eq!(distribution.count, 1000);
        assert_eq!(
            (distribution.min, distribution.max),
            (Some(1.0), Some(1000.0))
        );
        assert_eq!(distribution.mean(), Some(500.5));
        for (fraction, expected) in [(0.5, 501.0), (0.9, 900.0), (0.99, 990.0)] {
            let estimate = distribution.quantile(fraction).unwrap();
            assert!(close(estimate, expected), "{fraction}: {estimate}");
        }
        assert_eq!(distribution.quantile(0.0), Some(1.0));
        assert_eq!(distribution.quantile(1.0), Some(1000.0));
    }

    #[test]
    fn a_distribution_is_stored_as_its_buckets_by_number_and_read_back_the_same() {
        let distribution = Distribution::of([1.0, 1.0, -2.0, 0.0, 1000.0]);
        let stored = serde_json::to_value(&distribution).unwrap();
        // 1 is in bucket 0, 2 in bucket 35 and 1000 in bucket 346: the
        // ratio between bounds is 1.02.
        assert_eq!(stored["positive"], serde_json::json!({"0": 2, "346": 1}));
        assert_eq!(stored["negative"], serde_json::json!({"35": 1}));
        assert_eq!(stored["zeros"], 1);
        let read: Distribution = serde_json::from_value(stored).unwrap();
        assert_eq!(read, distribution);
    }

    #[test]
    fn values_that_arent_finite_are_counted_apart_and_change_nothing_else() {
        let distribution = Distribution::of([1.0, f64::NAN, 2.0, f64::INFINITY]);
        assert_eq!((distribution.count, distribution.non_finite), (2, 2));
        assert_eq!(distribution.mean(), Some(1.5));
        assert_eq!(Distribution::default().quantile(0.5), None);
    }

    proptest! {
        #[test]
        fn every_percentile_is_within_the_relative_error_of_the_exact_one(
            values in values(),
            fraction in 0.0..=1.0_f64,
        ) {
            let estimate = Distribution::of(values.iter().copied()).quantile(fraction).unwrap();
            let expected = exact(&values, fraction);
            prop_assert!(close(estimate, expected), "{estimate} vs {expected}");
        }

        #[test]
        fn merging_in_any_order_or_grouping_gives_what_counting_everything_at_once_gives(
            a in values(),
            b in values(),
            c in values(),
        ) {
            let [da, db, dc] = [&a, &b, &c].map(|part| Distribution::of(part.iter().copied()));
            let mut left = da.clone();
            left.merge(&db);
            left.merge(&dc);
            let mut right = dc.clone();
            let mut inner = db.clone();
            inner.merge(&da);
            right.merge(&inner);
            let all = Distribution::of(a.iter().chain(&b).chain(&c).copied());
            for merged in [&left, &right] {
                prop_assert_eq!(merged.count, all.count);
                prop_assert_eq!((merged.min, merged.max), (all.min, all.max));
                prop_assert_eq!(&merged.positive, &all.positive);
                prop_assert_eq!(&merged.negative, &all.negative);
                prop_assert_eq!(merged.zeros, all.zeros);
                // Sums differ only by the order floats were added in.
                prop_assert!((merged.sum - all.sum).abs() <= 1e-9 * all.sum.abs().max(1.0));
            }
        }
    }
}
