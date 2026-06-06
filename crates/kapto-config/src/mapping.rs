// SPDX-License-Identifier: Apache-2.0

//! Field-to-joint mapping: which decoded value drives which joint, and which
//! plays which role, from a robot package's `mapping.toml`.
//!
//! ```toml
//! [joints]
//! # A joint's position, as `<stream>.<field>` or `<stream>.<field>[<index>]`.
//! joint_1 = "arm_joints.joint_positions[0]"
//! # Position, and optionally velocity and effort.
//! joint_2 = { position = "arm_joints.joint_positions[1]", velocity = "arm_joints.joint_velocities[1]" }
//!
//! [roles]
//! # What a value is for, whatever the robot calls it.
//! "gripper.opening" = "arm_joints.gripper"
//! ```
//!
//! Every mapped joint plays a role without being written in `[roles]`:
//! `joints.<joint>.position`, `.velocity` and `.effort` ([`joint_role`]).
//!
//! This module only reads the file. Whether the joints and fields exist is
//! checked against the robot and its streams by `kapto-robot`.

use std::fmt;
use std::str::FromStr;

use kapto_core::{ErrorCode, KaptoError, Role};
use toml_edit::{Document, Item, TableLike, Value};

use crate::file::ConfigFile;
use crate::located::{self, at_key, at_line, line_of, syntax_error};
use crate::suggest::closest;

/// A whole `mapping.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MappingDeclaration {
    /// One entry per mapped joint, in file order.
    pub joints: Vec<JointMapping>,
    /// One entry per role written in `[roles]`, in file order.
    pub roles: Vec<RoleMapping>,
}

/// Which decoded value plays one role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleMapping {
    /// The role, e.g. `gripper.opening`.
    pub role: Role,
    /// The value: a field, or one element of an array field; a whole array
    /// field gives the role one value per element.
    pub field: FieldReference,
    /// The line of the entry, for error messages.
    pub line: usize,
}

/// The first segment of the roles every mapped joint plays.
pub const JOINT_ROLES: &str = "joints";

impl MappingDeclaration {
    /// Every role and the value playing it: those of `[roles]`, then each
    /// mapped joint's position, velocity and effort.
    #[must_use]
    pub fn all_roles(&self) -> Vec<(Role, FieldReference)> {
        let written = self
            .roles
            .iter()
            .map(|mapping| (mapping.role.clone(), mapping.field.clone()));
        let joints = self.joints.iter().flat_map(|joint| {
            [
                ("position", &joint.position),
                ("velocity", &joint.velocity),
                ("effort", &joint.effort),
            ]
            .into_iter()
            .filter_map(|(quantity, field)| {
                Some((joint_role(&joint.joint, quantity)?, field.clone()?))
            })
        });
        written.chain(joints).collect()
    }
}

/// The role a joint's `quantity` (`position`, `velocity` or `effort`)
/// plays: `joints.<joint>.<quantity>`, with the joint's name in lowercase
/// and any character other than letters, digits and underscores as `_`,
/// after `joint_` if it doesn't start with a letter.
#[must_use]
pub fn joint_role(joint: &str, quantity: &str) -> Option<Role> {
    let mut name: String = joint
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        name.insert_str(0, "joint_");
    }
    format!("{JOINT_ROLES}.{name}.{quantity}").parse().ok()
}

/// Where one joint's values come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JointMapping {
    /// The joint's name in the robot description.
    pub joint: String,
    /// Its position, in radians or metres.
    pub position: Option<FieldReference>,
    /// Its velocity, in radians or metres per second.
    pub velocity: Option<FieldReference>,
    /// Its torque or force, in newton-metres or newtons.
    pub effort: Option<FieldReference>,
    /// The line of the entry, for error messages.
    pub line: usize,
}

/// One decoded value: a stream's field, and for an array field the element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldReference {
    /// The stream's ID.
    pub stream: String,
    /// The field's name in the stream's decoder.
    pub field: String,
    /// The element of an array field, counting from 0.
    pub index: Option<usize>,
}

impl FromStr for FieldReference {
    type Err = ();

    /// `stream.field` or `stream.field[3]`.
    fn from_str(text: &str) -> Result<Self, ()> {
        let (stream, rest) = text.split_once('.').ok_or(())?;
        let (field, index) = match rest.split_once('[') {
            Some((field, index)) => {
                let index = index.strip_suffix(']').ok_or(())?;
                (field, Some(index.parse().map_err(|_| ())?))
            }
            None => (rest, None),
        };
        let name = |part: &str| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if !name(stream) || !name(field) {
            return Err(());
        }
        Ok(Self {
            stream: stream.to_owned(),
            field: field.to_owned(),
            index,
        })
    }
}

