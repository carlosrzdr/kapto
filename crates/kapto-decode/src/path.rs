// SPDX-License-Identifier: Apache-2.0

//! Self-describing payloads, JSON or MessagePack: each field found by its
//! path through the message's maps and arrays.
//!
//! A path is keys separated by dots, with `[n]` for the n-th element of an
//! array, counted from 0: `joints.position[2]`, `state.pose.x`, `[0].id`. A
//! key is any text without `.`, `[` or `]`. A field with `count` above 1
//! names an array and takes its first `count` elements.
//!
//! An element can also be found by name: `position[name=shoulder]` is the
//! element of `position` at the place where the array `name` beside it holds
//! `shoulder`, as a ROS 2 `JointState` lists its joints. Whatever order the
//! robot sends its joints in, the field is the same joint; a message without
//! the name is missing the field, never given another joint's value. A field
//! found by name is one value.
//!
//! Each value is converted to the field's declared type by the same rules
//! for both formats, listed in [`CONVERSIONS`].
//!
//! A path that isn't in the message leaves its field missing
//! ([`Value::Missing`]) and the others decoded, so a topic two publishers
//! share, each sending some of the fields, decodes each one's; a message
//! with none of the fields isn't decoded. Text, `null`, maps and arrays
//! where a number is expected leave the message undecoded. Either way the
//! payload is still recorded as it arrived.
//!
//! Decoding follows each path through the message with serde, skipping
//! everything else without building it, so it allocates only for keys
//! written with escape sequences.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;

use kapto_core::name::is_name;
use kapto_core::{ErrorCode, KaptoError};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};

use crate::Decoder;
use crate::field::{DecodeError, FieldDescription, ScalarType, Value};

/// What a field of each declared type accepts from the message, as
/// `(types, accepts)`. Documented as written.
pub const CONVERSIONS: [(&str, &str); 3] = [
    (
        "`u8` … `u64`, `i8` … `i64`",
        "integers in the type's range; numbers without a fraction (`3.0`), in range; `true` as 1 and `false` as 0",
    ),
    (
        "`f32`, `f64`",
        "any number, an `f32` rounded to 32 bits; `true` as 1 and `false` as 0",
    ),
    (
        "`bool`",
        "`true` and `false`; numbers: 0 is false, anything else true",
    ),
];

/// How a self-describing payload is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathFormat {
    /// JSON text, UTF-8.
    Json,
    /// MessagePack.
    MsgPack,
}

impl PathFormat {
    /// Both formats, in the order they are documented.
    pub const ALL: [Self; 2] = [Self::Json, Self::MsgPack];

    /// The name used in configuration (`kind = "json"`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::MsgPack => "msgpack",
        }
    }
}

/// One field found by its path.
#[derive(Clone, Debug, PartialEq)]
pub struct PathField {
    /// Its name: lowercase letters, digits and underscores, starting with a
    /// letter.
    pub name: String,
    /// Where it is in the message, e.g. `joints.position`.
    pub path: String,
    /// The type each value is converted to.
    pub ty: ScalarType,
    /// Number of values: 1 for a single value, more for the first elements
    /// of an array.
    pub count: usize,
    /// Multiplier applied to each value; scaled values are floats. Not
    /// allowed on `bool`.
    pub scale: Option<f64>,
    /// The unit of the (scaled) value, e.g. `rad`.
    pub unit: Option<String>,
}

impl PathField {
    /// A single unscaled value without a unit.
    #[must_use]
    pub fn new(name: &str, path: &str, ty: ScalarType) -> Self {
        Self {
            name: name.to_owned(),
            path: path.to_owned(),
            ty,
            count: 1,
            scale: None,
            unit: None,
        }
    }
}

/// The fields of every payload of a stream, found by path.
#[derive(Clone, Debug, PartialEq)]
pub struct PathLayout {
    /// How the payloads are encoded.
    pub format: PathFormat,
    /// The fields, in the order they are output.
    pub fields: Vec<PathField>,
}

