// SPDX-License-Identifier: Apache-2.0

//! Protobuf messages, read with a descriptor set (`protoc
//! --descriptor_set_out`, `buf build`) rather than generated code.
//!
//! [`ProtobufLayout`] names the message type in the set and finds fields in
//! it by path, the way JSON and MessagePack fields are found
//! ([`crate::path`]), with the same conversion rules: `header.stamp.sec`,
//! `position[2]`, `gains.kp` for a map's entry. Every field of a message
//! has a value: one not sent has its default, as proto3 defines. An enum is
//! its number.
//!
//! Decoding builds the message first (with `prost-reflect`), so unlike JSON
//! it allocates; it is meant for viewing and checking, not for every
//! message on the recording path.

use std::fmt;

use kapto_core::{ErrorCode, KaptoError};
use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, MapKey, MessageDescriptor, ReflectMessage,
};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::forward_to_deserialize_any;

use crate::Decoder;
use crate::field::{DecodeError, FieldDescription, Value};
use crate::path::{PathField, Paths};

/// A Protobuf message type, the descriptor set that defines it, and the
/// fields to find in it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProtobufLayout {
    /// The message's full name, e.g. `robot.v1.JointState`.
    pub message: String,
    /// A serialised `FileDescriptorSet` that defines it and every type it
    /// uses.
    pub descriptor_set: Vec<u8>,
    /// The fields, in output order, found by path.
    pub fields: Vec<PathField>,
}

impl ProtobufLayout {
    /// Reads the descriptor set, finds the message and compiles the fields.
    ///
    /// # Errors
    ///
    /// `decoder.invalid_schema` if the set can't be read or doesn't define
    /// the message; otherwise every problem with the fields, each
    /// `decoder.invalid_layout`.
    pub fn compile(&self) -> Result<ProtobufDecoder, Vec<KaptoError>> {
        let invalid =
            |detail: String| vec![KaptoError::new(ErrorCode::DecoderInvalidSchema).detail(detail)];
        let pool = DescriptorPool::decode(self.descriptor_set.as_slice())
            .map_err(|error| invalid(format!("the descriptor set can't be read: {error}")))?;
        let message = pool.get_message_by_name(&self.message).ok_or_else(|| {
            invalid(format!(
                "the descriptor set doesn't define `{}`",
                self.message
            ))
        })?;
        Ok(ProtobufDecoder {
            message,
            paths: Paths::compile(&self.fields)?,
        })
    }
}

/// A compiled [`ProtobufLayout`].
#[derive(Clone, Debug)]
pub struct ProtobufDecoder {
    message: MessageDescriptor,
    paths: Paths,
}

impl PartialEq for ProtobufDecoder {
    fn eq(&self, other: &Self) -> bool {
        self.message.full_name() == other.message.full_name() && self.paths == other.paths
    }
}

impl Decoder for ProtobufDecoder {
    fn fields(&self) -> &[FieldDescription] {
        &self.paths.fields
    }

    fn decode(&self, payload: &[u8], out: &mut [Value]) -> Result<(), DecodeError> {
        let message = DynamicMessage::decode(self.message.clone(), payload)
            .map_err(|_| DecodeError::Malformed)?;
        self.paths.decode(out, |follow| {
            follow.deserialize(Walk::Message(&message)).is_ok()
        })
    }
}

/// Why a message couldn't be walked: only what the visitor reports.
#[derive(Debug)]
struct WalkError(String);

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WalkError {}

impl de::Error for WalkError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

/// A decoded value, walked through serde.
enum Walk<'a> {
    Message(&'a DynamicMessage),
    Value(&'a prost_reflect::Value),
    Key(&'a MapKey),
}

impl<'de> Deserializer<'de> for Walk<'_> {
    type Error = WalkError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, WalkError> {
        use prost_reflect::Value as Pb;
        let value = match self {
            Self::Message(message) => {
                return visitor.visit_map(MessageFields {
                    message,
                    descriptors: message.descriptor().fields().collect(),
                    next: 0,
                });
            }
            Self::Key(key) => {
                return match key {
                    MapKey::Bool(flag) => visitor.visit_bool(*flag),
                    MapKey::I32(number) => visitor.visit_i64(i64::from(*number)),
                    MapKey::I64(number) => visitor.visit_i64(*number),
                    MapKey::U32(number) => visitor.visit_u64(u64::from(*number)),
                    MapKey::U64(number) => visitor.visit_u64(*number),
                    MapKey::String(text) => visitor.visit_str(text),
                };
            }
            Self::Value(value) => value,
        };
        match value {
            Pb::Bool(flag) => visitor.visit_bool(*flag),
            Pb::I32(number) | Pb::EnumNumber(number) => visitor.visit_i64(i64::from(*number)),
            Pb::I64(number) => visitor.visit_i64(*number),
            Pb::U32(number) => visitor.visit_u64(u64::from(*number)),
            Pb::U64(number) => visitor.visit_u64(*number),
            Pb::F32(number) => visitor.visit_f64(f64::from(*number)),
            Pb::F64(number) => visitor.visit_f64(*number),
            Pb::String(text) => visitor.visit_str(text),
            Pb::Bytes(bytes) => visitor.visit_bytes(bytes),
            Pb::Message(message) => Walk::Message(message).deserialize_any(visitor),
            Pb::List(items) => visitor.visit_seq(Items {
                items: items.iter(),
            }),
            Pb::Map(entries) => {
                // In key order, so walking a map is the same every time.
                let mut entries: Vec<_> = entries.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                visitor.visit_map(Entries {
                    entries: entries.into_iter(),
                    value: None,
                })
            }
        }
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

/// A message's fields, in declaration order, as a map.
struct MessageFields<'a> {
    message: &'a DynamicMessage,
    descriptors: Vec<FieldDescriptor>,
    next: usize,
}

impl<'de> MapAccess<'de> for MessageFields<'_> {
    type Error = WalkError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, WalkError> {
        let Some(field) = self.descriptors.get(self.next) else {
            return Ok(None);
        };
        seed.deserialize(de::value::StrDeserializer::<WalkError>::new(field.name()))
            .map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, WalkError> {
        let field = self
            .descriptors
            .get(self.next)
            .ok_or_else(|| WalkError("a value without a field".to_owned()))?;
        self.next += 1;
        let value = self.message.get_field(field);
        seed.deserialize(Walk::Value(&value))
    }
}

/// A repeated field's values.
struct Items<'a> {
    items: std::slice::Iter<'a, prost_reflect::Value>,
}

impl<'de> SeqAccess<'de> for Items<'_> {
    type Error = WalkError;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, WalkError> {
        self.items
            .next()
            .map(|item| seed.deserialize(Walk::Value(item)))
            .transpose()
    }
}

/// A map field's entries.
struct Entries<'a, I: Iterator<Item = (&'a MapKey, &'a prost_reflect::Value)>> {
    entries: I,
    value: Option<&'a prost_reflect::Value>,
}

impl<'a, 'de, I: Iterator<Item = (&'a MapKey, &'a prost_reflect::Value)>> MapAccess<'de>
    for Entries<'a, I>
{
    type Error = WalkError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, WalkError> {
        let Some((key, value)) = self.entries.next() else {
            return Ok(None);
        };
        self.value = Some(value);
        seed.deserialize(Walk::Key(key)).map(Some)
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, WalkError> {
        let value = self
            .value
            .take()
            .ok_or_else(|| WalkError("a value without a key".to_owned()))?;
        seed.deserialize(Walk::Value(value))
    }
}
