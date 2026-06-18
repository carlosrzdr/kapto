// SPDX-License-Identifier: Apache-2.0

//! Pre-flight in the capture process: once when it starts and again whenever
//! asked, it watches the streams for `capture.preflight_window` while a
//! `cap-preflight` thread writes the disk test, then checks the results
//! (see `kapto_capture::preflight`). It only runs between episodes: its test
//! file would compete with a recording's writes.
//!
//! Each episode is started against the latest run: with
//! `capture.preflight = refuse`, a start is refused while a check failed,
//! unless it waives each failed check. Either way the episode keeps the run,
//! saying which checks failed and which were waived.

use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kapto_capture::StreamIndex;
use kapto_capture::control::{Pending, Response};
use kapto_capture::preflight::{
    Measured, Needs, RouterSeen, StreamSeen, WRITE_TEST_BYTES, evaluate, write_speed,
};
use kapto_config::{DeploymentConfig, PreflightFailures, StreamDeclaration};
use kapto_core::clock::utc_now_ns;
use kapto_core::{ErrorCode, KaptoError, utc_iso8601};
use kapto_store::Preflight as Report;

use crate::record::RecordSettings;
use crate::station::Live;

/// Pre-flight's runs.
pub(crate) struct Preflight {
    folder: PathBuf,
    needs: Needs,
    window: Duration,
    /// Each stream's ID, declared rate, largest message and whether its
    /// messages are kept for late subscribers, in declaration order.
    streams: Vec<(String, Option<u32>, u64, bool)>,
    run: Option<Run>,
    /// Requests answered once the run going ends.
    waiting: Vec<Pending>,
    /// What the latest run found, once one has ended.
    latest: Option<Report>,
    /// What a failed check does to a start.
    failures: PreflightFailures,
    /// Over Zenoh, where the router should answer.
    router: Option<String>,
}

/// A run going on.
struct Run {
    /// When it began, in the idle phase's time.
    began: Duration,
    /// Each stream's messages received when it began.
    received: Vec<u64>,
    /// The disk test, or why it couldn't start.
    write: Result<JoinHandle<io::Result<f64>>, String>,
    /// Over Zenoh, trying the router: where, and how it went.
    router: Option<(String, JoinHandle<Result<(), String>>)>,
}

impl Preflight {
    /// Nothing run yet, for these settings and streams.
    pub(crate) fn new(deployment: &DeploymentConfig, declarations: &[StreamDeclaration]) -> Self {
        let default_size = RecordSettings::default().default_max_message_bytes;
        Self {
            folder: deployment.data_dir.value.clone(),
            needs: Needs {
                stop_below: crate::capture::disk_limits(deployment).stop_below,
                recording_time: deployment.preflight_disk_time.value,
            },
            window: deployment.preflight_window.value,
            streams: declarations
                .iter()
                .map(|declaration| {
                    let size = declaration.max_message_bytes.unwrap_or(default_size);
                    (
                        declaration.id.to_string(),
                        declaration.rate_hz,
                        u64::from(size),
                        declaration.kept_for_late_subscribers(),
                    )
                })
                .collect(),
            run: None,
            waiting: Vec::new(),
            latest: None,
            failures: deployment.preflight_failures.value,
            router: zenoh_router(declarations, |name| std::env::var(name).ok()),
        }
    }

    /// The latest run as an episode started now keeps it, each failed check
    /// whose code is in `waive` marked waived; `None` if none has run.
    ///
    /// # Errors
    ///
    /// `capture.preflight_failed`, naming each failed check not waived, if
    /// failed checks refuse a start.
    pub(crate) fn clear(&self, waive: &[String]) -> Result<Option<Report>, KaptoError> {
        cleared(self.latest.as_ref(), waive, self.failures)
    }

    /// What the latest run found, once one has ended.
    pub(crate) fn latest(&self) -> Option<&Report> {
        self.latest.as_ref()
    }

