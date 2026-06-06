// SPDX-License-Identifier: Apache-2.0

//! The types a deployment setting can have, and how text becomes a value.

use std::fmt;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use crate::upload::{DeleteLocal, UploadWhen, Verification};

/// The type of a setting, which decides how its text is parsed and checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    /// A TCP or UDP port, 1–65535. Zero is rejected: Kapto never lets the
    /// operating system pick a port, so the port in use is always the one
    /// configured.
    Port,
    /// An IPv4 or IPv6 address.
    IpAddress,
    /// A filesystem path.
    Path,
    /// One of `error`, `warn`, `info`, `debug`, `trace`.
    LogLevel,
    /// One of `refuse`, `warn`.
    PreflightFailures,
    /// One of `lz4`, `zstd`, `none`.
    Compression,
    /// A time in seconds, more than 0 and at most an hour, fractions
    /// allowed: `1.5`.
    Seconds,
    /// A rate per second, 1–1000.
    Hertz,
    /// A size in MiB, 1–1048576 (1 TiB).
    Mebibytes,
    /// How many times, 1–100.
    Count,
    /// A number of days, 1–3650 (about ten years).
    Days,
    /// `true` or `false`.
    Switch,
    /// Which episodes are uploaded by themselves.
    UploadWhen,
    /// What happens to an episode's local copy once uploaded.
    DeleteLocal,
    /// How an uploaded file is checked.
    Verification,
    /// A bandwidth in MiB/s, 0–100000; 0 for no cap.
    Bandwidth,
}

impl ValueType {
    /// How the expected value is described in error messages.
    #[must_use]
    pub const fn expected(self) -> &'static str {
        match self {
            Self::Port => "an integer from 1 to 65535",
            Self::IpAddress => "an IP address such as \"127.0.0.1\" or \"::1\"",
            Self::Path => "a non-empty path",
            Self::LogLevel => "one of \"error\", \"warn\", \"info\", \"debug\", \"trace\"",
            Self::PreflightFailures => "one of \"refuse\", \"warn\"",
            Self::Compression => "one of \"lz4\", \"zstd\", \"none\"",
            Self::Seconds => "a number of seconds above 0 and at most 3600, such as 1.5",
            Self::Hertz => "a whole number of times per second from 1 to 1000",
            Self::Mebibytes => "a whole number of MiB from 1 to 1048576",
            Self::Count => "a whole number from 1 to 100",
            Self::Days => "a whole number of days from 1 to 3650",
            Self::Switch => "true or false",
            Self::UploadWhen => {
                "one of \"never\", \"finalised\", \"finalised_clean\", \"accepted\""
            }
            Self::DeleteLocal => "one of \"never\", \"when_disk_low\", \"after_upload\"",
            Self::Verification => "one of \"server_checksum\", \"read_back\", \"size_only\"",
            Self::Bandwidth => "a whole number of MiB/s from 0 (no cap) to 100000",
        }
    }

    /// Parses the text form, as given in an environment variable or a flag.
    pub(crate) fn parse_text(self, text: &str) -> Option<Value> {
        match self {
            Self::Port => text.parse().ok().and_then(port),
            Self::IpAddress => text.parse().ok().map(Value::IpAddress),
            Self::Path => path(text),
            Self::LogLevel => text.parse().ok().map(Value::LogLevel),
            Self::PreflightFailures => text.parse().ok().map(Value::PreflightFailures),
            Self::Compression => text.parse().ok().map(Value::Compression),
            Self::Seconds => text.parse().ok().and_then(seconds),
            Self::Hertz => text.parse().ok().and_then(hertz),
            Self::Mebibytes => text.parse().ok().and_then(mebibytes),
            Self::Count => text.parse().ok().and_then(count),
            Self::Days => text.parse().ok().and_then(days),
            Self::Switch => text.parse().ok().map(Value::Switch),
            Self::UploadWhen => text.parse().ok().map(Value::UploadWhen),
            Self::DeleteLocal => text.parse().ok().map(Value::DeleteLocal),
            Self::Verification => text.parse().ok().map(Value::Verification),
            Self::Bandwidth => text.parse().ok().and_then(bandwidth),
        }
    }

    /// Converts a TOML value: numbers are numbers, everything else strings.
    pub(crate) fn parse_toml(self, value: &toml_edit::Value) -> Option<Value> {
        match self {
            Self::Port => value.as_integer().and_then(port),
            #[allow(
                clippy::cast_precision_loss,
                reason = "seconds are checked to be at most 3600 right after"
            )]
            Self::Seconds => value
                .as_float()
                .or_else(|| value.as_integer().map(|number| number as f64))
                .and_then(seconds),
            Self::Hertz => value
                .as_integer()
                .and_then(|number| u32::try_from(number).ok().and_then(hertz)),
            Self::Mebibytes => value
                .as_integer()
                .and_then(|number| u64::try_from(number).ok().and_then(mebibytes)),
            Self::Count => value
                .as_integer()
                .and_then(|number| u32::try_from(number).ok().and_then(count)),
            Self::Days => value
                .as_integer()
                .and_then(|number| u32::try_from(number).ok().and_then(days)),
            Self::Switch => value.as_bool().map(Value::Switch),
            Self::Bandwidth => value
                .as_integer()
                .and_then(|number| u32::try_from(number).ok().and_then(bandwidth)),
            Self::IpAddress
            | Self::Path
            | Self::LogLevel
            | Self::PreflightFailures
            | Self::Compression
            | Self::UploadWhen
            | Self::DeleteLocal
            | Self::Verification => value.as_str().and_then(|text| self.parse_text(text)),
        }
    }
}

