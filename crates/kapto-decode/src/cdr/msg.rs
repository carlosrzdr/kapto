// SPDX-License-Identifier: Apache-2.0

//! ROS 2 message definitions (`.msg`), as rosbag2 stores them in MCAP: the
//! message's own definition, then each type it uses after a line of `=` and
//! `MSG: package/Type`.
//!
//! Each line is a field, `type name` with an optional default value after
//! the name, or a constant, `TYPE NAME=VALUE`, which takes no space in a
//! message and is passed over. `#` starts a comment. A type is a primitive
//! (`bool`, `byte`, `char`, `int8` … `uint64`, `float32`, `float64`,
//! `string`, `wstring`, and `string<=N` or `wstring<=N` with a bound), or
//! another message: `package/Type`, `package/msg/Type`, or `Type` in the same
//! package. Each may be an array: `T[N]` of exactly N, `T[]` of any length,
//! `T[<=N]` of at most N.
//!
//! [`assemble`] puts such a definition together from `.msg` files, as
//! rosbag2 does when it records a ROS 2 topic; [`msg_files_in`] finds them
//! in folders laid out as ROS 2 installs them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kapto_core::{ErrorCode, KaptoError};

/// A primitive type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    /// `bool`: one byte, 0 or 1.
    Bool,
    /// `byte`: one unsigned byte.
    Byte,
    /// `char`: one unsigned byte.
    Char,
    /// `int8`.
    Int8,
    /// `uint8`.
    Uint8,
    /// `int16`.
    Int16,
    /// `uint16`.
    Uint16,
    /// `int32`.
    Int32,
    /// `uint32`.
    Uint32,
    /// `int64`.
    Int64,
    /// `uint64`.
    Uint64,
    /// `float32`.
    Float32,
    /// `float64`.
    Float64,
    /// `string`: UTF-8 text.
    String,
    /// `wstring`: text of 4-byte characters.
    WString,
}

impl Primitive {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "bool" => Self::Bool,
            "byte" => Self::Byte,
            "char" => Self::Char,
            "int8" => Self::Int8,
            "uint8" => Self::Uint8,
            "int16" => Self::Int16,
            "uint16" => Self::Uint16,
            "int32" => Self::Int32,
            "uint32" => Self::Uint32,
            "int64" => Self::Int64,
            "uint64" => Self::Uint64,
            "float32" => Self::Float32,
            "float64" => Self::Float64,
            "string" => Self::String,
            "wstring" => Self::WString,
            _ => return None,
        })
    }
}

/// What one value of a field is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Element {
    /// A primitive.
    Primitive(Primitive),
    /// A message, by its full name, e.g. `std_msgs/Header`.
    Message(String),
}

/// How many values a field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    /// One.
    One,
    /// Exactly this many, with no count written: `T[N]`.
    Fixed(usize),
    /// A count, then that many: `T[]`, or `T[<=N]` with at most N.
    Sequence(Option<usize>),
}

/// One field of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MsgField {
    /// Its name.
    pub name: String,
    /// The type of each value.
    pub element: Element,
    /// How many values.
    pub arity: Arity,
}

/// A message type and every type it uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageDefinitions {
    root: String,
    types: BTreeMap<String, Vec<MsgField>>,
}

