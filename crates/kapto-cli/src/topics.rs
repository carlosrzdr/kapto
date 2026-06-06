// SPDX-License-Identifier: Apache-2.0

//! `kapto topics`: the ROS 2 topics this machine's ROS 2 network has, and
//! their message types, for declaring a ROS 2 robot's streams.

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
    Ok(table(&topics, args.wait))
}

#[cfg(not(feature = "ros2"))]
fn listed(_args: &TopicsArgs) -> Result<String, KaptoError> {
    Err(KaptoError::new(
        kapto_core::ErrorCode::TransportRos2NotBuilt,
    ))
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

    #[test]
    fn no_topics_says_what_to_check() {
        assert!(table(&[], 5).starts_with("No ROS 2 topics seen in 5 s."));
    }
}