fn seconds(number: f64) -> Option<Value> {
    (number > 0.0 && number <= 3600.0).then(|| Value::Seconds(Duration::from_secs_f64(number)))
}

fn hertz(number: u32) -> Option<Value> {
    (1..=1000).contains(&number).then_some(Value::Hertz(number))
}

fn mebibytes(number: u64) -> Option<Value> {
    (1..=1 << 20)
        .contains(&number)
        .then_some(Value::Mebibytes(number))
}

fn count(number: u32) -> Option<Value> {
    (1..=100).contains(&number).then_some(Value::Count(number))
}

fn bandwidth(number: u32) -> Option<Value> {
    (number <= 100_000).then_some(Value::Bandwidth(number))
}

fn days(number: u32) -> Option<Value> {
    (1..=3650).contains(&number).then_some(Value::Days(number))
}

fn port(number: i64) -> Option<Value> {
    u16::try_from(number)
        .ok()
        .filter(|port| *port != 0)
        .map(Value::Port)
}

fn path(text: &str) -> Option<Value> {
    (!text.is_empty()).then(|| Value::Path(PathBuf::from(text)))
}

/// A parsed setting value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// A port.
    Port(u16),
    /// An IP address.
    IpAddress(IpAddr),
    /// A filesystem path.
    Path(PathBuf),
    /// A log level.
    LogLevel(LogLevel),
    /// What a failed pre-flight check does to starting an episode.
    PreflightFailures(PreflightFailures),
    /// How a recording's chunks are compressed.
    Compression(Compression),
    /// A time.
    Seconds(Duration),
    /// A rate per second.
    Hertz(u32),
    /// A size in MiB.
    Mebibytes(u64),
    /// How many times.
    Count(u32),
    /// A number of days.
    Days(u32),
    /// On or off.
    Switch(bool),
    /// Which episodes are uploaded by themselves.
    UploadWhen(UploadWhen),
    /// What happens to an episode's local copy once uploaded.
    DeleteLocal(DeleteLocal),
    /// How an uploaded file is checked.
    Verification(Verification),
    /// A bandwidth in MiB/s; 0 for no cap.
    Bandwidth(u32),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Port(port) => write!(f, "{port}"),
            Self::IpAddress(address) => write!(f, "{address}"),
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::LogLevel(level) => write!(f, "{level}"),
            Self::PreflightFailures(failures) => write!(f, "{failures}"),
            Self::Compression(compression) => write!(f, "{compression}"),
            Self::Seconds(time) => write!(f, "{}", time.as_secs_f64()),
            Self::Hertz(rate) | Self::Bandwidth(rate) => write!(f, "{rate}"),
            Self::Mebibytes(size) => write!(f, "{size}"),
            Self::Count(count) => write!(f, "{count}"),
            Self::Days(days) => write!(f, "{days}"),
            Self::Switch(on) => write!(f, "{on}"),
            Self::UploadWhen(when) => write!(f, "{when}"),
            Self::DeleteLocal(delete) => write!(f, "{delete}"),
            Self::Verification(verify) => write!(f, "{verify}"),
        }
    }
}

/// How much Kapto logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Only failures.
    Error,
    /// Failures and things that may need attention.
    Warn,
    /// Normal operation: sessions, episodes, connections.
    Info,
    /// Details useful when investigating a problem.
    Debug,
    /// Everything, including per-batch detail.
    Trace,
}

impl FromStr for LogLevel {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(()),
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        })
    }
}

/// How a recording's chunks are compressed: each holds every stream's
/// messages, so the choice is the whole recording's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// `lz4`: fast, about a third smaller for joint states.
    Lz4,
    /// `zstd`: smaller, for more CPU.
    Zstd,
    /// `none`: as received, for messages already compressed, such as a
    /// camera's JPEG frames.
    None,
}