    /// Begins a run, unless one is going. `asked`, if given, is answered when
    /// it ends.
    pub(crate) fn begin(&mut self, live: &Live<'_>, elapsed: Duration, asked: Option<Pending>) {
        self.waiting.extend(asked);
        if self.run.is_some() {
            return;
        }
        let folder = self.folder.clone();
        let write = std::fs::create_dir_all(&folder)
            .and_then(|()| {
                thread::Builder::new()
                    .name("cap-preflight".into())
                    .spawn(move || write_speed(&folder, WRITE_TEST_BYTES))
            })
            .map_err(|error| error.to_string());
        let router = self.router.clone().and_then(|endpoint| {
            let tried = endpoint.clone();
            thread::Builder::new()
                .name("cap-preflight-router".into())
                .spawn(move || reach(&tried))
                .ok()
                .map(|handle| (endpoint, handle))
        });
        self.run = Some(Run {
            began: elapsed,
            received: live.streams().map(|(_, totals)| totals.received).collect(),
            write,
            router,
        });
    }

    /// Whether a run is going.
    pub(crate) fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Ends the run going once its window is over and the disk test is
    /// done: checks the results, says what failed, and answers whoever
    /// asked. Returns whether it ended.
    pub(crate) fn finish(&mut self, live: &Live<'_>, elapsed: Duration) -> bool {
        let done = self.run.as_ref().is_some_and(|run| {
            elapsed.saturating_sub(run.began) >= self.window
                && run.write.as_ref().map_or(true, JoinHandle::is_finished)
                && run
                    .router
                    .as_ref()
                    .is_none_or(|(_, handle)| handle.is_finished())
        });
        if !done {
            return false;
        }
        let Some(run) = self.run.take() else {
            return false;
        };
        let window = elapsed.saturating_sub(run.began);
        let write_bytes_per_second = match run.write {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the test stopped unexpectedly")))
                .map_err(|error| error.to_string()),
            Err(reason) => Err(format!("can't start the test: {reason}")),
        };
        // Streams are numbered in declaration order.
        let streams = (0..=StreamIndex::MAX)
            .zip(&self.streams)
            .zip(live.streams())
            .zip(&run.received)
            .map(
                |(((index, (stream, declared_hz, size, kept)), (_, totals)), before)| StreamSeen {
                    stream: stream.clone(),
                    declared_hz: *declared_hz,
                    max_message_bytes: *size,
                    received: totals.received.saturating_sub(*before),
                    kept: *kept,
                    holds_kept: live.kept.has(index),
                },
            )
            .collect();
        let measured = Measured {
            window,
            streams,
            free_bytes: kapto_capture::disk::free_bytes(&self.folder).map_err(|e| e.to_string()),
            write_bytes_per_second,
            router: run.router.map(|(endpoint, handle)| RouterSeen {
                endpoint,
                reached: handle
                    .join()
                    .unwrap_or_else(|_| Err("the try stopped unexpectedly".to_owned())),
            }),
        };
        let report = Report {
            ran_at: utc_iso8601(utc_now_ns()),
            window_ms: u64::try_from(window.as_millis()).unwrap_or(u64::MAX),
            checks: evaluate(&measured, self.needs),
        };
        eprintln!("{}", summary(&report));
        for pending in self.waiting.drain(..) {
            pending.answer(Response::Preflight(report.clone()));
        }
        self.latest = Some(report);
        true
    }
}

/// Over Zenoh, where the robot's Zenoh router should answer, as `env` says:
/// the first TCP endpoint of `connect/endpoints` in `ZENOH_CONFIG_OVERRIDE`,
/// else the middleware's default. None when no stream is a ROS 2 topic, the
/// middleware isn't Zenoh, or a session file says where, which isn't read.
fn zenoh_router(
    declarations: &[StreamDeclaration],
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let over_zenoh = env("RMW_IMPLEMENTATION").as_deref() == Some("rmw_zenoh_cpp");
    if !over_zenoh || !declarations.iter().any(|each| each.ros2.is_some()) {
        return None;
    }
    let overridden = env("ZENOH_CONFIG_OVERRIDE").and_then(|text| {
        text.split(';')
            .find_map(|setting| setting.trim().strip_prefix("connect/endpoints="))
            .and_then(|list| {
                list.split(['[', ']', ',', '"', '\''])
                    .map(str::trim)
                    .find(|endpoint| endpoint.starts_with("tcp/"))
                    .map(str::to_owned)
            })
    });
    match overridden {
        Some(endpoint) => Some(endpoint),
        None if env("ZENOH_SESSION_CONFIG_URI").is_some() => None,
        None => Some("tcp/localhost:7447".to_owned()),
    }
}

