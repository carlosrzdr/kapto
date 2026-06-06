// SPDX-License-Identifier: Apache-2.0

//! Connecting links by their joints, and checking that they form one tree.

use std::collections::HashMap;

use kapto_core::{ErrorCode, KaptoError};

use super::RawJoint;
use crate::model::{Joint, JointKind, Limits, Link, Mimic, RobotModel};

/// The model, or `None` with at least one error added to `errors`.
/// `complete` says whether every link and joint in the file could be read.
pub(super) fn build(
    name: String,
    links: Vec<(Link, u32)>,
    joints: &[RawJoint],
    complete: bool,
    errors: &mut Vec<KaptoError>,
) -> Option<RobotModel> {
    let mut problems = Problems(errors);
    let mut link_index = HashMap::new();
    for (index, (link, line)) in links.iter().enumerate() {
        if link_index.insert(link.name.as_str(), index).is_some() {
            problems.add(
                *line,
                format!("link `{}` is declared more than once", link.name),
            );
        }
    }
    let mut joint_index = HashMap::new();
    for (index, joint) in joints.iter().enumerate() {
        if joint_index.insert(joint.name.as_str(), index).is_some() {
            problems.add(
                joint.line,
                format!("joint `{}` is declared more than once", joint.name),
            );
        }
    }
    let names_unique = link_index.len() == links.len();
    let built: Vec<Option<Joint>> = joints
        .iter()
        .map(|joint| problems.joint(joint, &link_index, &joint_index, joints))
        .collect();
    let links: Vec<Link> = links.into_iter().map(|(link, _)| link).collect();
    let all_joints_valid = built.iter().all(Option::is_some);
    let joints: Vec<Joint> = built.into_iter().flatten().collect();
    // Two parents is checked on the joints that could be read; a root or a
    // loop only once everything could be.
    let complete = complete && names_unique && all_joints_valid;
    let root = problems.root(&links, &joints, complete)?;
    Some(RobotModel {
        name,
        links,
        joints,
        root,
    })
}

struct Problems<'e>(&'e mut Vec<KaptoError>);

