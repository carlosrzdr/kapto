// SPDX-License-Identifier: Apache-2.0

//! `kapto demo`: Kapto with a robot to show, in one command. The sample
//! seven-joint arm (`packages/arm7`, built into the binary) is written to the
//! data directory, `kapto serve` runs with it, and a synthetic robot inside
//! this process sends the arm's joint states over UDP on this machine, and
//! a wrist camera's JPEG pictures over TCP beside them.

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use clap::Args;
use kapto_core::{ErrorCode, KaptoError};
use kapto_loadgen::{Config, Transport, prepare, run_until};

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

/// The synthetic wrist camera's pictures: small enough for any machine to
/// record and show, large enough to see the arm's motion in.
const CAMERA_SIZE: (u16, u16) = (640, 480);
/// Its frames a second.
const CAMERA_FPS: u32 = 30;

/// Arguments of `kapto demo`.
#[derive(Args)]
pub(crate) struct DemoArgs {
    /// UDP port the synthetic robot sends to and Kapto receives on.
    #[arg(long, value_name = "PORT", default_value_t = 9000)]
    robot_port: u16,
    /// Messages per second the synthetic robot sends.
    #[arg(long, value_name = "HZ", default_value_t = 2_000, value_parser = clap::value_parser!(u32).range(1..=100_000))]
    rate: u32,
    /// Leave out the synthetic wrist camera, whose pictures (640x480 JPEG,
    /// 30 a second) Kapto receives over TCP on the port after the robot's.
    #[arg(long)]
    no_camera: bool,
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
    let camera_port = (!args.no_camera).then(|| args.robot_port.wrapping_add(1));
    write_arm7(&folder, args.robot_port, args.rate, camera_port).map_err(|error| vec![error])?;
    eprintln!(
        "Demo: a synthetic seven-joint arm sends its joint positions at {} Hz to UDP port {}{}. Its files are in {}.",
        args.rate,
        args.robot_port,
        camera_port.map_or_else(String::new, |port| format!(
            ", and its wrist camera {}x{} JPEG pictures {CAMERA_FPS} a second to TCP port {port}",
            CAMERA_SIZE.0, CAMERA_SIZE.1
        )),
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
    // Kapto listens for the camera, which connects again until it does.
    let camera = camera_port.map(|port| Config {
        target: ([127, 0, 0, 1], port).into(),
        streams: 1,
        rate_hz: CAMERA_FPS,
        duration: None,
        spin: Duration::ZERO,
        transport: Transport::TcpConnect,
        wait_for_subscribers: None,
        camera: Some(CAMERA_SIZE),
    });
    let thread_failed = |name: &str, error: &dyn std::fmt::Display| {
        vec![KaptoError::new(ErrorCode::CaptureThreadFailed).detail(format!("{name}: {error}"))]
    };
    // Its pictures are encoded before Kapto starts, so it sends from the
    // moment Kapto listens and pre-flight finds it at its rate.
    let camera = camera
        .as_ref()
        .map(prepare)
        .transpose()
        .map_err(|error| thread_failed("demo-camera", &error))?;
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let stop = &stop;
        let sending = thread::Builder::new()
            .name("demo-robot".into())
            .spawn_scoped(scope, || run_until(&robot, stop, |_, _| {}))
            .map_err(|error| thread_failed("demo-robot", &error))?;
        let filming = camera
            .map(|camera| {
                thread::Builder::new()
                    .name("demo-camera".into())
                    .spawn_scoped(scope, move || camera.run_until(stop, |_, _| {}))
                    .map_err(|error| thread_failed("demo-camera", &error))
            })
            .transpose()?;
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
        if let Some(Ok(Err(error))) = filming.map(thread::ScopedJoinHandle::join) {
            eprintln!("warning: the synthetic camera stopped: {error}");
        }
        served
    })
}

/// Writes the sample arm into `folder`, receiving on `port` at `rate_hz`,
/// the rate the synthetic robot sends at, so the stream's declared rate is
/// the one it arrives at; and, given `camera_port`, a wrist camera received
/// on it over TCP.
pub(crate) fn write_arm7(
    folder: &Path,
    port: u16,
    rate_hz: u32,
    camera_port: Option<u16>,
) -> Result<(), KaptoError> {
    for (path, contents) in ARM7 {
        let path = folder.join(path);
        let contents = if path.ends_with("streams.toml") {
            let mut streams = contents
                .replace(ARM7_ADDRESS, &format!("127.0.0.1:{port}"))
                .replace(ARM7_RATE, &format!("rate_hz = {rate_hz}"));
            if let Some(camera) = camera_port {
                streams.push_str(&camera_stream(camera));
            }
            streams
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

/// The wrist camera's declaration: its JPEG pictures, each length-prefixed
/// as the synthetic camera sends them, received on `port`.
fn camera_stream(port: u16) -> String {
    format!(
        "\n# The demo's synthetic wrist camera: {}x{} JPEG pictures, {CAMERA_FPS} a second.\n\
         [streams.wrist_camera]\n\
         rate_hz = {CAMERA_FPS}\n\
         max_message_bytes = 1048576\n\
         tcp = {{ listen = \"127.0.0.1:{port}\", framing = {{ kind = \"length_prefix\", length_bytes = 4 }} }}\n\
         camera = {{ format = \"jpeg\" }}\n",
        CAMERA_SIZE.0, CAMERA_SIZE.1
    )
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

    #[test]
    fn the_wrist_camera_is_declared_a_jpeg_camera_on_its_port() {
        let folder = std::env::temp_dir().join(format!("kapto-demo-camera-{}", std::process::id()));
        write_arm7(&folder, 9100, 500, Some(9101)).unwrap();
        let file = kapto_config::ConfigFile {
            path: folder.join("streams.toml"),
            contents: std::fs::read_to_string(folder.join("streams.toml")).unwrap(),
        };
        let streams = kapto_config::parse_streams(&file).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].rate_hz, Some(500));
        let camera = &streams[1];
        assert_eq!(camera.id.as_str(), "wrist_camera");
        assert_eq!(camera.camera, Some(kapto_config::CameraDeclaration::Jpeg));
        assert_eq!(camera.rate_hz, Some(CAMERA_FPS));
        let tcp = camera.tcp.as_ref().unwrap();
        assert_eq!(
            tcp.endpoint,
            kapto_config::TcpEndpointDeclaration::Listen("127.0.0.1:9101".parse().unwrap())
        );
    }
}
