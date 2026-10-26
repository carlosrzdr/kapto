// SPDX-License-Identifier: Apache-2.0

//! How a simulation ran over an episode, from the values of the role
//! `time.simulated` its checks read.

use kapto_config::SimulatedSettings;
use kapto_core::Role;
use kapto_core::simulated::{ClockReading, ROLE, SimulatedClock};
use kapto_store::Simulated;

use crate::EpisodeView;

/// The clock of the simulation `episode` recorded, read from its values of
/// the role as `settings` say; `None` if its package gives no clock the
/// role, or the clock has no messages.
#[must_use]
pub fn clock(episode: &EpisodeView, settings: &SimulatedSettings) -> Option<SimulatedClock> {
    let role: Role = ROLE.parse().ok()?;
    let column = episode.role(&role)?;
    let readings = column
        .times_utc_ns
        .iter()
        .zip(&column.values)
        .filter(|(_, seconds)| seconds.is_finite())
        .map(|(&receive_utc_ns, &seconds)| ClockReading {
            receive_utc_ns,
            simulated_ns: nanoseconds(seconds),
        });
    let clock = SimulatedClock::from_readings(readings, settings.pause_silence);
    (!clock.segments().is_empty()).then_some(clock)
}

/// How the simulation `episode` recorded ran, from its clock
/// ([`EpisodeView::with_simulated`]); `None` if it recorded none.
#[must_use]
pub fn how_it_ran(episode: &EpisodeView) -> Option<Box<Simulated>> {
    episode
        .simulated()?
        .summary()
        .map(|summary| Box::new(Simulated::from(summary)))
}

/// Seconds read as a `time` field gives them, in nanoseconds again.
#[allow(clippy::cast_possible_truncation)] // Finite, and saturating beyond i64.
fn nanoseconds(seconds: f64) -> i64 {
    (seconds * 1e9).round() as i64
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::RoleColumn;

    const S: i64 = 1_000_000_000;
    const T0: i64 = 1_800_000_000 * S;

    fn episode(roles: BTreeMap<Role, RoleColumn>) -> EpisodeView {
        let summary =
            kapto_testkit::episode("20270115T080000.000Z-1a2b3c4d", (T0, T0 + 4 * S), vec![]);
        EpisodeView::new(std::path::Path::new("."), summary).with_roles(roles)
    }

    fn with_clock(episode: EpisodeView) -> EpisodeView {
        let clock = clock(&episode, &SimulatedSettings::default());
        episode.with_simulated(clock)
    }

    #[test]
    fn a_clock_at_half_real_time_and_reset_is_summed_from_its_role() {
        // Ten readings a second for 2 s at half speed, then reset to zero.
        let mut times = Vec::new();
        let mut values = Vec::new();
        for n in 0..=20_i32 {
            times.push(T0 + i64::from(n) * S / 10);
            values.push(0.05 * f64::from(n));
        }
        for n in 0..=10_i32 {
            times.push(T0 + 2 * S + i64::from(n + 1) * S / 10);
            values.push(0.05 * f64::from(n));
        }
        let roles = BTreeMap::from([(
            ROLE.parse().unwrap(),
            RoleColumn {
                times_utc_ns: times,
                width: 1,
                values,
            },
        )]);
        let ran = how_it_ran(&with_clock(episode(roles))).unwrap();
        assert_eq!(ran.simulated_ns, 1_500_000_000);
        assert_eq!(ran.resets.len(), 1);
        assert_eq!(ran.pauses, []);
        assert!((ran.ratio.unwrap() - 0.5).abs() < 0.03, "{:?}", ran.ratio);
    }

    #[test]
    fn an_episode_without_the_role_or_without_its_messages_ran_no_simulation() {
        assert_eq!(how_it_ran(&with_clock(episode(BTreeMap::new()))), None);
        let empty = BTreeMap::from([(
            ROLE.parse().unwrap(),
            RoleColumn {
                times_utc_ns: Vec::new(),
                width: 1,
                values: Vec::new(),
            },
        )]);
        let empty = with_clock(episode(empty));
        assert_eq!((empty.simulated(), how_it_ran(&empty)), (None, None));
    }
}