impl fmt::Display for FieldReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.stream, self.field)?;
        if let Some(index) = self.index {
            write!(f, "[{index}]")?;
        }
        Ok(())
    }
}

const ROOT_KEYS: &[&str] = &["joints", "roles"];
const JOINT_KEYS: &[&str] = &["position", "velocity", "effort"];

/// Reads the mapping in `file`.
///
/// # Errors
///
/// Every problem found, each with the file and line: invalid TOML
/// (`config.invalid_syntax`), unknown keys (`mapping.unknown_key`), and
/// values that aren't field references (`config.invalid_value`).
pub fn parse_mapping(file: &ConfigFile) -> Result<MappingDeclaration, Vec<KaptoError>> {
    let document = Document::parse(file.contents.as_str())
        .map_err(|error| vec![syntax_error(file, &error)])?;
    let mut errors = Vec::new();
    let mut mapping = MappingDeclaration::default();
    let root = document.as_table();
    for (key, _) in root {
        if !ROOT_KEYS.contains(&key) {
            errors.push(unknown_key(file, root, key, key, ROOT_KEYS));
        }
    }
    let joints = match root.get("joints") {
        None => None,
        Some(item) => {
            let table = item.as_table_like();
            if table.is_none() {
                errors.push(invalid(
                    file,
                    root,
                    "joints",
                    "`joints` must be a table".to_owned(),
                ));
            }
            table
        }
    };
    for (joint, item) in joints.iter().flat_map(|table| table.iter()) {
        let table = joints.unwrap_or(root);
        let line = line_of(file, table, joint);
        let entry = joint_entry(file, (joint, line), item, &mut errors);
        mapping.joints.extend(entry);
    }
    match root.get("roles").map(|item| (item, item.as_table_like())) {
        None => {}
        Some((_, Some(roles))) => role_entries(file, roles, "", &mut mapping.roles, &mut errors),
        Some((_, None)) => errors.push(invalid(
            file,
            root,
            "roles",
            "`roles` must be a table".to_owned(),
        )),
    }
    if errors.is_empty() {
        Ok(mapping)
    } else {
        Err(errors)
    }
}

/// One entry of `[joints]`: a field reference (the position), or a table of
/// position, velocity and effort.
fn joint_entry(
    file: &ConfigFile,
    (joint, line): (&str, usize),
    item: &Item,
    errors: &mut Vec<KaptoError>,
) -> Option<JointMapping> {
    if let Some(value) = item.as_value().filter(|value| value.is_str()) {
        return Some(JointMapping {
            joint: joint.to_owned(),
            position: reference(file, line, (joint, None), value, errors),
            velocity: None,
            effort: None,
            line,
        });
    }
    let Some(fields) = item.as_table_like() else {
        errors.push(at_line(
            KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                "`joints.{joint}` must name a decoded field, or be a table with `position`, `velocity` and `effort`"
            )),
            file,
            line,
        ));
        return None;
    };
    for (key, _) in fields.iter() {
        if !JOINT_KEYS.contains(&key) {
            let full = format!("joints.{joint}.{key}");
            errors.push(unknown_key(file, fields, key, &full, JOINT_KEYS));
        }
    }
    if fields.is_empty() {
        errors.push(at_line(
            KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                "`joints.{joint}` needs `position`, `velocity` or `effort`"
            )),
            file,
            line,
        ));
    }
    let mut get = |key: &str| {
        let value = fields.get(key).and_then(Item::as_value)?;
        reference(file, line, (joint, Some(key)), value, errors)
    };
    let (position, velocity, effort) = (get("position"), get("velocity"), get("effort"));
    Some(JointMapping {
        joint: joint.to_owned(),
        position,
        velocity,
        effort,
        line,
    })
}

