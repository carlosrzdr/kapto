// SPDX-License-Identifier: Apache-2.0

//! Declarative decoders that turn robot payloads into named, unit-tagged fields.
//!
//! Payloads are recorded exactly as received; decoding reads values out of
//! them without changing them. A [`Decoder`] says which fields it produces
//! ([`Decoder::fields`]) and fills a caller-provided slice of [`Value`]s from
//! one payload ([`Decoder::decode`]), so decoding never allocates.
//!
//! [`StructLayout`] describes payloads with a fixed binary layout: each field
//! at a byte offset, with a type, byte order, optional array length and
//! scale. It is compiled once into a [`StructDecoder`].
//!
//! [`PathLayout`] describes JSON and MessagePack payloads: each field found
//! by its path through the message (see [`path`]). It compiles into a
//! [`PathDecoder`].
//!
//! [`DecoderDeclaration`] is either, as a stream declares it.
//!
//! [`cdr`] reads ROS 2 messages: CDR payloads with their `.msg` definitions.
//! [`protobuf`] reads Protobuf messages with a descriptor set.

pub mod camera;
pub mod cdr;
pub mod declaration;
pub mod field;
pub mod layout;
pub mod path;
pub mod protobuf;
pub mod schema;

pub use cdr::{CdrDecoder, CdrLayout, MessageDefinitions};
pub use declaration::DecoderDeclaration;
pub use declaration::StoredDecoder;
pub use field::{DecodeError, Endianness, FieldDescription, ScalarType, Value};
pub use layout::{FieldSpec, StructDecoder, StructLayout};
pub use path::{PathDecoder, PathField, PathFormat, PathLayout};
pub use protobuf::{ProtobufDecoder, ProtobufLayout};
pub use schema::{
    CDR_ENCODING, FIELDS_METADATA, JSON_ENCODING, MSGPACK_ENCODING, ROS2MSG_ENCODING,
    STRUCT_ENCODING, TOPIC_METADATA,
};

/// Reads named values out of payloads of one kind.
pub trait Decoder: Send + Sync {
    /// The fields this decoder produces, in output order. Each takes
    /// [`FieldDescription::count`] consecutive values in the output.
    fn fields(&self) -> &[FieldDescription];

    /// The number of values one payload decodes into: the sum of every
    /// field's count. Size the output slice with it.
    fn value_count(&self) -> usize {
        self.fields().iter().map(|field| field.count).sum()
    }

    /// Decodes `payload` into `out`, which holds [`Decoder::value_count`]
    /// values. Struct decoding never allocates; JSON and MessagePack only for
    /// keys written with escape sequences.
    ///
    /// # Errors
    ///
    /// When the payload doesn't hold every field, or `out` is the wrong
    /// size. The caller counts the message as not decoded; the payload itself
    /// is still recorded.
    fn decode(&self, payload: &[u8], out: &mut [Value]) -> Result<(), DecodeError>;
}
