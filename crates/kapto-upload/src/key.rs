// SPDX-License-Identifier: Apache-2.0

//! Where an episode goes in its destination: the destination's prefix, then
//! its layout with the episode's own values in place of the placeholders,
//! e.g. `fleet-a/robot-7` and `{robot}/{date}/{episode}` give
//! `fleet-a/robot-7/arm7/2026-03-23/20260323T073012.345Z-5f729d2c`.
//!
//! A key is worked out once, when an episode's upload starts, and kept with
//! it: changing the layout later never moves an episode already uploaded.

use kapto_core::time::utc_iso8601;
use kapto_core::{ErrorCode, KaptoError};
use object_store::path::Path;

/// What an episode's key is made from.
#[derive(Clone, Copy, Debug)]
pub struct KeyValues<'a> {
    /// The ID of the robot package it was recorded with, if any.
    pub robot: Option<&'a str>,
    /// Its ID.
    pub episode: &'a str,
    /// When it started, in UTC nanoseconds since the Unix epoch.
    pub started_utc_ns: i64,
    /// The SHA-256 of its `manifest.json`, as hex.
    pub manifest_sha256: &'a str,
}

/// What `{robot}` is for an episode recorded without a robot package.
pub const NO_ROBOT: &str = "unpackaged";

/// The key of an episode with `values` under `prefix` (empty for the
/// destination's root), laid out as `layout` says.
///
/// # Errors
///
/// `config.invalid_value` if the result isn't a key storage takes, which a
/// layout `kapto config validate` accepted never gives.
pub fn episode_key(prefix: &str, layout: &str, values: &KeyValues<'_>) -> Result<Path, KaptoError> {
    // `2026-03-23T07:30:12.345000000Z`: every date placeholder is a part of it.
    let time = utc_iso8601(values.started_utc_ns);
    let part = |range: std::ops::Range<usize>| time.get(range).unwrap_or("0000");
    let rendered = layout
        .replace("{robot}", values.robot.unwrap_or(NO_ROBOT))
        .replace("{date}", part(0..10))
        .replace("{year}", part(0..4))
        .replace("{month}", part(5..7))
        .replace("{day}", part(8..10))
        .replace("{hour}", part(11..13))
        .replace("{episode}", values.episode)
        .replace(
            "{sha8}",
            values
                .manifest_sha256
                .get(..8)
                .unwrap_or(values.manifest_sha256),
        );
    let key = if prefix.is_empty() {
        rendered
    } else {
        format!("{prefix}/{rendered}")
    };
    Path::parse(&key).map_err(|error| {
        KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
            "the episode's key `{key}` isn't one storage takes: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: KeyValues<'static> = KeyValues {
        robot: Some("arm7"),
        episode: "20260323T073012.345Z-5f729d2c",
        started_utc_ns: 1_774_251_012_345_000_000,
        manifest_sha256: "0123456789abcdef",
    };

    #[test]
    fn the_layout_is_filled_in_under_the_prefix() {
        let key = episode_key("fleet-a/robot-7", "{robot}/{date}/{episode}", &VALUES).unwrap();
        assert_eq!(
            key.as_ref(),
            "fleet-a/robot-7/arm7/2026-03-23/20260323T073012.345Z-5f729d2c"
        );
        let key = episode_key(
            "",
            "year={year}/month={month}/{day}/{hour}/{episode}_{sha8}",
            &VALUES,
        )
        .unwrap();
        assert_eq!(
            key.as_ref(),
            "year=2026/month=03/23/07/20260323T073012.345Z-5f729d2c_01234567"
        );
    }

    #[test]
    fn an_episode_without_a_package_is_filed_under_a_name_of_its_own() {
        let values = KeyValues {
            robot: None,
            ..VALUES
        };
        let key = episode_key("", "{robot}/{episode}", &values).unwrap();
        assert!(key.as_ref().starts_with("unpackaged/"), "{key}");
    }
}
