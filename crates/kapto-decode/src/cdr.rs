// SPDX-License-Identifier: Apache-2.0

//! ROS 2 messages: CDR payloads read with their `.msg` definitions.
//!
//! [`MessageDefinitions`] reads a message type's definition and those of the
//! types it uses, as rosbag2 stores them in MCAP. [`deserializer`] reads one
//! CDR payload of that type through serde, so it can be turned into any
//! serde type or walked without building anything. [`CdrLayout`] finds
//! fields in it by path, the way JSON and MessagePack fields are found
//! ([`crate::path`]), with the same conversion rules: `header.stamp.sec`,
//! `position[2]`.

pub mod de;
pub mod msg;

use kapto_core::KaptoError;
use serde::de::DeserializeSeed;

pub use de::{CdrDeserializer, CdrError, deserializer};
pub use msg::{Arity, Element, MessageDefinitions, MsgField, Primitive, assemble, msg_files_in};

use crate::Decoder;
use crate::field::{DecodeError, FieldDescription, Value};
use crate::path::{PathField, Paths};

/// A ROS 2 message type, its definition, and the fields to find in it.
#[derive(Clone, Debug, PartialEq)]
pub struct CdrLayout {
    /// The message type, e.g. `sensor_msgs/msg/JointState`.
    pub type_name: String,
    /// Its definition and those of the types it uses, as rosbag2 stores
    /// them.
    pub definition: String,
    /// The fields, in output order, found by path.
    pub fields: Vec<PathField>,
}

impl CdrLayout {
    /// Reads the definition, checks the fields and compiles them, for
    /// messages of at most `max_message_bytes`.
    ///
    /// # Errors
    ///
    /// `decoder.invalid_schema` if the definition can't be read; otherwise
    /// every problem with the fields, each `decoder.invalid_layout`, or
    /// `decoder.too_many_values` for more values than `max_message_bytes`.
    pub fn compile(&self, max_message_bytes: usize) -> Result<CdrDecoder, Vec<KaptoError>> {
        let definitions = MessageDefinitions::parse(&self.type_name, &self.definition)
            .map_err(|error| vec![error])?;
        Ok(CdrDecoder {
            definitions,
            paths: Paths::compile(&self.fields, max_message_bytes)?,
        })
    }
}

/// A compiled [`CdrLayout`].
#[derive(Clone, Debug, PartialEq)]
pub struct CdrDecoder {
    definitions: MessageDefinitions,
    paths: Paths,
}

impl Decoder for CdrDecoder {
    fn fields(&self) -> &[FieldDescription] {
        &self.paths.fields
    }

    fn value_count(&self) -> usize {
        self.paths.value_count()
    }

    fn decode(&self, payload: &[u8], out: &mut [Value]) -> Result<(), DecodeError> {
        self.paths.decode(out, |follow| {
            deserializer(payload, &self.definitions)
                .is_ok_and(|mut cdr| follow.deserialize(&mut cdr).is_ok())
        })
    }
}
