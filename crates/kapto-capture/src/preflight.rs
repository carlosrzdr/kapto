// SPDX-License-Identifier: Apache-2.0

//! Pre-flight: whether this machine can record what the robot sends, found
//! out before recording rather than during it. Each check compares what was
//! measured with what recording needs:
//!
//! - the disk holds at least a minimum recording time at the streams' rate,
//!   above the free space recording stops at;
//! - the disk takes a short written-and-flushed test file at least as fast
//!   as the streams' rate;
//! - every declared stream arrives, and at roughly its declared rate.
//!
//! The streams' rate is an upper bound: each stream's declared or measured
//! rate, whichever is higher, times its largest message.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use kapto_core::ErrorCode;
use kapto_store::PreflightCheck;

/// Below this share of its declared rate, a stream is too slow to record as
/// declared. Matches what the web UI marks as a problem.
pub const SLOW_SHARE: f64 = 0.9;

/// How much the write test writes: enough to get past the operating
/// system's cache once flushed, little enough to take well under a second
/// on any disk worth recording on.
pub const WRITE_TEST_BYTES: usize = 32 * 1024 * 1024;

/// What pre-flight asks of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Needs {
    /// The free space recording stops at, in bytes; the minimum recording
    /// time must fit above it.
    pub stop_below: u64,
    /// The recording time the disk must hold at the streams' rate.
    pub recording_time: Duration,
}

/// One stream as pre-flight watched it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamSeen {
    /// The stream's ID.
    pub stream: String,
    /// Messages per second it is declared to send, if declared.
    pub declared_hz: Option<u32>,
    /// The largest message it sends, in bytes.
    pub max_message_bytes: u64,
    /// Messages received while it was watched.
    pub received: u64,
    /// Whether its messages are kept for late subscribers, so it may
    /// send nothing more once it has sent them.
    pub kept: bool,
    /// For such a stream: whether a message of it is kept, which every
    /// episode starts with.
    pub holds_kept: bool,
}

/// What pre-flight measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    /// How long the streams were watched.
    pub window: Duration,
    /// Each declared stream.
    pub streams: Vec<StreamSeen>,
    /// Bytes free on the disk holding the recordings, or why that can't be
    /// read.
    pub free_bytes: Result<u64, String>,
    /// How fast the disk took the write test, in bytes per second, or why
    /// it couldn't be written.
    pub write_bytes_per_second: Result<f64, String>,
}

fn check(code: ErrorCode, passed: bool, measured: String, needed: String) -> PreflightCheck {
    PreflightCheck {
        code: code.id().to_owned(),
        stream: None,
        passed,
        measured,
        needed,
        waived: false,
    }
}

/// Every check, disk first, then each stream in declaration order.
#[must_use]
pub fn evaluate(measured: &Measured, needs: Needs) -> Vec<PreflightCheck> {
    let rate = data_rate(measured);
    let mut checks = vec![room(measured, needs, rate), speed(measured, rate)];
    checks.extend(
        measured
            .streams
            .iter()
            .map(|seen| stream(seen, measured.window)),
    );
    checks
}

/// The streams' rate in bytes per second, an upper bound.
#[allow(
    clippy::cast_precision_loss,
    reason = "a data rate needs no more than a few significant digits"
)]
fn data_rate(measured: &Measured) -> f64 {
    let seconds = measured.window.as_secs_f64().max(f64::EPSILON);
    measured
        .streams
        .iter()
        .map(|seen| {
            let declared = f64::from(seen.declared_hz.unwrap_or(0));
            let observed = seen.received as f64 / seconds;
            declared.max(observed) * seen.max_message_bytes as f64
        })
        .sum()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "free space and rates need no more than a few significant digits"
)]
fn room(measured: &Measured, needs: Needs, rate: f64) -> PreflightCheck {
    let needed_bytes = rate * needs.recording_time.as_secs_f64() + needs.stop_below as f64;
    let needed = format!(
        "{} free: {} of recording at {}/s, above the {} recording stops at",
        size(needed_bytes),
        minutes(needs.recording_time),
        size(rate),
        size(needs.stop_below as f64)
    );
    match &measured.free_bytes {
        Ok(free) => check(
            ErrorCode::PreflightDiskTooSmall,
            *free as f64 >= needed_bytes,
            format!("{} free", size(*free as f64)),
            needed,
        ),
        Err(reason) => check(
            ErrorCode::PreflightDiskTooSmall,
            false,
            format!("free space can't be read: {reason}"),
            needed,
        ),
    }
}

