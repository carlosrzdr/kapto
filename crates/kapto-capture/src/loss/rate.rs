// SPDX-License-Identifier: Apache-2.0

//! Upstream loss inferred from silences in a stream that has no sequence
//! numbers but a declared rate.

use std::time::Duration;

use kapto_core::RecvTime;

/// How to infer loss from a stream's declared rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateGap {
    /// Time between two messages at the declared rate.
    pub period: Duration,
    /// A silence counts as loss only once it lasts this many periods.
    /// Shorter ones are ordinary jitter, and ignoring them keeps small
    /// differences between the robot's clock and the host's from adding up
    /// to loss that never happened.
    pub min_gap_periods: u32,
    /// How long after a silence its messages may still arrive late, as a
    /// middleware holding them back delivers them in a burst. Loss is
    /// counted once this has passed: the periods since the silence began,
    /// less every message that arrived, when more than the receive times
    /// can tell apart from a change in delay.
    pub settle: Duration,
    /// How much later than its batch's receive time a stream's message in
    /// it may have arrived: the receiver's longest wait to fill a batch.
    pub batch_latency: Duration,
}

impl RateGap {
    /// How long a silence settles unless the stream says otherwise.
    pub const DEFAULT_SETTLE: Duration = Duration::from_secs(1);

    /// Inference for a stream sending `rate_hz` messages per second, with
    /// the default threshold of 3 periods and [`Self::DEFAULT_SETTLE`], its
    /// receive times exact.
    #[must_use]
    pub fn for_rate(rate_hz: u32) -> Self {
        Self {
            period: Duration::from_secs(1) / rate_hz.max(1),
            min_gap_periods: 3,
            settle: Self::DEFAULT_SETTLE,
            batch_latency: Duration::ZERO,
        }
    }
}

/// Follows when one stream's messages arrive and estimates how many a
/// silence swallowed, once the messages it only delayed have had time to
/// arrive.
#[derive(Clone, Debug)]
pub struct RateGapDetector {
    period_ns: u128,
    min_gap_ns: u128,
    /// The most periods a settled silence can read as missing with none
    /// lost, from when its first and last messages were received.
    unsure_periods: u128,
    settle_ns: u128,
    /// The previous batch that held this stream: when its last message is
    /// placed, at the rate after its receive time, and how many it held.
    last: Option<(u128, u64)>,
    /// The silence being settled, if one is.
    open: Option<Settling>,
}

/// A silence whose loss is not counted yet.
#[derive(Clone, Copy, Debug)]
struct Settling {
    /// The last message before it.
    from_ns: u128,
    /// Messages received since.
    received: u64,
    /// When it is counted.
    until_ns: u128,
}

impl RateGapDetector {
    /// A detector before any message.
    #[must_use]
    pub fn new(gap: RateGap) -> Self {
        let period_ns = gap.period.as_nanos().max(1);
        Self {
            period_ns,
            min_gap_ns: period_ns * u128::from(gap.min_gap_periods.max(1)),
            unsure_periods: u128::from(gap.min_gap_periods.max(1))
                + gap.batch_latency.as_nanos().div_ceil(period_ns),
            settle_ns: gap.settle.as_nanos(),
            last: None,
            open: None,
        }
    }

