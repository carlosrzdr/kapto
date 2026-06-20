// SPDX-License-Identifier: Apache-2.0

//! `kapto topics`: the ROS 2 topics this machine's ROS 2 network has, and
//! their message types, for declaring a ROS 2 robot's streams; with
//! `--streams`, as `streams.toml` declarations to paste and complete, each
//! topic's rate measured over the wait.

use std::fmt::Write as _;
use std::process::ExitCode;

use clap::Args;
use kapto_core::KaptoError;

/// Arguments of `kapto topics`.
#[derive(Args)]
pub(crate) struct TopicsArgs {
    /// Seconds to wait for publishers to announce themselves before
    /// listing their topics.
    #[arg(long, value_name = "SECONDS", default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..=60))]
    wait: u32,
    /// Print each topic as a `streams.toml` declaration, with the rate it
    /// published at while Kapto listened (another --wait seconds), and the
    /// largest message seen where that is larger than the default.
    #[arg(long)]
    streams: bool,
}

/// ROS 2's own topics, which every node has: not the robot's data.
#[cfg_attr(
    not(feature = "ros2"),
    allow(dead_code, reason = "only a Kapto built with ROS 2 lists topics")
)]
const ROS_OWN: &[&str] = &["/rosout", "/parameter_events"];

/// Messages longer than this are recorded only if declared:
/// `max_message_bytes`, whose default it is.
const DEFAULT_MAX_MESSAGE_BYTES: u64 = 1472;

/// One topic as found, and what listening to it showed.
#[derive(Clone, Debug, PartialEq)]
struct Heard {
    topic: String,
    types: Vec<String>,
    /// Messages a second while Kapto listened; `None` if it couldn't
    /// subscribe (the type isn't installed here).
    rate: Option<f64>,
    /// The largest message seen, in bytes.
    largest: u64,
}

pub(crate) fn topics(args: &TopicsArgs) -> ExitCode {
    match listed(args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", error.report());
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "ros2")]
fn listed(args: &TopicsArgs) -> Result<String, KaptoError> {
    let found =
        kapto_transport::ros2::topics(std::time::Duration::from_secs(u64::from(args.wait)))?;
    let topics: Vec<(String, Vec<String>)> = found
        .into_iter()
        .map(|topic| (topic.name, topic.types))
        .collect();
    if args.streams {
        let wait = std::time::Duration::from_secs(u64::from(args.wait));
        return Ok(declarations(&listened(&topics, wait)?, args.wait));
    }
    Ok(table(&topics, args.wait))
}

