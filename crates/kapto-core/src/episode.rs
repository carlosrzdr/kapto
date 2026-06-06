// SPDX-License-Identifier: Apache-2.0

//! Episode identifiers.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{ErrorCode, KaptoError};

/// Identifies one recorded episode, e.g. `20260305T192345.123Z-1a2b3c4d`.
///
/// It is the UTC time the episode was created, to the millisecond, followed by
/// 8 random hex digits so episodes created in the same millisecond differ. The
/// text form is safe in file and directory names, and sorting IDs as text
/// sorts episodes by creation time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpisodeId {
    unix_ms: u64,
    suffix: u32,
}

const MS_PER_SECOND: u64 = 1_000;
const MS_PER_DAY: u64 = 86_400_000;
/// 9999-12-31T23:59:59.999Z, the last time a four-digit year can show.
const MAX_UNIX_MS: u64 = 253_402_300_799_999;
/// Length of the text form: `YYYYMMDDTHHMMSS.mmmZ-xxxxxxxx`.
const TEXT_LEN: usize = 29;

impl EpisodeId {
    /// A new ID for an episode created now.
    #[must_use]
    pub fn generate() -> Self {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since_epoch| {
                u64::try_from(since_epoch.as_millis()).unwrap_or(MAX_UNIX_MS)
            });
        Self::from_parts(unix_ms, random_u32())
    }

    /// An ID from its creation time in milliseconds since the Unix epoch and
    /// its random suffix. Times after the year 9999 are clamped to its end.
    #[must_use]
    pub const fn from_parts(unix_ms: u64, suffix: u32) -> Self {
        let unix_ms = if unix_ms > MAX_UNIX_MS {
            MAX_UNIX_MS
        } else {
            unix_ms
        };
        Self { unix_ms, suffix }
    }

    /// Creation time in milliseconds since the Unix epoch (UTC).
    #[must_use]
    pub const fn unix_ms(self) -> u64 {
        self.unix_ms
    }
}

impl fmt::Display for EpisodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.unix_ms / MS_PER_DAY);
        let ms_of_day = self.unix_ms % MS_PER_DAY;
        let seconds_of_day = ms_of_day / MS_PER_SECOND;
        write!(
            f,
            "{year:04}{month:02}{day:02}T{:02}{:02}{:02}.{:03}Z-{:08x}",
            seconds_of_day / 3_600,
            seconds_of_day / 60 % 60,
            seconds_of_day % 60,
            ms_of_day % MS_PER_SECOND,
            self.suffix,
        )
    }
}

impl FromStr for EpisodeId {
    type Err = KaptoError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        parse(text)
            .ok_or_else(|| KaptoError::new(ErrorCode::EpisodeInvalidId).detail(format!("`{text}`")))
    }
}

fn parse(text: &str) -> Option<EpisodeId> {
    if text.len() != TEXT_LEN || !text.is_ascii() {
        return None;
    }
    let separators_in_place =
        text.get(8..9)? == "T" && text.get(15..16)? == "." && text.get(19..21)? == "Z-";
    if !separators_in_place {
        return None;
    }
    let year = digits(text.get(0..4)?)?;
    let month = digits(text.get(4..6)?)?;
    let day = digits(text.get(6..8)?)?;
    let hour = digits(text.get(9..11)?)?;
    let minute = digits(text.get(11..13)?)?;
    let second = digits(text.get(13..15)?)?;
    let millis = digits(text.get(16..19)?)?;
    let suffix_text = text.get(21..)?;
    if !suffix_text
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
    {
        return None;
    }
    let suffix = u32::from_str_radix(suffix_text, 16).ok()?;

    if year < 1970 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let seconds_of_day = hour * 3_600 + minute * 60 + second;
    Some(EpisodeId::from_parts(
        days * MS_PER_DAY + seconds_of_day * MS_PER_SECOND + millis,
        suffix,
    ))
}

/// Parses a fixed-width run of ASCII digits (no sign, no spaces).
fn digits(text: &str) -> Option<u64> {
    if text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

/// Year, month and day of the given day since 1970-01-01, in the proleptic
/// Gregorian calendar (Howard Hinnant's `civil_from_days`, for days ≥ 0).
pub(crate) fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468; // days since 0000-03-01
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 for a date from 1970 on, or `None` if the date
/// doesn't exist (e.g. February 30th).
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let year_from_march = if month <= 2 { year - 1 } else { year };
    let era = year_from_march / 400;
    let year_of_era = year_from_march % 400;
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = (era * 146_097 + day_of_era).checked_sub(719_468)?;
    // Rejects days past the end of the month: they map to a different date.
    (civil_from_days(days) == (year, month, day)).then_some(days)
}