/// Whether something answers at `endpoint` (`tcp/<host>:<port>`) within a
/// second.
fn reach(endpoint: &str) -> Result<(), String> {
    let address = endpoint.strip_prefix("tcp/").unwrap_or(endpoint);
    let addresses =
        std::net::ToSocketAddrs::to_socket_addrs(address).map_err(|error| error.to_string())?;
    let mut last = "the address names no host".to_owned();
    for each in addresses {
        match std::net::TcpStream::connect_timeout(&each, Duration::from_secs(1)) {
            Ok(_) => return Ok(()),
            Err(error) => last = error.to_string(),
        }
    }
    Err(last)
}

/// `latest` as an episode started with `waive` keeps it; see
/// [`Preflight::clear`].
fn cleared(
    latest: Option<&Report>,
    waive: &[String],
    failures: PreflightFailures,
) -> Result<Option<Report>, KaptoError> {
    let Some(latest) = latest else {
        return Ok(None);
    };
    let mut kept = latest.clone();
    for check in &mut kept.checks {
        check.waived = !check.passed && waive.contains(&check.code);
    }
    let refused: Vec<String> = kept
        .checks
        .iter()
        .filter(|check| !check.passed && !check.waived)
        .map(|check| match &check.stream {
            Some(stream) => format!("{} ({stream})", check.code),
            None => check.code.clone(),
        })
        .collect();
    if refused.is_empty() || failures == PreflightFailures::Warn {
        return Ok(Some(kept));
    }
    Err(
        KaptoError::new(ErrorCode::CapturePreflightFailed).detail(format!(
            "the run at {} failed {}; not waived",
            kept.ran_at,
            refused.join(", ")
        )),
    )
}

