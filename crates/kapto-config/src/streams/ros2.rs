// SPDX-License-Identifier: Apache-2.0

//! The `ros2` table of a stream: the ROS 2 topic it is, and how Kapto
//! subscribes to it.
//!
//! ```toml
//! [streams.joints]
//! ros2 = { topic = "/joint_states", type = "sensor_msgs/msg/JointState" }
//!
//! [streams.joints.ros2.qos]
//! reliability = "reliable"
//! durability = "transient_local"
//! depth = 100
//! ```

use toml_edit::TableLike;

use super::Reader;

/// A stream that is a ROS 2 topic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ros2Declaration {
    /// The topic's name, e.g. `/joint_states`.
    pub topic: String,
    /// The type of its messages, e.g. `sensor_msgs/msg/JointState`.
    pub message_type: String,
    /// The type's definition as rosbag2 stores it, put together from its
    /// `.msg` files.
    pub definition: String,
    /// How Kapto subscribes to it.
    pub qos: QosDeclaration,
}

/// The quality of service Kapto subscribes with (`ros2.qos`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QosDeclaration {
    /// Whether the publisher resends what didn't arrive.
    pub reliability: Reliability,
    /// Whether messages published before Kapto subscribed are delivered.
    pub durability: Durability,
    /// How many messages the middleware keeps for Kapto before taking them.
    pub depth: u32,
}

/// Best effort, volatile, the last 10: what ROS 2 calls sensor data. A best
/// effort subscription matches a reliable publisher as well as a best
/// effort one, and a robot's streams are wanted as they are now, not
/// resent late.
impl Default for QosDeclaration {
    fn default() -> Self {
        Self {
            reliability: Reliability::BestEffort,
            durability: Durability::Volatile,
            depth: 10,
        }
    }
}

/// `qos.reliability`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reliability {
    /// `"best_effort"`: what doesn't arrive is lost, and counted when the
    /// stream has a sequence number or a rate.
    BestEffort,
    /// `"reliable"`: the publisher resends; only matches a reliable one.
    Reliable,
}

/// `qos.durability`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// `"volatile"`: only messages published from now on.
    Volatile,
    /// `"transient_local"`: also the last ones the publisher kept, e.g. a
    /// robot description published once.
    TransientLocal,
}

pub(super) const ROS2_KEYS: &[&str] = &["topic", "type", "qos"];
pub(super) const QOS_KEYS: &[&str] = &["reliability", "durability", "depth"];
const MAX_DEPTH: i64 = 100_000;

impl Reader<'_> {
    pub(super) fn ros2(
        &mut self,
        table: &dyn TableLike,
        stream_table: &dyn TableLike,
        stream: &str,
    ) -> Option<Ros2Declaration> {
        let path = format!("{stream}.ros2");
        self.unknown_keys(table, &path, ROS2_KEYS);
        let topic = self.parsed::<String>(table, &path, "topic", "a text value");
        if topic.as_deref().is_some_and(|topic| !is_topic(topic)) {
            let detail = format!(
                "`{path}.topic` must be a ROS 2 topic name: words of letters, digits and single `_`, separated by `/`, none starting with a digit, e.g. \"/joint_states\""
            );
            self.invalid(table, "topic", detail);
        }
        let message_type = self.parsed::<String>(table, &path, "type", "a text value");
        if message_type
            .as_deref()
            .is_some_and(|ty| !is_message_type(ty))
        {
            let detail = format!(
                "`{path}.type` must be a ROS 2 message type, `<package>/msg/<Type>`, e.g. \"sensor_msgs/msg/JointState\""
            );
            self.invalid(table, "type", detail);
        }
        for required in ["topic", "type"] {
            if table.get(required).is_none() {
                self.invalid(stream_table, "ros2", format!("`{path}` needs `{required}`"));
            }
        }
        let qos = match self.table(table, &format!("{path}."), "qos") {
            Some(qos) => self.qos(qos, &path)?,
            None if table.get("qos").is_some() => return None,
            None => QosDeclaration::default(),
        };
        let topic = topic.filter(|topic| is_topic(topic))?;
        let message_type = message_type.filter(|ty| is_message_type(ty))?;
        Some(Ros2Declaration {
            definition: self.definition(table, &message_type)?,
            topic,
            message_type,
            qos,
        })
    }

    fn qos(&mut self, table: &dyn TableLike, ros2: &str) -> Option<QosDeclaration> {
        let path = format!("{ros2}.qos");
        self.unknown_keys(table, &path, QOS_KEYS);
        let default = QosDeclaration::default();
        let reliability = self.choice(
            table,
            &path,
            "reliability",
            &[
                ("best_effort", Reliability::BestEffort),
                ("reliable", Reliability::Reliable),
            ],
        );
        let durability = self.choice(
            table,
            &path,
            "durability",
            &[
                ("volatile", Durability::Volatile),
                ("transient_local", Durability::TransientLocal),
            ],
        );
        let depth = self.integer(table, &path, "depth", 1..=MAX_DEPTH);
        let given = |key: &str, valid: bool| valid || table.get(key).is_none();
        let all_valid = given("reliability", reliability.is_some())
            && given("durability", durability.is_some())
            && given("depth", depth.is_some());
        all_valid.then(|| QosDeclaration {
            reliability: reliability.unwrap_or(default.reliability),
            durability: durability.unwrap_or(default.durability),
            depth: depth
                .and_then(|depth| u32::try_from(depth).ok())
                .unwrap_or(default.depth),
        })
    }
}

/// A topic name as ROS 2 accepts one from Kapto's node, which has no
/// namespace: words of letters, digits and single underscores, separated by
/// `/`, none starting with a digit; a leading `/` is optional.
fn is_topic(name: &str) -> bool {
    let relative = name.strip_prefix('/').unwrap_or(name);
    !relative.is_empty() && relative.split('/').all(is_word)
}

/// `<package>/msg/<Type>`.
fn is_message_type(name: &str) -> bool {
    matches!(
        name.split('/').collect::<Vec<_>>().as_slice(),
        [package, "msg", type_name] if is_word(package) && is_word(type_name)
    )
}

fn is_word(word: &str) -> bool {
    word.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !word.contains("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names_are_checked_as_ros_2_checks_them() {
        for good in ["/joint_states", "joint_states", "/arm/left/joints", "/_x1"] {
            assert!(is_topic(good), "{good}");
        }
        for bad in [
            "",
            "/",
            "/joint states",
            "/2arm",
            "/arm//x",
            "/arm/",
            "/a__b",
            "~/x",
        ] {
            assert!(!is_topic(bad), "{bad}");
        }
        assert!(is_message_type("sensor_msgs/msg/JointState"));
        for bad in [
            "sensor_msgs/JointState",
            "sensor_msgs/srv/Trigger",
            "/msg/X",
            "a/msg/",
        ] {
            assert!(!is_message_type(bad), "{bad}");
        }
    }
}
