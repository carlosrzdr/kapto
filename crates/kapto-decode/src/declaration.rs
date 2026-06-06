// SPDX-License-Identifier: Apache-2.0

//! A stream's decoder as declared: any kind, compiled, described and stored
//! the same way.

use std::collections::BTreeMap;

use kapto_core::{ErrorCode, KaptoError};

use crate::Decoder;
use crate::cdr::CdrLayout;
use crate::field::FieldDescription;
use crate::layout::StructLayout;
use crate::path::{PathField, PathFormat, PathLayout};
use crate::schema::{
    CDR_ENCODING, FIELDS_METADATA, ROS2MSG_ENCODING, STRUCT_ENCODING, path_fields_from_json,
    path_fields_json,
};

/// How a stream's payloads decode into named values.
#[derive(Clone, Debug, PartialEq)]
pub enum DecoderDeclaration {
    /// A fixed binary layout (`kind = "struct"`).
    Struct(StructLayout),
    /// JSON or MessagePack, each field found by its path (`kind = "json"`
    /// or `"msgpack"`).
    Path(PathLayout),
    /// ROS 2 messages of one type, each field found by its path
    /// (`kind = "cdr"`).
    Cdr(CdrLayout),
}

/// A decoder as it is stored with its stream's channel in a recording, so
/// the recording decodes without the configuration it was made with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDecoder {
    /// The channel's message encoding: how its payloads are laid out.
    pub message_encoding: &'static str,
    /// The schema's name.
    pub schema_name: String,
    /// The format of `schema_data`.
    pub schema_encoding: &'static str,
    /// The schema itself.
    pub schema_data: Vec<u8>,
    /// What the channel's metadata holds besides.
    pub channel_metadata: BTreeMap<String, String>,
}

impl DecoderDeclaration {
    /// Checks the declaration and compiles it into a decoder.
    ///
    /// # Errors
    ///
    /// Every problem found, each `decoder.invalid_layout`, or
    /// `decoder.invalid_schema` for a ROS 2 definition that can't be read.
    pub fn compile(&self) -> Result<Box<dyn Decoder>, Vec<KaptoError>> {
        match self {
            Self::Struct(layout) => Ok(Box::new(layout.compile()?)),
            Self::Path(layout) => Ok(Box::new(layout.compile()?)),
            Self::Cdr(layout) => Ok(Box::new(layout.compile()?)),
        }
    }

    /// The fields it produces, in output order, without compiling it.
    #[must_use]
    pub fn fields(&self) -> Vec<FieldDescription> {
        let by_path = |fields: &[PathField]| {
            fields
                .iter()
                .map(|field| FieldDescription {
                    name: field.name.clone(),
                    unit: field.unit.clone(),
                    count: field.count,
                    ty: field.ty,
                })
                .collect()
        };
        match self {
            Self::Struct(layout) => layout
                .fields
                .iter()
                .map(|field| FieldDescription {
                    name: field.name.clone(),
                    unit: field.unit.clone(),
                    count: field.count,
                    ty: field.ty,
                })
                .collect(),
            Self::Path(layout) => by_path(&layout.fields),
            Self::Cdr(layout) => by_path(&layout.fields),
        }
    }

