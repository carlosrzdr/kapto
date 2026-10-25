// SPDX-License-Identifier: Apache-2.0

//! Reading CDR, as ROS 2 serialises messages, through serde: a message is a
//! map from field names to values, an array a sequence, a number or flag a
//! number or flag, a string a string. Anything serde can build or walk, such
//! as the path follower of JSON and MessagePack decoding, reads CDR the same
//! way.
//!
//! The 4-byte encapsulation header says the byte order (`00 01` little,
//! `00 00` big endian). Every primitive is aligned to its own size, counted
//! from the end of that header. A string is a 4-byte length including its
//! terminating NUL, then its bytes; a `wstring` a 4-byte count of
//! characters, then 4 bytes per character. A sequence is a 4-byte count,
//! then its elements; a fixed array just its elements. A message without
//! fields takes one byte. Bytes after the message are ignored: serialisers
//! may hand over a padded buffer.

use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::forward_to_deserialize_any;

use super::msg::{Arity, MessageDefinitions, Primitive, ReadElement, ReadField};

/// Why CDR couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdrError(String);

impl fmt::Display for CdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CdrError {}

impl de::Error for CdrError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

/// The bytes being read, and where.
#[derive(Debug)]
struct Reader<'de> {
    data: &'de [u8],
    pos: usize,
    little: bool,
    /// How many messages deep the value being read is.
    depth: usize,
}

/// The most messages one can be nested in, as `serde_json` allows: a type
/// may hold itself through a sequence, and a payload nesting it deeper
/// would take the stack, not a few bytes a level, before it was refused.
const MOST_NESTED: usize = 128;

/// Where alignment is counted from: the end of the encapsulation header.
const ORIGIN: usize = 4;