/// What pre-flight found, in a line, with a line for each failed check.
pub(crate) fn summary(report: &Report) -> String {
    let failed: Vec<_> = report.checks.iter().filter(|check| !check.passed).collect();
    if failed.is_empty() {
        return format!("Pre-flight: all {} checks passed.", report.checks.len());
    }
    let mut text = format!(
        "Pre-flight: {} of {} checks failed:",
        failed.len(),
        report.checks.len()
    );
    for check in failed {
        let about = check
            .stream
            .as_ref()
            .map_or_else(String::new, |stream| format!(" ({stream})"));
        let _ = write!(
            text,
            "
  {}{about}: {}; needs {}",
            check.code, check.measured, check.needed
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use kapto_store::PreflightCheck;

    use super::*;

    fn check(code: &str, stream: Option<&str>, passed: bool) -> PreflightCheck {
        PreflightCheck {
            code: code.to_owned(),
            stream: stream.map(str::to_owned),
            passed,
            measured: "measured".to_owned(),
            needed: "needed".to_owned(),
            waived: false,
        }
    }

    fn report() -> Report {
        Report {
            ran_at: "2026-03-15T20:00:00.000Z".to_owned(),
            window_ms: 5000,
            checks: vec![
                check("preflight.disk_too_small", None, true),
                check("preflight.disk_too_slow", None, false),
                check("preflight.stream_silent", Some("arm_joints"), false),
            ],
        }
    }

    fn waived(kept: &Report) -> Vec<bool> {
        kept.checks.iter().map(|check| check.waived).collect()
    }

    #[test]
    fn a_failed_check_refuses_the_start_naming_each_one_not_waived() {
        let error = cleared(Some(&report()), &[], PreflightFailures::Refuse).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CapturePreflightFailed);
        assert!(
            error
                .message()
                .contains("preflight.disk_too_slow, preflight.stream_silent (arm_joints)"),
            "{}",
            error.message()
        );
        let waive = ["preflight.stream_silent".to_owned()];
        let error = cleared(Some(&report()), &waive, PreflightFailures::Refuse).unwrap_err();
        assert!(
            !error.message().contains("stream_silent"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn waiving_each_failed_check_starts_with_them_marked_waived_and_only_them() {
        let waive = [
            "preflight.disk_too_slow".to_owned(),
            "preflight.stream_silent".to_owned(),
            // Passed: nothing to waive.
            "preflight.disk_too_small".to_owned(),
        ];
        let kept = cleared(Some(&report()), &waive, PreflightFailures::Refuse)
            .unwrap()
            .unwrap();
        assert_eq!(waived(&kept), [false, true, true]);
    }

    #[test]
    fn with_failures_set_to_warn_it_starts_keeping_what_failed_unwaived() {
        let kept = cleared(Some(&report()), &[], PreflightFailures::Warn)
            .unwrap()
            .unwrap();
        assert_eq!(waived(&kept), [false, false, false]);
        assert_eq!(kept.checks, report().checks);
    }

    #[test]
    fn before_any_run_or_after_one_that_passed_it_starts() {
        assert_eq!(cleared(None, &[], PreflightFailures::Refuse).unwrap(), None);
        let mut passed = report();
        for check in &mut passed.checks {
            check.passed = true;
        }
        let kept = cleared(Some(&passed), &[], PreflightFailures::Refuse).unwrap();
        assert_eq!(kept, Some(passed));
    }

    /// One stream over UDP and, with `ros2`, one ROS 2 topic.
    fn declared(ros2: bool) -> Vec<StreamDeclaration> {
        let mut text =
            "[streams.arm]\nrate_hz = 100\nudp = { bind = \"127.0.0.1:9000\" }\n".to_owned();
        if ros2 {
            text.push_str(
                "[streams.state]\nrate_hz = 100\nros2 = { topic = \"/state\", type = \"std_msgs/msg/String\" }\n",
            );
        }
        kapto_config::parse_streams_with(
            &kapto_config::ConfigFile {
                path: "streams.toml".into(),
                contents: text,
            },
            // `std_msgs/msg/String`'s definition, for the topic's decoder.
            &mut |_, _| Some("string data\n".to_owned()),
        )
        .unwrap()
    }

    /// An environment holding only `pairs`.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn over_zenoh_the_router_tried_is_the_one_its_settings_name_else_the_default() {
        let zenoh = [("RMW_IMPLEMENTATION", "rmw_zenoh_cpp")];
        assert_eq!(
            zenoh_router(&declared(true), env(&zenoh)),
            Some("tcp/localhost:7447".into())
        );
        assert_eq!(
            zenoh_router(
                &declared(true),
                env(&[
                    ("RMW_IMPLEMENTATION", "rmw_zenoh_cpp"),
                    (
                        "ZENOH_CONFIG_OVERRIDE",
                        "mode=\"peer\";connect/endpoints=[\"tcp/192.168.1.20:7447\"]"
                    ),
                ])
            ),
            Some("tcp/192.168.1.20:7447".into())
        );
        // Where it can't be told, and where there is no router to try.
        let file = [
            ("RMW_IMPLEMENTATION", "rmw_zenoh_cpp"),
            ("ZENOH_SESSION_CONFIG_URI", "/robot/zenoh.json5"),
        ];
        assert_eq!(zenoh_router(&declared(true), env(&file)), None);
        assert_eq!(zenoh_router(&declared(false), env(&zenoh)), None);
        assert_eq!(
            zenoh_router(
                &declared(true),
                env(&[("RMW_IMPLEMENTATION", "rmw_cyclonedds_cpp")])
            ),
            None
        );
    }

    #[test]
    fn a_router_answers_where_something_listens_and_not_where_nothing_does() {
        let listening = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listening.local_addr().unwrap();
        assert_eq!(reach(&format!("tcp/{open}")), Ok(()));
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(reach(&format!("tcp/{closed}")).is_err());
    }
}
