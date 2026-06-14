// SPDX-License-Identifier: Apache-2.0

//! Upstream loss inferred from silences in a stream that has no sequence
//! numbers but a declared rate.
//!
//! A silence is judged by when the stream's messages were sent, when their
//! source stamps them (as ROS 2 does every message), and otherwise by when
//! they were received. Sending times are exact: a message delayed on the way
//! but sent on time leaves no silence, however long its delay, whereas by
//! receive times a delay that grows across a silence reads as messages
//! missing.

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

/// When a batch's messages of one stream were sent, as their source stamped
/// them: its first's and its last's time, in nanoseconds of the source's
/// clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The first message's sending time.
    pub first_ns: u64,
    /// The last message's.
    pub last_ns: u64,
}

/// Follows when one stream's messages arrive and estimates how many a
/// silence swallowed, once the messages it only delayed have had time to
/// arrive.
#[derive(Clone, Debug)]
pub struct RateGapDetector {
    period_ns: u128,
    min_gap_ns: u128,
    /// The most periods a settled silence can read as missing with none
    /// lost, from when its first and last messages were received or sent.
    unsure_periods: u128,
    settle_ns: u128,
    /// The previous batch that held this stream.
    last: Option<Mark>,
    /// The silence being settled, if one is.
    open: Option<Settling>,
}

/// A batch of the stream, on the clock it is judged by.
#[derive(Clone, Copy, Debug)]
struct Mark {
    /// When its last message was sent, or is placed at the rate after its
    /// receive time.
    last_ns: u128,
    /// When it was received.
    recv_ns: u128,
    /// Whether it is judged by sending times.
    by_sent: bool,
}

