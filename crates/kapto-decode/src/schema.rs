// SPDX-License-Identifier: Apache-2.0

//! How a struct layout is stored in a recording, so the recording can be
//! decoded later without the configuration it was made with.
//!
//! A stream recorded with a struct layout gets an MCAP channel whose message
//! encoding is [`STRUCT_ENCODING`], and a schema of the same encoding whose
//! data is the layout as JSON:
//!
//! ```json
//! {"fields": [
//!   {"name": "sequence", "type": "u32", "offset": 0, "count": 1,
//!    "byte_order": "little", "scale": null, "unit": null}
//! ]}
//! ```
//!
//! Every key is always written. When reading, only `name`, `type` and
//! `offset` are required; the others take the same defaults as in
//! `streams.toml`.
//!
//! JSON and MessagePack streams are stored the same way, with encodings
//! [`JSON_ENCODING`] and [`MSGPACK_ENCODING`] and a `path` for each field
//! instead of `offset` and `byte_order`:
//!
//! ```json
//! {"fields": [
//!   {"name": "position", "type": "f64", "path": "joints.position",
//!    "count": 7, "scale": null, "unit": "rad"}
//! ]}
//! ```
//!
//! ROS 2 streams (CDR) are stored as ROS 2's own tools store them, so they
//! read the recording too: message encoding [`CDR_ENCODING`], and a schema
//! of encoding [`ROS2MSG_ENCODING`] named after the message type, whose data
//! is the type's definition as rosbag2 stores it. Kapto's fields, in the
//! same JSON as a JSON stream's, go in the channel's metadata under
//! [`FIELDS_METADATA`].

use kapto_core::{ErrorCode, KaptoError};
use serde::{Deserialize, Serialize};

use crate::field::{Endianness, ScalarType};
use crate::layout::{FieldSpec, StructLayout};
use crate::path::{PathField, PathFormat, PathLayout};

/// The MCAP message and schema encoding of payloads with a struct layout.
pub const STRUCT_ENCODING: &str = "kapto.struct/v1";

/// The MCAP message and schema encoding of JSON payloads decoded by path.
pub const JSON_ENCODING: &str = "kapto.json/v1";

/// The MCAP message and schema encoding of MessagePack payloads decoded by
/// path.
pub const MSGPACK_ENCODING: &str = "kapto.msgpack/v1";

/// The MCAP message encoding of ROS 2 messages, as ROS 2's tools write it.
pub const CDR_ENCODING: &str = "cdr";

/// The MCAP schema encoding of a ROS 2 message definition, as rosbag2 writes
/// it.
pub const ROS2MSG_ENCODING: &str = "ros2msg";

/// The channel metadata key holding a ROS 2 stream's fields.
pub const FIELDS_METADATA: &str = "kapto.fields";

/// The channel metadata key holding the ROS 2 topic a stream was received
/// on: a channel is named after its stream, which isn't the topic's name.
pub const TOPIC_METADATA: &str = "ros2.topic";

#[derive(Serialize, Deserialize)]
struct SchemaFile {
    fields: Vec<SchemaField>,
}

#[derive(Serialize, Deserialize)]
struct SchemaField {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    offset: usize,
    #[serde(default = "one")]
    count: usize,
    #[serde(default = "little")]
    byte_order: String,
    #[serde(default)]
    scale: Option<f64>,
    #[serde(default)]
    unit: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct PathSchemaFile {
    fields: Vec<PathSchemaField>,
}

#[derive(Serialize, Deserialize)]
struct PathSchemaField {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    path: String,
    #[serde(default = "one")]
    count: usize,
    #[serde(default)]
    scale: Option<f64>,
    #[serde(default)]
    unit: Option<String>,
}

fn one() -> usize {
    1
}

fn little() -> String {
    "little".to_owned()
}

impl StructLayout {
    /// The layout as the JSON stored in a recording's schema.
    #[must_use]
    pub fn to_schema_json(&self) -> String {
        // Serialising plain strings, numbers and options can't fail.
        serde_json::to_string(&self.schema_file()).unwrap_or_default()
    }

