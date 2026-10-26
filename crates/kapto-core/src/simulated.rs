// SPDX-License-Identifier: Apache-2.0

//! A simulation's time, mapped from the time Kapto received its clock's
//! messages.
//!
//! The package gives one stream's field the role `time.simulated`; each of
//! its messages pairs the receive time with the simulated time it carries
//! ([`ClockReading`]). Between two readings simulated time is taken to move
//! as it did between them. Where it goes back by more than a tick the
//! simulation was reset, and a new [`Segment`] starts; a smaller step back
//! is a reading out of order, left out. Where it doesn't move, or where the
//! clock fell silent for `pause_silence` times longer than it usually waits
//! between readings and came back no more than a tick on, the simulation
//! was paused.
//!
//! Worked out the same way by the worker, the server and the viewer's
//! bridge, so all three agree; nothing here reads a clock.

/// The role of the field holding the simulation's time, given in a
/// package's mapping.
pub const ROLE: &str = "time.simulated";

/// One message of the clock stream: when Kapto received it and the
/// simulated time it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockReading {
    /// When it was received, in UTC nanoseconds since the Unix epoch.
    pub receive_utc_ns: i64,
    /// The simulated time it carries, in nanoseconds.
    pub simulated_ns: i64,
}

/// A span of receive time in which the simulation was paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pause {
    /// When it was paused, in UTC nanoseconds.
    pub start_utc_ns: i64,
    /// When it played again, in UTC nanoseconds.
    pub end_utc_ns: i64,
    /// The simulated time it was paused at, in nanoseconds.
    pub simulated_ns: i64,
}

impl Pause {
    /// How long it was paused for, in nanoseconds of receive time.
    #[must_use]
    pub fn duration_ns(&self) -> i64 {
        self.end_utc_ns - self.start_utc_ns
    }
}

/// Simulated time going back, which starts a new segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reset {
    /// When the first reading after it was received, in UTC nanoseconds.
    pub at_utc_ns: i64,
    /// The simulated time before it, in nanoseconds.
    pub from_ns: i64,
    /// The simulated time it went back to, in nanoseconds.
    pub to_ns: i64,
}

/// A run of readings between resets, simulated time never going back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    readings: Vec<ClockReading>,
    pauses: Vec<Pause>,
}

impl Segment {
    /// Its readings, in receive order.
    #[must_use]
    pub fn readings(&self) -> &[ClockReading] {
        &self.readings
    }

    /// Its pauses, in order.
    #[must_use]
    pub fn pauses(&self) -> &[Pause] {
        &self.pauses
    }

    /// How much simulated time it spans, in nanoseconds.
    #[must_use]
    pub fn simulated_ns(&self) -> i64 {
        match (self.readings.first(), self.readings.last()) {
            (Some(first), Some(last)) => last.simulated_ns - first.simulated_ns,
            _ => 0,
        }
    }

    fn at(&self, receive_utc_ns: i64) -> Option<i64> {
        let (first, last) = (self.readings.first()?, self.readings.last()?);
        if receive_utc_ns < first.receive_utc_ns || receive_utc_ns > last.receive_utc_ns {
            return None;
        }
        if let Some(pause) = self
            .pauses
            .iter()
            .find(|pause| (pause.start_utc_ns..=pause.end_utc_ns).contains(&receive_utc_ns))
        {
            return Some(pause.simulated_ns);
        }
        let after = self
            .readings
            .partition_point(|reading| reading.receive_utc_ns <= receive_utc_ns);
        let before = self.readings[after.saturating_sub(1)];
        let Some(next) = self.readings.get(after) else {
            return Some(before.simulated_ns);
        };
        // Moving after a pause that ended between the two readings starts
        // from where the pause ended, not from the reading before it.
        let from = self
            .pauses
            .iter()
            .rev()
            .find(|pause| {
                pause.end_utc_ns > before.receive_utc_ns && pause.end_utc_ns < next.receive_utc_ns
            })
            .map_or(before, |pause| ClockReading {
                receive_utc_ns: pause.end_utc_ns,
                simulated_ns: pause.simulated_ns,
            });
        Some(between(from, *next, receive_utc_ns))
    }
}

