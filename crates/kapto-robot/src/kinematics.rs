// SPDX-License-Identifier: Apache-2.0

//! Forward kinematics: where every link is, given the joint values.
//!
//! The tree is walked once, when the robot is loaded, into a list of steps
//! from the root outwards, with every mimic joint resolved to the value it
//! copies. Computing poses is then one pass over that list: no searching,
//! no allocation, the same result for the same input on every machine.

use std::collections::VecDeque;

use kapto_core::{ErrorCode, KaptoError};

use crate::model::{JointKind, RobotModel};
use crate::transform::Transform;

/// A robot's forward kinematics, ready to compute.
#[derive(Clone, Debug)]
pub struct Kinematics {
    links: usize,
    root: usize,
    steps: Vec<Step>,
    joint_names: Vec<String>,
    free_joint_names: Vec<String>,
}

/// One joint: the child's pose is the parent's, then `origin`, then the
/// joint's motion.
#[derive(Clone, Debug)]
struct Step {
    parent: usize,
    child: usize,
    origin: Transform,
    axis: [f64; 3],
    motion: Motion,
}

#[derive(Clone, Copy, Debug)]
enum Motion {
    Fixed,
    Turn(Value),
    Slide(Value),
    /// A floating, planar or spherical joint, whose pose is given directly.
    Free(usize),
}

/// A joint's value: `multiplier × values[input] + offset`; for a joint that
/// isn't a mimic, multiplier 1 and offset 0.
#[derive(Clone, Copy, Debug)]
struct Value {
    input: usize,
    multiplier: f64,
    offset: f64,
}

impl Kinematics {
    /// Prepares `robot`'s kinematics.
    ///
    /// # Errors
    ///
    /// `robot.kinematic_tree_invalid` if mimic joints copy each other in a
    /// loop, so no value can be worked out.
    pub fn new(robot: &RobotModel) -> Result<Self, KaptoError> {
        let mut inputs = vec![None; robot.joints.len()];
        let mut joint_names = Vec::new();
        let mut free_inputs = vec![None; robot.joints.len()];
        let mut free_joint_names = Vec::new();
        for (index, joint) in robot.joints.iter().enumerate() {
            if joint.kind.has_one_value() && joint.mimic.is_none() {
                inputs[index] = Some(joint_names.len());
                joint_names.push(joint.name.clone());
            } else if matches!(
                joint.kind,
                JointKind::Floating | JointKind::Planar | JointKind::Spherical
            ) {
                free_inputs[index] = Some(free_joint_names.len());
                free_joint_names.push(joint.name.clone());
            }
        }

        let mut children = vec![Vec::new(); robot.links.len()];
        for (index, joint) in robot.joints.iter().enumerate() {
            children[joint.parent].push(index);
        }
        let mut steps = Vec::with_capacity(robot.joints.len());
        let mut queue = VecDeque::from([robot.root]);
        while let Some(link) = queue.pop_front() {
            for &index in &children[link] {
                let joint = &robot.joints[index];
                let motion = match joint.kind {
                    JointKind::Revolute | JointKind::Continuous => {
                        Motion::Turn(value(robot, index, &inputs)?)
                    }
                    JointKind::Prismatic => Motion::Slide(value(robot, index, &inputs)?),
                    JointKind::Fixed => Motion::Fixed,
                    JointKind::Floating | JointKind::Planar | JointKind::Spherical => {
                        Motion::Free(free_inputs[index].unwrap_or(0))
                    }
                };
                steps.push(Step {
                    parent: joint.parent,
                    child: joint.child,
                    origin: Transform::from_pose(&joint.origin),
                    axis: joint.axis,
                    motion,
                });
                queue.push_back(joint.child);
            }
        }
        Ok(Self {
            links: robot.links.len(),
            root: robot.root,
            steps,
            joint_names,
            free_joint_names,
        })
    }

    /// The joints whose values [`Kinematics::poses`] takes, in that order:
    /// the revolute, continuous and prismatic joints that don't mimic
    /// another, in declaration order.
    #[must_use]
    pub fn joint_names(&self) -> &[String] {
        &self.joint_names
    }

    /// The floating, planar and spherical joints, whose poses
    /// [`Kinematics::poses`] takes directly, in declaration order.
    #[must_use]
    pub fn free_joint_names(&self) -> &[String] {
        &self.free_joint_names
    }

