// SPDX-License-Identifier: Apache-2.0

//! Protobuf decoding by path, checked against messages encoded by buf from
//! JSON written by hand (`testdata/protobuf/`, from `cargo xtask golden
//! protobuf`), so the encoder is never Kapto's or prost's own.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::{Path, PathBuf};

use kapto_core::ErrorCode;
use kapto_decode::{
    DecodeError, Decoder, PathField, ProtobufDecoder, ProtobufLayout, ScalarType, Value,
};
use proptest::prelude::*;

fn testdata(file: &str) -> Vec<u8> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/protobuf")
        .join(file);
    std::fs::read(path).unwrap()
}

fn layout(fields: Vec<PathField>) -> ProtobufLayout {
    ProtobufLayout {
        message: "kapto.test.v1.JointState".to_owned(),
        descriptor_set: testdata("robot.binpb"),
        fields,
    }
}

fn decoder(fields: Vec<PathField>) -> ProtobufDecoder {
    layout(fields).compile().unwrap()
}

fn decode(decoder: &ProtobufDecoder, payload: &[u8]) -> Result<Vec<Value>, DecodeError> {
    let mut out = vec![Value::default(); decoder.value_count()];
    decoder.decode(payload, &mut out).map(|()| out)
}

fn every_kind() -> Vec<PathField> {
    vec![
        PathField::new("sec", "header.stamp.sec", ScalarType::I64),
        PathField::new("nanosec", "header.stamp.nanosec", ScalarType::U32),
        PathField {
            count: 3,
            unit: Some("rad".to_owned()),
            ..PathField::new("position", "position", ScalarType::F64)
        },
        PathField::new("effort", "effort[2]", ScalarType::F32),
        PathField::new("mode", "mode", ScalarType::U8),
        PathField::new("kp", "gains.kp", ScalarType::F64),
        PathField::new("raw", "raw", ScalarType::U8),
        PathField::new("offset", "offset", ScalarType::I32),
        PathField::new("counter", "counter", ScalarType::U64),
        PathField::new("enabled", "enabled", ScalarType::Bool),
        PathField::new("angle", "angle", ScalarType::F64),
        PathField::new("big", "big", ScalarType::U64),
        PathField::new("past", "history[1].sec", ScalarType::I64),
    ]
}

#[test]
fn every_kind_of_field_is_found_by_path() {
    let mut fields = every_kind();
    // Bytes aren't a number.
    fields.retain(|field| field.name != "raw");
    let values = decode(&decoder(fields), &testdata("cases/full.binpb")).unwrap();
    assert_eq!(
        values,
        [
            Value::Int(1_773_066_812),
            Value::UInt(500),
            Value::Float(0.5),
            Value::Float(-1.25),
            Value::Float(0.04),
            Value::Float(0.25),
            // MODE_RUN: an enum is its number.
            Value::UInt(2),
            Value::Float(120.5),
            Value::Int(-42),
            Value::UInt(18_000_000_000_000_000_000),
            Value::Bool(true),
            Value::Float(1.5),
            // Beyond 2^53, every digit kept.
            Value::UInt(9_007_199_254_740_993),
            Value::Int(-3),
        ]
    );
}

#[test]
fn fields_not_sent_have_their_defaults() {
    let mut fields = every_kind();
    fields.retain(|field| {
        !matches!(
            field.name.as_str(),
            "raw" | "position" | "effort" | "kp" | "past"
        )
    });
    let values = decode(&decoder(fields), &testdata("cases/empty.binpb")).unwrap();
    assert_eq!(
        values,
        [
            Value::Int(0),
            Value::UInt(0),
            Value::UInt(0),
            Value::Int(0),
            Value::UInt(0),
            Value::Bool(false),
            Value::Float(0.0),
            Value::UInt(0),
        ]
    );
}

#[test]
fn what_isnt_there_or_isnt_a_number_is_said_so() {
    let full = testdata("cases/full.binpb");
    let one = |path: &str| decoder(vec![PathField::new("value", path, ScalarType::F64)]);
    assert_eq!(
        decode(&one("raw"), &full),
        Err(DecodeError::WrongType { field: 0 })
    );
    assert_eq!(
        decode(&one("name[0]"), &full),
        Err(DecodeError::WrongType { field: 0 })
    );
    assert_eq!(
        decode(&one("gains.ki"), &full),
        Err(DecodeError::Missing { field: 0 })
    );
    assert_eq!(
        decode(&one("position[3]"), &full),
        Err(DecodeError::Missing { field: 0 })
    );
    assert_eq!(
        decode(&one("header.seq"), &full),
        Err(DecodeError::Missing { field: 0 })
    );
    // Cut off in the middle of a field.
    assert_eq!(
        decode(&one("big"), &full[..full.len() - 3]),
        Err(DecodeError::Malformed)
    );
}

#[test]
fn a_descriptor_set_that_cant_be_read_or_lacks_the_message_is_refused() {
    let mut missing = layout(every_kind());
    missing.message = "kapto.test.v1.Odometry".to_owned();
    let errors = missing.compile().unwrap_err();
    assert_eq!(errors[0].code(), ErrorCode::DecoderInvalidSchema);
    assert!(
        errors[0]
            .message()
            .contains("doesn't define `kapto.test.v1.Odometry`")
    );

    let mut garbage = layout(every_kind());
    garbage.descriptor_set = b"not a descriptor set".to_vec();
    assert_eq!(
        garbage.compile().unwrap_err()[0].code(),
        ErrorCode::DecoderInvalidSchema
    );

    let invalid_path = layout(vec![PathField::new("v", "a..b", ScalarType::U8)]);
    assert_eq!(
        invalid_path.compile().unwrap_err()[0].code(),
        ErrorCode::DecoderInvalidLayout
    );
}

proptest! {
    /// Whatever arrives, decoding answers: it never panics.
    #[test]
    fn any_bytes_decode_or_are_refused(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
        let decoder = decoder(every_kind());
        let mut out = vec![Value::default(); decoder.value_count()];
        let _ = decoder.decode(&bytes, &mut out);
    }
}