/// Simulated time at `receive_utc_ns`, moving from `from` to `to` at the
/// pace it did between them.
fn between(from: ClockReading, to: ClockReading, receive_utc_ns: i64) -> i64 {
    let real = i128::from(to.receive_utc_ns - from.receive_utc_ns);
    if real == 0 {
        return to.simulated_ns;
    }
    let gone = i128::from(receive_utc_ns - from.receive_utc_ns);
    let moved = i128::from(to.simulated_ns - from.simulated_ns) * gone / real;
    from.simulated_ns + i64::try_from(moved).unwrap_or(0)
}

/// Where in the simulation a receive time falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimulatedInstant {
    /// Which segment, counting from 0: one more after every reset.
    pub segment: usize,
    /// The simulated time, in nanoseconds.
    pub simulated_ns: i64,
}

/// The mapping from receive time to simulated time of one episode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimulatedClock {
    segments: Vec<Segment>,
    resets: Vec<Reset>,
}

impl SimulatedClock {
    /// The mapping made from a clock stream's readings, sorted by receive
    /// time first if they aren't. `pause_silence` is how many times longer
    /// than the clock usually waits between readings it must fall silent,
    /// coming back no more than a tick on, to count as a pause: a simulation
    /// that publishes its clock only while it plays goes silent when paused,
    /// while one that is merely slow still publishes at its own steady pace.
    #[must_use]
    pub fn from_readings(
        readings: impl IntoIterator<Item = ClockReading>,
        pause_silence: u32,
    ) -> Self {
        let mut readings: Vec<ClockReading> = readings.into_iter().collect();
        readings.sort_by_key(|reading| reading.receive_utc_ns);
        let mut clock = Self::default();
        let mut current: Vec<ClockReading> = Vec::new();
        let mut largest_step = 0;
        for reading in readings {
            let Some(last) = current.last().copied() else {
                current.push(reading);
                continue;
            };
            let step = reading.simulated_ns - last.simulated_ns;
            if step >= 0 {
                largest_step = largest_step.max(step);
                current.push(reading);
            } else if -step > largest_step {
                clock.resets.push(Reset {
                    at_utc_ns: reading.receive_utc_ns,
                    from_ns: last.simulated_ns,
                    to_ns: reading.simulated_ns,
                });
                clock
                    .segments
                    .push(segment(std::mem::take(&mut current), pause_silence));
                current.push(reading);
                largest_step = 0;
            }
            // Otherwise back by no more than a tick: out of order, left out.
        }
        if !current.is_empty() {
            clock.segments.push(segment(current, pause_silence));
        }
        clock
    }

    /// Its segments, one more after every reset; none without readings.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Where simulated time went back, in order.
    #[must_use]
    pub fn resets(&self) -> &[Reset] {
        &self.resets
    }

    /// Every pause, in order.
    pub fn pauses(&self) -> impl Iterator<Item = &Pause> {
        self.segments.iter().flat_map(|segment| &segment.pauses)
    }

    /// Simulated time at `receive_utc_ns`; `None` before the first reading,
    /// after the last, or between a reset's two readings.
    #[must_use]
    pub fn at(&self, receive_utc_ns: i64) -> Option<SimulatedInstant> {
        self.segments
            .iter()
            .enumerate()
            .find_map(|(segment, readings)| {
                readings
                    .at(receive_utc_ns)
                    .map(|simulated_ns| SimulatedInstant {
                        segment,
                        simulated_ns,
                    })
            })
    }

