// SPDX-License-Identifier: Apache-2.0

//! A synthetic robot: sends streams over UDP, TCP or ROS 2 at a fixed rate
//! so Kapto can be demonstrated, tested and benchmarked without hardware.
//!
//! Stream `i` sends to the target port plus `i`, one message per tick, in the
//! layout documented in [`payload`]; over TCP each message is framed as
//! [`output`] describes. Over ROS 2 (the `ros2` feature) stream `i` publishes
//! on its own topic ([`Config::stream_topic`]) instead, as `ros2` describes. Ticks follow absolute deadlines
//! ([`pacing::Schedule`]), and the generator measures how late each tick
//! started, so a benchmark can trust the load it was given.

pub mod camera;
pub mod faults;
pub mod output;
pub mod pacing;
pub mod payload;
#[cfg(feature = "ros2")]
mod ros2;

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use faults::Faults;
use output::Output;
pub use output::Transport;
use pacing::{Histogram, Percentiles, Schedule, wait_until};
use payload::Sample;

/// If the generator falls this far behind (for example after being paused),
/// it skips the missed ticks instead of sending them in a burst.
const MAX_BEHIND: Duration = Duration::from_millis(100);

/// What to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Address and port of the first stream; stream `i` uses port + `i`.
    pub target: SocketAddr,
    /// Number of streams.
    pub streams: u16,
    /// Messages per second on each stream.
    pub rate_hz: u32,
    /// How long to run, or until stopped.
    pub duration: Option<Duration>,
    /// How long before each deadline to stop sleeping and spin: more is
    /// more punctual and uses more CPU (see [`pacing::wait_until`]).
    pub spin: Duration,
    /// UDP, TCP and which side connects, or ROS 2.
    pub transport: Transport,
    /// Over ROS 2, how long to wait before the first message for every
    /// topic to have a subscriber, so none is published before anyone
    /// can receive it; `None` starts at once.
    pub wait_for_subscribers: Option<Duration>,
    /// A camera's JPEG frames of this width and height instead of joint
    /// states, over TCP or ROS 2 (`sensor_msgs/msg/CompressedImage`).
    pub camera: Option<(u16, u16)>,
}

impl Config {
    /// Checks the configuration, explaining the first problem found.
    ///
    /// # Errors
    ///
    /// A message saying which value is wrong and what is accepted.
    pub fn validate(&self) -> Result<(), String> {
        if self.streams == 0 {
            return Err("--streams must be at least 1".to_owned());
        }
        if !(1..=1_000_000).contains(&self.rate_hz) {
            return Err("--rate must be from 1 to 1000000 messages per second".to_owned());
        }
        if self.target.port() == 0 {
            return Err("--target needs a port other than 0".to_owned());
        }
        if u32::from(self.target.port()) + u32::from(self.streams) - 1 > u32::from(u16::MAX) {
            return Err(format!(
                "--streams {} from port {} would go past port 65535",
                self.streams,
                self.target.port()
            ));
        }
        if self.duration == Some(Duration::ZERO) {
            return Err("--duration must be longer than 0 seconds".to_owned());
        }
        if let Some((width, height)) = self.camera {
            if self.transport == Transport::Udp {
                return Err(
                    "--camera sends over TCP or ROS 2: a frame is larger than a UDP datagram can be"
                        .to_owned(),
                );
            }
            if width < 8 || height < 8 {
                return Err("--camera needs a frame of at least 8x8".to_owned());
            }
        }
        if self.wait_for_subscribers.is_some() && self.transport != Transport::Ros2 {
            return Err("--wait-for-subscribers is only for --transport ros2".to_owned());
        }
        Ok(())
    }

    /// The address stream `stream` sends to.
    #[must_use]
    pub fn stream_target(&self, stream: u16) -> SocketAddr {
        SocketAddr::new(self.target.ip(), self.target.port() + stream)
    }

    /// The ROS 2 topic stream `stream` publishes on: `/joint_states` for the
    /// first, as a robot's arm would, then `/joint_states_1` and on; a
    /// camera's on `/camera/image_raw/compressed`, then
    /// `/camera_1/image_raw/compressed` and on, as `image_transport` names
    /// them.
    #[must_use]
    pub fn stream_topic(&self, stream: u16) -> String {
        match (self.camera, stream) {
            (None, 0) => "/joint_states".to_owned(),
            (None, _) => format!("/joint_states_{stream}"),
            (Some(_), 0) => "/camera/image_raw/compressed".to_owned(),
            (Some(_), _) => format!("/camera_{stream}/image_raw/compressed"),
        }
    }