/// 32 unpredictable bits without a dependency: `RandomState` is seeded from
/// the operating system's randomness, so hashing a value with a fresh one
/// gives bits no other process or earlier call can predict.
fn random_u32() -> u32 {
    let bits = RandomState::new().hash_one(());
    u32::try_from(bits >> 32).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn text_form_shows_the_utc_creation_time_and_suffix() {
        // 2026-03-05T19:23:45.123Z
        let id = EpisodeId::from_parts(1_772_738_625_123, 0x1a2b_3c4d);
        assert_eq!(id.to_string(), "20260305T192345.123Z-1a2b3c4d");
    }

    #[test]
    fn the_unix_epoch_and_the_last_four_digit_year_are_representable() {
        assert_eq!(
            EpisodeId::from_parts(0, 0).to_string(),
            "19700101T000000.000Z-00000000"
        );
        assert_eq!(
            EpisodeId::from_parts(MAX_UNIX_MS, u32::MAX).to_string(),
            "99991231T235959.999Z-ffffffff"
        );
    }

    #[test]
    fn times_after_year_9999_are_clamped() {
        assert_eq!(
            EpisodeId::from_parts(u64::MAX, 0),
            EpisodeId::from_parts(MAX_UNIX_MS, 0)
        );
    }

    #[test]
    fn leap_days_exist_only_in_leap_years() {
        assert!("20240229T000000.000Z-00000000".parse::<EpisodeId>().is_ok());
        assert!("20000229T000000.000Z-00000000".parse::<EpisodeId>().is_ok());
        assert!(
            "21000229T000000.000Z-00000000"
                .parse::<EpisodeId>()
                .is_err()
        );
        assert!(
            "20260229T000000.000Z-00000000"
                .parse::<EpisodeId>()
                .is_err()
        );
    }

    #[test]
    fn malformed_ids_are_rejected_with_their_code() {
        for bad in [
            "",
            "20260305T192345.123Z-1a2b3c4",   // suffix too short
            "20260305T192345.123Z-1A2B3C4D",  // uppercase hex
            "20261005 192345.123Z-1a2b3c4d",  // missing T
            "20261305T192345.123Z-1a2b3c4d",  // month 13
            "20260332T192345.123Z-1a2b3c4d",  // day 32
            "20260305T242345.123Z-1a2b3c4d",  // hour 24
            "20260305T196045.123Z-1a2b3c4d",  // minute 60
            "19691231T235959.999Z-1a2b3c4d",  // before 1970
            "2026+005T192345.123Z-1a2b3c4d",  // sign in a number
            "20260305T192345.123Z-1a2b3c4d ", // trailing space
        ] {
            let error = bad.parse::<EpisodeId>().unwrap_err();
            assert_eq!(error.code(), ErrorCode::EpisodeInvalidId, "{bad:?}");
        }
    }

    #[test]
    fn ids_generated_together_are_distinct() {
        let ids: HashSet<String> = (0..1_000)
            .map(|_| EpisodeId::generate().to_string())
            .collect();
        assert_eq!(ids.len(), 1_000);
    }

    #[test]
    fn generated_ids_use_the_current_time() {
        let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let id = EpisodeId::generate();
        let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let created = u128::from(id.unix_ms());
        assert!(before.as_millis() <= created && created <= after.as_millis());
    }

    proptest! {
        #[test]
        fn ids_round_trip_through_text(unix_ms in 0..=MAX_UNIX_MS, suffix: u32) {
            let id = EpisodeId::from_parts(unix_ms, suffix);
            prop_assert_eq!(id.to_string().parse::<EpisodeId>().unwrap(), id);
        }

        #[test]
        fn text_order_matches_creation_order(
            a in 0..=MAX_UNIX_MS,
            b in 0..=MAX_UNIX_MS,
            suffix_a: u32,
            suffix_b: u32,
        ) {
            let id_a = EpisodeId::from_parts(a, suffix_a);
            let id_b = EpisodeId::from_parts(b, suffix_b);
            prop_assert_eq!(id_a.to_string().cmp(&id_b.to_string()), id_a.cmp(&id_b));
            if a < b {
                prop_assert!(id_a < id_b);
            }
        }
    }
}