/// One step of a path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
    /// `[key=text]`: the element at the place where the array `key`, beside
    /// this one, holds `text`.
    ByName {
        key: String,
        text: String,
    },
}

/// Splits a path into its steps.
fn steps(path: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let mut rest = path;
    let mut after_dot = false;
    while !rest.is_empty() {
        if let Some(inside) = rest.strip_prefix('[') {
            let (index, after) = inside
                .split_once(']')
                .ok_or_else(|| format!("`{path}` has a `[` without its `]`"))?;
            if let Some((key, text)) = index.split_once('=') {
                steps.push(by_name(path, &steps, key, text)?);
                rest = after;
                after_dot = false;
                continue;
            }
            let index = index
                .parse()
                .map_err(|_| format!("`{path}` has `[{index}]`; an index is a whole number"))?;
            steps.push(Step::Index(index));
            rest = after;
            after_dot = false;
        } else if rest.starts_with(']') {
            return Err(format!("`{path}` has a `]` without its `[`"));
        } else if let Some(after) = rest.strip_prefix('.') {
            if steps.is_empty() || after_dot {
                return Err(format!("`{path}` has an empty key"));
            }
            rest = after;
            after_dot = true;
        } else {
            if !steps.is_empty() && !after_dot {
                return Err(format!("`{path}` needs a `.` before each key"));
            }
            let end = rest.find(['.', '[', ']']).unwrap_or(rest.len());
            steps.push(Step::Key(rest[..end].to_owned()));
            rest = &rest[end..];
            after_dot = false;
        }
    }
    if steps.is_empty() {
        return Err("the path is empty".to_owned());
    }
    if after_dot {
        return Err(format!("`{path}` ends with a `.`"));
    }
    Ok(steps)
}

/// `[key=text]` after `steps`: one per path, after an array's key.
fn by_name(path: &str, steps: &[Step], key: &str, text: &str) -> Result<Step, String> {
    if key.is_empty() || text.is_empty() || key.contains(['.', '[']) {
        return Err(format!(
            "`{path}` has `[{key}={text}]`; looking up by name is `[<array>=<name>]`, e.g. `position[name=shoulder]`"
        ));
    }
    if !matches!(steps.last(), Some(Step::Key(_))) {
        return Err(format!(
            "`{path}` looks up `[{key}={text}]` in something other than an array named by its key"
        ));
    }
    if steps.iter().any(|step| matches!(step, Step::ByName { .. })) {
        return Err(format!("`{path}` looks up by name more than once"));
    }
    Ok(Step::ByName {
        key: key.to_owned(),
        text: text.to_owned(),
    })
}

impl PathLayout {
    /// Checks the layout and compiles it into a decoder for a stream whose
    /// messages are at most `max_message_bytes` long.
    ///
    /// # Errors
    ///
    /// Every problem found, each naming its field and its position (1 for
    /// the first): `decoder.invalid_layout` for no fields, an invalid or
    /// repeated name, an invalid path, an empty array, a scale that is 0,
    /// not finite or on a `bool`, or an empty unit; `decoder.too_many_values`
    /// for more values than `max_message_bytes`.
    pub fn compile(&self, max_message_bytes: usize) -> Result<PathDecoder, Vec<KaptoError>> {
        Ok(PathDecoder {
            format: self.format,
            paths: Paths::compile(&self.fields, max_message_bytes)?,
        })
    }
}

/// Fields found by path, compiled: shared by every format read through
/// serde.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Paths {
    pub(crate) fields: Vec<FieldDescription>,
    reads: Vec<Read>,
    /// Every field's count together: what one message decodes into.
    values: usize,
}