    /// Takes the `count` messages of this stream in one batch received at
    /// `time`, and returns how many are counted lost now.
    ///
    /// A batch shares one receive time, that of its first message, so its
    /// last message is placed `count - 1` periods after it. A silence opens
    /// a settling window instead of counting at once; the first batch at
    /// or after its end counts the periods since the silence began less
    /// the messages received since, so a burst of delayed messages makes
    /// up for the silence it ended. A stream arriving at its rate never
    /// shows a silence, however it is batched.
    pub fn observe(&mut self, time: RecvTime, count: u64) -> u64 {
        let time = u128::from(time.as_nanos());
        let this_last = time + u128::from(count.saturating_sub(1)) * self.period_ns;
        let Some((previous_last, _)) = self.last.replace((this_last, count)) else {
            return 0;
        };
        let gap = time.saturating_sub(previous_last);
        if let Some(open) = &mut self.open {
            open.received += count;
            if time < open.until_ns {
                return 0;
            }
            // A batch only a little late is jitter: placed on schedule, so
            // its lateness isn't counted as messages missing.
            let end = if gap < self.min_gap_ns {
                previous_last + u128::from(count) * self.period_ns
            } else {
                this_last
            };
            return self.settle_at(end);
        }
        if gap >= self.min_gap_ns {
            self.open = Some(Settling {
                from_ns: previous_last,
                received: count,
                until_ns: time + self.settle_ns,
            });
        }
        0
    }

    /// Counts the silence being settled now, if there is one, as of the
    /// last message seen: for when the stream's messages stop coming, as
    /// when the episode ends.
    pub fn settle(&mut self) -> u64 {
        match self.last {
            Some((last, _)) if self.open.is_some() => self.settle_at(last),
            _ => 0,
        }
    }

    fn settle_at(&mut self, end_ns: u128) -> u64 {
        let Some(open) = self.open.take() else {
            return 0;
        };
        let expected = (end_ns.saturating_sub(open.from_ns) + self.period_ns / 2) / self.period_ns;
        // The window is measured between arrivals, so a message's delay on
        // the way changing from its start to its end, and a stream's
        // message arriving later in its batch than the batch's time, read
        // as periods missing: as much as jitter and a batch's wait cover
        // is never counted.
        let missing = expected.saturating_sub(u128::from(open.received));
        let lost = if missing > self.unsure_periods {
            missing
        } else {
            0
        };
        u64::try_from(lost).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn at(ms: u64) -> RecvTime {
        RecvTime::from_nanos(ms * MS)
    }

    /// Messages one a millisecond from `from` to `to` ms, each its own
    /// batch; what they count lost.
    fn at_rate(detector: &mut RateGapDetector, from: u64, to: u64) -> u64 {
        (from..=to).map(|ms| detector.observe(at(ms), 1)).sum()
    }

    #[test]
    fn a_silence_of_ten_periods_ended_by_one_message_lost_nine_once_it_settles() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        assert_eq!(detector.observe(at(100), 1), 0);
        assert_eq!(detector.observe(at(110), 1), 0, "not before it settles");
        assert_eq!(at_rate(&mut detector, 111, 1_109), 0);
        assert_eq!(detector.observe(at(1_110), 1), 9);
        assert_eq!(at_rate(&mut detector, 1_111, 3_000), 0, "counted once");
    }

