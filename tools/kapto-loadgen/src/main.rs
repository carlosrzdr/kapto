// SPDX-License-Identifier: Apache-2.0

//! `kapto-loadgen`: sends synthetic robot streams over UDP, TCP or ROS 2.

use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use kapto_loadgen::{Config, Stats, Transport, run};

/// Sends synthetic robot streams over UDP, TCP or ROS 2 at a fixed rate, and
/// reports how accurately it kept to the schedule.
#[derive(Parser)]
#[command(name = "kapto-loadgen", version)]
struct Cli {
    /// Address or host name, and port, of the first stream; stream i uses
    /// the port plus i.
    #[arg(long, default_value = "127.0.0.1:9000")]
    target: String,
    /// Number of streams.
    #[arg(long, default_value_t = 1)]
    streams: u16,
    /// Messages per second on each stream.
    #[arg(long, default_value_t = 2_000)]
    rate: u32,
    /// Seconds to run (fractions allowed); runs until stopped if omitted.
    #[arg(long, value_name = "SECONDS")]
    duration: Option<f64>,
    /// Microseconds before each tick to stop sleeping and spin. More is more
    /// punctual and uses more CPU.
    #[arg(long, value_name = "MICROSECONDS", default_value_t = 100)]
    spin: u64,
    /// udp; tcp-connect to connect to each stream's port, where Kapto
    /// listens; tcp-listen to listen there for Kapto to connect. Over TCP
    /// each message follows its length as 4 bytes little-endian. ros2 to
    /// publish `sensor_msgs/msg/JointState` on `/joint_states`, then
    /// `/joint_states_1` and on, ignoring --target (only in a build with
    /// ROS 2).
    #[arg(long, value_enum, default_value_t = TransportArg::Udp)]
    transport: TransportArg,
    /// With --transport ros2, wait up to this long for every topic to have
    /// a subscriber before the first message, and fail if one still has
    /// none, so nothing is published before it can be received.
    #[arg(long, value_name = "SECONDS")]
    wait_for_subscribers: Option<f64>,
    /// Send a camera's JPEG frames of this size, e.g. 640x480, instead of
    /// joint states: each message one frame after its length over TCP, or
    /// a `sensor_msgs/msg/CompressedImage` on `/camera/image_raw/compressed`
    /// over ROS 2. Not over UDP, which a frame doesn't fit.
    #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = frame_size)]
    camera: Option<(u16, u16)>,
    /// With --camera, send each frame's pixels in rgb8, uncompressed,
    /// instead of JPEG: over ROS 2 a `sensor_msgs/msg/Image` on
    /// `/camera/image_raw`.
    #[arg(long)]
    raw: bool,
}

/// A frame size written `WIDTHxHEIGHT`.
fn frame_size(text: &str) -> Result<(u16, u16), String> {
    let (width, height) = text
        .split_once('x')
        .ok_or_else(|| format!("`{text}` isn't WIDTHxHEIGHT, e.g. 640x480"))?;
    let side = |side: &str| {
        side.parse::<u16>()
            .map_err(|_| format!("`{side}` isn't a number of pixels from 1 to 65535"))
    };
    Ok((side(width)?, side(height)?))
}

#[derive(Clone, Copy, ValueEnum)]
enum TransportArg {
    Udp,
    TcpConnect,
    TcpListen,
    Ros2,
}

impl From<TransportArg> for Transport {
    fn from(transport: TransportArg) -> Self {
        match transport {
            TransportArg::Udp => Self::Udp,
            TransportArg::TcpConnect => Self::TcpConnect,
            TransportArg::TcpListen => Self::TcpListen,
            TransportArg::Ros2 => Self::Ros2,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Ok(duration) = cli.duration.map(Duration::try_from_secs_f64).transpose() else {
        eprintln!("--duration must be a number of seconds, 0 or more");
        return ExitCode::FAILURE;
    };
    let Ok(wait_for_subscribers) = cli
        .wait_for_subscribers
        .map(Duration::try_from_secs_f64)
        .transpose()
    else {
        eprintln!("--wait-for-subscribers must be a number of seconds, 0 or more");
        return ExitCode::FAILURE;
    };
    let Some(target) = resolve(&cli.target) else {
        eprintln!(
            "--target {} isn't an address and port, or the name didn't resolve within {} s",
            cli.target,
            RESOLVE_FOR.as_secs()
        );
        return ExitCode::FAILURE;
    };
    let config = Config {
        target,
        streams: cli.streams,
        rate_hz: cli.rate,
        duration,
        spin: Duration::from_micros(cli.spin),
        transport: cli.transport.into(),
        wait_for_subscribers,
        camera: cli.camera,
        raw_camera: cli.raw,
    };
    if let Err(problem) = config.validate() {
        eprintln!("{problem}");
        return ExitCode::FAILURE;
    }

    let last = config.streams - 1;
    let to = if config.transport == Transport::Ros2 {
        format!(
            "topics {}–{}",
            config.stream_topic(0),
            config.stream_topic(last)
        )
    } else {
        format!(
            "{} (ports {}–{})",
            config.target.ip(),
            config.target.port(),
            config.stream_target(last).port()
        )
    };
    let what = config.camera.map_or_else(String::new, |(width, height)| {
        let format = if config.raw_camera { "rgb8" } else { "JPEG" };
        format!(" of {width}x{height} {format} frames")
    });
    println!(
        "Sending {} stream(s){what} at {} Hz over {} to {to}",
        config.streams,
        config.rate_hz,
        config.transport.name(),
    );
    match run(&config, |at, stats| {
        println!("{:>6} s  {}", at.as_secs(), line(stats));
    }) {
        Ok((total, elapsed)) => {
            let rate = u128::from(total.messages) * 1_000_000_000 / elapsed.as_nanos().max(1);
            println!(
                "total   {}  over {:.3} s = {rate} msg/s",
                line(&total),
                elapsed.as_secs_f64()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Can't open the {} output: {error}", config.transport.name());
            ExitCode::FAILURE
        }
    }
}

/// How long a host name may take to resolve: in Docker Compose the robot
/// can start before the container it sends to has a name.
const RESOLVE_FOR: Duration = Duration::from_secs(30);

/// The target's first IPv4 address, or IPv6 if it has none.
fn resolve(target: &str) -> Option<SocketAddr> {
    let deadline = Instant::now() + RESOLVE_FOR;
    loop {
        if let Ok(addresses) = target.to_socket_addrs() {
            let addresses: Vec<SocketAddr> = addresses.collect();
            let first = addresses.iter().find(|address| address.is_ipv4());
            if let Some(address) = first.or(addresses.first()) {
                return Some(*address);
            }
        }
        // A plain address that doesn't parse won't parse later either.
        if target.parse::<SocketAddr>().is_err() && !target.contains(':') {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// One line of counts and lateness.
fn line(stats: &Stats) -> String {
    let lateness = stats.lateness();
    format!(
        "{} msg  late {}  skipped {}  send errors {}  lateness p50 {} µs  p99 {} µs  max {} µs",
        stats.messages,
        stats.late_ticks,
        stats.skipped_ticks,
        stats.send_errors,
        lateness.p50.as_micros(),
        lateness.p99.as_micros(),
        lateness.max.as_micros()
    )
}
