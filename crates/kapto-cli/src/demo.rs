// SPDX-License-Identifier: Apache-2.0

//! `kapto demo`: Kapto with a robot to show, in one command. The sample
//! seven-joint arm (`packages/arm7`, built into the binary) is written to the
//! data directory, `kapto serve` runs with it, and a synthetic robot inside
//! this process sends the arm's joint states over UDP on this machine.

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use clap::Args;
use kapto_core::{ErrorCode, KaptoError};
use kapto_loadgen::{Config, Transport, run_until};

use crate::SettingFlags;
use crate::serve::{self, ServeArgs};
use crate::station::Outcome;

/// The sample arm's files, as (path inside its folder, contents).
const ARM7: &[(&str, &str)] = &[
    (
        "package.toml",
        include_str!("../../../packages/arm7/package.toml"),
    ),
    (
        "urdf/arm7.urdf",
        include_str!("../../../packages/arm7/urdf/arm7.urdf"),
    ),
    (
        "streams.toml",
        include_str!("../../../packages/arm7/streams.toml"),
    ),
    (
        "mapping.toml",
        include_str!("../../../packages/arm7/mapping.toml"),
    ),
    ("qa.toml", include_str!("../../../packages/arm7/qa.toml")),
];

/// Where the sample arm's stream is received, as its streams.toml says.
const ARM7_ADDRESS: &str = "127.0.0.1:9000";
/// The sample arm's declared rate, as its streams.toml writes it.
const ARM7_RATE: &str = "rate_hz = 2000";

/// Arguments of `kapto demo`.
#[derive(Args)]
pub(crate) struct DemoArgs {
    /// UDP port the synthetic robot sends to and Kapto receives on.
    #[arg(long, value_name = "PORT", default_value_t = 9000)]
    robot_port: u16,
    /// Messages per second the synthetic robot sends.
    #[arg(long, value_name = "HZ", default_value_t = 2_000, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    rate: u32,
}

pub(crate) fn demo(flags: &SettingFlags, args: &DemoArgs) -> ExitCode {
    match run(flags, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

fn run(flags: &SettingFlags, args: &DemoArgs) -> Outcome<()> {
    let (_, deployment) = crate::config::load(flags)?;
    let folder = deployment.data_dir.value.join("demo/arm7");
    write_arm7(&folder, args.robot_port, args.rate).map_err(|error| vec![error])?;
    eprintln!(
        "Demo: a synthetic seven-joint arm sends its joint positions at {} Hz to UDP port {}. Its files are in {}.",
        args.rate,
        args.robot_port,
        folder.display()
    );

    let robot = Config {
        target: ([127, 0, 0, 1], args.robot_port).into(),
        streams: 1,
        rate_hz: args.rate,
        duration: None,
        // Sleeping to each tick rather than spinning: the demo shouldn't keep
        // a CPU core busy, and the arm doesn't need microsecond timing.
        spin: Duration::ZERO,
        transport: Transport::Udp,
        wait_for_subscribers: None,
        camera: None,
    };
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let sending = thread::Builder::new()
            .name("demo-robot".into())
            .spawn_scoped(scope, || run_until(&robot, &stop, |_, _| {}))
            .map_err(|error| {
                vec![
                    KaptoError::new(ErrorCode::CaptureThreadFailed)
                        .detail(format!("demo-robot: {error}")),
                ]
            })?;
        let served = serve::run(
            flags,
            &ServeArgs {
                robot: folder.join("urdf/arm7.urdf"),
                streams: folder.join("streams.toml"),
                mapping: folder.join("mapping.toml"),
            },
        );
        stop.store(true, Ordering::Relaxed);
        if let Ok(Err(error)) = sending.join() {
            eprintln!("warning: the synthetic robot stopped: {error}");
        }
        served
    })
}

/// Writes the sample arm into `folder`, receiving on `port` at `rate_hz`,
/// the rate the synthetic robot sends at, so the stream's declared rate is
/// the one it arrives at.
pub(crate) fn write_arm7(folder: &Path, port: u16, rate_hz: u32) -> Result<(), KaptoError> {
    for (path, contents) in ARM7 {
        let path = folder.join(path);
        let contents = if path.ends_with("streams.toml") {
            contents
                .replace(ARM7_ADDRESS, &format!("127.0.0.1:{port}"))
                .replace(ARM7_RATE, &format!("rate_hz = {rate_hz}"))
        } else {
            (*contents).to_owned()
        };
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, contents))
            .map_err(|error| {
                KaptoError::new(ErrorCode::CaptureWriteFailed)
                    .detail(format!("can't write the demo robot's files: {error}"))
                    .context("path", path.display().to_string())
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sample_arms_stream_is_received_where_and_at_the_rate_the_demo_robot_sends() {
        let (_, streams) = ARM7
            .iter()
            .find(|(path, _)| *path == "streams.toml")
            .unwrap();
        assert_eq!(streams.matches(ARM7_ADDRESS).count(), 1);
        assert_eq!(streams.matches(ARM7_RATE).count(), 1);
    }
}