impl MessageDefinitions {
    /// Reads the definition of `root` (e.g. `sensor_msgs/msg/JointState`)
    /// and of the types it uses, as rosbag2 stores them.
    ///
    /// # Errors
    ///
    /// `decoder.invalid_schema` naming the type and line, for a line that
    /// isn't a field or constant, or a field of a type that isn't defined.
    pub fn parse(root: &str, text: &str) -> Result<Self, KaptoError> {
        let root = normalise(root, "");
        let mut types = BTreeMap::new();
        let mut current = root.clone();
        let mut body = String::new();
        for line in text.lines() {
            // rosbag2 puts a line of 80 `=` before each type used.
            let trimmed = line.trim_end();
            if trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'=') {
                continue;
            }
            if let Some(name) = line.strip_prefix("MSG: ") {
                let fields = parse_body(&current, &body)?;
                types.insert(current, fields);
                current = normalise(name.trim(), "");
                body.clear();
                continue;
            }
            body.push_str(line);
            body.push('\n');
        }
        let fields = parse_body(&current, &body)?;
        types.insert(current, fields);
        let definitions = Self { root, types };
        definitions.check()?;
        Ok(definitions)
    }

    /// The root type's name, e.g. `sensor_msgs/JointState`.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The fields of a type, if it is defined.
    #[must_use]
    pub fn fields(&self, name: &str) -> Option<&[MsgField]> {
        self.types.get(name).map(Vec::as_slice)
    }

    /// Every message type a field names is defined, and no type contains
    /// itself, which no finite message could.
    fn check(&self) -> Result<(), KaptoError> {
        for (name, fields) in &self.types {
            for field in fields {
                if let Element::Message(used) = &field.element
                    && !self.types.contains_key(used)
                {
                    return Err(invalid(format!(
                        "`{name}.{}` is a `{used}`, which the definition doesn't include",
                        field.name
                    )));
                }
            }
        }
        let mut visiting = Vec::new();
        self.acyclic(&self.root, &mut visiting)
    }

    fn acyclic<'a>(&'a self, name: &'a str, visiting: &mut Vec<&'a str>) -> Result<(), KaptoError> {
        if visiting.contains(&name) {
            return Err(invalid(format!("`{name}` contains itself")));
        }
        visiting.push(name);
        for field in self.types.get(name).into_iter().flatten() {
            // A sequence may be empty, so only a single or fixed field
            // makes a cycle impossible to end.
            if let (Element::Message(used), Arity::One | Arity::Fixed(_)) =
                (&field.element, field.arity)
            {
                self.acyclic(used, visiting)?;
            }
        }
        visiting.pop();
        Ok(())
    }
}

/// The line rosbag2 puts before each type a definition uses.
const SEPARATOR: &str =
    "================================================================================";

/// The definition of `root` (e.g. `sensor_msgs/msg/JointState`) as rosbag2
/// stores it in MCAP, put together from `.msg` files: `root`'s own file,
/// then the file of each type it uses, once, depth first, each type's in
/// alphabetical order, after a line of 80 `=` and `MSG: package/Type`.
/// `msg_file(package, name)` gives the text of `package/msg/name.msg`, or
/// `None` if there is none.
///
/// # Errors
///
/// `decoder.msg_not_found` naming a type without a file and the type that
/// uses it; `decoder.invalid_schema` for a file that isn't a definition.
pub fn assemble(
    root: &str,
    msg_file: &mut dyn FnMut(&str, &str) -> Option<String>,
) -> Result<String, KaptoError> {
    let root = normalise(root, "");
    let own = read_msg(&root, None, msg_file)?;
    let mut text = own.clone();
    append_used(&root, &own, &mut BTreeSet::new(), &mut text, msg_file)?;
    MessageDefinitions::parse(&root, &text)?;
    Ok(text)
}

/// Appends the file of each type `name` uses that isn't in `seen` yet, each
/// followed by the types it uses.
fn append_used(
    name: &str,
    own: &str,
    seen: &mut BTreeSet<String>,
    text: &mut String,
    msg_file: &mut dyn FnMut(&str, &str) -> Option<String>,
) -> Result<(), KaptoError> {
    let used: BTreeSet<String> = parse_body(name, own)?
        .into_iter()
        .filter_map(|field| match field.element {
            Element::Message(used) => Some(used),
            Element::Primitive(_) => None,
        })
        .collect();
    for dependency in used {
        if !seen.insert(dependency.clone()) {
            continue;
        }
        let file = read_msg(&dependency, Some(name), msg_file)?;
        text.push('\n');
        text.push_str(SEPARATOR);
        text.push_str("\nMSG: ");
        text.push_str(&dependency);
        text.push('\n');
        text.push_str(&file);
        append_used(&dependency, &file, seen, text, msg_file)?;
    }
    Ok(())
}