impl Paths {
    /// Checks `fields` and compiles their paths, for messages of at most
    /// `max_message_bytes`.
    ///
    /// Every value takes at least a byte of a message, in every format read
    /// by path, so a layout with more values than `max_message_bytes` can
    /// never be filled: it is refused before room is made for its values.
    pub(crate) fn compile(
        fields: &[PathField],
        max_message_bytes: usize,
    ) -> Result<Self, Vec<KaptoError>> {
        let mut errors = Vec::new();
        if fields.is_empty() {
            errors.push(invalid("the layout has no fields".to_owned()));
        }
        let mut names = BTreeSet::new();
        let mut reads = Vec::new();
        let mut values = Some(0_usize);
        for (position, field) in (1_usize..).zip(fields) {
            // Only the field the layout first passes the bound at is named.
            if let Some(before) = values {
                values = before
                    .checked_add(field.count)
                    .filter(|&sum| sum <= max_message_bytes);
                if values.is_none() {
                    errors.push(too_many_values(field, position, before, max_message_bytes));
                }
            }
            let mut problems = problems(field, &mut names);
            match steps(&field.path) {
                Ok(steps) => {
                    let names = names_array(&steps);
                    if names.is_some() && field.count > 1 {
                        problems.push(format!(
                            "`{}` finds one value by name; declare a field for each name",
                            field.name
                        ));
                    }
                    reads.push(Read {
                        steps,
                        names,
                        ty: field.ty,
                        count: field.count,
                        scale: field.scale,
                    });
                }
                Err(problem) => problems.push(problem),
            }
            for problem in problems {
                errors.push(
                    invalid(problem)
                        .context("field", field.name.clone())
                        .context("position", position.to_string()),
                );
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(Self {
            fields: fields
                .iter()
                .map(|field| FieldDescription {
                    name: field.name.clone(),
                    unit: field.unit.clone(),
                    count: field.count,
                    ty: field.ty,
                })
                .collect(),
            reads,
            values: values.unwrap_or_default(),
        })
    }

    pub(crate) fn value_count(&self) -> usize {
        self.values
    }

    /// Fills `out` with each field's values: `parse` runs one [`Walk`] over
    /// the payload, from its start, and says whether it succeeded. A field
    /// found by name takes two: one finding the place of the name, then one
    /// reading the value there. A field the message doesn't have is
    /// [`Value::Missing`], so a topic two publishers share, each sending
    /// some of the fields, decodes each one's; only a message with none of
    /// them isn't decoded.
    ///
    /// Note that a value of the wrong type, or a payload that isn't valid,
    /// still leaves the whole message undecoded: those are the robot or the
    /// layout being wrong, not a field left out.
    pub(crate) fn decode(
        &self,
        out: &mut [Value],
        mut parse: impl FnMut(Walk<'_>) -> bool,
    ) -> Result<(), DecodeError> {
        let needed = self.value_count();
        if out.len() != needed {
            return Err(DecodeError::OutputSize {
                needed,
                got: out.len(),
            });
        }
        let mut rest = out;
        let mut first_missing = None;
        let mut missing = 0;
        for (field, read) in self.reads.iter().enumerate() {
            let (values, after) = rest.split_at_mut(read.count);
            rest = after;
            let mut place = 0;
            if let Some((names, text)) = &read.names {
                let at = Cell::new(None);
                let locate = Locate {
                    steps: names,
                    text,
                    at: &at,
                };
                parse(Walk::Locate(locate));
                let Some(at) = at.get() else {
                    values.fill(Value::Missing);
                    first_missing.get_or_insert(field);
                    missing += 1;
                    continue;
                };
                place = at;
            }
            let found = Cell::new(None);
            let follow = Follow {
                steps: &read.steps,
                read,
                values,
                field,
                found: &found,
                place,
            };
            if !parse(Walk::Follow(follow)) {
                match found.get() {
                    Some(DecodeError::Missing { .. }) => {
                        values.fill(Value::Missing);
                        first_missing.get_or_insert(field);
                        missing += 1;
                    }
                    other => return Err(other.unwrap_or(DecodeError::Malformed)),
                }
            }
        }
        match first_missing {
            Some(field) if missing == self.reads.len() => Err(DecodeError::Missing { field }),
            _ => Ok(()),
        }
    }
}

fn invalid(detail: String) -> KaptoError {
    KaptoError::new(ErrorCode::DecoderInvalidLayout).detail(detail)
}

/// `field`, at `position` with `before` values ahead of it, takes the layout
/// past `max_message_bytes` values.
fn too_many_values(
    field: &PathField,
    position: usize,
    before: usize,
    max_message_bytes: usize,
) -> KaptoError {
    let total = before.checked_add(field.count).map_or_else(
        || "more values than can be counted".to_owned(),
        |sum| format!("{sum} values"),
    );
    KaptoError::new(ErrorCode::DecoderTooManyValues)
        .detail(format!(
            "`{}` (count {}) brings the layout to {total}, but the stream's messages are at \
             most {max_message_bytes} bytes, and each value takes at least one",
            field.name, field.count
        ))
        .context("field", field.name.clone())
        .context("position", position.to_string())
}

/// Everything wrong with one field, but its path.
fn problems(field: &PathField, names: &mut BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let name = &field.name;
    if !is_name(name) {
        problems.push(format!(
            "`{name}` isn't a valid field name: use lowercase letters, digits and underscores, starting with a letter"
        ));
    } else if !names.insert(name.clone()) {
        problems.push(format!("`{name}` is declared more than once"));
    }
    if field.count == 0 {
        problems.push(format!("`{name}` is an array of 0 values"));
    }
    match field.scale {
        Some(_) if field.ty == ScalarType::Bool => {
            problems.push(format!("`{name}` is a bool, which can't be scaled"));
        }
        Some(scale) if !scale.is_finite() || scale == 0.0 => {
            problems.push(format!(
                "`{name}` has scale {scale}; it must be a finite number other than 0"
            ));
        }
        _ => {}
    }
    if field.unit.as_deref().is_some_and(str::is_empty) {
        problems.push(format!("`{name}` has an empty unit; leave it out instead"));
    }
    problems
}

/// Where the array a `[key=text]` step looks `text` up in is, and `text`:
/// the steps before the looked-up array's own key, then `key`.
fn names_array(steps: &[Step]) -> Option<(Vec<Step>, String)> {
    let at = steps
        .iter()
        .position(|step| matches!(step, Step::ByName { .. }))?;
    let Step::ByName { key, text } = &steps[at] else {
        return None;
    };
    let mut names = steps[..at.saturating_sub(1)].to_vec();
    names.push(Step::Key(key.clone()));
    Some((names, text.clone()))
}

/// One field to find: where, as what, how many.
#[derive(Clone, Debug, PartialEq)]
struct Read {
    steps: Vec<Step>,
    /// For a path looking up by name: where the names are, and the name.
    names: Option<(Vec<Step>, String)>,
    ty: ScalarType,
    count: usize,
    scale: Option<f64>,
}

/// A compiled [`PathLayout`].
#[derive(Clone, Debug, PartialEq)]
pub struct PathDecoder {
    format: PathFormat,
    paths: Paths,
}

impl Decoder for PathDecoder {
    fn fields(&self) -> &[FieldDescription] {
        &self.paths.fields
    }

    fn value_count(&self) -> usize {
        self.paths.value_count()
    }

    fn decode(&self, payload: &[u8], out: &mut [Value]) -> Result<(), DecodeError> {
        self.paths.decode(out, |follow| match self.format {
            PathFormat::Json => {
                let mut deserializer = serde_json::Deserializer::from_slice(payload);
                follow
                    .deserialize(&mut deserializer)
                    .and_then(|()| deserializer.end())
                    .is_ok()
            }
            PathFormat::MsgPack => {
                let mut deserializer = rmp_serde::Deserializer::from_read_ref(payload);
                follow.deserialize(&mut deserializer).is_ok()
            }
        })
    }
}

/// One pass over a payload: finding where a name is, or reading a field.
pub(crate) enum Walk<'a> {
    Locate(Locate<'a>),
    Follow(Follow<'a>),
}

impl<'de> DeserializeSeed<'de> for Walk<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        match self {
            Self::Locate(locate) => deserializer.deserialize_any(locate),
            Self::Follow(follow) => deserializer.deserialize_any(follow),
        }
    }
}

/// Follows `steps`, keys and indices, to an array of names and leaves in
/// `at` the place of the first that is `text`, if one is.
pub(crate) struct Locate<'a> {
    steps: &'a [Step],
    text: &'a str,
    at: &'a Cell<Option<usize>>,
}

impl<'de> Visitor<'de> for Locate<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a message holding the names")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Some((Step::Key(wanted), rest)) = self.steps.split_first() else {
            return Err(de::Error::custom("no names here"));
        };
        let mut next = Some(Locate {
            steps: rest,
            ..self
        });
        while let Some(matches) = map.next_key_seed(KeyIs(wanted))? {
            match next.take() {
                Some(locate) if matches => {
                    map.next_value_seed(Walk::Locate(locate))?;
                }
                other => {
                    next = other;
                    map.next_value_seed(Skip)?;
                }
            }
        }
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        match self.steps.split_first() {
            None => {
                let mut place = 0;
                while let Some(matches) = seq.next_element_seed(KeyIs(self.text))? {
                    if matches && self.at.get().is_none() {
                        self.at.set(Some(place));
                    }
                    place += 1;
                }
            }
            Some((Step::Index(wanted), rest)) => {
                for _ in 0..*wanted {
                    seq.next_element_seed(Skip)?;
                }
                let locate = Locate {
                    steps: rest,
                    ..self
                };
                seq.next_element_seed(Walk::Locate(locate))?;
                while seq.next_element_seed(Skip)?.is_some() {}
            }
            Some(_) => return Err(de::Error::custom("no names here")),
        }
        Ok(())
    }
}