    /// The same JSON, indented for people to read (e.g. in documentation).
    #[must_use]
    pub fn to_schema_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.schema_file()).unwrap_or_default()
    }

    fn schema_file(&self) -> SchemaFile {
        SchemaFile {
            fields: self
                .fields
                .iter()
                .map(|field| SchemaField {
                    name: field.name.clone(),
                    ty: field.ty.name().to_owned(),
                    offset: field.offset,
                    count: field.count,
                    byte_order: match field.endianness {
                        Endianness::Little => "little",
                        Endianness::Big => "big",
                    }
                    .to_owned(),
                    scale: field.scale,
                    unit: field.unit.clone(),
                })
                .collect(),
        }
    }

    /// Reads a layout stored in a recording's schema. The layout isn't
    /// checked: compile it to check it.
    ///
    /// # Errors
    ///
    /// `decoder.invalid_schema` if the data isn't a layout in this format.
    pub fn from_schema_json(json: &str) -> Result<Self, KaptoError> {
        let invalid =
            |detail: String| KaptoError::new(ErrorCode::DecoderInvalidSchema).detail(detail);
        let file: SchemaFile =
            serde_json::from_str(json).map_err(|error| invalid(error.to_string()))?;
        let fields = file
            .fields
            .into_iter()
            .map(|field| {
                let ty = ScalarType::from_name(&field.ty).ok_or_else(|| {
                    invalid(format!(
                        "field `{}` has unknown type `{}`",
                        field.name, field.ty
                    ))
                })?;
                let endianness = match field.byte_order.as_str() {
                    "little" => Endianness::Little,
                    "big" => Endianness::Big,
                    other => {
                        return Err(invalid(format!(
                            "field `{}` has unknown byte order `{other}`",
                            field.name
                        )));
                    }
                };
                Ok(FieldSpec {
                    name: field.name,
                    ty,
                    offset: field.offset,
                    count: field.count,
                    endianness,
                    scale: field.scale,
                    unit: field.unit,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { fields })
    }
}

impl PathFormat {
    /// The MCAP encoding of payloads in this format.
    #[must_use]
    pub const fn encoding(self) -> &'static str {
        match self {
            Self::Json => JSON_ENCODING,
            Self::MsgPack => MSGPACK_ENCODING,
        }
    }

    /// The format stored under `encoding`, if it is one of these.
    #[must_use]
    pub fn from_encoding(encoding: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|format| format.encoding() == encoding)
    }
}

impl PathLayout {
    /// The layout as the JSON stored in a recording's schema; the format is
    /// the schema's encoding.
    #[must_use]
    pub fn to_schema_json(&self) -> String {
        // Serialising plain strings, numbers and options can't fail.
        serde_json::to_string(&self.schema_file()).unwrap_or_default()
    }

    /// The same JSON, indented for people to read (e.g. in documentation).
    #[must_use]
    pub fn to_schema_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.schema_file()).unwrap_or_default()
    }

    fn schema_file(&self) -> PathSchemaFile {
        path_schema_file(&self.fields)
    }

    /// Reads a layout stored in a recording's schema. The layout isn't
    /// checked: compile it to check it.
    ///
    /// # Errors
    ///
    /// `decoder.invalid_schema` if the data isn't a layout in this format.
    pub fn from_schema_json(format: PathFormat, json: &str) -> Result<Self, KaptoError> {
        Ok(Self {
            format,
            fields: path_fields_from_json(json)?,
        })
    }
}

/// Fields found by path as stored: a JSON stream's schema, or a ROS 2
/// stream's [`FIELDS_METADATA`].
pub(crate) fn path_fields_json(fields: &[PathField]) -> String {
    // Serialising plain strings, numbers and options can't fail.
    serde_json::to_string(&path_schema_file(fields)).unwrap_or_default()
}

fn path_schema_file(fields: &[PathField]) -> PathSchemaFile {
    PathSchemaFile {
        fields: fields
            .iter()
            .map(|field| PathSchemaField {
                name: field.name.clone(),
                ty: field.ty.name().to_owned(),
                path: field.path.clone(),
                count: field.count,
                scale: field.scale,
                unit: field.unit.clone(),
            })
            .collect(),
    }
}

