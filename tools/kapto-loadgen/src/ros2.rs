// SPDX-License-Identifier: Apache-2.0

//! Publishing on ROS 2 topics, as a robot's arm publishes its joint states:
//! each stream's samples as `sensor_msgs/msg/JointState` messages on a topic
//! of its own, serialised by ROS 2 itself, through `r2r`, from a node of
//! the generator's own, `kapto_loadgen`.
//!
//! A message names the sample arm's joints, `joint_1` to `joint_7` and then
//! `gripper` (as `packages/arm7`'s URDF does), with their positions in that
//! order: the seven joint angles in radians, then the gripper opening in
//! metres. Its header's stamp is the sample's source time. A message has no
//! sequence number: `JointState` has none.
//!
//! Each topic is published reliably, keeping the last 1000 messages for a
//! subscriber that falls behind, so nothing is lost on the machine itself.
//!
//! A camera ([`crate::camera`]) publishes each frame as a
//! `sensor_msgs/msg/CompressedImage` instead, its format `jpeg`, its
//! header's stamp the frame's source time, keeping 10 of them, as a
//! camera's driver does.

use std::io;
use std::time::{Duration, Instant};

use r2r::builtin_interfaces::msg::Time;
use r2r::sensor_msgs::msg::{CompressedImage, JointState};
use r2r::{Context, Node, Publisher, QosProfile};

use crate::payload::{JOINTS, Sample};

/// The sample arm's joint names, in the order of a message's positions.
const JOINT_NAMES: [&str; JOINTS + 1] = [
    "joint_1", "joint_2", "joint_3", "joint_4", "joint_5", "joint_6", "joint_7", "gripper",
];

/// How many messages each topic keeps for a slow subscriber.
const DEPTH: usize = 1000;

/// How many frames a camera's topic keeps: frames are large and a late
/// one is worth less than the next.
const FRAMES_KEPT: usize = 10;

/// Each stream's publisher, of joint states or of a camera's frames.
enum Topics {
    Joints(Vec<Publisher<JointState>>),
    Frames(Vec<Publisher<CompressedImage>>),
}

/// The node and a publisher per stream.
pub(crate) struct Publishers {
    /// Owns the publishers' handles: they only hold on to it.
    _node: Node,
    /// Stream by stream.
    topics: Topics,
    /// Their names, in the same order.
    names: Vec<String>,
    /// The message, rewritten for each sample: the names never change.
    message: JointState,
    /// The frame's message, its data rewritten for each frame in place.
    image: CompressedImage,
}

impl Publishers {
    /// Starts the node and a publisher on each of `topics`, stream by
    /// stream: of a camera's frames if `camera`, else of joint states.
    pub(crate) fn open(topics: &[String], camera: bool) -> io::Result<Self> {
        let context = Context::create().map_err(|error| failed(&error))?;
        let mut node =
            Node::create(context, "kapto_loadgen", "").map_err(|error| failed(&error))?;
        let qos = |depth| QosProfile {
            depth,
            ..QosProfile::default()
        };
        let publishers = if camera {
            Topics::Frames(
                topics
                    .iter()
                    .map(|topic| node.create_publisher(topic, qos(FRAMES_KEPT)))
                    .collect::<Result<_, _>>()
                    .map_err(|error| failed(&error))?,
            )
        } else {
            Topics::Joints(
                topics
                    .iter()
                    .map(|topic| node.create_publisher(topic, qos(DEPTH)))
                    .collect::<Result<_, _>>()
                    .map_err(|error| failed(&error))?,
            )
        };
        let message = JointState {
            name: JOINT_NAMES.map(str::to_owned).to_vec(),
            position: vec![0.0; JOINT_NAMES.len()],
            ..JointState::default()
        };
        let image = CompressedImage {
            format: "jpeg".to_owned(),
            ..CompressedImage::default()
        };
        Ok(Self {
            _node: node,
            topics: publishers,
            names: topics.to_vec(),
            message,
            image,
        })
    }

    /// How many subscribers each topic has, stream by stream; none where
    /// the middleware can't say.
    fn subscribers(&self) -> Vec<usize> {
        let count = |counted: r2r::Result<usize>| counted.unwrap_or(1);
        match &self.topics {
            Topics::Joints(publishers) => publishers
                .iter()
                .map(|publisher| count(publisher.get_inter_process_subscription_count()))
                .collect(),
            Topics::Frames(publishers) => publishers
                .iter()
                .map(|publisher| count(publisher.get_inter_process_subscription_count()))
                .collect(),
        }
    }

    /// Returns once every topic has a subscriber, or fails naming those
    /// still without one after `limit`. Discovery runs on the middleware's
    /// own threads, so asking is enough.
    pub(crate) fn wait_for_subscribers(&self, limit: Duration) -> io::Result<()> {
        let deadline = Instant::now() + limit;
        loop {
            let missing: Vec<&str> = self
                .subscribers()
                .into_iter()
                .zip(&self.names)
                .filter(|(count, _)| *count == 0)
                .map(|(_, name)| name.as_str())
                .collect();
            if missing.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "no subscriber on {} within {:.1} s",
                        missing.join(", "),
                        limit.as_secs_f64()
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Publishes `sample` on stream `stream`'s topic; whether it went.
    pub(crate) fn send(&mut self, stream: usize, sample: &Sample) -> bool {
        let Topics::Joints(publishers) = &self.topics else {
            return false;
        };
        let Some(publisher) = publishers.get(stream) else {
            return false;
        };
        self.message.header.stamp = stamp(sample.source_time_ns);
        self.message.position[..JOINTS].copy_from_slice(&sample.joints);
        self.message.position[JOINTS] = sample.gripper;
        publisher.publish(&self.message).is_ok()
    }
}

impl Publishers {
    /// Publishes a camera's `frame` on stream `stream`'s topic, stamped
    /// `source_time_ns`; whether it went.
    pub(crate) fn send_frame(&mut self, stream: usize, frame: &[u8], source_time_ns: u64) -> bool {
        let Topics::Frames(publishers) = &self.topics else {
            return false;
        };
        let Some(publisher) = publishers.get(stream) else {
            return false;
        };
        self.image.header.stamp = stamp(source_time_ns);
        self.image.data.clear();
        self.image.data.extend_from_slice(frame);
        publisher.publish(&self.image).is_ok()
    }
}

/// A time in nanoseconds since the Unix epoch, as ROS 2 stamps a header.
fn stamp(nanoseconds: u64) -> Time {
    Time {
        sec: i32::try_from(nanoseconds / 1_000_000_000).unwrap_or(i32::MAX),
        nanosec: u32::try_from(nanoseconds % 1_000_000_000).unwrap_or(0),
    }
}

fn failed(error: &r2r::Error) -> io::Error {
    io::Error::other(format!("ROS 2: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_splits_nanoseconds_into_seconds_and_the_rest() {
        let time = stamp(1_761_000_000_123_456_789);
        assert_eq!((time.sec, time.nanosec), (1_761_000_000, 123_456_789));
    }
}
