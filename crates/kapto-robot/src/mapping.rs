// SPDX-License-Identifier: Apache-2.0

//! Checking a field-to-joint mapping against the robot and its streams, and
//! turning it into where each joint's values are found in decoded messages.

use std::path::Path;

use kapto_config::{FieldReference, JointMapping, MappingDeclaration, StreamDeclaration, closest};
use kapto_core::{ErrorCode, KaptoError};
use kapto_decode::{FieldDescription, ScalarType};

use crate::kinematics::Kinematics;
use crate::model::{JointKind, RobotModel};

/// Where each joint's values are found, one entry per joint
/// [`Kinematics::joint_names`] takes, in that order.
#[derive(Debug)]
pub struct JointMap {
    joints: Vec<MappedJoint>,
    warnings: Vec<KaptoError>,
}

/// Where one joint's position, velocity and effort come from, if mapped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MappedJoint {
    /// The joint's name.
    pub joint: String,
    /// Its position.
    pub position: Option<ValueSource>,
    /// Its velocity.
    pub velocity: Option<ValueSource>,
    /// Its torque or force.
    pub effort: Option<ValueSource>,
}

/// One decoded value: which stream, and which of its decoded values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueSource {
    /// The stream's position in the stream declarations.
    pub stream: usize,
    /// The value's position among the stream's decoded values, which come
    /// field after field, array elements in order.
    pub value: usize,
}

/// Which of a joint's quantities a field gives.
#[derive(Clone, Copy)]
enum Quantity {
    Position,
    Velocity,
    Effort,
}