impl Problems<'_> {
    fn add(&mut self, line: u32, detail: String) {
        self.0.push(
            KaptoError::new(ErrorCode::RobotKinematicTreeInvalid)
                .detail(detail)
                .context("line", line.to_string()),
        );
    }

    fn add_unlocated(&mut self, detail: String) {
        self.0
            .push(KaptoError::new(ErrorCode::RobotKinematicTreeInvalid).detail(detail));
    }

    fn joint(
        &mut self,
        joint: &RawJoint,
        links: &HashMap<&str, usize>,
        joint_index: &HashMap<&str, usize>,
        joints: &[RawJoint],
    ) -> Option<Joint> {
        let (name, line) = (&joint.name, joint.line);
        let mut find_link = |role: &str, link: &str| {
            let found = links.get(link).copied();
            if found.is_none() {
                self.add(
                    line,
                    format!("joint `{name}` has {role} link `{link}`, which isn't declared"),
                );
            }
            found
        };
        let parent = find_link("parent", &joint.parent);
        let child = find_link("child", &joint.child);
        if parent.is_some() && parent == child {
            self.add(
                line,
                format!("joint `{name}` connects link `{}` to itself", joint.parent),
            );
        }
        let axis = self.axis(joint);
        let limits = joint.limits.map(|[lower, upper, velocity, effort]| Limits {
            lower,
            upper,
            velocity,
            effort,
        });
        if let Some(limits) = limits
            && limits.lower > limits.upper
        {
            self.add(
                line,
                format!(
                    "joint `{name}` has a lower limit ({}) above its upper limit ({})",
                    limits.lower, limits.upper
                ),
            );
        }
        let mimic = match &joint.mimic {
            // A fixed joint has no value to copy into, so its mimic says
            // nothing; descriptions made from one template for several
            // hand variants carry them, and urdfdom ignores them too.
            Some(_) if joint.kind == JointKind::Fixed => None,
            Some((copied, multiplier, offset)) => {
                let index = joint_index.get(copied.as_str()).copied();
                let copied_kind = index.map(|index| joints[index].kind);
                if index.is_none() {
                    self.add(
                        line,
                        format!("joint `{name}` mimics joint `{copied}`, which isn't declared"),
                    );
                } else if copied == name {
                    self.add(line, format!("joint `{name}` mimics itself"));
                } else if !joint.kind.has_one_value()
                    || !copied_kind.is_some_and(JointKind::has_one_value)
                {
                    self.add(
                        line,
                        format!(
                            "joint `{name}` mimics joint `{copied}`, but only revolute, continuous and prismatic joints can mimic or be mimicked"
                        ),
                    );
                }
                Some(Mimic {
                    joint: index?,
                    multiplier: *multiplier,
                    offset: *offset,
                })
            }
            None => None,
        };
        Some(Joint {
            name: name.clone(),
            kind: joint.kind,
            parent: parent?,
            child: child?,
            origin: joint.origin,
            axis: axis?,
            limits,
            mimic,
        })
    }

    /// The axis scaled to length 1. A joint that moves needs one that isn't
    /// zero.
    fn axis(&mut self, joint: &RawJoint) -> Option<[f64; 3]> {
        let axis = joint.axis;
        if let Some(unit) = nalgebra::Vector3::from(axis).try_normalize(0.0) {
            return Some(unit.into());
        }
        if matches!(
            joint.kind,
            JointKind::Fixed | JointKind::Floating | JointKind::Spherical
        ) {
            // The axis means nothing for these joints.
            return Some([1.0, 0.0, 0.0]);
        }
        self.add(
            joint.line,
            format!(
                "joint `{}` has the axis {axis:?}, which has no direction",
                joint.name
            ),
        );
        None
    }

    /// The one link without a parent, after checking that every link hangs
    /// from it exactly once. Roots and loops are only looked for when
    /// `complete`: with two links of one name, or a joint that couldn't be
    /// read, every count would be misleading.
    fn root(&mut self, links: &[Link], joints: &[Joint], complete: bool) -> Option<usize> {
        if links.is_empty() {
            self.add_unlocated("the robot has no links".to_owned());
            return None;
        }
        let mut parent_of: Vec<Option<&Joint>> = vec![None; links.len()];
        for joint in joints {
            if let Some(earlier) = parent_of[joint.child] {
                self.add_unlocated(format!(
                    "link `{}` is the child of both `{}` and `{}`: a link has one parent joint",
                    links[joint.child].name, earlier.name, joint.name
                ));
            } else {
                parent_of[joint.child] = Some(joint);
            }
        }
        if !complete {
            return None;
        }
        let roots: Vec<usize> = (0..links.len())
            .filter(|&i| parent_of[i].is_none())
            .collect();
        let root = match roots.as_slice() {
            [root] => *root,
            [] => {
                self.add_unlocated(
                    "every link has a parent joint, so the joints form a loop: one link must be the root"
                        .to_owned(),
                );
                return None;
            }
            several => {
                let names: Vec<String> = several
                    .iter()
                    .map(|&i| format!("`{}`", links[i].name))
                    .collect();
                self.add_unlocated(format!(
                    "links {} have no parent joint: the robot must be one tree with one root link",
                    names.join(", ")
                ));
                return None;
            }
        };
        // With one parent per link and one root, a link that can't reach the
        // root by following parents is part of a loop.
        for start in 0..links.len() {
            let mut link = start;
            let mut steps = 0;
            while let Some(joint) = parent_of[link] {
                link = joint.parent;
                steps += 1;
                if steps > links.len() {
                    self.add_unlocated(format!(
                        "link `{}` is part of a loop of joints",
                        links[start].name
                    ));
                    return None;
                }
            }
        }
        Some(root)
    }
}
