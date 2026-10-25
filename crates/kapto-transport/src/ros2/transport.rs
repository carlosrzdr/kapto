// SPDX-License-Identifier: Apache-2.0

//! Listing ROS 2 topics through `r2r`, and receiving them through Kapto's
//! own node.

use std::sync::Arc;
use std::time::Duration;

use kapto_capture::{Batch, StreamIndex};
use kapto_core::clock::monotonic_now;
use kapto_core::{ErrorCode, KaptoError};

use super::{Ros2Subscription, Topic, rcl};
use crate::{Transport, TransportStats};

/// The topics published on the machine's ROS 2 network, by name, as a node
/// of Kapto's own sees them after waiting `discovery` for the publishers to
/// announce themselves.
///
/// # Errors
///
/// `transport.ros2.node_failed` if Kapto's node can't start or can't ask.
pub fn topics(discovery: Duration) -> Result<Vec<Topic>, KaptoError> {
    let context = r2r::Context::create().map_err(node_failed)?;
    let node = r2r::Node::create(context, "kapto_topics", "").map_err(node_failed)?;
    // Discovery happens in the middleware's own threads; nothing to spin.
    std::thread::sleep(discovery);
    let mut topics: Vec<Topic> = node
        .get_topic_names_and_types()
        .map_err(node_failed)?
        .into_iter()
        .map(|(name, types)| Topic { name, types })
        .collect();
    topics.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(topics)
}

fn node_failed(error: r2r::Error) -> KaptoError {
    KaptoError::new(ErrorCode::TransportRos2NodeFailed)
        .detail(error.to_string())
        .source(error)
}

/// Receives the messages of ROS 2 topics through a node of Kapto's own,
/// subscribed to them for as long as it lives.
pub struct Ros2Transport {
    node: rcl::Node,
    streams: Vec<StreamIndex>,
    stats: Arc<TransportStats>,
}

// The node is rcl's handles, which can't be printed.
impl std::fmt::Debug for Ros2Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ros2Transport")
            .field("streams", &self.streams)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Ros2Transport {
    /// Starts Kapto's node, named `node_name`, and subscribes to every topic.
    ///
    /// # Errors
    ///
    /// `transport.ros2.node_failed` if the node can't start,
    /// `transport.ros2.type_unavailable` if a topic's message type isn't
    /// installed here, and `transport.ros2.subscribe_failed` if a topic
    /// can't be subscribed to.
    pub fn open(node_name: &str, subscriptions: &[Ros2Subscription]) -> Result<Self, KaptoError> {
        let wanted: Vec<rcl::Wanted<'_>> = subscriptions
            .iter()
            .map(|subscription| rcl::Wanted {
                topic: &subscription.topic,
                message_type: &subscription.message_type,
                qos: subscription.qos,
            })
            .collect();
        let node = rcl::Node::open(node_name, &wanted)?;
        let streams: Vec<StreamIndex> = subscriptions.iter().map(|s| s.stream).collect();
        let count = streams.iter().map(|&stream| usize::from(stream) + 1).max();
        Ok(Self {
            node,
            stats: Arc::new(TransportStats::new(count.unwrap_or(0))),
            streams,
        })
    }
}

impl Transport for Ros2Transport {
    fn receive(&mut self, batch: &mut Batch, timeout: Duration) -> Result<usize, KaptoError> {
        if batch.is_full() || !self.node.wait(timeout)? {
            return Ok(0);
        }
        let now = monotonic_now();
        let was_empty = batch.is_empty();
        let mut added = 0;
        for (index, &stream) in self.streams.iter().enumerate() {
            if !self.node.is_ready(index) {
                continue;
            }
            // Every message waiting, until the batch is full; the rest wait
            // in the middleware for the next batch.
            while !batch.is_full() {
                let Some(taken) = self.node.take(index)? else {
                    break;
                };
                let pushed = match taken.publication {
                    Some(publication) => batch.push_from_publisher(
                        stream,
                        taken.message,
                        taken.published,
                        publication,
                    ),
                    None => batch.push_with_source_time(stream, taken.message, taken.published),
                };
                if pushed {
                    added += 1;
                } else {
                    self.stats.count_truncated(stream);
                }
            }
        }
        if was_empty && added > 0 {
            batch.set_recv_time(now);
        }
        Ok(added)
    }

    fn stats(&self) -> &Arc<TransportStats> {
        &self.stats
    }
}