fn read_msg(
    name: &str,
    used_by: Option<&str>,
    msg_file: &mut dyn FnMut(&str, &str) -> Option<String>,
) -> Result<String, KaptoError> {
    let (package, type_name) = name.split_once('/').unwrap_or(("", name));
    msg_file(package, type_name).ok_or_else(|| {
        let error = KaptoError::new(ErrorCode::DecoderMsgNotFound)
            .detail(format!("no `{package}/msg/{type_name}.msg`"));
        match used_by {
            Some(user) => error.context("used by", user),
            None => error,
        }
    })
}

/// Finds `.msg` files in `folders` laid out as ROS 2 installs them,
/// `<folder>/<package>/msg/<Type>.msg` (a ROS 2 installation's `share`
/// folder is one), for [`assemble`]. The first folder that has the file
/// wins.
pub fn msg_files_in(folders: &[PathBuf]) -> impl FnMut(&str, &str) -> Option<String> + '_ {
    |package, name| {
        folders.iter().find_map(|folder| {
            std::fs::read_to_string(folder.join(package).join("msg").join(format!("{name}.msg")))
                .ok()
        })
    }
}

fn invalid(detail: String) -> KaptoError {
    KaptoError::new(ErrorCode::DecoderInvalidSchema).detail(detail)
}

/// `package/msg/Type` or `package/Type` as `package/Type`; a bare `Type`
/// in `package`.
fn normalise(name: &str, package: &str) -> String {
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        [package, "msg", name] | [package, name] => format!("{package}/{name}"),
        // The one bare name ROS 1 definitions use from elsewhere.
        ["Header"] if package != "std_msgs" => "std_msgs/Header".to_owned(),
        [name] => format!("{package}/{name}"),
        _ => name.to_owned(),
    }
}

/// The fields of one type's definition.
fn parse_body(type_name: &str, body: &str) -> Result<Vec<MsgField>, KaptoError> {
    let package = type_name.split('/').next().unwrap_or_default();
    let mut fields = Vec::new();
    for (number, line) in (1_usize..).zip(body.lines()) {
        let line = without_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let at = |detail: String| invalid(format!("{type_name}, line {number}: {detail}"));
        let (type_text, rest) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| at(format!("`{line}` has a type but no name")))?;
        let rest = rest.trim_start();
        // A constant: NAME=VALUE.
        if let Some((name, _)) = rest.split_once('=')
            && is_identifier(name.trim())
        {
            continue;
        }
        let name = rest.split_whitespace().next().unwrap_or_default();
        if !is_identifier(name) {
            return Err(at(format!("`{name}` isn't a field name")));
        }
        let (element, arity) = field_type(type_text, package)
            .ok_or_else(|| at(format!("`{type_text}` isn't a type")))?;
        fields.push(MsgField {
            name: name.to_owned(),
            element,
            arity,
        });
    }
    Ok(fields)
}