/// Follows `steps` from the value being deserialized and converts what is
/// there into `values`. Why it failed, when the payload itself is fine, is
/// left in `found`: serde errors only carry text.
pub(crate) struct Follow<'a> {
    steps: &'a [Step],
    read: &'a Read,
    values: &'a mut [Value],
    field: usize,
    found: &'a Cell<Option<DecodeError>>,
    /// Where the name a `[key=text]` step looks up is, found beforehand.
    place: usize,
}

impl Follow<'_> {
    fn fail<E: de::Error>(&self, error: DecodeError) -> E {
        self.found.set(Some(error));
        E::custom("not decoded")
    }

    fn missing<E: de::Error>(&self) -> E {
        self.fail(DecodeError::Missing { field: self.field })
    }

    fn wrong_type<E: de::Error>(&self) -> E {
        self.fail(DecodeError::WrongType { field: self.field })
    }
}

impl<'de> DeserializeSeed<'de> for Follow<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Follow<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a message holding the field")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let Some((Step::Key(wanted), rest)) = self.steps.split_first() else {
            // An index into a map, or a map where a value is expected.
            return Err(if self.steps.is_empty() {
                self.wrong_type()
            } else {
                self.missing()
            });
        };
        let mut found = false;
        while let Some(matches) = map.next_key_seed(KeyIs(wanted))? {
            if matches && !found {
                found = true;
                map.next_value_seed(Follow {
                    steps: rest,
                    read: self.read,
                    values: &mut *self.values,
                    field: self.field,
                    found: self.found,
                    place: self.place,
                })?;
            } else {
                map.next_value_seed(Skip)?;
            }
        }
        if found { Ok(()) } else { Err(self.missing()) }
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        match self.steps.split_first() {
            // The field's own array: its first `count` elements.
            None if self.read.count > 1 => {
                for index in 0..self.read.count {
                    let value = seq
                        .next_element_seed(Scalar {
                            read: self.read,
                            field: self.field,
                            found: self.found,
                        })?
                        .ok_or_else(|| self.missing())?;
                    self.values[index] = value;
                }
            }
            Some((Step::Index(_) | Step::ByName { .. }, rest)) => {
                let wanted = match &self.steps[0] {
                    Step::Index(index) => *index,
                    _ => self.place,
                };
                for _ in 0..wanted {
                    seq.next_element_seed(Skip)?.ok_or_else(|| self.missing())?;
                }
                let follow = Follow {
                    steps: rest,
                    read: self.read,
                    values: &mut *self.values,
                    field: self.field,
                    found: self.found,
                    place: self.place,
                };
                seq.next_element_seed(follow)?
                    .ok_or_else(|| self.missing())?;
            }
            None => return Err(self.wrong_type()),
            Some((Step::Key(_), _)) => return Err(self.missing()),
        }
        while seq.next_element_seed(Skip)?.is_some() {}
        Ok(())
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<(), E> {
        self.scalar(Number::Bool(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<(), E> {
        self.scalar(Number::UInt(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<(), E> {
        self.scalar(Number::Int(value))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<(), E> {
        self.scalar(Number::Float(value))
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Err(self.not_a_container())
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<(), E> {
        Err(self.not_a_container())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Err(self.not_a_container())
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Err(self.not_a_container())
    }
}

impl Follow<'_> {
    /// A number or flag reached: the field's value if the path ends here.
    fn scalar<E: de::Error>(self, number: Number) -> Result<(), E> {
        if !self.steps.is_empty() {
            return Err(self.missing());
        }
        if self.read.count > 1 {
            return Err(self.wrong_type());
        }
        let value = convert(number, self.read).ok_or_else(|| self.wrong_type())?;
        self.values[0] = value;
        Ok(())
    }

    /// Text, bytes or null reached: wrong if the path ends here, missing
    /// if it goes on.
    fn not_a_container<E: de::Error>(&self) -> E {
        if self.steps.is_empty() {
            self.wrong_type()
        } else {
            self.missing()
        }
    }
}

/// Passes over a value without keeping it. Unlike serde's `IgnoredAny`,
/// which `serde_json` skips with a stack it allocates, it walks nested maps
/// and arrays one value at a time.
struct Skip;

impl<'de> DeserializeSeed<'de> for Skip {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Skip {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(Skip)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map.next_key_seed(Skip)?.is_some() {
            map.next_value_seed(Skip)?;
        }
        Ok(())
    }

    fn visit_enum<A: de::EnumAccess<'de>>(self, data: A) -> Result<(), A::Error> {
        let ((), variant) = data.variant_seed(Skip)?;
        de::VariantAccess::newtype_variant_seed(variant, Skip)
    }
}

/// Whether a map key is the one wanted, compared without keeping it.
struct KeyIs<'a>(&'a str);

impl<'de> DeserializeSeed<'de> for KeyIs<'_> {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<bool, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl Visitor<'_> for KeyIs<'_> {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map key")
    }

    fn visit_str<E: de::Error>(self, key: &str) -> Result<bool, E> {
        Ok(key == self.0)
    }

    fn visit_bytes<E: de::Error>(self, key: &[u8]) -> Result<bool, E> {
        Ok(key == self.0.as_bytes())
    }

    // MessagePack maps may have numbers as keys: they match their digits.
    fn visit_u64<E: de::Error>(self, key: u64) -> Result<bool, E> {
        Ok(self.0.parse() == Ok(key))
    }

    fn visit_i64<E: de::Error>(self, key: i64) -> Result<bool, E> {
        Ok(self.0.parse() == Ok(key))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<bool, E> {
        Ok(false)
    }

    fn visit_unit<E: de::Error>(self) -> Result<bool, E> {
        Ok(false)
    }
}

/// One element of a field's array.
struct Scalar<'a> {
    read: &'a Read,
    field: usize,
    found: &'a Cell<Option<DecodeError>>,
}

impl Scalar<'_> {
    fn convert<E: de::Error>(&self, number: Number) -> Result<Value, E> {
        convert(number, self.read).ok_or_else(|| {
            self.found
                .set(Some(DecodeError::WrongType { field: self.field }));
            E::custom("not decoded")
        })
    }

    fn wrong<E: de::Error>(&self) -> E {
        self.found
            .set(Some(DecodeError::WrongType { field: self.field }));
        E::custom("not decoded")
    }
}

impl<'de> DeserializeSeed<'de> for Scalar<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Scalar<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a number or true/false")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        self.convert(Number::Bool(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        self.convert(Number::UInt(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        self.convert(Number::Int(value))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        self.convert(Number::Float(value))
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Value, E> {
        Err(self.wrong())
    }

    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Value, E> {
        Err(self.wrong())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Err(self.wrong())
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Err(self.wrong())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _: A) -> Result<Value, A::Error> {
        Err(self.wrong())
    }

    fn visit_map<A: MapAccess<'de>>(self, _: A) -> Result<Value, A::Error> {
        Err(self.wrong())
    }
}