    /// Every link's pose in the frame `base` maps into, given `values` (one
    /// per [`Kinematics::joint_names`], radians or metres) and `free` (one
    /// per [`Kinematics::free_joint_names`], or none for all at rest).
    /// `poses` is indexed like the robot's links. Never allocates.
    ///
    /// ```
    /// use std::path::Path;
    ///
    /// use kapto_robot::{Kinematics, Transform, parse_urdf};
    ///
    /// let urdf = r#"
    ///     <robot name="pendulum">
    ///       <link name="base"/><link name="arm"/>
    ///       <joint name="swing" type="revolute">
    ///         <parent link="base"/><child link="arm"/>
    ///         <origin xyz="0 0 1"/><axis xyz="0 0 1"/>
    ///         <limit lower="-3" upper="3" effort="1" velocity="1"/>
    ///       </joint>
    ///     </robot>"#;
    /// let robot = parse_urdf(urdf, Path::new("pendulum.urdf"), &[]).unwrap();
    /// let kinematics = Kinematics::new(&robot).unwrap();
    /// let mut poses = vec![Transform::IDENTITY; robot.links.len()];
    /// kinematics
    ///     .poses(&[std::f64::consts::FRAC_PI_2], &[], &Transform::IDENTITY, &mut poses)
    ///     .unwrap();
    /// let arm = poses[robot.link("arm").unwrap().0];
    /// assert_eq!(arm.translation(), [0.0, 0.0, 1.0]);
    /// assert!((arm.apply([1.0, 0.0, 0.0])[1] - 1.0).abs() < 1e-12);
    /// ```
    ///
    /// # Errors
    ///
    /// `robot.joint_values_mismatch` if `values`, `free` or `poses` has the
    /// wrong length.
    pub fn poses(
        &self,
        values: &[f64],
        free: &[Transform],
        base: &Transform,
        poses: &mut [Transform],
    ) -> Result<(), KaptoError> {
        self.check(values.len(), free.len(), poses.len())?;
        poses[self.root] = *base;
        for step in &self.steps {
            let placed = poses[step.parent].then(&step.origin);
            poses[step.child] = match step.motion {
                // Nothing moves: composing with no motion would only cost time.
                Motion::Fixed => placed,
                Motion::Turn(value) => {
                    placed.then(&Transform::rotation_about(step.axis, value.of(values)))
                }
                Motion::Slide(value) => {
                    placed.then(&Transform::translation_along(step.axis, value.of(values)))
                }
                Motion::Free(index) => placed.then(&free.get(index).copied().unwrap_or_default()),
            };
        }
        Ok(())
    }

    /// Poses for many samples at once: `rows` holds one sample's
    /// [`Kinematics::joint_names`] values after another, and `poses` gets
    /// one robot's worth of link poses per sample, relative to the root.
    /// Free joints are at rest.
    ///
    /// # Errors
    ///
    /// `robot.joint_values_mismatch` if `rows` isn't a whole number of
    /// samples, or `poses` doesn't hold one robot per sample.
    pub fn poses_for_rows(&self, rows: &[f64], poses: &mut [Transform]) -> Result<(), KaptoError> {
        let width = self.joint_names.len();
        let samples = rows.len().checked_div(width).unwrap_or(0);
        if (width == 0 && !rows.is_empty())
            || samples * width != rows.len()
            || poses.len() != samples * self.links
        {
            return Err(mismatch(format!(
                "{} values and {} poses don't make whole samples of {width} joints and {} links",
                rows.len(),
                poses.len(),
                self.links
            )));
        }
        for (values, sample) in rows
            .chunks_exact(width.max(1))
            .zip(poses.chunks_exact_mut(self.links.max(1)))
        {
            self.poses(values, &[], &Transform::IDENTITY, sample)?;
        }
        Ok(())
    }

    fn check(&self, values: usize, free: usize, poses: usize) -> Result<(), KaptoError> {
        let free_ok = free == 0 || free == self.free_joint_names.len();
        if values == self.joint_names.len() && free_ok && poses == self.links {
            return Ok(());
        }
        Err(mismatch(format!(
            "got {values} joint values, {free} free joint poses and room for {poses} link poses; the robot has {} joints, {} free joints and {} links",
            self.joint_names.len(),
            self.free_joint_names.len(),
            self.links
        )))
    }
}

impl Value {
    fn of(self, values: &[f64]) -> f64 {
        self.multiplier * values[self.input] + self.offset
    }
}

/// The value of joint `index`, following mimic joints to the joint they
/// finally copy.
fn value(robot: &RobotModel, index: usize, inputs: &[Option<usize>]) -> Result<Value, KaptoError> {
    let (mut multiplier, mut offset) = (1.0, 0.0);
    let mut joint = index;
    for _ in 0..=robot.joints.len() {
        if let Some(input) = inputs[joint] {
            return Ok(Value {
                input,
                multiplier,
                offset,
            });
        }
        let Some(mimic) = robot.joints[joint].mimic else {
            break;
        };
        // Copying `a × other + b` of a joint that is itself `c × x + d`.
        offset += multiplier * mimic.offset;
        multiplier *= mimic.multiplier;
        joint = mimic.joint;
    }
    Err(KaptoError::new(ErrorCode::RobotKinematicTreeInvalid).detail(format!(
        "joint `{}` mimics joints that mimic each other in a loop, so its value can't be worked out",
        robot.joints[index].name
    )))
}

fn mismatch(detail: String) -> KaptoError {
    KaptoError::new(ErrorCode::RobotJointValuesMismatch).detail(detail)
}