impl FromStr for Compression {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "lz4" => Ok(Self::Lz4),
            "zstd" => Ok(Self::Zstd),
            "none" => Ok(Self::None),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lz4 => "lz4",
            Self::Zstd => "zstd",
            Self::None => "none",
        })
    }
}

/// What a failed pre-flight check does to starting an episode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightFailures {
    /// The start is refused unless it waives each failed check.
    Refuse,
    /// The episode starts anyway; it says which checks had failed.
    Warn,
}

impl FromStr for PreflightFailures {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "refuse" => Ok(Self::Refuse),
            "warn" => Ok(Self::Warn),
            _ => Err(()),
        }
    }
}

impl fmt::Display for PreflightFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Refuse => "refuse",
            Self::Warn => "warn",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_must_be_between_1_and_65535() {
        assert_eq!(ValueType::Port.parse_text("1"), Some(Value::Port(1)));
        assert_eq!(
            ValueType::Port.parse_text("65535"),
            Some(Value::Port(65_535))
        );
        for bad in ["0", "65536", "-1", "7700a", "", " 7700"] {
            assert_eq!(ValueType::Port.parse_text(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ip_addresses_accept_ipv4_and_ipv6() {
        assert!(ValueType::IpAddress.parse_text("0.0.0.0").is_some());
        assert!(ValueType::IpAddress.parse_text("::1").is_some());
        assert!(ValueType::IpAddress.parse_text("localhost").is_none());
    }

    #[test]
    fn paths_must_not_be_empty() {
        assert!(ValueType::Path.parse_text("/data").is_some());
        assert!(ValueType::Path.parse_text("").is_none());
    }

    #[test]
    fn seconds_are_above_zero_and_at_most_an_hour_with_fractions_allowed() {
        assert_eq!(
            ValueType::Seconds.parse_text("1.5"),
            Some(Value::Seconds(Duration::from_millis(1500)))
        );
        assert_eq!(
            ValueType::Seconds.parse_text("3600"),
            Some(Value::Seconds(Duration::from_secs(3600)))
        );
        for bad in ["0", "-1", "3600.5", "1s", "", "NaN", "inf"] {
            assert_eq!(ValueType::Seconds.parse_text(bad), None, "{bad:?}");
        }
        let toml = |text: &str| text.parse::<toml_edit::Value>().unwrap();
        assert_eq!(
            ValueType::Seconds.parse_toml(&toml("2")),
            Some(Value::Seconds(Duration::from_secs(2)))
        );
        assert_eq!(
            ValueType::Seconds.parse_toml(&toml("0.25")),
            Some(Value::Seconds(Duration::from_millis(250)))
        );
        assert_eq!(ValueType::Seconds.parse_toml(&toml("\"2\"")), None);
        assert_eq!(
            Value::Seconds(Duration::from_millis(1500)).to_string(),
            "1.5"
        );
        assert_eq!(Value::Seconds(Duration::from_secs(30)).to_string(), "30");
    }

    #[test]
    fn rates_and_sizes_are_whole_numbers_in_their_range() {
        assert_eq!(ValueType::Hertz.parse_text("60"), Some(Value::Hertz(60)));
        assert_eq!(
            ValueType::Mebibytes.parse_text("64"),
            Some(Value::Mebibytes(64))
        );
        for bad in ["0", "1001", "2.5", "-1", ""] {
            assert_eq!(ValueType::Hertz.parse_text(bad), None, "{bad:?}");
        }
        assert_eq!(ValueType::Count.parse_text("3"), Some(Value::Count(3)));
        assert_eq!(
            ValueType::Switch.parse_text("false"),
            Some(Value::Switch(false))
        );
        for bad in ["yes", "1", "True", ""] {
            assert_eq!(ValueType::Switch.parse_text(bad), None, "{bad:?}");
        }
        for bad in ["0", "101", "1.5", ""] {
            assert_eq!(ValueType::Count.parse_text(bad), None, "{bad:?}");
        }
        for bad in ["0", "1048577", "1.5", ""] {
            assert_eq!(ValueType::Mebibytes.parse_text(bad), None, "{bad:?}");
        }
        assert_eq!(ValueType::Days.parse_text("7"), Some(Value::Days(7)));
        for bad in ["0", "3651", "1.5", "7d", ""] {
            assert_eq!(ValueType::Days.parse_text(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn log_levels_round_trip_through_text() {
        for name in ["error", "warn", "info", "debug", "trace"] {
            let level: LogLevel = name.parse().unwrap();
            assert_eq!(level.to_string(), name);
        }
        assert!("INFO".parse::<LogLevel>().is_err());
    }
}