    /// How much time passed from `from_utc_ns` to `to_utc_ns` as the
    /// simulation counts it, in nanoseconds: simulated time where its
    /// readings cover the span, a pause counting none, and receive time
    /// where they don't (before its first reading, after its last, between
    /// a reset's two), as nothing says how the simulation ran there. Zero
    /// if `to_utc_ns` isn't after `from_utc_ns`.
    #[must_use]
    pub fn elapsed_ns(&self, from_utc_ns: i64, to_utc_ns: i64) -> i64 {
        let mut total = 0;
        let mut at = from_utc_ns;
        for segment in &self.segments {
            let (Some(first), Some(last)) = (segment.readings.first(), segment.readings.last())
            else {
                continue;
            };
            if last.receive_utc_ns <= at {
                continue;
            }
            if first.receive_utc_ns >= to_utc_ns {
                break;
            }
            if first.receive_utc_ns > at {
                total += first.receive_utc_ns - at;
                at = first.receive_utc_ns;
            }
            let end = last.receive_utc_ns.min(to_utc_ns);
            total += segment.at(end).unwrap_or(0) - segment.at(at).unwrap_or(0);
            at = end;
        }
        if to_utc_ns > at {
            total += to_utc_ns - at;
        }
        total
    }

    /// How the simulation ran, in sum; `None` without readings.
    #[must_use]
    pub fn summary(&self) -> Option<SimulatedSummary> {
        let first = self.segments.first()?.readings.first()?;
        let last = self.segments.last()?.readings.last()?;
        let simulated_ns = self.segments.iter().map(Segment::simulated_ns).sum();
        let paused_ns: i64 = self.pauses().map(Pause::duration_ns).sum();
        let real_ns = last.receive_utc_ns - first.receive_utc_ns;
        let running_ns = real_ns - paused_ns;
        #[allow(clippy::cast_precision_loss)] // A ratio, not a time.
        let ratio = (running_ns > 0).then(|| simulated_ns as f64 / running_ns as f64);
        Some(SimulatedSummary {
            start_ns: first.simulated_ns,
            end_ns: last.simulated_ns,
            simulated_ns,
            real_ns,
            paused_ns,
            ratio,
            pauses: self.pauses().copied().collect(),
            resets: self.resets.clone(),
        })
    }
}

/// How a simulation ran over an episode.
#[derive(Clone, Debug, PartialEq)]
pub struct SimulatedSummary {
    /// The simulated time of the first reading, in nanoseconds.
    pub start_ns: i64,
    /// The simulated time of the last reading, in nanoseconds.
    pub end_ns: i64,
    /// How much simulated time passed, each segment's added up, in
    /// nanoseconds.
    pub simulated_ns: i64,
    /// How much receive time passed from the first reading to the last, in
    /// nanoseconds.
    pub real_ns: i64,
    /// How much of it the simulation was paused, in nanoseconds.
    pub paused_ns: i64,
    /// Simulated time over receive time while it wasn't paused: 0.5 at half
    /// real time, 2 at twice; `None` if it was paused throughout.
    pub ratio: Option<f64>,
    /// Each pause, in order.
    pub pauses: Vec<Pause>,
    /// Each reset, in order.
    pub resets: Vec<Reset>,
}

/// A segment of `readings`, never going back, with its pauses found.
fn segment(readings: Vec<ClockReading>, pause_silence: u32) -> Segment {
    let steps = || readings.windows(2).map(|pair| (pair[0], pair[1]));
    // What the clock usually does between readings while it plays: its
    // median wait and its median tick.
    let moving: Vec<(i64, i64)> = steps()
        .map(|(from, to)| {
            (
                to.receive_utc_ns - from.receive_utc_ns,
                to.simulated_ns - from.simulated_ns,
            )
        })
        .filter(|&(_, tick)| tick > 0)
        .collect();
    let usual = (!moving.is_empty()).then(|| {
        (
            median(moving.iter().map(|&(wait, _)| wait)),
            median(moving.iter().map(|&(_, tick)| tick)),
        )
    });
    let mut pauses: Vec<Pause> = Vec::new();
    for (from, to) in steps() {
        let wait = to.receive_utc_ns - from.receive_utc_ns;
        let tick = to.simulated_ns - from.simulated_ns;
        let end_utc_ns = if tick == 0 {
            to.receive_utc_ns
        } else {
            match usual {
                // Silent far longer than usual, and back less than two ticks
                // on: paused until a usual wait before it came back.
                Some((usual_wait, usual_tick))
                    if wait >= i64::from(pause_silence).saturating_mul(usual_wait)
                        && tick < usual_tick.saturating_mul(2) =>
                {
                    to.receive_utc_ns - usual_wait
                }
                _ => continue,
            }
        };
        if end_utc_ns <= from.receive_utc_ns {
            continue;
        }
        match pauses.last_mut() {
            Some(last)
                if last.end_utc_ns == from.receive_utc_ns
                    && last.simulated_ns == from.simulated_ns =>
            {
                last.end_utc_ns = end_utc_ns;
            }
            _ => pauses.push(Pause {
                start_utc_ns: from.receive_utc_ns,
                end_utc_ns,
                simulated_ns: from.simulated_ns,
            }),
        }
    }
    Segment { readings, pauses }
}

