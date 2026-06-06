// SPDX-License-Identifier: Apache-2.0

//! The checks Kapto ships, and the set an episode gets from its quality
//! settings.

use kapto_config::QaSettings;
use kapto_core::KaptoError;

use crate::{Check, CheckKind, EpisodeView, Finding};

mod motion;

pub use motion::{FrozenJoints, GripperChatter, Idle, JointJumps};

/// The checks `settings` turn on, each made with its settings, in the order
/// they run.
#[must_use]
pub fn from_settings(settings: &QaSettings) -> Vec<Box<dyn Check>> {
    let mut checks: Vec<Box<dyn Check>> = Vec::new();
    let complete = &settings.streams_complete;
    if complete.enabled {
        checks.push(Box::new(StreamsComplete {
            max_dropped: complete.max_dropped,
        }));
    }
    let jumps = &settings.joint_jumps;
    if jumps.enabled {
        checks.push(Box::new(JointJumps {
            max_speed: jumps.max_speed,
        }));
    }
    let frozen = &settings.frozen_joints;
    if frozen.enabled {
        checks.push(Box::new(FrozenJoints {
            max_seconds: frozen.max_seconds,
            moving_speed: frozen.moving_speed,
        }));
    }
    let chatter = &settings.gripper_chatter;
    if chatter.enabled {
        checks.push(Box::new(GripperChatter {
            max_closings: chatter.max_closings,
            window_seconds: chatter.window_seconds,
        }));
    }
    let idle = &settings.idle;
    if idle.enabled {
        checks.push(Box::new(Idle {
            max_ratio: idle.max_ratio,
            min_seconds: idle.min_seconds,
            moving_speed: idle.moving_speed,
        }));
    }
    checks
}

/// Every message that reached Kapto was written: none was dropped by Kapto
/// itself, e.g. because the disk couldn't keep up, or no more than
/// `max_dropped` from any one stream. A gate: an episode with a hole Kapto
/// made isn't accepted. Messages lost before they reached Kapto are the
/// robot's or the network's, and aren't this check's.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamsComplete {
    /// The most messages Kapto may drop from one stream.
    pub max_dropped: u64,
}

impl Check for StreamsComplete {
    fn id(&self) -> &'static str {
        "streams.complete"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> CheckKind {
        CheckKind::Gate
    }

    fn run(&self, episode: &EpisodeView) -> Result<Finding, KaptoError> {
        let streams = &episode.summary().streams;
        if streams.is_empty() {
            return Ok(Finding::not_applicable("the episode has no streams"));
        }
        let too_many: Vec<String> = streams
            .iter()
            .filter(|stream| stream.dropped_by_us > self.max_dropped)
            .map(|stream| {
                format!(
                    "{} ({} of {} received)",
                    stream.id, stream.dropped_by_us, stream.received
                )
            })
            .collect();
        let dropped: u64 = streams.iter().map(|stream| stream.dropped_by_us).sum();
        let allowed = if self.max_dropped == 0 {
            String::new()
        } else {
            format!(", more than the {} allowed per stream", self.max_dropped)
        };
        let finding = if !too_many.is_empty() {
            Finding::failed(format!(
                "Kapto dropped {dropped} messages: {}{allowed}",
                too_many.join(", ")
            ))
        } else if dropped > 0 {
            Finding::passed(format!(
                "Kapto dropped {dropped} messages, no more than the {} allowed per stream",
                self.max_dropped
            ))
        } else {
            Finding::passed("every message that reached Kapto was written")
        };
        // Message counts stay far below 2^53, up to which an `f64` is exact.
        #[allow(clippy::cast_precision_loss, reason = "message counts")]
        let finding = streams.iter().fold(
            finding.with_value("dropped_by_us", dropped as f64),
            |finding, stream| {
                finding.with_value(
                    &format!("{}.dropped_by_us", stream.id),
                    stream.dropped_by_us as f64,
                )
            },
        );
        Ok(finding)
    }
}