impl<'de> Reader<'de> {
    fn take(&mut self, length: usize) -> Result<&'de [u8], CdrError> {
        let end = self
            .pos
            .checked_add(length)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| CdrError("the message ends early".to_owned()))?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn align(&mut self, size: usize) -> Result<(), CdrError> {
        let misaligned = (self.pos - ORIGIN) % size;
        if misaligned != 0 {
            self.take(size - misaligned)?;
        }
        Ok(())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CdrError> {
        self.align(N)?;
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.take(N)?);
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, CdrError> {
        let bytes = self.array::<4>()?;
        Ok(if self.little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    /// A count of what follows, checked against the bytes left so a
    /// damaged count can't promise more than the message holds.
    fn count(&mut self, each_at_least: usize) -> Result<usize, CdrError> {
        let count = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        let left = self.data.len() - self.pos;
        if count.saturating_mul(each_at_least) > left {
            return Err(CdrError(format!(
                "a count of {count} doesn't fit in the {left} bytes left"
            )));
        }
        Ok(count)
    }
}

macro_rules! number {
    ($reader:expr, $ty:ty) => {{
        let bytes = $reader.array::<{ size_of::<$ty>() }>()?;
        if $reader.little {
            <$ty>::from_le_bytes(bytes)
        } else {
            <$ty>::from_be_bytes(bytes)
        }
    }};
}

/// Starts reading `payload`, a message of `definitions`' root type.
///
/// # Errors
///
/// The payload is too short for its header, or the header names an
/// encoding other than plain CDR.
pub fn deserializer<'de, 'd>(
    payload: &'de [u8],
    definitions: &'d MessageDefinitions,
) -> Result<CdrDeserializer<'de, 'd>, CdrError> {
    let little = match payload.get(..2) {
        Some([0, 1]) => true,
        Some([0, 0]) => false,
        Some(other) => {
            return Err(CdrError(format!(
                "encapsulation {:02x}{:02x} isn't plain CDR",
                other[0], other[1]
            )));
        }
        None => return Err(CdrError("the message ends early".to_owned())),
    };
    if payload.len() < ORIGIN {
        return Err(CdrError("the message ends early".to_owned()));
    }
    Ok(CdrDeserializer {
        reader: Reader {
            data: payload,
            pos: ORIGIN,
            little,
            depth: 0,
        },
        definitions,
    })
}

/// A CDR message, ready to be read through serde.
#[derive(Debug)]
pub struct CdrDeserializer<'de, 'd> {
    reader: Reader<'de>,
    definitions: &'d MessageDefinitions,
}

impl<'de> Deserializer<'de> for &mut CdrDeserializer<'de, '_> {
    type Error = CdrError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, CdrError> {
        Value {
            reader: &mut self.reader,
            definitions: self.definitions,
            element: self.definitions.root_element(),
            arity: Arity::One,
        }
        .deserialize_any(visitor)
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

/// One field's values, about to be read.
struct Value<'r, 'de, 'd> {
    reader: &'r mut Reader<'de>,
    definitions: &'d MessageDefinitions,
    element: &'d ReadElement,
    arity: Arity,
}

impl<'de> Deserializer<'de> for Value<'_, 'de, '_> {
    type Error = CdrError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, CdrError> {
        let remaining = match self.arity {
            Arity::One => return element(self.reader, self.definitions, self.element, visitor),
            Arity::Fixed(count) => count,
            Arity::Sequence(_) => self.reader.count(minimum_size(self.element))?,
        };
        visitor.visit_seq(Elements {
            reader: self.reader,
            definitions: self.definitions,
            element: self.element,
            remaining,
        })
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

/// The fewest bytes one element can take, for checking counts.
fn minimum_size(element: &ReadElement) -> usize {
    match element {
        ReadElement::Primitive(Primitive::Int16 | Primitive::Uint16) => 2,
        ReadElement::Primitive(
            Primitive::Int32
            | Primitive::Uint32
            | Primitive::Float32
            | Primitive::String
            | Primitive::WString,
        ) => 4,
        ReadElement::Primitive(Primitive::Int64 | Primitive::Uint64 | Primitive::Float64) => 8,
        // An empty message still takes a byte.
        ReadElement::Primitive(_) | ReadElement::Message(_) => 1,
    }
}

/// One value of `element`.
fn element<'de, V: Visitor<'de>>(
    reader: &mut Reader<'de>,
    definitions: &MessageDefinitions,
    element: &ReadElement,
    visitor: V,
) -> Result<V::Value, CdrError> {
    let primitive = match element {
        ReadElement::Message(place) => {
            let fields = definitions
                .read_fields(*place)
                .ok_or_else(|| CdrError("a message type that isn't defined".to_owned()))?;
            if fields.is_empty() {
                // ROS 2 gives an empty message one placeholder byte.
                reader.take(1)?;
            }
            if reader.depth >= MOST_NESTED {
                return Err(CdrError(format!(
                    "messages nested more than {MOST_NESTED} deep"
                )));
            }
            reader.depth += 1;
            let read = visitor.visit_map(MessageMap {
                reader: &mut *reader,
                definitions,
                fields,
                next: 0,
            });
            reader.depth -= 1;
            return read;
        }
        ReadElement::Primitive(primitive) => *primitive,
    };
    match primitive {
        Primitive::Bool => visitor.visit_bool(reader.take(1)?[0] != 0),
        Primitive::Byte | Primitive::Char | Primitive::Uint8 => {
            visitor.visit_u64(u64::from(reader.take(1)?[0]))
        }
        Primitive::Int8 => visitor.visit_i64(i64::from(i8::from_ne_bytes([reader.take(1)?[0]]))),
        Primitive::Int16 => visitor.visit_i64(i64::from(number!(reader, i16))),
        Primitive::Uint16 => visitor.visit_u64(u64::from(number!(reader, u16))),
        Primitive::Int32 => visitor.visit_i64(i64::from(number!(reader, i32))),
        Primitive::Uint32 => visitor.visit_u64(u64::from(number!(reader, u32))),
        Primitive::Int64 => visitor.visit_i64(number!(reader, i64)),
        Primitive::Uint64 => visitor.visit_u64(number!(reader, u64)),
        Primitive::Float32 => visitor.visit_f64(f64::from(number!(reader, f32))),
        Primitive::Float64 => visitor.visit_f64(number!(reader, f64)),
        Primitive::String => {
            let length = reader.count(1)?;
            let bytes = reader.take(length)?;
            // The length counts the terminating NUL; some writers give 0 for
            // an empty string.
            let text = bytes.strip_suffix(&[0]).unwrap_or(bytes);
            match std::str::from_utf8(text) {
                Ok(text) => visitor.visit_borrowed_str(text),
                Err(_) => visitor.visit_borrowed_bytes(text),
            }
        }
        Primitive::WString => {
            let count = reader.count(4)?;
            let mut text = String::with_capacity(count);
            for _ in 0..count {
                let code = number!(reader, u32);
                text.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            visitor.visit_string(text)
        }
    }
}

/// A message's fields, in order, as a map.
struct MessageMap<'r, 'de, 'd> {
    reader: &'r mut Reader<'de>,
    definitions: &'d MessageDefinitions,
    fields: &'d [ReadField],
    next: usize,
}

impl<'de> MapAccess<'de> for MessageMap<'_, 'de, '_> {
    type Error = CdrError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, CdrError> {
        let Some(field) = self.fields.get(self.next) else {
            return Ok(None);
        };
        seed.deserialize(de::value::StrDeserializer::<CdrError>::new(&field.name))
            .map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, CdrError> {
        let field = self
            .fields
            .get(self.next)
            .ok_or_else(|| CdrError("a value without a field".to_owned()))?;
        self.next += 1;
        seed.deserialize(Value {
            reader: self.reader,
            definitions: self.definitions,
            element: &field.element,
            arity: field.arity,
        })
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.fields.len() - self.next)
    }
}

/// An array's elements, as a sequence.
struct Elements<'r, 'de, 'd> {
    reader: &'r mut Reader<'de>,
    definitions: &'d MessageDefinitions,
    element: &'d ReadElement,
    remaining: usize,
}

impl<'de> SeqAccess<'de> for Elements<'_, 'de, '_> {
    type Error = CdrError;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, CdrError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        seed.deserialize(Value {
            reader: self.reader,
            definitions: self.definitions,
            element: self.element,
            arity: Arity::One,
        })
        .map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.remaining)
    }
}
