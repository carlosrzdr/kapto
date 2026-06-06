// SPDX-License-Identifier: Apache-2.0

//! How far a robot reaches: a sphere around every place its visual shapes
//! can be, over joint values sampled within their limits, so a viewer can
//! frame the robot once and keep it in view however it moves.

use std::f64::consts::PI;
use std::path::Path;

use kapto_core::KaptoError;

use crate::kinematics::Kinematics;
use crate::model::{Geometry, JointKind, RobotModel};
use crate::transform::Transform;

/// How many joint configurations are sampled, besides every joint at zero.
pub const SAMPLES: usize = 512;

/// A box's lowest and highest corner: x, y and z, in metres.
pub type Bounds = ([f64; 3], [f64; 3]);

/// A sphere around everywhere the robot's shapes can be, in the root
/// link's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reach {
    /// Its centre: x, y and z, in metres.
    pub center: [f64; 3],
    /// Its radius, in metres.
    pub radius: f64,
}

/// The reach of `robot` with each joint moved within its limits, a
/// continuous joint a full turn. Each visual counts as the box around it;
/// `mesh_bounds` gives a mesh file's lowest and highest corner, or `None`
/// if it can't be read, which leaves that shape out. Floating, planar and
/// spherical joints stay at rest. `None` for a robot with no shapes.
///
/// # Errors
///
/// As [`Kinematics::new`].
pub fn reach(
    robot: &RobotModel,
    mut mesh_bounds: impl FnMut(&Path) -> Option<Bounds>,
) -> Result<Option<Reach>, KaptoError> {
    let kinematics = Kinematics::new(robot)?;
    // Each link's corners: those of the box around each of its shapes, in its frame.
    let corners: Vec<Vec<[f64; 3]>> = robot
        .links
        .iter()
        .map(|link| {
            link.visuals
                .iter()
                .filter_map(|visual| {
                    let (low, high) = local_box(&visual.geometry, &mut mesh_bounds)?;
                    let origin = Transform::from_pose(&visual.origin);
                    Some(box_corners(low, high).map(|corner| origin.apply(corner)))
                })
                .flatten()
                .collect()
        })
        .collect();
    if corners.iter().all(Vec::is_empty) {
        return Ok(None);
    }
    let ranges: Vec<(f64, f64)> = kinematics
        .joint_names()
        .iter()
        .map(|name| range(robot, name))
        .collect();
    let free = vec![Transform::IDENTITY; kinematics.free_joint_names().len()];
    let mut poses = vec![Transform::IDENTITY; robot.links.len()];
    let mut values = vec![0.0; ranges.len()];
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut points = Vec::new();
    for sample in 0..=SAMPLES {
        for (value, (low, high)) in values.iter_mut().zip(&ranges) {
            // The first sample has every joint at zero, clamped into its range.
            *value = if sample == 0 {
                0.0_f64.clamp(*low, *high)
            } else {
                low + (high - low) * random.unit()
            };
        }
        kinematics.poses(&values, &free, &Transform::IDENTITY, &mut poses)?;
        for (pose, link) in poses.iter().zip(&corners) {
            points.extend(link.iter().map(|corner| pose.apply(*corner)));
        }
    }
    Ok(Some(enclosing(&points)))
}

/// The range a joint is sampled over.
fn range(robot: &RobotModel, name: &str) -> (f64, f64) {
    let Some((_, joint)) = robot.joint(name) else {
        return (0.0, 0.0);
    };
    if joint.kind == JointKind::Continuous {
        return (-PI, PI);
    }
    match joint.limits {
        Some(limits)
            if limits.lower.is_finite()
                && limits.upper.is_finite()
                && limits.lower <= limits.upper =>
        {
            (limits.lower, limits.upper)
        }
        _ => (0.0, 0.0),
    }
}

/// The box around a shape, in its own frame.
fn local_box(
    geometry: &Geometry,
    mesh_bounds: &mut impl FnMut(&Path) -> Option<Bounds>,
) -> Option<Bounds> {
    let half = |size: [f64; 3]| Some((size.map(|each| -each / 2.0), size.map(|each| each / 2.0)));
    match geometry {
        Geometry::Box { size } => half(*size),
        Geometry::Cylinder { radius, length } => half([2.0 * radius, 2.0 * radius, *length]),
        Geometry::Capsule { radius, length } => {
            half([2.0 * radius, 2.0 * radius, length + 2.0 * radius])
        }
        Geometry::Sphere { radius } => half([2.0 * radius; 3]),
        Geometry::Mesh { file, scale } => {
            let (low, high) = mesh_bounds(file.path.as_ref()?)?;
            let scaled = |corner: [f64; 3]| [0, 1, 2].map(|axis| corner[axis] * scale[axis]);
            let (low, high) = (scaled(low), scaled(high));
            // A negative scale mirrors the mesh: its corners swap.
            Some((
                [0, 1, 2].map(|axis| low[axis].min(high[axis])),
                [0, 1, 2].map(|axis| low[axis].max(high[axis])),
            ))
        }
    }
}

fn box_corners(low: [f64; 3], high: [f64; 3]) -> [[f64; 3]; 8] {
    std::array::from_fn(|corner| {
        [0, 1, 2].map(|axis| {
            if corner >> axis & 1 == 0 {
                low[axis]
            } else {
                high[axis]
            }
        })
    })
}

/// A sphere around every point: centred on the box around them, so it
/// stays put as long as the extremes do.
fn enclosing(points: &[[f64; 3]]) -> Reach {
    let (low, high) = points.iter().fold(
        ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
        |(low, high), point| {
            (
                [0, 1, 2].map(|axis| low[axis].min(point[axis])),
                [0, 1, 2].map(|axis| high[axis].max(point[axis])),
            )
        },
    );
    let center = [0, 1, 2].map(|axis| f64::midpoint(low[axis], high[axis]));
    let radius = points
        .iter()
        .map(|point| {
            let [x, y, z] = [0, 1, 2].map(|axis| point[axis] - center[axis]);
            (x * x + y * y + z * z).sqrt()
        })
        .fold(0.0, f64::max);
    Reach { center, radius }
}

/// Fixed pseudo-random numbers, so the same robot always gets the same reach.
struct XorShift(u64);

impl XorShift {
    /// The next number, from 0 to 1.
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        #[allow(
            clippy::cast_precision_loss,
            reason = "53 bits are plenty for a sample"
        )]
        let unit = (self.0 >> 11) as f64 / (1_u64 << 53) as f64;
        unit
    }
}