/// The entries of `table`, the part of `[roles]` under `prefix`: a role
/// written `"gripper.opening" = …` or `gripper.opening = …`, or in a table
/// `[roles.gripper]`, is the same role.
fn role_entries(
    file: &ConfigFile,
    table: &dyn TableLike,
    prefix: &str,
    roles: &mut Vec<RoleMapping>,
    errors: &mut Vec<KaptoError>,
) {
    for (key, item) in table.iter() {
        let name = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some(inner) = item.as_table_like() {
            role_entries(file, inner, &name, roles, errors);
            continue;
        }
        let line = line_of(file, table, key);
        let problem = |detail: String| {
            at_line(
                KaptoError::new(ErrorCode::ConfigInvalidValue).detail(detail),
                file,
                line,
            )
        };
        let Ok(role) = name.parse::<Role>() else {
            errors.push(problem(format!(
                "`roles.{name}`: a role is two or more dot-separated names of lowercase letters, digits and underscores, e.g. \"gripper.opening\""
            )));
            continue;
        };
        if role.segments().next() == Some(JOINT_ROLES) {
            errors.push(problem(format!(
                "`roles.{name}`: roles under `{JOINT_ROLES}.` are each mapped joint's, from `[joints]`"
            )));
            continue;
        }
        if roles.iter().any(|mapped| mapped.role == role) {
            errors.push(problem(format!("`roles.{name}` is mapped twice")));
            continue;
        }
        match item.as_value().and_then(Value::as_str).map(str::parse) {
            Some(Ok(field)) => roles.push(RoleMapping { role, field, line }),
            _ => errors.push(problem(format!(
                "`roles.{name}` must name a decoded field, e.g. \"arm_joints.gripper\""
            ))),
        }
    }
}

/// The field `value` names, for `quantity` of `joint` (`None` for the
/// plain text form, which is the joint's position); `None` (an error) if it
/// doesn't name one.
fn reference(
    file: &ConfigFile,
    line: usize,
    (joint, quantity): (&str, Option<&str>),
    value: &Value,
    errors: &mut Vec<KaptoError>,
) -> Option<FieldReference> {
    let parsed = value.as_str().and_then(|text| text.parse().ok());
    if parsed.is_none() {
        let path = match quantity {
            Some(quantity) => format!("joints.{joint}.{quantity}"),
            None => format!("joints.{joint}"),
        };
        errors.push(at_line(
            KaptoError::new(ErrorCode::ConfigInvalidValue).detail(format!(
                "`{path}` must name a decoded field, e.g. \"arm_joints.joint_positions[0]\""
            )),
            file,
            line,
        ));
    }
    parsed
}

fn unknown_key(
    file: &ConfigFile,
    table: &dyn TableLike,
    key: &str,
    full: &str,
    known: &[&str],
) -> KaptoError {
    let suggestion = closest(key, known.iter().copied());
    let error = located::unknown_key(ErrorCode::MappingUnknownKey, full, suggestion);
    at_key(error, file, table, key)
}