    /// Ticks in the configured duration, if there is one.
    #[must_use]
    pub fn total_ticks(&self) -> Option<u64> {
        self.duration.map(|duration| {
            let ticks = duration.as_nanos() * u128::from(self.rate_hz) / 1_000_000_000;
            u64::try_from(ticks).unwrap_or(u64::MAX)
        })
    }
}

/// Counts for a stretch of time, either one reporting interval or the whole run.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Ticks sent.
    pub ticks: u64,
    /// Messages sent (ticks × streams).
    pub messages: u64,
    /// Messages the operating system refused to send, or that had no TCP
    /// connection to go on.
    pub send_errors: u64,
    /// Ticks that started more than half a period after their deadline.
    pub late_ticks: u64,
    /// Ticks skipped because the generator fell too far behind.
    pub skipped_ticks: u64,
    /// How late ticks started after their deadline.
    pub lateness: Histogram,
}

impl Stats {
    /// Lateness percentiles.
    #[must_use]
    pub fn lateness(&self) -> Percentiles {
        self.lateness.percentiles()
    }
}

/// Sends until the duration ends, calling `report` once per second of
/// schedule time with that second's counts, and returns the totals with the
/// wall-clock time the run took.
///
/// # Errors
///
/// Only if no UDP socket can be opened, a TCP port listened on, or a ROS 2
/// node started and its topics published on, or a topic still has no
/// subscriber once [`Config::wait_for_subscribers`] is over. Failed
/// sends are counted, not fatal, as they would be for a real robot.
pub fn run(config: &Config, report: impl FnMut(Duration, &Stats)) -> io::Result<(Stats, Duration)> {
    run_until(config, &AtomicBool::new(false), report)
}

/// Like [`run`], but also stops, within one tick, once `stop` is set.
///
/// # Errors
///
/// As [`run`].
pub fn run_until(
    config: &Config,
    stop: &AtomicBool,
    report: impl FnMut(Duration, &Stats),
) -> io::Result<(Stats, Duration)> {
    run_with_faults(config, stop, &Faults::new(), report)
}

/// Like [`run_until`], misbehaving whenever `faults` is told to.
///
/// # Errors
///
/// As [`run`].
pub fn run_with_faults(
    config: &Config,
    stop: &AtomicBool,
    faults: &Faults,
    mut report: impl FnMut(Duration, &Stats),
) -> io::Result<(Stats, Duration)> {
    let frames = config
        .camera
        .map(|(width, height)| camera::Frames::new(width, height, config.rate_hz))
        .transpose()?;
    let mut output = Output::open(config)?;
    let total_ticks = config.total_ticks();

    let start = Instant::now();
    let schedule = Schedule::new(start, config.rate_hz);
    let half_period = schedule.offset(1) / 2;
    let mut total = Stats::default();
    let mut interval = Stats::default();
    let mut next_report_second = 1;
    let mut seq: u32 = 0;
    let mut tick = 0;

    while total_ticks.is_none_or(|total_ticks| tick < total_ticks) && !stop.load(Ordering::Relaxed)
    {
        let deadline = schedule.deadline(tick);
        if Instant::now() > deadline + MAX_BEHIND {
            let caught_up = ticks_elapsed(start.elapsed(), config.rate_hz) + 1;
            let caught_up = total_ticks.map_or(caught_up, |total_ticks| caught_up.min(total_ticks));
            let skipped = caught_up.saturating_sub(tick);
            total.skipped_ticks += skipped;
            interval.skipped_ticks += skipped;
            tick = caught_up;
            continue;
        }
        wait_until(deadline, config.spin);
        let lateness = Instant::now().saturating_duration_since(deadline);

        let seconds = schedule.offset(tick).as_secs_f64();
        let source_time_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        let mut errors = 0;
        for stream in 0..config.streams {
            if let Some(frames) = &frames {
                let frame = frames.frame(stream, tick);
                if !output.send_frame(usize::from(stream), frame, source_time_ns) {
                    errors += 1;
                }
                continue;
            }
            let mut joints = payload::joints_at(stream, seconds);
            if !faults.shape(stream, seconds, &mut joints) {
                continue;
            }
            let sample = Sample {
                seq,
                source_time_ns,
                joints,
                gripper: payload::gripper_at(stream, seconds),
            };
            if !output.send(usize::from(stream), &sample) {
                errors += 1;
            }
        }
        seq = seq.wrapping_add(1);

        for stats in [&mut total, &mut interval] {
            stats.ticks += 1;
            stats.messages += u64::from(config.streams);
            stats.send_errors += errors;
            stats.late_ticks += u64::from(lateness > half_period);
            stats.lateness.record(lateness);
        }
        tick += 1;

        if schedule.offset(tick) >= Duration::from_secs(next_report_second) {
            report(Duration::from_secs(next_report_second), &interval);
            interval = Stats::default();
            next_report_second += 1;
        }
    }
    Ok((total, start.elapsed()))
}