/// A silence whose loss is not counted yet.
#[derive(Clone, Copy, Debug)]
struct Settling {
    /// The last message before it, on the clock it is judged by.
    from_ns: u128,
    /// Messages received since.
    received: u64,
    /// When it is counted, by the receive clock.
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
        self.observe_sent(time, count, None)
    }

    /// As [`RateGapDetector::observe`], with when the batch's messages were
    /// sent if every one of them says: then its silences are judged by
    /// sending times, which a delay on the way doesn't change. A sending
    /// time that jumps (the source's clock set) is never taken for loss:
    /// the stream is followed afresh from there.
    pub fn observe_sent(&mut self, time: RecvTime, count: u64, sent: Option<Sent>) -> u64 {
        let recv = u128::from(time.as_nanos());
        let (first, last, by_sent) = match sent {
            Some(sent) => (u128::from(sent.first_ns), u128::from(sent.last_ns), true),
            None => (
                recv,
                recv + u128::from(count.saturating_sub(1)) * self.period_ns,
                false,
            ),
        };
        let mark = Mark {
            last_ns: last,
            recv_ns: recv,
            by_sent,
        };
        let Some(previous) = self.last.replace(mark) else {
            return 0;
        };
        // On another clock than the batch before, or a sending time that
        // moved on by more than its arrival could account for: nothing to
        // compare, so start again.
        let jumped = by_sent
            && (first.abs_diff(previous.last_ns)).abs_diff(recv.saturating_sub(previous.recv_ns))
                > self.settle_ns;
        if previous.by_sent != by_sent || jumped || first + self.settle_ns < previous.last_ns {
            self.open = None;
            return 0;
        }
        let gap = first.saturating_sub(previous.last_ns);
        if let Some(open) = &mut self.open {
            open.received += count;
            if recv < open.until_ns {
                return 0;
            }
            // A batch only a little late is jitter: placed on schedule, so
            // its lateness isn't counted as messages missing.
            let end = if gap < self.min_gap_ns {
                previous.last_ns + u128::from(count) * self.period_ns
            } else {
                last
            };
            return self.settle_at(end);
        }
        if gap >= self.min_gap_ns {
            self.open = Some(Settling {
                from_ns: previous.last_ns,
                received: count,
                until_ns: recv + self.settle_ns,
            });
        }
        0
    }

    /// Counts the silence being settled now, if there is one, as of the
    /// last message seen: for when the stream's messages stop coming, as
    /// when the episode ends.
    pub fn settle(&mut self) -> u64 {
        match self.last {
            Some(last) if self.open.is_some() => self.settle_at(last.last_ns),
            _ => 0,
        }
    }

    fn settle_at(&mut self, end_ns: u128) -> u64 {
        let Some(open) = self.open.take() else {
            return 0;
        };
        let expected = (end_ns.saturating_sub(open.from_ns) + self.period_ns / 2) / self.period_ns;
        // By receive times, a message arriving later in its batch than the
        // batch's time reads as periods missing; by sending times, a source
        // waking late and catching up does the same: as much as jitter and
        // a batch's wait cover is never counted.
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

    /// One message sent at `sent_ms`, its own batch, received at
    /// `recv_ns`: what it counts lost.
    fn stamped(detector: &mut RateGapDetector, sent_ms: u64, recv_ns: u64) -> u64 {
        let sent = Sent {
            first_ns: sent_ms * MS,
            last_ns: sent_ms * MS,
        };
        detector.observe_sent(RecvTime::from_nanos(recv_ns), 1, Some(sent))
    }

    /// Messages sent one a millisecond over `sent`, each received `delay` ns
    /// later: what they count lost.
    fn stamped_at_rate(
        detector: &mut RateGapDetector,
        sent: std::ops::Range<u64>,
        delay: u64,
    ) -> u64 {
        sent.map(|ms| stamped(detector, ms, ms * MS + delay)).sum()
    }

    #[test]
    fn a_delay_on_the_way_growing_across_a_silence_is_no_loss_when_sending_times_say() {
        // Sent at its rate throughout, but from 101 ms delivered 6 ms later
        // than before, as a camera's frames hold the middleware back.
        let recv = |ms: u64| if ms <= 100 { ms * MS } else { ms * MS + 6 * MS };
        let mut by_sent = RateGapDetector::new(RateGap::for_rate(1_000));
        let lost: u64 = (0..1_300)
            .map(|ms| stamped(&mut by_sent, ms, recv(ms)))
            .sum();
        assert_eq!(lost + by_sent.settle(), 0);
        // By receive times alone the same stream reads as 6 missing.
        let mut by_recv = RateGapDetector::new(RateGap::for_rate(1_000));
        let lost: u64 = (0..1_300)
            .map(|ms| by_recv.observe(RecvTime::from_nanos(recv(ms)), 1))
            .sum();
        assert_eq!(lost + by_recv.settle(), 6);
    }

    #[test]
    fn messages_sent_on_time_but_arriving_together_are_not_lost() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        stamped_at_rate(&mut detector, 0..100, 0);
        // Ten sent a millisecond apart, held back and delivered as one.
        let sent = Sent {
            first_ns: 100 * MS,
            last_ns: 109 * MS,
        };
        assert_eq!(detector.observe_sent(at(124), 10, Some(sent)), 0);
        assert_eq!(stamped_at_rate(&mut detector, 110..1_300, 15 * MS), 0);
        assert_eq!(detector.settle(), 0);
    }

    #[test]
    fn a_silence_in_the_sending_times_is_counted_whatever_the_delay() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        stamped_at_rate(&mut detector, 0..101, 0);
        assert_eq!(stamped_at_rate(&mut detector, 110..1_300, 0), 9);
        // Delivered 3 ms sooner after the silence, which by receive times
        // would hide 3 of them.
        let mut sooner = RateGapDetector::new(RateGap::for_rate(1_000));
        stamped_at_rate(&mut sooner, 0..101, 3 * MS);
        assert_eq!(stamped_at_rate(&mut sooner, 110..1_300, 0), 9);
    }

    #[test]
    fn a_source_clock_set_forward_or_back_is_not_counted_as_loss() {
        let hour = 3_600_000;
        let mut forward = RateGapDetector::new(RateGap::for_rate(1_000));
        stamped_at_rate(&mut forward, 0..101, 0);
        let lost: u64 = (101..1_300)
            .map(|ms| stamped(&mut forward, hour + ms, ms * MS))
            .sum();
        assert_eq!(lost + forward.settle(), 0);
        let mut back = RateGapDetector::new(RateGap::for_rate(1_000));
        (0..101).for_each(|ms| {
            stamped(&mut back, hour + ms, ms * MS);
        });
        let lost: u64 = (101..1_300).map(|ms| stamped(&mut back, ms, ms * MS)).sum();
        assert_eq!(lost + back.settle(), 0);
    }

    #[test]
    fn a_stream_that_starts_stamping_its_messages_is_followed_afresh() {
        let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
        at_rate(&mut detector, 0, 100);
        // Stamped from 110 ms: the silence before is on neither clock alone,
        // so it isn't judged; the stream is followed on its sending times.
        assert_eq!(stamped_at_rate(&mut detector, 110..1_300, 0), 0);
        let lost = stamped_at_rate(&mut detector, 1_310..2_500, 0);
        assert_eq!(lost + detector.settle(), 10);
    }

    proptest::proptest! {
        #[test]
        fn a_stream_sent_at_its_rate_never_shows_loss_however_late_it_arrives(
            delays in proptest::collection::vec(0_u64..50_000_000, 1..2_000),
            batch in 1_usize..16,
        ) {
            // Sent one a millisecond; each batch arrives up to 50 ms late,
            // but never before the batch it follows.
            let mut detector = RateGapDetector::new(RateGap::for_rate(1_000));
            let mut lost = 0;
            let mut recv = 0;
            for (n, chunk) in delays.chunks(batch).enumerate() {
                let first = (n * batch) as u64 * MS;
                let last = first + (chunk.len() as u64 - 1) * MS;
                recv = recv.max(last + chunk[0]);
                let sent = Sent { first_ns: first, last_ns: last };
                lost += detector.observe_sent(
                    RecvTime::from_nanos(recv),
                    chunk.len() as u64,
                    Some(sent),
                );
            }
            proptest::prop_assert_eq!(lost + detector.settle(), 0);
        }

        #[test]
        fn a_source_waking_late_and_catching_up_never_shows_loss(
            wakes in proptest::collection::vec((0_u8..50, 0_u64..2_000_000), 1..4_000),
        ) {
            // At 2 kHz, now and then sending up to the batch's wait late,
            // then the messages it owes at once, as a control loop does.
            let gap = RateGap {
                batch_latency: Duration::from_millis(2),
                ..RateGap::for_rate(2_000)
            };
            let mut detector = RateGapDetector::new(gap);
            let mut lost = 0;
            let mut sent = 0;
            for (n, &(chance, late)) in (0_u64..).zip(&wakes) {
                let late = if chance == 0 { late } else { 0 };
                sent = (n * 500_000 + late).max(sent);
                let stamp = Sent { first_ns: sent, last_ns: sent };
                lost += detector.observe_sent(RecvTime::from_nanos(sent), 1, Some(stamp));
            }
            proptest::prop_assert_eq!(lost + detector.settle(), 0);
        }
    }
}