fn invalid(file: &ConfigFile, table: &dyn TableLike, key: &str, detail: String) -> KaptoError {
    at_key(
        KaptoError::new(ErrorCode::ConfigInvalidValue).detail(detail),
        file,
        table,
        key,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn parse(contents: &str) -> Result<MappingDeclaration, Vec<KaptoError>> {
        parse_mapping(&ConfigFile {
            path: PathBuf::from("robot/mapping.toml"),
            contents: contents.to_owned(),
        })
    }

    fn problems(contents: &str) -> Vec<String> {
        parse(contents)
            .unwrap_err()
            .iter()
            .map(|error| {
                let line = error
                    .context_entries()
                    .iter()
                    .find(|(key, _)| *key == "line")
                    .map_or("?", |(_, line)| line.as_str());
                format!("{} {line}: {}", error.code(), error.message())
            })
            .collect()
    }

    fn field(stream: &str, field: &str, index: Option<usize>) -> FieldReference {
        FieldReference {
            stream: stream.to_owned(),
            field: field.to_owned(),
            index,
        }
    }

    #[test]
    fn joints_name_their_fields_as_text_or_with_velocity_and_effort() {
        let mapping = parse(
            "[joints]\n\
             shoulder = \"arm.joint_positions[0]\"\n\
             gripper = { position = \"hand.width\", effort = \"hand.force\" }\n\
             [joints.elbow]\n\
             position = \"arm.joint_positions[1]\"\n\
             velocity = \"arm.joint_velocities[1]\"\n",
        )
        .unwrap();
        assert_eq!(
            mapping.joints,
            [
                JointMapping {
                    joint: "shoulder".to_owned(),
                    position: Some(field("arm", "joint_positions", Some(0))),
                    velocity: None,
                    effort: None,
                    line: 2,
                },
                JointMapping {
                    joint: "gripper".to_owned(),
                    position: Some(field("hand", "width", None)),
                    velocity: None,
                    effort: Some(field("hand", "force", None)),
                    line: 3,
                },
                JointMapping {
                    joint: "elbow".to_owned(),
                    position: Some(field("arm", "joint_positions", Some(1))),
                    velocity: Some(field("arm", "joint_velocities", Some(1))),
                    effort: None,
                    line: 4,
                },
            ]
        );
    }

    #[test]
    fn roles_are_written_any_way_toml_allows_and_every_joint_plays_its_own() {
        let mapping = parse(
            "[joints]\n\
             shoulder = { position = \"arm.joint_positions[0]\", effort = \"arm.torques[0]\" }\n\
             Elbow-2 = \"arm.joint_positions[1]\"\n\
             3rd = \"arm.joint_positions[2]\"\n\
             [roles]\n\
             \"gripper.opening\" = \"hand.width\"\n\
             wrench.left = \"ft.force\"\n\
             [roles.base]\n\
             odom = \"base.pose[0]\"\n",
        )
        .unwrap();
        let roles: Vec<(String, String)> = mapping
            .all_roles()
            .into_iter()
            .map(|(role, field)| (role.to_string(), field.to_string()))
            .collect();
        let pair = |role: &str, field: &str| (role.to_owned(), field.to_owned());
        assert_eq!(
            roles,
            [
                pair("gripper.opening", "hand.width"),
                pair("wrench.left", "ft.force"),
                pair("base.odom", "base.pose[0]"),
                pair("joints.shoulder.position", "arm.joint_positions[0]"),
                pair("joints.shoulder.effort", "arm.torques[0]"),
                pair("joints.elbow_2.position", "arm.joint_positions[1]"),
                pair("joints.joint_3rd.position", "arm.joint_positions[2]"),
            ]
        );
        assert_eq!(mapping.roles[0].line, 6);
    }

    #[test]
    fn a_role_that_isnt_one_is_a_joints_or_is_mapped_twice_is_reported_with_its_line() {
        assert_eq!(
            problems(
                "[roles]\n\
                 gripper = \"hand.width\"\n\
                 \"Wrench.Left\" = \"ft.force\"\n\
                 \"joints.elbow.position\" = \"arm.p[1]\"\n\
                 \"base.odom\" = 3\n\
                 \"tool.tip\" = \"tool.tip\"\n\
                 [roles.tool]\n\
                 tip = \"tool.tip\"\n"
            ),
            [
                "config.invalid_value 2: Invalid configuration value: `roles.gripper`: a role is two or more dot-separated names of lowercase letters, digits and underscores, e.g. \"gripper.opening\"",
                "config.invalid_value 3: Invalid configuration value: `roles.Wrench.Left`: a role is two or more dot-separated names of lowercase letters, digits and underscores, e.g. \"gripper.opening\"",
                "config.invalid_value 4: Invalid configuration value: `roles.joints.elbow.position`: roles under `joints.` are each mapped joint's, from `[joints]`",
                "config.invalid_value 5: Invalid configuration value: `roles.base.odom` must name a decoded field, e.g. \"arm_joints.gripper\"",
                "config.invalid_value 8: Invalid configuration value: `roles.tool.tip` is mapped twice",
            ]
        );
    }

    #[test]
    fn an_empty_file_maps_nothing() {
        assert_eq!(parse("").unwrap(), MappingDeclaration::default());
    }

    #[test]
    fn mistakes_are_reported_together_with_their_lines() {
        assert_eq!(
            problems(
                "[joints]\n\
                 a = \"arm\"\n\
                 b = \"arm.positions[x]\"\n\
                 c = { positon = \"arm.p[0]\" }\n\
                 d = 3\n\
                 e = {}\n\
                 [robot]\n"
            ),
            [
                "mapping.unknown_key 7: Unknown key in mapping.toml: `robot`",
                "config.invalid_value 2: Invalid configuration value: `joints.a` must name a decoded field, e.g. \"arm_joints.joint_positions[0]\"",
                "config.invalid_value 3: Invalid configuration value: `joints.b` must name a decoded field, e.g. \"arm_joints.joint_positions[0]\"",
                "mapping.unknown_key 4: Unknown key in mapping.toml: `joints.c.positon` (did you mean `position`?)",
                "config.invalid_value 5: Invalid configuration value: `joints.d` must name a decoded field, or be a table with `position`, `velocity` and `effort`",
                "config.invalid_value 6: Invalid configuration value: `joints.e` needs `position`, `velocity` or `effort`",
            ]
        );
    }

    #[test]
    fn field_references_print_as_they_are_written() {
        for text in ["arm.joint_positions[6]", "hand.width"] {
            let reference: FieldReference = text.parse().unwrap();
            assert_eq!(reference.to_string(), text);
        }
        for text in [
            "arm",
            ".x",
            "arm.",
            "arm.x[",
            "arm.x[-1]",
            "arm.x y",
            "a.b.c",
        ] {
            assert!(text.parse::<FieldReference>().is_err(), "{text}");
        }
    }
}