/// Whole ticks that fit in `elapsed` at `rate_hz`.
fn ticks_elapsed(elapsed: Duration, rate_hz: u32) -> u64 {
    let ticks = elapsed.as_nanos() * u128::from(rate_hz) / 1_000_000_000;
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_without_an_end_stops_soon_after_it_is_told_to() {
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let config = Config {
            target: receiver.local_addr().unwrap(),
            streams: 1,
            rate_hz: 1_000,
            duration: None,
            spin: Duration::ZERO,
            transport: Transport::Udp,
            wait_for_subscribers: None,
            camera: None,
        };
        let stop = AtomicBool::new(false);
        let (total, elapsed) = std::thread::scope(|scope| {
            let running = scope.spawn(|| run_until(&config, &stop, |_, _| {}).unwrap());
            std::thread::sleep(Duration::from_millis(200));
            stop.store(true, Ordering::Relaxed);
            running.join().unwrap()
        });
        assert!(total.messages > 0);
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    fn config(target: &str, streams: u16, rate_hz: u32) -> Config {
        Config {
            target: target.parse().unwrap(),
            streams,
            rate_hz,
            duration: Some(Duration::from_secs(1)),
            spin: Duration::ZERO,
            transport: Transport::Udp,
            wait_for_subscribers: None,
            camera: None,
        }
    }

    #[cfg(not(feature = "ros2"))]
    #[test]
    fn ros_2_without_the_feature_says_it_wasnt_built_in() {
        let mut config = config("127.0.0.1:9000", 1, 100);
        config.transport = Transport::Ros2;
        let error = run(&config, |_, _| {}).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("without ROS 2"), "{error}");
    }

    #[test]
    fn streams_use_consecutive_ports() {
        let config = config("127.0.0.1:9000", 3, 100);
        assert_eq!(config.stream_target(0), "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.stream_target(2), "127.0.0.1:9002".parse().unwrap());
    }

    #[test]
    fn the_first_stream_publishes_on_joint_states_and_the_others_after_it() {
        let config = config("127.0.0.1:9000", 3, 100);
        assert_eq!(config.stream_topic(0), "/joint_states");
        assert_eq!(config.stream_topic(2), "/joint_states_2");
    }

    #[test]
    fn a_camera_publishes_on_its_compressed_image_topic() {
        let mut config = config("127.0.0.1:9000", 2, 30);
        config.camera = Some((640, 480));
        assert_eq!(config.stream_topic(0), "/camera/image_raw/compressed");
        assert_eq!(config.stream_topic(1), "/camera_1/image_raw/compressed");
    }

    #[test]
    fn duration_gives_an_exact_number_of_ticks() {
        let mut config = config("127.0.0.1:9000", 1, 2_000);
        config.duration = Some(Duration::from_millis(2_500));
        assert_eq!(config.total_ticks(), Some(5_000));
        config.duration = None;
        assert_eq!(config.total_ticks(), None);
    }

    #[test]
    fn invalid_configurations_explain_what_is_wrong() {
        let cases = [
            (
                config("127.0.0.1:9000", 0, 100),
                "--streams must be at least 1",
            ),
            (
                config("127.0.0.1:9000", 1, 0),
                "--rate must be from 1 to 1000000",
            ),
            (
                config("127.0.0.1:0", 1, 100),
                "--target needs a port other than 0",
            ),
            (
                config("127.0.0.1:65535", 2, 100),
                "would go past port 65535",
            ),
            (
                Config {
                    camera: Some((640, 480)),
                    ..config("127.0.0.1:9000", 1, 30)
                },
                "--camera sends over TCP or ROS 2",
            ),
            (
                Config {
                    camera: Some((4, 480)),
                    transport: Transport::TcpConnect,
                    ..config("127.0.0.1:9000", 1, 30)
                },
                "--camera needs a frame of at least 8x8",
            ),
            (
                Config {
                    wait_for_subscribers: Some(Duration::from_secs(1)),
                    ..config("127.0.0.1:9000", 1, 100)
                },
                "--wait-for-subscribers is only for --transport ros2",
            ),
        ];
        for (config, expected) in cases {
            let error = config.validate().unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
        assert!(config("127.0.0.1:65534", 2, 100).validate().is_ok());
    }

    #[test]
    fn whole_ticks_are_counted_from_elapsed_time() {
        assert_eq!(ticks_elapsed(Duration::from_millis(1), 2_000), 2);
        assert_eq!(ticks_elapsed(Duration::from_micros(499), 2_000), 0);
    }
}