/// The line before a `#` that isn't inside quotes.
fn without_comment(line: &str) -> &str {
    let mut quote = None;
    for (at, character) in line.char_indices() {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), _) if character == open => quote = None,
            (None, '#') => return &line[..at],
            _ => {}
        }
    }
    line
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A field's element and arity from its type, e.g. `string<=8[<=3]`.
fn field_type(text: &str, package: &str) -> Option<(Element, Arity)> {
    let (base, arity) = match text.split_once('[') {
        None => (text, Arity::One),
        Some((base, size)) => {
            let size = size.strip_suffix(']')?;
            let arity = if size.is_empty() {
                Arity::Sequence(None)
            } else if let Some(bound) = size.strip_prefix("<=") {
                Arity::Sequence(Some(bound.parse().ok()?))
            } else {
                Arity::Fixed(size.parse().ok()?)
            };
            (base, arity)
        }
    };
    // A bounded string reads like any string.
    let base = match base.split_once("<=") {
        Some((string @ ("string" | "wstring"), bound)) => {
            bound.parse::<usize>().ok()?;
            string
        }
        Some(_) => return None,
        None => base,
    };
    let element = match Primitive::from_name(base) {
        Some(primitive) => Element::Primitive(primitive),
        None if base.split('/').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }) =>
        {
            Element::Message(normalise(base, package))
        }
        None => return None,
    };
    Some((element, arity))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, element: Element, arity: Arity) -> MsgField {
        MsgField {
            name: name.to_owned(),
            element,
            arity,
        }
    }

    #[test]
    fn fields_constants_defaults_comments_and_used_types_are_read() {
        let text = "\
# A comment line
std_msgs/Header header   # trailing comment
int32 SPEED=3            # a constant
string NAME = \"x # y\"
float64[] position
uint8[16] id
string<=8[<=2] labels
Point tip
bool flag true
string note \"a # b\"
================================================================================
MSG: std_msgs/Header
builtin_interfaces/Time stamp
string frame_id
================================================================================
MSG: builtin_interfaces/Time
int32 sec
uint32 nanosec
================================================================================
MSG: arm_msgs/Point
float64 x
";
        let definitions = MessageDefinitions::parse("arm_msgs/msg/State", text).unwrap();
        assert_eq!(definitions.root(), "arm_msgs/State");
        assert_eq!(
            definitions.fields("arm_msgs/State").unwrap(),
            [
                field(
                    "header",
                    Element::Message("std_msgs/Header".into()),
                    Arity::One
                ),
                field(
                    "position",
                    Element::Primitive(Primitive::Float64),
                    Arity::Sequence(None)
                ),
                field("id", Element::Primitive(Primitive::Uint8), Arity::Fixed(16)),
                field(
                    "labels",
                    Element::Primitive(Primitive::String),
                    Arity::Sequence(Some(2))
                ),
                field("tip", Element::Message("arm_msgs/Point".into()), Arity::One),
                field("flag", Element::Primitive(Primitive::Bool), Arity::One),
                field("note", Element::Primitive(Primitive::String), Arity::One),
            ]
        );
        assert_eq!(definitions.fields("std_msgs/Header").unwrap().len(), 2);
    }

    #[test]
    fn mistakes_are_reported_with_the_type_and_line() {
        let message =
            |root: &str, text: &str| MessageDefinitions::parse(root, text).unwrap_err().message();
        assert!(
            message("a/B", "int32\n").ends_with("a/B, line 1: `int32` has a type but no name"),
            "{}",
            message("a/B", "int32\n")
        );
        // An unknown lowercase name is a message type of the same package.
        assert!(
            message("a/B", "\nint33 x\n")
                .ends_with("`a/B.x` is a `a/int33`, which the definition doesn't include")
        );
        assert!(message("a/B", "\nint-33 x\n").ends_with("a/B, line 2: `int-33` isn't a type"));
        assert!(message("a/B", "int32 2x\n").ends_with("`2x` isn't a field name"));
        assert!(message("a/B", "float64[x] v\n").ends_with("`float64[x]` isn't a type"));
        assert!(message("a/B", "int32<=3 v\n").ends_with("`int32<=3` isn't a type"));
        assert!(
            message("a/B", "Missing m\n")
                .ends_with("`a/B.m` is a `a/Missing`, which the definition doesn't include")
        );
        assert!(
            message("a/B", "C c\n=====\nMSG: a/C\nB b\n").ends_with("`a/B` contains itself"),
            "{}",
            message("a/B", "C c\n=====\nMSG: a/C\nB b\n")
        );
        // A sequence may be empty, so a tree of itself is fine.
        assert!(MessageDefinitions::parse("a/Tree", "a/Tree[] children\n").is_ok());
    }
}
