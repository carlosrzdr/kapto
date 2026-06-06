// SPDX-License-Identifier: Apache-2.0

//! The robot model: links, the joints between them, and what each link looks
//! like. Everything that needs the robot's shape (the viewer, forward
//! kinematics, the linter, checks) uses this model, whatever file it was
//! loaded from.

use std::path::PathBuf;

/// A robot: a tree of links connected by joints, from one root link.
#[derive(Clone, Debug, PartialEq)]
pub struct RobotModel {
    /// The robot's name.
    pub name: String,
    /// Every link. Joints refer to links by their position in this list.
    pub links: Vec<Link>,
    /// Every joint, in the order they were declared.
    pub joints: Vec<Joint>,
    /// The link every other link hangs from.
    pub root: usize,
}

/// A rigid body of the robot.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The link's name, unique in the robot.
    pub name: String,
    /// What it looks like.
    pub visuals: Vec<Shape>,
    /// The simpler shapes used for contact and distance checks.
    pub collisions: Vec<Shape>,
}

/// A shape attached to a link, placed relative to the link's frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    /// The shape's name, if it has one.
    pub name: Option<String>,
    /// Where the shape sits in the link's frame.
    pub origin: Pose,
    /// The shape itself.
    pub geometry: Geometry,
    /// Its colour or texture, if given. Collision shapes have none.
    pub material: Option<Material>,
}

/// A geometric shape. Lengths are in metres.
#[derive(Clone, Debug, PartialEq)]
pub enum Geometry {
    /// A box centred on its origin.
    Box {
        /// Its size along x, y and z.
        size: [f64; 3],
    },
    /// A cylinder centred on its origin, along z.
    Cylinder {
        /// Its radius.
        radius: f64,
        /// Its length along z.
        length: f64,
    },
    /// A cylinder with half-spheres on both ends, centred on its origin,
    /// along z.
    Capsule {
        /// Its radius.
        radius: f64,
        /// The length of its cylindrical part.
        length: f64,
    },
    /// A sphere centred on its origin.
    Sphere {
        /// Its radius.
        radius: f64,
    },
    /// A mesh from a file.
    Mesh {
        /// The mesh file.
        file: Resource,
        /// Scale along x, y and z, applied to the mesh's coordinates.
        scale: [f64; 3],
    },
}

/// A file the robot description refers to, such as a mesh or a texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The reference as written, e.g. `package://arm/meshes/base.stl`.
    pub uri: String,
    /// The file it was found at, or `None` if it wasn't found.
    pub path: Option<PathBuf>,
}

/// A shape's colour or texture.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    /// The material's name, if it has one.
    pub name: Option<String>,
    /// Red, green, blue and alpha, each from 0 to 1, if given.
    pub rgba: Option<[f64; 4]>,
    /// An image wrapped around the shape, if given.
    pub texture: Option<Resource>,
}

/// A joint between a parent and a child link.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    /// The joint's name, unique in the robot.
    pub name: String,
    /// How the child may move relative to the parent.
    pub kind: JointKind,
    /// The parent link's position in [`RobotModel::links`].
    pub parent: usize,
    /// The child link's position in [`RobotModel::links`].
    pub child: usize,
    /// The joint frame in the parent link's frame. At a joint value of 0 it
    /// is also the child link's frame.
    pub origin: Pose,
    /// The axis it turns about or slides along, in the joint frame, of
    /// length 1.
    pub axis: [f64; 3],
    /// How far it may move, for revolute and prismatic joints.
    pub limits: Option<Limits>,
    /// The joint whose value this one copies, if it does.
    pub mimic: Option<Mimic>,
}

/// How a joint's child may move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JointKind {
    /// Turns about the axis, within limits.
    Revolute,
    /// Turns about the axis without limits, like a wheel.
    Continuous,
    /// Slides along the axis, within limits.
    Prismatic,
    /// Doesn't move.
    Fixed,
    /// Moves freely in all six directions; its pose comes from elsewhere,
    /// e.g. a robot base's odometry.
    Floating,
    /// Moves in the plane perpendicular to the axis.
    Planar,
    /// Turns about any axis through its origin.
    Spherical,
}

impl JointKind {
    /// Whether a single value says where the joint is: revolute,
    /// continuous and prismatic joints.
    #[must_use]
    pub fn has_one_value(self) -> bool {
        matches!(self, Self::Revolute | Self::Continuous | Self::Prismatic)
    }
}

/// How far a joint may move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits {
    /// The lowest value, in radians or metres; negative infinity if none.
    pub lower: f64,
    /// The highest value, in radians or metres; infinity if none.
    pub upper: f64,
    /// The largest speed, in radians or metres per second.
    pub velocity: f64,
    /// The largest torque or force, in newton-metres or newtons.
    pub effort: f64,
}

/// A joint that copies another joint's value: `value = multiplier × other +
/// offset`, as for the two fingers of a gripper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mimic {
    /// The copied joint's position in [`RobotModel::joints`].
    pub joint: usize,
    /// 1 when not given.
    pub multiplier: f64,
    /// 0 when not given.
    pub offset: f64,
}

/// A position and orientation: translate by `xyz`, then rotate by `rpy`
/// (roll about x, then pitch about y, then yaw about z, all about the fixed
/// axes). Metres and radians.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    /// Translation, in metres.
    pub xyz: [f64; 3],
    /// Roll, pitch and yaw, in radians.
    pub rpy: [f64; 3],
}

impl RobotModel {
    /// The link called `name`, with its position in [`RobotModel::links`].
    #[must_use]
    pub fn link(&self, name: &str) -> Option<(usize, &Link)> {
        self.links
            .iter()
            .enumerate()
            .find(|(_, link)| link.name == name)
    }

    /// The joint called `name`, with its position in [`RobotModel::joints`].
    #[must_use]
    pub fn joint(&self, name: &str) -> Option<(usize, &Joint)> {
        self.joints
            .iter()
            .enumerate()
            .find(|(_, joint)| joint.name == name)
    }

    /// The joints a single value positions and that don't copy another
    /// joint: the ones a robot reports, in declaration order.
    pub fn actuated_joints(&self) -> impl Iterator<Item = &Joint> {
        self.joints
            .iter()
            .filter(|joint| joint.kind.has_one_value() && joint.mimic.is_none())
    }

    /// Every file referred to but not found, with the link it belongs to.
    pub fn missing_files(&self) -> impl Iterator<Item = (&Link, &Resource)> {
        self.links.iter().flat_map(|link| {
            link.visuals
                .iter()
                .chain(&link.collisions)
                .flat_map(|shape| {
                    let mesh = match &shape.geometry {
                        Geometry::Mesh { file, .. } => Some(file),
                        _ => None,
                    };
                    let texture = shape.material.as_ref().and_then(|m| m.texture.as_ref());
                    mesh.into_iter().chain(texture)
                })
                .filter(|resource| resource.path.is_none())
                .map(move |resource| (link, resource))
        })
    }
}