fn median(values: impl Iterator<Item = i64>) -> i64 {
    let mut values: Vec<i64> = values.collect();
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const S: i64 = 1_000 * MS;
    /// When the episode's first reading arrived, in UTC.
    const T0: i64 = 1_800_000_000 * S;
    /// `qa.toml`'s default.
    const PAUSE_SILENCE: u32 = 10;

    /// A clock read every `wait` of receive time, moving `tick` each time,
    /// `count` times from `start` (receive offset) and `simulated`.
    fn steady(start: i64, simulated: i64, wait: i64, tick: i64, count: i64) -> Vec<ClockReading> {
        (0..count)
            .map(|n| ClockReading {
                receive_utc_ns: T0 + start + n * wait,
                simulated_ns: simulated + n * tick,
            })
            .collect()
    }

    fn summary(readings: Vec<ClockReading>) -> SimulatedSummary {
        SimulatedClock::from_readings(readings, PAUSE_SILENCE)
            .summary()
            .unwrap()
    }

    #[test]
    fn a_simulation_at_half_real_time_has_half_the_simulated_time() {
        // 100 readings 10 ms apart, 5 ms of simulation each.
        let run = summary(steady(0, 0, 10 * MS, 5 * MS, 101));
        assert_eq!(run.simulated_ns, 500 * MS);
        assert_eq!(run.real_ns, S);
        assert_eq!(run.ratio, Some(0.5));
        assert_eq!((run.pauses, run.resets), (vec![], vec![]));
    }

    #[test]
    fn a_simulation_at_twice_real_time_has_twice_the_simulated_time() {
        let run = summary(steady(0, 3 * S, 10 * MS, 20 * MS, 101));
        assert_eq!((run.start_ns, run.end_ns), (3 * S, 5 * S));
        assert_eq!(run.ratio, Some(2.0));
    }

    #[test]
    fn a_pause_with_the_clock_still_published_holds_simulated_time() {
        // Plays 1 s, holds at 1 s for 2 s while still publishing, plays 1 s.
        let mut readings = steady(0, 0, 10 * MS, 10 * MS, 101);
        readings.extend(steady(S + 10 * MS, S, 10 * MS, 0, 200));
        readings.extend(steady(3 * S + 10 * MS, S + 10 * MS, 10 * MS, 10 * MS, 100));
        let clock = SimulatedClock::from_readings(readings, PAUSE_SILENCE);
        let run = clock.summary().unwrap();
        assert_eq!(
            run.pauses,
            [Pause {
                start_utc_ns: T0 + S,
                end_utc_ns: T0 + 3 * S,
                simulated_ns: S,
            }]
        );
        assert_eq!(run.paused_ns, 2 * S);
        assert_eq!(run.simulated_ns, 2 * S);
        assert_eq!(run.ratio, Some(1.0));
        assert_eq!(clock.at(T0 + 2 * S).unwrap().simulated_ns, S);
        assert_eq!(
            clock.at(T0 + 3 * S + 500 * MS).unwrap().simulated_ns,
            S + 500 * MS
        );
    }

    #[test]
    fn a_pause_with_the_clock_silent_is_found_by_its_silence() {
        // Plays 1 s, publishes nothing for 5 s, then plays on a tick later.
        let mut readings = steady(0, 0, 10 * MS, 10 * MS, 101);
        readings.extend(steady(6 * S, S + 10 * MS, 10 * MS, 10 * MS, 100));
        let clock = SimulatedClock::from_readings(readings, PAUSE_SILENCE);
        let run = clock.summary().unwrap();
        assert_eq!(
            run.pauses,
            [Pause {
                start_utc_ns: T0 + S,
                end_utc_ns: T0 + 6 * S - 10 * MS,
                simulated_ns: S,
            }]
        );
        assert_eq!(run.ratio, Some(1.0));
        // Held while paused, then on at its pace to the reading after.
        assert_eq!(clock.at(T0 + 3 * S).unwrap().simulated_ns, S);
        assert_eq!(
            clock.at(T0 + 6 * S - 5 * MS).unwrap().simulated_ns,
            S + 5 * MS
        );
    }

    #[test]
    fn a_silence_shorter_than_pause_silence_times_the_usual_wait_is_no_pause() {
        // Silent 500 times the usual 10 ms: a pause at 500, not at 1000.
        let mut readings = steady(0, 0, 10 * MS, 10 * MS, 101);
        readings.extend(steady(6 * S, S + 10 * MS, 10 * MS, 10 * MS, 100));
        let paused = |silence| {
            SimulatedClock::from_readings(readings.clone(), silence)
                .pauses()
                .count()
        };
        assert_eq!((paused(500), paused(1000)), (1, 0));
    }

    #[test]
    fn a_slow_simulation_publishing_steadily_is_never_paused() {
        // A tick every 2 s of receive time, all along: slow, not paused.
        let run = summary(steady(0, 0, 2 * S, 10 * MS, 20));
        assert_eq!(run.pauses, []);
        assert_eq!(run.ratio, Some(0.005));
    }

    #[test]
    fn a_stepped_simulation_is_paused_between_steps_far_apart() {
        // Stepped a tick every 10 ms for a while, then once after 1 s.
        let mut readings = steady(0, 0, 10 * MS, MS, 50);
        readings.push(ClockReading {
            receive_utc_ns: T0 + 49 * 10 * MS + S,
            simulated_ns: 50 * MS,
        });
        let run = summary(readings);
        assert_eq!(run.simulated_ns, 50 * MS);
        assert_eq!(run.pauses.len(), 1);
        assert_eq!(run.pauses[0].simulated_ns, 49 * MS);
        assert_eq!(run.paused_ns, S - 10 * MS);
    }

    #[test]
    fn a_reset_to_zero_starts_a_new_segment_and_both_count() {
        let mut readings = steady(0, 0, 10 * MS, 10 * MS, 101);
        readings.extend(steady(S + 10 * MS, 0, 10 * MS, 10 * MS, 51));
        let clock = SimulatedClock::from_readings(readings, PAUSE_SILENCE);
        let run = clock.summary().unwrap();
        assert_eq!(clock.segments().len(), 2);
        assert_eq!(
            run.resets,
            [Reset {
                at_utc_ns: T0 + S + 10 * MS,
                from_ns: S,
                to_ns: 0,
            }]
        );
        assert_eq!(run.simulated_ns, S + 500 * MS);
        assert_eq!((run.start_ns, run.end_ns), (0, 500 * MS));
        let after = clock.at(T0 + S + 110 * MS).unwrap();
        assert_eq!((after.segment, after.simulated_ns), (1, 100 * MS));
        // Between the reset's two readings it is in neither segment.
        assert_eq!(clock.at(T0 + S + 5 * MS), None);
    }

    #[test]
    fn a_reading_back_by_less_than_a_tick_is_left_out_as_out_of_order() {
        let mut readings = steady(0, 0, 10 * MS, 10 * MS, 11);
        readings.push(ClockReading {
            receive_utc_ns: T0 + 105 * MS,
            simulated_ns: 95 * MS,
        });
        readings.push(ClockReading {
            receive_utc_ns: T0 + 110 * MS,
            simulated_ns: 110 * MS,
        });
        let clock = SimulatedClock::from_readings(readings, PAUSE_SILENCE);
        assert_eq!(clock.resets(), []);
        assert_eq!(clock.segments()[0].readings().len(), 12);
    }

    #[test]
    fn between_readings_simulated_time_moves_at_their_pace() {
        let clock = SimulatedClock::from_readings(steady(0, 0, 10 * MS, 5 * MS, 3), PAUSE_SILENCE);
        assert_eq!(clock.at(T0 + 4 * MS).unwrap().simulated_ns, 2 * MS);
        assert_eq!(clock.at(T0 + 20 * MS).unwrap().simulated_ns, 10 * MS);
        assert_eq!(clock.at(T0 - 1), None);
        assert_eq!(clock.at(T0 + 20 * MS + 1), None);
    }

    #[test]
    fn time_elapsed_is_simulated_where_the_clock_says_and_real_where_it_doesnt() {
        // Half real time for 1 s, paused (holding) for 1 s, half again for
        // 1 s; nothing for 1 s before its first reading or after its last.
        let mut readings = steady(0, 0, 10 * MS, 5 * MS, 101);
        readings.extend(steady(S + 10 * MS, 500 * MS, 10 * MS, 0, 100));
        readings.extend(steady(2 * S + 10 * MS, 505 * MS, 10 * MS, 5 * MS, 100));
        let clock = SimulatedClock::from_readings(readings, PAUSE_SILENCE);
        assert_eq!(clock.elapsed_ns(T0, T0 + S), 500 * MS);
        assert_eq!(clock.elapsed_ns(T0 + S, T0 + 2 * S), 0);
        assert_eq!(clock.elapsed_ns(T0, T0 + 3 * S), S);
        assert_eq!(clock.elapsed_ns(T0 - S, T0), S);
        assert_eq!(clock.elapsed_ns(T0 + 3 * S, T0 + 4 * S), S);
        assert_eq!(clock.elapsed_ns(T0 + S, T0), 0);
        // Without readings, all of it is real time.
        let none = SimulatedClock::from_readings([], PAUSE_SILENCE);
        assert_eq!(none.elapsed_ns(T0, T0 + S), S);
    }

    #[test]
    fn readings_out_of_receive_order_are_sorted_first() {
        let mut readings = steady(0, 0, 10 * MS, 10 * MS, 11);
        readings.reverse();
        assert_eq!(summary(readings).simulated_ns, 100 * MS);
    }

    #[test]
    fn no_readings_have_no_summary_and_one_has_no_ratio() {
        assert_eq!(
            SimulatedClock::from_readings([], PAUSE_SILENCE).summary(),
            None
        );
        let one = summary(steady(0, 7 * S, 0, 0, 1));
        assert_eq!(
            (one.start_ns, one.end_ns, one.simulated_ns),
            (7 * S, 7 * S, 0)
        );
        assert_eq!(one.ratio, None);
    }

    proptest::proptest! {
        #[test]
        fn simulated_time_never_goes_back_in_a_segment_and_meets_every_reading(
            steps in proptest::collection::vec((1..50 * MS, 0..20 * MS), 1..60),
        ) {
            let mut readings = vec![ClockReading { receive_utc_ns: T0, simulated_ns: 0 }];
            for (wait, tick) in steps {
                let last = readings[readings.len() - 1];
                readings.push(ClockReading {
                    receive_utc_ns: last.receive_utc_ns + wait,
                    simulated_ns: last.simulated_ns + tick,
                });
            }
            let clock = SimulatedClock::from_readings(readings.clone(), PAUSE_SILENCE);
            proptest::prop_assert_eq!(clock.segments().len(), 1);
            for reading in &readings {
                let at = clock.at(reading.receive_utc_ns).unwrap();
                proptest::prop_assert_eq!(at.simulated_ns, reading.simulated_ns);
            }
            let end = readings[readings.len() - 1];
            let mut previous = 0;
            for receive in (T0..=end.receive_utc_ns).step_by(usize::try_from(MS).unwrap()) {
                let now = clock.at(receive).unwrap().simulated_ns;
                proptest::prop_assert!(now >= previous, "back from {previous} to {now}");
                previous = now;
            }
            proptest::prop_assert_eq!(clock.at(end.receive_utc_ns).unwrap().simulated_ns, end.simulated_ns);
        }
    }
}