/// Subscribes to each of `topics` but ROS 2's own, by its first type, and
/// counts its messages for `wait`. A topic whose type isn't installed here
/// is left unsubscribed.
#[cfg(feature = "ros2")]
#[allow(
    clippy::cast_precision_loss,
    reason = "a rate needs a few significant digits, far fewer than any count has"
)]
fn listened(
    topics: &[(String, Vec<String>)],
    wait: std::time::Duration,
) -> Result<Vec<Heard>, KaptoError> {
    use kapto_capture::Batch;
    use kapto_transport::Transport;
    use kapto_transport::ros2::{Durability, Qos, Reliability, Ros2Subscription, Ros2Transport};

    let mut heard: Vec<Heard> = topics
        .iter()
        .filter(|(topic, types)| !ROS_OWN.contains(&topic.as_str()) && !types.is_empty())
        .map(|(topic, types)| Heard {
            topic: topic.clone(),
            types: types.clone(),
            rate: Some(0.0),
            largest: 0,
        })
        .collect();
    // Opened again without a type that isn't installed, until it opens.
    let (mut transport, subscribed) = loop {
        let subscribed: Vec<usize> = (0..heard.len())
            .filter(|&at| heard[at].rate.is_some())
            .collect();
        let subscriptions: Vec<Ros2Subscription> = subscribed
            .iter()
            .zip(0..)
            .map(|(&at, stream)| Ros2Subscription {
                stream,
                topic: heard[at].topic.clone(),
                message_type: heard[at].types[0].clone(),
                qos: Qos {
                    reliability: Reliability::BestEffort,
                    durability: Durability::Volatile,
                    depth: 100,
                },
            })
            .collect();
        match Ros2Transport::open("kapto_topics_listening", &subscriptions) {
            Ok(transport) => break (transport, subscribed),
            Err(error) if error.code() == kapto_core::ErrorCode::TransportRos2TypeUnavailable => {
                let missing = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "type")
                    .map(|(_, value)| value.clone())
                    .ok_or(error)?;
                for each in &mut heard {
                    if each.types[0] == missing {
                        each.rate = None;
                    }
                }
            }
            Err(error) => return Err(error),
        }
    };
    let slot = 4 << 20;
    let mut batch = Batch::new(8, slot);
    let mut counts = vec![0_u64; subscribed.len()];
    let started = std::time::Instant::now();
    while started.elapsed() < wait {
        transport.receive(&mut batch, std::time::Duration::from_millis(100))?;
        for (stream, message) in batch.messages() {
            let at = subscribed[usize::from(stream)];
            counts[usize::from(stream)] += 1;
            heard[at].largest = heard[at].largest.max(message.len() as u64);
        }
        batch.clear();
    }
    let seconds = started.elapsed().as_secs_f64();
    for (stream, &at) in (0_u16..).zip(&subscribed) {
        let truncated = transport.stats().truncated(stream);
        if truncated > 0 {
            heard[at].largest = heard[at].largest.max(slot as u64);
        }
        heard[at].rate = Some((counts[usize::from(stream)] + truncated) as f64 / seconds);
    }
    Ok(heard)
}

#[cfg(not(feature = "ros2"))]
fn listed(_args: &TopicsArgs) -> Result<String, KaptoError> {
    Err(KaptoError::new(
        kapto_core::ErrorCode::TransportRos2NotBuilt,
    ))
}

/// `topic` as a stream's ID: lowercase letters, digits and underscores,
/// starting with a letter.
#[cfg_attr(
    not(feature = "ros2"),
    allow(dead_code, reason = "only a Kapto built with ROS 2 lists topics")
)]
fn stream_id(topic: &str) -> String {
    let mut id = String::new();
    for c in topic.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('_') {
            id.push('_');
        }
    }
    let id = id.trim_end_matches('_');
    if id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id.to_owned()
    } else {
        format!("topic_{id}")
    }
}

/// Each topic heard as a `streams.toml` declaration, with what wasn't
/// learned said as a comment, to paste and complete.
#[cfg_attr(
    not(feature = "ros2"),
    allow(dead_code, reason = "only a Kapto built with ROS 2 lists topics")
)]
fn declarations(heard: &[Heard], waited: u32) -> String {
    if heard.is_empty() {
        return table(&[], waited);
    }
    let mut text = format!(
        "# The ROS 2 topics seen, as streams.toml declarations, rates measured over {waited} s.\n\
         # Complete each: a decoder for the values to plot, the quality of service to match.\n"
    );
    let mut ids: Vec<String> = Vec::new();
    for each in heard {
        let mut id = stream_id(&each.topic);
        if ids.contains(&id) {
            id = format!("{id}_{}", ids.len() + 1);
        }
        let _ = write!(text, "\n[streams.{id}]\n");
        match each.rate {
            None => text.push_str("# Its type isn't installed here, so its rate wasn't measured.\n# rate_hz = \n"),
            Some(rate) if rate < 0.5 => text.push_str(
                "# Nothing published while Kapto listened: set its rate, or leave it out for a\n\
                 # topic kept for late subscribers (durability = \"transient_local\").\n# rate_hz = \n",
            ),
            Some(rate) => {
                let _ = writeln!(text, "rate_hz = {}", rate.round());
            }
        }
        if each.largest > DEFAULT_MAX_MESSAGE_BYTES {
            // Room for a larger one than seen, in whole KiB.
            let room = (each.largest * 2).div_ceil(1024) * 1024;
            let _ = writeln!(text, "max_message_bytes = {room}");
        }
        let _ = writeln!(
            text,
            "ros2 = {{ topic = \"{}\", type = \"{}\" }}",
            each.topic, each.types[0]
        );
        if each.types.len() > 1 {
            let _ = writeln!(text, "# Also published as {}.", each.types[1..].join(", "));
        }
        ids.push(id);
    }
    text
}

