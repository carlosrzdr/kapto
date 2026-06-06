// SPDX-License-Identifier: Apache-2.0

//! `kapto export`: an episode written in another tool's format. For now,
//! `rosbag2`: its ROS 2 topics as a bag ROS 2's own tools play back on the
//! robot's topics. A recording can't be played as it is, as rosbag2 refuses
//! a bag whose channels have more than one encoding, and a recording also
//! holds Kapto's own JSON channel and any UDP or TCP streams.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use kapto_capture::reader::read_recording;
use kapto_core::{ErrorCode, KaptoError};
use kapto_decode::{CDR_ENCODING, TOPIC_METADATA};

use crate::decode::recording_path;

/// The formats an episode is exported to.
#[derive(Subcommand)]
pub(crate) enum ExportCommand {
    /// Write the episode's ROS 2 topics as a rosbag2 bag (one MCAP file),
    /// each on the topic it was recorded from, with both timestamps as
    /// recorded, for `ros2 bag play <file>`; Kapto's own channels and other
    /// streams are left out.
    Rosbag2 {
        /// The episode folder, or its recording.mcap.
        episode: PathBuf,
        /// The bag to write, e.g. episode.mcap; never overwritten.
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
    },
}

/// Runs `kapto export`.
pub(crate) fn export(command: &ExportCommand) -> ExitCode {
    let ExportCommand::Rosbag2 { episode, output } = command;
    match rosbag2(&recording_path(episode), output) {
        Ok(topics) => {
            for (topic, messages) in &topics {
                println!("{topic}: {messages} messages");
            }
            println!(
                "Wrote {}; play it with: ros2 bag play {}",
                output.display(),
                output.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", error.report());
            ExitCode::FAILURE
        }
    }
}

/// Writes the ROS 2 topics of the recording at `recording` to a new MCAP
/// file at `output`, as rosbag2 writes a bag: each channel named after the
/// topic its stream was received on, with its schema, and each message with
/// its sequence and both timestamps as recorded. Returns each topic with
/// how many messages it has.
///
/// # Errors
///
/// `export.output_exists` if `output` exists; `recording.unreadable` if the
/// recording can't be read; `export.no_ros2_topics` if it has none, and
/// `export.write_failed` if writing fails. On an error after `output` was
/// created, it is removed.
pub(crate) fn rosbag2(
    recording: &Path,
    output: &Path,
) -> Result<BTreeMap<String, u64>, KaptoError> {
    let file = File::create_new(output).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::AlreadyExists {
            ErrorCode::ExportOutputExists
        } else {
            ErrorCode::ExportWriteFailed
        };
        KaptoError::new(code)
            .detail(error.to_string())
            .context("file", output.display().to_string())
    })?;
    let written = write_topics(recording, file, output);
    if written.is_err() {
        // Never leave a partial bag that looks like a whole one.
        let _ = std::fs::remove_file(output);
    }
    written
}

fn write_topics(
    recording: &Path,
    file: File,
    output: &Path,
) -> Result<BTreeMap<String, u64>, KaptoError> {
    let failed = |error: mcap::McapError| {
        KaptoError::new(ErrorCode::ExportWriteFailed)
            .detail(error.to_string())
            .context("file", output.display().to_string())
    };
    let mut bag = mcap::WriteOptions::new()
        .profile("ros2")
        .library(concat!("kapto ", env!("CARGO_PKG_VERSION")))
        .create(BufWriter::new(file))
        .map_err(failed)?;
    // Each recorded stream's channel in the bag, by stream, once seen.
    let mut channels: BTreeMap<String, u16> = BTreeMap::new();
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut error = None;
    read_recording(recording, |channel, message| {
        let Some(topic) = channel.metadata.get(TOPIC_METADATA) else {
            return ControlFlow::Continue(());
        };
        if channel.message_encoding != CDR_ENCODING {
            return ControlFlow::Continue(());
        }
        let id = if let Some(id) = channels.get(&channel.topic) {
            *id
        } else {
            let schema = channel.schema.as_ref().map_or(Ok(0), |schema| {
                bag.add_schema(&schema.name, &schema.encoding, &schema.data)
            });
            let added = schema
                .and_then(|schema| bag.add_channel(schema, topic, CDR_ENCODING, &BTreeMap::new()));
            match added {
                Ok(id) => {
                    channels.insert(channel.topic.clone(), id);
                    id
                }
                Err(failure) => {
                    error = Some(failed(failure));
                    return ControlFlow::Break(());
                }
            }
        };
        let header = mcap::records::MessageHeader {
            channel_id: id,
            sequence: message.sequence,
            log_time: message.log_time,
            publish_time: message.publish_time,
        };
        if let Err(failure) = bag.write_to_known_channel(&header, message.data) {
            error = Some(failed(failure));
            return ControlFlow::Break(());
        }
        *counts.entry(topic.clone()).or_default() += 1;
        ControlFlow::Continue(())
    })?;
    if let Some(error) = error {
        return Err(error);
    }
    if channels.is_empty() {
        return Err(KaptoError::new(ErrorCode::ExportNoRos2Topics)
            .context("recording", recording.display().to_string()));
    }
    bag.finish().map_err(failed)?;
    Ok(counts)
}