/// A value as the payload has it.
#[derive(Clone, Copy, Debug)]
enum Number {
    UInt(u64),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Converts `number` to the field's type and scale, by the rules in the
/// module documentation; `None` if it doesn't fit.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each cast is checked to be exact first, or rounds by definition (f32, scale)"
)]
fn convert(number: Number, read: &Read) -> Option<Value> {
    // 2^64, the first float past every u64.
    const U64_END: f64 = 18_446_744_073_709_551_616.0;
    // 2^63.
    const I64_END: f64 = 9_223_372_036_854_775_808.0;
    let whole = |value: f64| value.is_finite() && value.fract() == 0.0;
    let value = match read.ty {
        ScalarType::Bool => Value::Bool(match number {
            Number::Bool(flag) => flag,
            Number::UInt(value) => value != 0,
            Number::Int(value) => value != 0,
            Number::Float(value) => value != 0.0,
        }),
        ScalarType::F32 | ScalarType::F64 => {
            let float = match number {
                Number::UInt(value) => value as f64,
                Number::Int(value) => value as f64,
                Number::Float(value) => value,
                Number::Bool(flag) => f64::from(u8::from(flag)),
            };
            let float = if read.ty == ScalarType::F32 {
                f64::from(float as f32)
            } else {
                float
            };
            Value::Float(read.scale.map_or(float, |scale| float * scale))
        }
        ty @ (ScalarType::U8 | ScalarType::U16 | ScalarType::U32 | ScalarType::U64) => {
            let unsigned = match number {
                Number::UInt(value) => value,
                Number::Int(value) => u64::try_from(value).ok()?,
                Number::Float(value) if whole(value) && (0.0..U64_END).contains(&value) => {
                    value as u64
                }
                Number::Float(_) => return None,
                Number::Bool(flag) => u64::from(flag),
            };
            let max = match ty {
                ScalarType::U8 => u64::from(u8::MAX),
                ScalarType::U16 => u64::from(u16::MAX),
                ScalarType::U32 => u64::from(u32::MAX),
                _ => u64::MAX,
            };
            if unsigned > max {
                return None;
            }
            read.scale.map_or(Value::UInt(unsigned), |scale| {
                Value::Float(unsigned as f64 * scale)
            })
        }
        ty => {
            let signed = match number {
                Number::Int(value) => value,
                Number::UInt(value) => i64::try_from(value).ok()?,
                Number::Float(value) if whole(value) && (-I64_END..I64_END).contains(&value) => {
                    value as i64
                }
                Number::Float(_) => return None,
                Number::Bool(flag) => i64::from(flag),
            };
            let (min, max) = match ty {
                ScalarType::I8 => (i64::from(i8::MIN), i64::from(i8::MAX)),
                ScalarType::I16 => (i64::from(i16::MIN), i64::from(i16::MAX)),
                ScalarType::I32 => (i64::from(i32::MIN), i64::from(i32::MAX)),
                _ => (i64::MIN, i64::MAX),
            };
            if signed < min || signed > max {
                return None;
            }
            read.scale.map_or(Value::Int(signed), |scale| {
                Value::Float(signed as f64 * scale)
            })
        }
    };
    Some(value)
}
