// SPDX-License-Identifier: Apache-2.0

//! What a decoder produces: field descriptions and values.

use std::fmt;

/// A number's type in the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScalarType {
    /// Unsigned 8-bit integer.
    U8,
    /// Signed 8-bit integer.
    I8,
    /// Unsigned 16-bit integer.
    U16,
    /// Signed 16-bit integer.
    I16,
    /// Unsigned 32-bit integer.
    U32,
    /// Signed 32-bit integer.
    I32,
    /// Unsigned 64-bit integer.
    U64,
    /// Signed 64-bit integer.
    I64,
    /// 32-bit IEEE 754 floating point.
    F32,
    /// 64-bit IEEE 754 floating point.
    F64,
    /// One byte: zero is false, anything else true.
    Bool,
}

impl ScalarType {
    /// Every type, in the order they are listed in documentation.
    pub const ALL: [Self; 11] = [
        Self::U8,
        Self::I8,
        Self::U16,
        Self::I16,
        Self::U32,
        Self::I32,
        Self::U64,
        Self::I64,
        Self::F32,
        Self::F64,
        Self::Bool,
    ];

    /// Bytes one value takes.
    #[must_use]
    pub const fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 | Self::Bool => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    /// The name used in configuration, e.g. `"u32"`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I8 => "i8",
            Self::U16 => "u16",
            Self::I16 => "i16",
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::U64 => "u64",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
        }
    }

    /// The type with this configuration name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|ty| ty.name() == name)
    }
}

/// The order of a number's bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Endianness {
    /// Least significant byte first, as on x86 and most ARM systems.
    #[default]
    Little,
    /// Most significant byte first, as in network protocols.
    Big,
}

/// One decoded value.
///
/// Integers stay integers unless the field has a scale, so 64-bit counters
/// and IDs keep every digit; scaled values are floats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    /// An unsigned integer.
    UInt(u64),
    /// A signed integer.
    Int(i64),
    /// A floating-point number, or a scaled integer.
    Float(f64),
    /// A true/false flag.
    Bool(bool),
    /// Not in this message: a field found by path, or by name, that the
    /// message doesn't have, while it has others. Never read as 0.
    Missing,
}

impl Default for Value {
    fn default() -> Self {
        Self::UInt(0)
    }
}

impl Value {
    /// The value as a float, e.g. for plotting. Integers beyond 2^53 lose
    /// their lowest digits; a missing value is NaN, which plots leave out.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "documented: plotting doesn't need integers beyond 2^53 exactly"
    )]
    pub fn as_f64(self) -> f64 {
        match self {
            Self::UInt(value) => value as f64,
            Self::Int(value) => value as f64,
            Self::Float(value) => value,
            Self::Bool(value) => f64::from(u8::from(value)),
            Self::Missing => f64::NAN,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UInt(value) => write!(f, "{value}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Bool(value) => write!(f, "{value}"),
            // Nothing: an empty cell, not a number.
            Self::Missing => Ok(()),
        }
    }
}

/// A field a decoder produces.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDescription {
    /// The field's name, e.g. `joint_positions`.
    pub name: String,
    /// Its unit, e.g. `rad`, if it has one.
    pub unit: Option<String>,
    /// How many values it holds: 1 for a single value, more for an array.
    pub count: usize,
    /// The type of each value as declared.
    pub ty: ScalarType,
}

/// Why a payload couldn't be decoded. Cheap to create: decoding runs once per
/// message, and a failure is counted, not reported each time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload is shorter than the layout needs.
    TooShort {
        /// Bytes the layout reads up to.
        needed: usize,
        /// Bytes the payload has.
        got: usize,
    },
    /// The output slice doesn't hold exactly one payload's values.
    OutputSize {
        /// Values one payload decodes into.
        needed: usize,
        /// Values the slice holds.
        got: usize,
    },
    /// The payload isn't valid in its format, e.g. not JSON.
    Malformed,
    /// A field's path isn't in the payload, or its array is too short.
    Missing {
        /// The field's position in the decoder's fields, from 0.
        field: usize,
    },
    /// A field's value can't be converted to its declared type.
    WrongType {
        /// The field's position in the decoder's fields, from 0.
        field: usize,
    },
}

impl DecodeError {
    /// The error in words, naming the field it is about from the decoder's
    /// `fields` where it is about one.
    #[must_use]
    pub fn describe(&self, fields: &[FieldDescription]) -> String {
        let name = |field: usize| fields.get(field).map(|field| field.name.as_str());
        match *self {
            Self::Missing { field } => name(field).map_or_else(
                || self.to_string(),
                |name| format!("`{name}` isn't in the message"),
            ),
            Self::WrongType { field } => name(field).map_or_else(
                || self.to_string(),
                |name| format!("`{name}` doesn't hold a value of its type"),
            ),
            _ => self.to_string(),
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { needed, got } => {
                write!(f, "the message has {got} bytes; the layout needs {needed}")
            }
            Self::OutputSize { needed, got } => {
                write!(
                    f,
                    "{got} output values given; one message decodes into {needed}"
                )
            }
            Self::Malformed => f.write_str("the message isn't valid in its format"),
            Self::Missing { field } => {
                write!(f, "field {} isn't in the message", field + 1)
            }
            Self::WrongType { field } => {
                write!(f, "field {} doesn't hold a value of its type", field + 1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names_round_trip() {
        for ty in ScalarType::ALL {
            assert_eq!(ScalarType::from_name(ty.name()), Some(ty));
        }
        assert_eq!(ScalarType::from_name("float"), None);
    }

    #[test]
    fn sizes_match_the_types() {
        assert_eq!(ScalarType::Bool.size(), 1);
        assert_eq!(ScalarType::I16.size(), 2);
        assert_eq!(ScalarType::F32.size(), 4);
        assert_eq!(ScalarType::U64.size(), 8);
    }

    #[test]
    fn values_convert_to_floats_for_plotting() {
        assert_eq!(Value::UInt(3).as_f64(), 3.0);
        assert_eq!(Value::Int(-2).as_f64(), -2.0);
        assert_eq!(Value::Bool(true).as_f64(), 1.0);
    }

    #[test]
    fn errors_about_a_field_name_it_when_the_fields_are_known() {
        let fields = [FieldDescription {
            name: "joints".to_owned(),
            unit: None,
            count: 7,
            ty: ScalarType::F64,
        }];
        let missing = DecodeError::Missing { field: 0 };
        assert_eq!(missing.describe(&fields), "`joints` isn't in the message");
        assert_eq!(missing.describe(&[]), "field 1 isn't in the message");
        assert_eq!(
            DecodeError::WrongType { field: 0 }.describe(&fields),
            "`joints` doesn't hold a value of its type"
        );
        assert_eq!(
            DecodeError::Malformed.describe(&fields),
            "the message isn't valid in its format"
        );
    }

    #[test]
    fn decode_errors_say_what_was_expected() {
        let error = DecodeError::TooShort {
            needed: 68,
            got: 40,
        };
        assert_eq!(
            error.to_string(),
            "the message has 40 bytes; the layout needs 68"
        );
    }
}
