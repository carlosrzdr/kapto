// SPDX-License-Identifier: Apache-2.0

//! ROS 2 topics, through ROS 2's own client library (rcl, bound by `r2r`),
//! so Kapto sees what the robot's nodes see whatever middleware they use.
//!
//! What a stream's topic is ([`Ros2Subscription`]) is known to every build,
//! so one without ROS 2 can say a stream needs it. Listing the topics
//! (`topics`) and receiving them (`Ros2Transport`, a node of Kapto's own
//! that takes each message as it arrived, still serialised as CDR, straight
//! into a batch's free slot) are only built with the `ros2` feature, which
//! needs a ROS 2 installation.

// Built and tested on every machine, used only where ROS 2 is.
#[cfg_attr(
    not(feature = "ros2"),
    allow(dead_code, reason = "used by the ros2 transport alone")
)]
mod publishers;
#[cfg(feature = "ros2")]
mod rcl;
#[cfg(feature = "ros2")]
mod transport;

use kapto_capture::StreamIndex;

#[cfg(feature = "ros2")]
pub use transport::{Ros2Transport, topics};

/// A topic published on the machine's ROS 2 network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    /// Its name, e.g. `/joint_states`.
    pub name: String,
    /// The message types it is published with, e.g.
    /// `sensor_msgs/msg/JointState`: one, unless publishers disagree.
    pub types: Vec<String>,
}

/// A ROS 2 topic one stream is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ros2Subscription {
    /// The stream every message on the topic belongs to.
    pub stream: StreamIndex,
    /// The topic's name, e.g. `/joint_states`.
    pub topic: String,
    /// The type of its messages, e.g. `sensor_msgs/msg/JointState`.
    pub message_type: String,
    /// How to subscribe.
    pub qos: Qos,
}

/// The quality of service a topic is subscribed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Qos {
    /// Whether the publisher resends what didn't arrive.
    pub reliability: Reliability,
    /// Whether messages published before subscribing are delivered.
    pub durability: Durability,
    /// How many messages the middleware keeps before Kapto takes them.
    pub depth: usize,
}

/// Whether the publisher resends what didn't arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reliability {
    /// What doesn't arrive is lost; matches every publisher.
    BestEffort,
    /// Resent; only matches a reliable publisher.
    Reliable,
}

/// Whether messages published before subscribing are delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Only those published from now on.
    Volatile,
    /// Also the last ones the publisher kept.
    TransientLocal,
}