fn speed(measured: &Measured, rate: f64) -> PreflightCheck {
    let needed = format!("at least {}/s, the streams' rate", size(rate));
    match &measured.write_bytes_per_second {
        Ok(speed) => check(
            ErrorCode::PreflightDiskTooSlow,
            *speed >= rate,
            format!("{}/s written and flushed", size(*speed)),
            needed,
        ),
        Err(reason) => check(
            ErrorCode::PreflightDiskTooSlow,
            false,
            format!("the test file couldn't be written: {reason}"),
            needed,
        ),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a rate needs no more than a few significant digits"
)]
fn stream(seen: &StreamSeen, window: Duration) -> PreflightCheck {
    let observed = seen.received as f64 / window.as_secs_f64().max(f64::EPSILON);
    let measured = format!(
        "{} messages in {:.1} s, {observed:.0}/s",
        seen.received,
        window.as_secs_f64()
    );
    let mut check = match seen.declared_hz {
        _ if seen.received == 0 && seen.kept => check(
            ErrorCode::PreflightStreamSilent,
            seen.holds_kept,
            if seen.holds_kept {
                format!("{measured}; one kept for every episode")
            } else {
                format!("{measured}, none since capture started")
            },
            "a message kept for every episode: it sends them once, to late subscribers".to_owned(),
        ),
        _ if seen.received == 0 => check(
            ErrorCode::PreflightStreamSilent,
            false,
            measured,
            "messages arriving".to_owned(),
        ),
        Some(declared) => check(
            ErrorCode::PreflightStreamSlow,
            observed >= SLOW_SHARE * f64::from(declared),
            measured,
            format!(
                "at least {:.0}/s, {:.0} % of the declared {declared}/s",
                SLOW_SHARE * f64::from(declared),
                SLOW_SHARE * 100.0
            ),
        ),
        None => check(
            ErrorCode::PreflightStreamSilent,
            true,
            measured,
            "messages arriving".to_owned(),
        ),
    };
    check.stream = Some(seen.stream.clone());
    check
}

/// Writes a test file of `bytes` into `folder`, flushes it to the disk and
/// removes it. Returns how fast it went, in bytes per second.
///
/// # Errors
///
/// The file couldn't be created, written or flushed.
#[allow(
    clippy::cast_precision_loss,
    reason = "a speed needs no more than a few significant digits"
)]
pub fn write_speed(folder: &Path, bytes: usize) -> io::Result<f64> {
    let path = folder.join(format!(".preflight-{}", std::process::id()));
    let block = vec![0x5A_u8; 1024 * 1024];
    let started = Instant::now();
    let written = (|| {
        let mut file = File::create(&path)?;
        let mut left = bytes;
        while left > 0 {
            let now = left.min(block.len());
            file.write_all(&block[..now])?;
            left -= now;
        }
        file.sync_all()
    })();
    let elapsed = started.elapsed();
    // Nothing to do about a failure: the next pre-flight writes over it.
    let _ = std::fs::remove_file(&path);
    written?;
    Ok(bytes as f64 / elapsed.as_secs_f64().max(f64::EPSILON))
}

/// A size in the unit that reads best: MiB below a GiB, GiB above.
fn size(bytes: f64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024.0 * MIB {
        format!("{:.1} GiB", bytes / (1024.0 * MIB))
    } else if bytes >= 0.1 * MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else {
        format!("{:.1} KiB", bytes / 1024.0)
    }
}

