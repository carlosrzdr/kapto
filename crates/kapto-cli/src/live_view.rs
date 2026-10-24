// SPDX-License-Identifier: Apache-2.0

//! Runs `kapto viz-bridge`, the robot moving live in the web UI's live
//! view. Kept apart from the command's definition, so tools that only read
//! the definitions build without the viewer.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use kapto_core::{ClockSample, ErrorCode, KaptoError};
use kapto_viz::camera_history::CameraHistorySettings;
use kapto_viz::picture::CameraKind;
use kapto_viz::{Bridge, BridgeSettings, LiveRobot, Plots, StreamFields};

use crate::SettingFlags;
use crate::viz_bridge::{Prepared, VizBridgeArgs, prepare, warning};

pub(crate) fn viz_bridge(flags: &SettingFlags, args: &VizBridgeArgs) -> ExitCode {
    match run(flags, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            let reports: Vec<String> = errors.iter().map(KaptoError::report).collect();
            eprintln!("{}", reports.join("\n\n"));
            ExitCode::FAILURE
        }
    }
}

fn run(flags: &SettingFlags, args: &VizBridgeArgs) -> Result<(), Vec<KaptoError>> {
    // SIGTERM, once serve has ended, ends the bridge at once: it has
    // nothing to finish.
    if let Some(parent) = args.parent
        && let Err(error) = crate::parent::end_with(parent)
    {
        eprintln!("The viewer bridge won't end with serve: {error}");
    }
    let (_, deployment) = crate::config::load(flags)?;
    let prepared = prepare(
        &args.robot,
        &args.streams,
        &args.mapping,
        &deployment.packages_dir.value,
    )?;
    for problem in prepared.warnings() {
        eprintln!("{}\n", warning(problem));
    }
    let Prepared {
        robot,
        kinematics,
        map,
        streams,
        ..
    } = prepared;
    let cameras: Vec<(u16, CameraKind)> = (0..=u16::MAX)
        .zip(&streams)
        .filter_map(|(stream, declaration)| Some((stream, declaration.camera_kind()?)))
        .collect();
    let clock = ClockSample::now();
    let live = LiveRobot::new(&robot, kinematics, &map, streams.len(), clock);
    let plots = Plots::new(&StreamFields::of(&streams));

    let mut settings = BridgeSettings::new(
        deployment.server_bind.value,
        (
            deployment.viewer_stream_port.value,
            deployment.server_port.value,
        ),
        deployment.tap_socket.value.clone(),
    );
    settings.control_socket = Some(deployment.control_socket.value.clone());
    settings.frame_socket = Some(deployment.frame_socket.value.clone());
    settings.cameras = cameras;
    settings.display_interval =
        Duration::from_secs(1) / deployment.viewer_display_rate.value.max(1);
    settings.history_bytes = deployment.viewer_history_mib.value * 1024 * 1024;
    settings.history_age = deployment.viewer_history_age.value;
    settings.camera_history = CameraHistorySettings {
        every: Duration::from_secs(1) / deployment.viewer_camera_history_fps.value.max(1),
        width: deployment.viewer_camera_history_width.value,
        bytes: usize::try_from(deployment.viewer_camera_history_mib.value * 1024 * 1024)
            .unwrap_or(usize::MAX),
        // The bridge keeps them as long as the rest of its history.
        age_ns: i64::MAX,
    };
    let mut bridge = Bridge::start(&settings, &robot, live, &plots).map_err(|error| vec![error])?;
    println!(
        "Streaming {} to the live view on port {}",
        robot.name,
        bridge.stream_address().port()
    );
    println!(
        "Live samples from capture: {}",
        settings.tap_socket.display()
    );
    if !settings.cameras.is_empty() {
        println!(
            "Cameras' frames from capture: {}",
            deployment.frame_socket.value.display()
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    if let Some(seconds) = args.duration {
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("viz-timer".into())
            .spawn(move || {
                thread::sleep(Duration::from_secs(u64::from(seconds)));
                stop.store(true, Ordering::Relaxed);
            })
            .map_err(|error| {
                vec![KaptoError::new(ErrorCode::CaptureThreadFailed).detail(error.to_string())]
            })?;
    }
    bridge.run(&stop).map_err(|error| vec![error])?;
    let stats = bridge.tap_stats();
    println!(
        "Read {} live samples; skipped {} of another version and {} unreadable",
        stats.samples, stats.other_version, stats.unreadable
    );
    if let Some(cameras) = bridge.camera_totals() {
        let stream = bridge.stream_stats();
        println!(
            "Read {} cameras' frames: {} shown, {} that can't be shown, {} connections that sent something else; pictures sent to pages {}, missed by slow pages {}",
            cameras.frames,
            cameras.shown,
            cameras.unshown,
            cameras.unreadable,
            stream.pictures_sent,
            stream.pictures_dropped
        );
    }
    Ok(())
}