/// Each topic and its types, aligned; or what to check if there are none.
#[cfg_attr(
    not(feature = "ros2"),
    allow(dead_code, reason = "only a Kapto built with ROS 2 lists topics")
)]
fn table(topics: &[(String, Vec<String>)], waited: u32) -> String {
    if topics.is_empty() {
        return format!(
            "No ROS 2 topics seen in {waited} s. Check that the robot's nodes run on this \
             network with the same ROS_DOMAIN_ID and RMW_IMPLEMENTATION as Kapto, or wait \
             longer with --wait.\n"
        );
    }
    let width = topics.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let mut text = String::new();
    for (name, types) in topics {
        let _ = writeln!(text, "{name:width$}  {}", types.join(", "));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_listed_with_their_types_aligned() {
        let topics = [
            (
                "/joint_states".to_owned(),
                vec!["sensor_msgs/msg/JointState".to_owned()],
            ),
            ("/tf".to_owned(), vec!["tf2_msgs/msg/TFMessage".to_owned()]),
        ];
        assert_eq!(
            table(&topics, 2),
            "/joint_states  sensor_msgs/msg/JointState\n/tf            tf2_msgs/msg/TFMessage\n"
        );
    }

    fn heard(topic: &str, kind: &str, rate: Option<f64>, largest: u64) -> Heard {
        Heard {
            topic: topic.to_owned(),
            types: vec![kind.to_owned()],
            rate,
            largest,
        }
    }

    #[test]
    fn each_topic_is_declared_as_a_stream_with_what_listening_showed() {
        let found = [
            heard(
                "/joint_states",
                "sensor_msgs/msg/JointState",
                Some(499.6),
                312,
            ),
            heard(
                "/camera/image_raw/compressed",
                "sensor_msgs/msg/CompressedImage",
                Some(29.9),
                61_000,
            ),
            heard("/tf_static", "tf2_msgs/msg/TFMessage", Some(0.0), 0),
            heard("/arm/state", "acme_msgs/msg/ArmState", None, 0),
        ];
        assert_eq!(
            declarations(&found, 3),
            "# The ROS 2 topics seen, as streams.toml declarations, rates measured over 3 s.\n\
             # Complete each: a decoder for the values to plot, the quality of service to match.\n\
             \n[streams.joint_states]\nrate_hz = 500\n\
             ros2 = { topic = \"/joint_states\", type = \"sensor_msgs/msg/JointState\" }\n\
             \n[streams.camera_image_raw_compressed]\nrate_hz = 30\nmax_message_bytes = 122880\n\
             ros2 = { topic = \"/camera/image_raw/compressed\", type = \"sensor_msgs/msg/CompressedImage\" }\n\
             \n[streams.tf_static]\n\
             # Nothing published while Kapto listened: set its rate, or leave it out for a\n\
             # topic kept for late subscribers (durability = \"transient_local\").\n# rate_hz = \n\
             ros2 = { topic = \"/tf_static\", type = \"tf2_msgs/msg/TFMessage\" }\n\
             \n[streams.arm_state]\n\
             # Its type isn't installed here, so its rate wasn't measured.\n# rate_hz = \n\
             ros2 = { topic = \"/arm/state\", type = \"acme_msgs/msg/ArmState\" }\n"
        );
    }

    #[test]
    fn a_topic_becomes_a_stream_id_a_name_can_be() {
        assert_eq!(stream_id("/joint_states"), "joint_states");
        assert_eq!(stream_id("/Robot/IMU--data/"), "robot_imu_data");
        assert_eq!(stream_id("/3d/points"), "topic_3d_points");
    }

    #[test]
    fn no_topics_says_what_to_check() {
        assert!(table(&[], 5).starts_with("No ROS 2 topics seen in 5 s."));
    }
}