fn minutes(time: Duration) -> String {
    let minutes = time.as_secs() / 60;
    if minutes == 1 {
        "1 minute".to_owned()
    } else {
        format!("{minutes} minutes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn needs() -> Needs {
        Needs {
            stop_below: 100 * MIB,
            recording_time: Duration::from_secs(600),
        }
    }

    /// One stream declared at 1 kHz with 1 KiB messages: 1000 KiB/s, or
    /// 585.9 MiB for ten minutes.
    fn measured() -> Measured {
        Measured {
            window: Duration::from_secs(2),
            streams: vec![StreamSeen {
                stream: "arm".into(),
                declared_hz: Some(1000),
                max_message_bytes: 1024,
                received: 2000,
                kept: false,
                holds_kept: false,
            }],
            free_bytes: Ok(10_000 * MIB),
            write_bytes_per_second: Ok(500.0 * 1024.0 * 1024.0),
        }
    }

    fn failed(checks: &[PreflightCheck]) -> Vec<(&str, Option<&str>)> {
        checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| (check.code.as_str(), check.stream.as_deref()))
            .collect()
    }

    #[test]
    fn a_machine_that_can_record_the_streams_passes_every_check() {
        let checks = evaluate(&measured(), needs());
        assert_eq!(checks.len(), 3);
        assert_eq!(failed(&checks), []);
        assert_eq!(checks[2].measured, "2000 messages in 2.0 s, 1000/s");
        assert_eq!(
            checks[2].needed,
            "at least 900/s, 90 % of the declared 1000/s"
        );
    }

    #[test]
    fn a_disk_without_room_for_the_recording_time_above_the_stop_limit_fails() {
        let mut measured = measured();
        // 585.9 MiB of recording plus the 100 MiB limit don't fit in 600 MiB.
        measured.free_bytes = Ok(600 * MIB);
        let checks = evaluate(&measured, needs());
        assert_eq!(failed(&checks), [("preflight.disk_too_small", None)]);
        assert_eq!(checks[0].measured, "600.0 MiB free");
        assert_eq!(
            checks[0].needed,
            "685.9 MiB free: 10 minutes of recording at 1.0 MiB/s, above the 100.0 MiB recording stops at"
        );
        measured.free_bytes = Err("permission denied".into());
        let checks = evaluate(&measured, needs());
        assert_eq!(
            checks[0].measured,
            "free space can't be read: permission denied"
        );
        assert!(!checks[0].passed);
    }

    #[test]
    fn a_disk_slower_than_the_streams_fails_and_so_does_one_that_cant_be_written() {
        let mut measured = measured();
        measured.write_bytes_per_second = Ok(512.0 * 1024.0);
        assert_eq!(
            failed(&evaluate(&measured, needs())),
            [("preflight.disk_too_slow", None)]
        );
        measured.write_bytes_per_second = Err("read-only file system".into());
        let checks = evaluate(&measured, needs());
        assert_eq!(
            checks[1].measured,
            "the test file couldn't be written: read-only file system"
        );
    }

    #[test]
    fn a_stream_that_never_arrived_or_arrives_well_below_its_declared_rate_fails() {
        let mut measured = measured();
        measured.streams.push(StreamSeen {
            stream: "camera".into(),
            declared_hz: None,
            max_message_bytes: 1024,
            received: 0,
            kept: false,
            holds_kept: false,
        });
        measured.streams.push(StreamSeen {
            stream: "odometry".into(),
            declared_hz: None,
            max_message_bytes: 64,
            received: 3,
            kept: false,
            holds_kept: false,
        });
        measured.streams[0].received = 1700;
        assert_eq!(
            failed(&evaluate(&measured, needs())),
            [
                ("preflight.stream_slow", Some("arm")),
                ("preflight.stream_silent", Some("camera"))
            ]
        );
    }

    #[test]
    fn a_stream_kept_for_late_subscribers_passes_once_one_is_kept_with_nothing_arriving() {
        let mut measured = measured();
        measured.streams.push(StreamSeen {
            stream: "tf_static".into(),
            declared_hz: None,
            max_message_bytes: 1024,
            received: 0,
            kept: true,
            holds_kept: false,
        });
        let checks = evaluate(&measured, needs());
        assert_eq!(
            failed(&checks),
            [("preflight.stream_silent", Some("tf_static"))]
        );
        let kept = checks
            .iter()
            .find(|check| check.stream.as_deref() == Some("tf_static"))
            .unwrap();
        assert_eq!(
            kept.measured,
            "0 messages in 2.0 s, 0/s, none since capture started"
        );

        measured.streams[1].holds_kept = true;
        let checks = evaluate(&measured, needs());
        assert_eq!(failed(&checks), []);
        assert_eq!(
            checks[3].needed,
            "a message kept for every episode: it sends them once, to late subscribers"
        );
    }

    #[test]
    fn a_stream_faster_than_declared_counts_at_its_measured_rate() {
        let mut measured = measured();
        // Twice the declared rate: 2000 KiB/s, slower than the disk test.
        measured.streams[0].received = 4000;
        measured.write_bytes_per_second = Ok(1500.0 * 1024.0);
        assert_eq!(
            failed(&evaluate(&measured, needs())),
            [("preflight.disk_too_slow", None)]
        );
    }

    #[test]
    fn the_write_test_leaves_nothing_behind() {
        let folder = std::env::temp_dir().join(format!("kapto-preflight-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let speed = write_speed(&folder, 2 * 1024 * 1024).unwrap();
        assert!(speed > 0.0);
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        std::fs::remove_dir(&folder).unwrap();
        assert!(write_speed(&folder, 1024).is_err(), "the folder is gone");
    }
}