    /// How it is stored with the channel of the stream named `stream`. Kapto's
    /// own kinds store the whole declaration as the schema, named after the
    /// stream; a ROS 2 stream stores its type's definition as rosbag2 does,
    /// and its fields in the channel's metadata.
    #[must_use]
    pub fn stored(&self, stream: &str) -> StoredDecoder {
        let own = |encoding: &'static str, json: String| StoredDecoder {
            message_encoding: encoding,
            schema_name: stream.to_owned(),
            schema_encoding: encoding,
            schema_data: json.into_bytes(),
            channel_metadata: BTreeMap::new(),
        };
        match self {
            Self::Struct(layout) => own(STRUCT_ENCODING, layout.to_schema_json()),
            Self::Path(layout) => own(layout.format.encoding(), layout.to_schema_json()),
            Self::Cdr(layout) => StoredDecoder {
                message_encoding: CDR_ENCODING,
                schema_name: layout.type_name.clone(),
                schema_encoding: ROS2MSG_ENCODING,
                schema_data: layout.definition.clone().into_bytes(),
                channel_metadata: BTreeMap::from([(
                    FIELDS_METADATA.to_owned(),
                    path_fields_json(&layout.fields),
                )]),
            },
        }
    }

    /// The declaration stored with a channel: its schema's name, encoding and
    /// data, and the channel's metadata. `None` if the schema's encoding
    /// isn't one Kapto reads. A ROS 2 channel without Kapto's fields (one
    /// recorded by another tool) decodes into no values.
    ///
    /// # Errors
    ///
    /// `decoder.invalid_schema` if the data isn't valid for its encoding.
    pub fn from_stored(
        schema_name: &str,
        schema_encoding: &str,
        data: &[u8],
        channel_metadata: &BTreeMap<String, String>,
    ) -> Option<Result<Self, KaptoError>> {
        let format = PathFormat::from_encoding(schema_encoding);
        let known = [STRUCT_ENCODING, ROS2MSG_ENCODING];
        if format.is_none() && !known.contains(&schema_encoding) {
            return None;
        }
        let text = match std::str::from_utf8(data) {
            Ok(text) => text,
            Err(error) => {
                return Some(Err(
                    KaptoError::new(ErrorCode::DecoderInvalidSchema).detail(error.to_string())
                ));
            }
        };
        Some(match format {
            Some(format) => PathLayout::from_schema_json(format, text).map(Self::Path),
            None if schema_encoding == ROS2MSG_ENCODING => channel_metadata
                .get(FIELDS_METADATA)
                .map_or_else(|| Ok(Vec::new()), |json| path_fields_from_json(json))
                .map(|fields| {
                    Self::Cdr(CdrLayout {
                        type_name: schema_name.to_owned(),
                        definition: text.to_owned(),
                        fields,
                    })
                }),
            None => StructLayout::from_schema_json(text).map(Self::Struct),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{ScalarType, Value};
    use crate::layout::FieldSpec;
    use crate::path::PathField;

    fn joint_state() -> CdrLayout {
        CdrLayout {
            type_name: "sensor_msgs/msg/JointState".to_owned(),
            definition: "float64[] position\n".to_owned(),
            fields: vec![PathField {
                count: 2,
                ..PathField::new("position", "position", ScalarType::F64)
            }],
        }
    }

    fn read_back(stored: &StoredDecoder) -> Option<Result<DecoderDeclaration, KaptoError>> {
        DecoderDeclaration::from_stored(
            &stored.schema_name,
            stored.schema_encoding,
            &stored.schema_data,
            &stored.channel_metadata,
        )
    }

    #[test]
    fn every_kind_is_stored_and_read_back_from_its_channel() {
        let declarations = [
            DecoderDeclaration::Struct(StructLayout {
                fields: vec![FieldSpec::new("a", ScalarType::U8, 0)],
            }),
            DecoderDeclaration::Path(PathLayout {
                format: PathFormat::Json,
                fields: vec![PathField::new("a", "a", ScalarType::U8)],
            }),
            DecoderDeclaration::Path(PathLayout {
                format: PathFormat::MsgPack,
                fields: vec![PathField::new("a", "b.c", ScalarType::F64)],
            }),
            DecoderDeclaration::Cdr(joint_state()),
        ];
        for declaration in declarations {
            let stored = declaration.stored("arm");
            assert_eq!(read_back(&stored).unwrap().unwrap(), declaration);
        }
        let unknown = BTreeMap::new();
        assert!(DecoderDeclaration::from_stored("arm", "protobuf", b"", &unknown).is_none());
        let error = DecoderDeclaration::from_stored("arm", STRUCT_ENCODING, &[0xff], &unknown)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::DecoderInvalidSchema);
    }

    #[test]
    fn a_ros_2_stream_is_stored_as_rosbag2_stores_its_type_with_kapto_s_fields_beside() {
        let stored = DecoderDeclaration::Cdr(joint_state()).stored("arm");
        assert_eq!(
            (
                stored.message_encoding,
                stored.schema_name.as_str(),
                stored.schema_encoding
            ),
            ("cdr", "sensor_msgs/msg/JointState", "ros2msg")
        );
        assert_eq!(stored.schema_data, b"float64[] position\n");
        assert_eq!(
            stored.channel_metadata[FIELDS_METADATA],
            r#"{"fields":[{"name":"position","type":"f64","path":"position","count":2,"scale":null,"unit":null}]}"#
        );
        // Recorded by another tool, without Kapto's fields: no values.
        let without = StoredDecoder {
            channel_metadata: BTreeMap::new(),
            ..stored.clone()
        };
        let Some(Ok(DecoderDeclaration::Cdr(layout))) = read_back(&without) else {
            panic!("a ros2msg channel reads as a ROS 2 stream");
        };
        assert_eq!(layout.fields, []);
        let broken = StoredDecoder {
            channel_metadata: BTreeMap::from([(FIELDS_METADATA.to_owned(), "[".to_owned())]),
            ..stored
        };
        let error = read_back(&broken).unwrap().unwrap_err();
        assert_eq!(error.code(), ErrorCode::DecoderInvalidSchema);
    }

    #[test]
    fn a_compiled_declaration_decodes_and_describes_its_fields() {
        let declaration = DecoderDeclaration::Path(PathLayout {
            format: PathFormat::Json,
            fields: vec![PathField {
                unit: Some("m".to_owned()),
                ..PathField::new("height", "pose.z", ScalarType::F64)
            }],
        });
        let decoder = declaration.compile().unwrap();
        assert_eq!(decoder.fields(), declaration.fields());
        let mut out = [Value::default()];
        decoder.decode(br#"{"pose":{"z":1.5}}"#, &mut out).unwrap();
        assert_eq!(out, [Value::Float(1.5)]);
    }
}
