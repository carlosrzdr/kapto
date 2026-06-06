// SPDX-License-Identifier: Apache-2.0

//! Robot packages: URDF and mesh loading, field-to-joint mapping, the package linter and forward kinematics.
//!
//! A robot is loaded into a [`RobotModel`], Kapto's own description of its
//! links, joints and shapes; nothing else in Kapto depends on the file
//! format it came from.

pub mod kinematics;
pub mod lint;
pub mod mapping;
pub mod mesh;
pub mod model;
pub mod package;
pub mod reach;
pub mod transform;
pub mod urdf;

pub use kinematics::Kinematics;
pub use lint::{Finding, LintReport, RULES, Rule, Severity, lint};
pub use mapping::{JointMap, MappedJoint, ValueSource};
pub use mesh::{Mesh, MeshFile, UpAxis, load_mesh};
pub use model::{
    Geometry, Joint, JointKind, Limits, Link, Material, Mimic, Pose, Resource, RobotModel, Shape,
};
pub use package::{
    MESH_EXTENSIONS, Package, PackageFile, content_hash, discover, is_mesh_or_texture, load_package,
};
pub use reach::{Bounds, Reach, reach};
pub use transform::Transform;
pub use urdf::{load_urdf, parse_urdf};
