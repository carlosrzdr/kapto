// SPDX-License-Identifier: Apache-2.0

//! What `kapto serve` was started with: the robot, its joints and what
//! drives each, and the streams it receives. Worked out once when serve
//! starts, from the files it checked then; the API only hands it out.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::ApiError;

pub use kapto_viz::page_layout::{
    ColumnInput, LayoutInput, PanelInput, RobotRole, ViewColumn, ViewLayout, ViewPanel, arranged,
    roles_of,
};

/// The robot and its streams, as `kapto serve` loaded them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RobotOverview {
    /// The robot's name in its description.
    pub name: String,
    /// The files it was loaded from.
    pub files: RobotFiles,
    /// How many links it has.
    pub links: usize,
    /// Its joints, in the order the description declares them.
    pub joints: Vec<RobotJoint>,
    /// The streams Kapto receives, in the order they are declared.
    pub streams: Vec<RobotStream>,
    /// What each decoded value is for: every role the mapping gives a
    /// field, written roles first, then each mapped joint's.
    pub roles: Vec<RobotRole>,
    /// How the live view is arranged.
    pub layout: ViewLayout,
    /// Problems that don't stop Kapto, such as a mesh that isn't there or
    /// a joint nothing drives.
    pub warnings: Vec<ApiError>,
}

/// The files a robot was loaded from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RobotFiles {
    /// The robot description (URDF).
    pub urdf: String,
    /// What the robot sends (`streams.toml`).
    pub streams: String,
    /// Which decoded value drives which joint (`mapping.toml`).
    pub mapping: String,
}

/// One joint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RobotJoint {
    /// Its name.
    pub name: String,
    /// How it moves: `revolute`, `continuous`, `prismatic`, `fixed`,
    /// `floating`, `planar` or `spherical`.
    pub kind: String,
    /// Its lowest value, in radians or metres, if limited.
    pub lower: Option<f64>,
    /// Its highest value, in radians or metres, if limited.
    pub upper: Option<f64>,
    /// Its largest speed, in radians or metres per second, if known.
    pub velocity: Option<f64>,
    /// Its largest torque or force, in newton-metres or newtons, if known.
    pub effort: Option<f64>,
    /// The joint whose value it copies, if it does.
    pub mimics: Option<String>,
    /// The decoded value its position comes from, e.g.
    /// `arm_joints.joint_positions[0]`, if mapped.
    pub position_from: Option<String>,
    /// The decoded value its velocity comes from, if mapped.
    pub velocity_from: Option<String>,
    /// The decoded value its effort comes from, if mapped.
    pub effort_from: Option<String>,
}

/// One stream Kapto receives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RobotStream {
    /// Its name.
    pub name: String,
    /// Where it arrives, e.g. `udp 0.0.0.0:9000`, `tcp listen 0.0.0.0:9001`,
    /// `tcp connect 192.168.1.20:9002` or `ros2 /joint_states`.
    pub input: String,
    /// Messages per second the robot sends, if declared.
    pub rate_hz: Option<u32>,
    /// Whether its messages are kept for late subscribers (a ROS 2 topic
    /// such as `/tf_static`): it may send them once and nothing after,
    /// and every episode starts with the newest.
    pub kept: bool,
    /// Whether its messages carry a sequence number, so loss before Kapto
    /// is counted exactly.
    pub sequence: bool,
    /// How its messages decode: `struct`, `json` or `msgpack`; none if they
    /// are only recorded.
    pub decoder: Option<String>,
    /// The fields it decodes into, in order.
    pub fields: Vec<RobotField>,
}

/// One decoded field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RobotField {
    /// Its name.
    pub name: String,
    /// Its type as declared, e.g. `f64`.
    pub ty: String,
    /// How many values it holds: more than 1 for an array.
    pub count: usize,
    /// Its unit, if declared.
    pub unit: Option<String>,
}