/// Reads fields stored by [`path_fields_json`].
///
/// # Errors
///
/// `decoder.invalid_schema` if the data isn't such fields.
pub(crate) fn path_fields_from_json(json: &str) -> Result<Vec<PathField>, KaptoError> {
    let invalid = |detail: String| KaptoError::new(ErrorCode::DecoderInvalidSchema).detail(detail);
    let file: PathSchemaFile =
        serde_json::from_str(json).map_err(|error| invalid(error.to_string()))?;
    file.fields
        .into_iter()
        .map(|field| {
            let ty = ScalarType::from_name(&field.ty).ok_or_else(|| {
                invalid(format!(
                    "field `{}` has unknown type `{}`",
                    field.name, field.ty
                ))
            })?;
            Ok(PathField {
                name: field.name,
                path: field.path,
                ty,
                count: field.count,
                scale: field.scale,
                unit: field.unit,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> StructLayout {
        StructLayout {
            fields: vec![
                FieldSpec::new("sequence", ScalarType::U32, 0),
                FieldSpec {
                    count: 7,
                    endianness: Endianness::Big,
                    scale: Some(0.001),
                    unit: Some("rad".to_owned()),
                    ..FieldSpec::new("joint_positions", ScalarType::I32, 4)
                },
            ],
        }
    }

    #[test]
    fn a_layout_round_trips_through_its_schema() {
        let json = layout().to_schema_json();
        assert_eq!(StructLayout::from_schema_json(&json).unwrap(), layout());
    }

    #[test]
    fn the_schema_writes_every_key() {
        assert_eq!(
            StructLayout {
                fields: vec![FieldSpec::new("sequence", ScalarType::U32, 0)]
            }
            .to_schema_json(),
            r#"{"fields":[{"name":"sequence","type":"u32","offset":0,"count":1,"byte_order":"little","scale":null,"unit":null}]}"#
        );
    }

    #[test]
    fn reading_fills_in_the_same_defaults_as_the_configuration() {
        let layout =
            StructLayout::from_schema_json(r#"{"fields":[{"name":"a","type":"u8","offset":3}]}"#)
                .unwrap();
        assert_eq!(layout.fields, [FieldSpec::new("a", ScalarType::U8, 3)]);
    }

    fn path_layout() -> PathLayout {
        PathLayout {
            format: PathFormat::MsgPack,
            fields: vec![
                PathField::new("sequence", "header.seq", ScalarType::U32),
                PathField {
                    count: 7,
                    scale: Some(0.001),
                    unit: Some("rad".to_owned()),
                    ..PathField::new("position", "joints.position", ScalarType::I32)
                },
            ],
        }
    }

    #[test]
    fn a_path_layout_round_trips_through_its_schema_with_the_format_as_encoding() {
        let layout = path_layout();
        let encoding = layout.format.encoding();
        assert_eq!(encoding, "kapto.msgpack/v1");
        let format = PathFormat::from_encoding(encoding).unwrap();
        let json = layout.to_schema_json();
        assert_eq!(PathLayout::from_schema_json(format, &json).unwrap(), layout);
        assert_eq!(
            PathLayout::from_schema_json(
                PathFormat::Json,
                r#"{"fields":[{"name":"a","type":"bool","path":"a"}]}"#
            )
            .unwrap()
            .fields,
            [PathField::new("a", "a", ScalarType::Bool)]
        );
        assert_eq!(PathFormat::from_encoding(STRUCT_ENCODING), None);
    }

    #[test]
    fn data_that_is_not_a_layout_is_reported() {
        for json in [
            "not json",
            r#"{"fields":[{"name":"a","type":"u24","offset":0}]}"#,
            r#"{"fields":[{"name":"a","type":"u8","offset":0,"byte_order":"middle"}]}"#,
            r#"{"fields":[{"name":"a","offset":0}]}"#,
        ] {
            let error = StructLayout::from_schema_json(json).unwrap_err();
            assert_eq!(error.code(), ErrorCode::DecoderInvalidSchema, "{json}");
        }
    }
}