    #[test]
    fn messages_held_back_and_delivered_together_are_not_lost() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        detector.observe(at(110), 10);
        assert_eq!(at_rate(&mut detector, 111, 2_000), 0);
    }

    #[test]
    fn messages_held_back_and_delivered_in_a_burst_over_several_batches_are_not_lost() {
        // Silent from 100 to 110 ms, then the nine held back arrive in
        // three batches within a millisecond, and the stream goes on.
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        assert_eq!(detector.observe(at(110), 1), 0);
        for burst in 0..3 {
            let time = RecvTime::from_nanos(110 * MS + 200_000 * (burst + 1));
            assert_eq!(detector.observe(time, 3), 0);
        }
        assert_eq!(at_rate(&mut detector, 111, 2_000), 0);
        assert_eq!(detector.settle(), 0);
    }

    #[test]
    fn a_silence_made_up_only_in_part_counts_the_rest() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        detector.observe(at(110), 1);
        detector.observe(RecvTime::from_nanos(110 * MS + 300_000), 4);
        assert_eq!(at_rate(&mut detector, 111, 1_200), 5);
    }

    #[test]
    fn a_silence_still_settling_when_the_stream_stops_is_counted_by_settling_it() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        detector.observe(at(110), 1);
        at_rate(&mut detector, 111, 200);
        assert_eq!(detector.settle(), 9);
        assert_eq!(detector.settle(), 0, "counted once");
    }

    #[test]
    fn jitter_below_the_threshold_is_never_counted() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        let jittery = RecvTime::from_nanos(102 * MS + 900_000);
        assert_eq!(detector.observe(jittery, 1), 0);
        assert_eq!(detector.settle(), 0);
    }

    #[test]
    fn a_slightly_late_batch_ending_the_window_adds_no_loss() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        detector.observe(at(110), 10);
        at_rate(&mut detector, 111, 1_109);
        let late = RecvTime::from_nanos(1_110 * MS + 1_900_000);
        assert_eq!(detector.observe(late, 1), 0);
    }

    #[test]
    fn a_delay_on_the_way_growing_over_a_silence_is_not_counted_as_loss() {
        // Arriving 2.5 ms later after a silence than before it, as a
        // middleware batching more: 2 periods read as missing.
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        detector.observe(at(100), 1);
        let late = |ms: u64| RecvTime::from_nanos(ms * MS + 2_500_000);
        assert_eq!(detector.observe(late(110), 10), 0);
        let lost: u64 = (111..=1_200).map(|ms| detector.observe(late(ms), 1)).sum();
        assert_eq!(lost, 0);
    }

    #[test]
    fn a_stream_placed_late_in_its_batches_is_unsure_by_the_batchs_wait() {
        // Receive times as precise as 2 ms: 2 more periods at 1 kHz.
        let gap = RateGap {
            batch_latency: Duration::from_millis(2),
            ..RateGap::for_rate(1_000)
        };
        let silence = |lost_between: u64| {
            let mut detector = RateGapDetector::new(gap);
            detector.observe(at(100), 1);
            detector.observe(at(110), 10 - lost_between);
            at_rate(&mut detector, 111, 1_200)
        };
        assert_eq!(silence(5), 0);
        assert_eq!(silence(6), 6);
    }

    #[test]
    fn a_slower_source_clock_never_adds_up_to_loss() {
        // The robot sends at 999.9 Hz while 1 kHz is declared.
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        let lost: u64 = (0..100_000_u64)
            .map(|n| detector.observe(RecvTime::from_nanos(n * 1_000_100), 1))
            .sum();
        assert_eq!(lost + detector.settle(), 0);
    }

    #[test]
    fn a_stream_at_its_rate_never_shows_loss_however_it_is_batched() {
        // 2 kHz, read in batches of 4 every 2 ms, each stamped with its first
        // message's time, some batches woken up to 0.9 ms late.
        let mut detector = RateGapDetector::new(RateGap::for_rate(2_000));
        let lost: u64 = (0..10_000_u64)
            .map(|batch| {
                let late = if batch % 7 == 0 { 900_000 } else { 0 };
                detector.observe(RecvTime::from_nanos(batch * 2 * MS + late), 4)
            })
            .sum();
        assert_eq!(lost + detector.settle(), 0);
    }

    #[test]
    fn a_silence_after_a_batch_is_measured_from_its_last_message() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        // 5 messages from 100 ms, so the last one at 104 ms; the next batch
        // at 115 ms: 10 missing in between, 1 of them counted as arrived.
        detector.observe(at(100), 5);
        detector.observe(at(115), 1);
        assert_eq!(detector.settle(), 10);
    }

    #[test]
    fn the_threshold_and_the_settling_time_are_configurable() {
        let mut detector = RateGapDetector::new(RateGap {
            period: Duration::from_millis(1),
            min_gap_periods: 20,
            settle: Duration::from_millis(5),
            batch_latency: Duration::ZERO,
        });
        detector.observe(at(100), 1);
        assert_eq!(detector.observe(at(110), 1), 0);
        assert_eq!(detector.observe(at(140), 1), 0);
        assert_eq!(at_rate(&mut detector, 141, 145), 29);
    }
}