impl JointMap {
    /// Checks `mapping` (read from `file`) against `robot` and the stream
    /// declarations, and works out where each joint's values are found.
    ///
    /// # Errors
    ///
    /// Every problem at once, each with the file and line:
    /// `mapping.unknown_joint` for a joint the robot doesn't have or that
    /// can't take a value (fixed, or a mimic), `mapping.unknown_field` for
    /// a stream, field or array element that isn't decoded or a field that
    /// isn't a number, and `mapping.unit_mismatch` for a field whose unit
    /// doesn't fit the joint.
    pub fn new(
        robot: &RobotModel,
        kinematics: &Kinematics,
        streams: &[StreamDeclaration],
        mapping: &MappingDeclaration,
        file: &Path,
    ) -> Result<Self, Vec<KaptoError>> {
        let mut joints: Vec<MappedJoint> = kinematics
            .joint_names()
            .iter()
            .map(|joint| MappedJoint {
                joint: joint.clone(),
                ..MappedJoint::default()
            })
            .collect();
        let mut errors = Vec::new();
        for entry in &mapping.joints {
            let located = |error: KaptoError| {
                error
                    .context("file", file.display().to_string())
                    .context("line", entry.line.to_string())
            };
            let Some(index) = joints.iter().position(|mapped| mapped.joint == entry.joint) else {
                errors.push(located(unknown_joint(robot, &entry.joint)));
                continue;
            };
            let kind = robot
                .joint(&entry.joint)
                .map_or(JointKind::Revolute, |(_, joint)| joint.kind);
            let mapped = &mut joints[index];
            for (quantity, reference, slot) in [
                (Quantity::Position, &entry.position, &mut mapped.position),
                (Quantity::Velocity, &entry.velocity, &mut mapped.velocity),
                (Quantity::Effort, &entry.effort, &mut mapped.effort),
            ] {
                if let Some(reference) = reference {
                    match source(streams, reference, entry, (kind, quantity)) {
                        Ok(source) => *slot = Some(source),
                        Err(error) => errors.push(located(error)),
                    }
                }
            }
        }
        for role in &mapping.roles {
            if let Err(error) = role_field(streams, &role.field) {
                errors.push(
                    error
                        .context("file", file.display().to_string())
                        .context("line", role.line.to_string()),
                );
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let warnings = joints
            .iter()
            .filter(|mapped| mapped.position.is_none())
            .map(|mapped| {
                KaptoError::new(ErrorCode::MappingJointUnmapped)
                    .detail(format!("joint `{}`", mapped.joint))
                    .context("file", file.display().to_string())
            })
            .collect();
        Ok(Self { joints, warnings })
    }

    /// Every joint, in [`Kinematics::joint_names`] order.
    #[must_use]
    pub fn joints(&self) -> &[MappedJoint] {
        &self.joints
    }

    /// Problems that don't stop the mapping from being used:
    /// `mapping.joint_unmapped` for each joint without a position.
    #[must_use]
    pub fn warnings(&self) -> &[KaptoError] {
        &self.warnings
    }
}

fn unknown_joint(robot: &RobotModel, name: &str) -> KaptoError {
    let error = KaptoError::new(ErrorCode::MappingUnknownJoint);
    match robot.joint(name) {
        Some((_, joint)) if joint.mimic.is_some() => {
            let copied = &robot.joints[joint.mimic.map_or(0, |mimic| mimic.joint)].name;
            error.detail(format!(
                "joint `{name}` copies joint `{copied}`; map `{copied}` instead"
            ))
        }
        Some((_, joint)) => {
            let kind = match joint.kind {
                JointKind::Fixed => "fixed",
                JointKind::Floating => "floating",
                JointKind::Planar => "planar",
                _ => "spherical",
            };
            error.detail(format!(
                "joint `{name}` is {kind}, so no single value says where it is"
            ))
        }
        None => {
            let names = robot
                .joints
                .iter()
                .filter(|joint| joint.kind.has_one_value() && joint.mimic.is_none())
                .map(|joint| joint.name.as_str());
            match closest(name, names) {
                Some(suggestion) => error.detail(format!(
                    "the robot has no joint `{name}` (did you mean `{suggestion}`?)"
                )),
                None => error.detail(format!("the robot has no joint `{name}`")),
            }
        }
    }
}

/// The stream `reference` names, by its position in `streams`, and the
/// field, by its position among the stream's decoder fields.
fn find_field(
    streams: &[StreamDeclaration],
    reference: &FieldReference,
) -> Result<(usize, Vec<FieldDescription>, usize), KaptoError> {
    let unknown = |detail: String| KaptoError::new(ErrorCode::MappingUnknownField).detail(detail);
    let Some(stream) = streams
        .iter()
        .position(|stream| stream.id.as_str() == reference.stream)
    else {
        let ids = streams.iter().map(|stream| stream.id.as_str());
        let hint = closest(&reference.stream, ids).map_or_else(String::new, |suggestion| {
            format!(" (did you mean `{suggestion}`?)")
        });
        return Err(unknown(format!(
            "`{reference}`: streams.toml declares no stream `{}`{hint}",
            reference.stream
        )));
    };
    let Some(decoder) = &streams[stream].decoder else {
        return Err(unknown(format!(
            "`{reference}`: stream `{}` has no decoder, so it has no fields",
            reference.stream
        )));
    };
    let fields = decoder.fields();
    let Some(field) = fields
        .iter()
        .position(|field| field.name == reference.field)
    else {
        let names = fields.iter().map(|field| field.name.as_str());
        let hint = closest(&reference.field, names).map_or_else(String::new, |suggestion| {
            format!(" (did you mean `{suggestion}`?)")
        });
        return Err(unknown(format!(
            "`{reference}`: stream `{}` decodes no field `{}`{hint}",
            reference.stream, reference.field
        )));
    };
    Ok((stream, fields, field))
}

/// Checks that the value `reference` names for a role is decoded: a whole
/// field, or an element of an array field. Any type plays a role, a
/// true/false flag as much as a number.
fn role_field(streams: &[StreamDeclaration], reference: &FieldReference) -> Result<(), KaptoError> {
    let (_, fields, field) = find_field(streams, reference)?;
    let spec = &fields[field];
    let wrong =
        |detail: String| Err(KaptoError::new(ErrorCode::MappingUnknownField).detail(detail));
    match (spec.count, reference.index) {
        (1, Some(_)) => wrong(format!(
            "`{reference}`: `{}` is a single value; leave out the index",
            spec.name
        )),
        (count, Some(index)) if index >= count => wrong(format!(
            "`{reference}`: `{}` holds {count} values, numbered 0 to {}",
            spec.name,
            count - 1
        )),
        _ => Ok(()),
    }
}

/// Where `reference` is found among its stream's decoded values.
fn source(
    streams: &[StreamDeclaration],
    reference: &FieldReference,
    entry: &JointMapping,
    (kind, quantity): (JointKind, Quantity),
) -> Result<ValueSource, KaptoError> {
    let unknown = |detail: String| KaptoError::new(ErrorCode::MappingUnknownField).detail(detail);
    let (stream, fields, field) = find_field(streams, reference)?;
    let spec = &fields[field];
    let element = match (spec.count, reference.index) {
        (1, None) => 0,
        (1, Some(_)) => {
            return Err(unknown(format!(
                "`{reference}`: `{}` is a single value; leave out the index",
                spec.name
            )));
        }
        (count, None) => {
            return Err(unknown(format!(
                "`{reference}`: `{}` holds {count} values; say which, e.g. `{}.{}[0]`",
                spec.name, reference.stream, spec.name
            )));
        }
        (count, Some(index)) if index >= count => {
            return Err(unknown(format!(
                "`{reference}`: `{}` holds {count} values, numbered 0 to {}",
                spec.name,
                count - 1
            )));
        }
        (_, Some(index)) => index,
    };
    if spec.ty == ScalarType::Bool {
        return Err(unknown(format!(
            "`{reference}`: `{}` is true/false, not a number",
            spec.name
        )));
    }
    if let Some(unit) = &spec.unit {
        let expected = expected_units(kind, quantity);
        if !expected.contains(&unit.as_str()) {
            return Err(
                KaptoError::new(ErrorCode::MappingUnitMismatch).detail(format!(
                    "`{reference}` is in {unit}, but joint `{}` needs {}",
                    entry.joint,
                    expected.join(" or ")
                )),
            );
        }
    }
    let before: usize = fields[..field].iter().map(|field| field.count).sum();
    Ok(ValueSource {
        stream,
        value: before + element,
    })
}

/// The units a field may declare for `quantity` of a joint of `kind`.
fn expected_units(kind: JointKind, quantity: Quantity) -> &'static [&'static str] {
    let sliding = kind == JointKind::Prismatic;
    match (quantity, sliding) {
        (Quantity::Position, false) => &["rad"],
        (Quantity::Position, true) => &["m"],
        (Quantity::Velocity, false) => &["rad/s"],
        (Quantity::Velocity, true) => &["m/s"],
        (Quantity::Effort, false) => &["N·m", "N m", "Nm"],
        (Quantity::Effort, true) => &["N"],
    }
}
