// SPDX-License-Identifier: Apache-2.0

//! Any bytes decoded as the joint state of testdata/protobuf, with fields
//! found by path through every kind of field it has: nested messages,
//! repeated scalars and messages, an element found by name, a map, an
//! enum, a oneof and 64-bit integers.

#![no_main]

use std::sync::LazyLock;

use kapto_decode::{Decoder, PathField, ProtobufDecoder, ProtobufLayout, ScalarType, Value};
use libfuzzer_sys::fuzz_target;

static DECODER: LazyLock<ProtobufDecoder> = LazyLock::new(|| {
    let field = |name: &str, path: &str, ty| PathField::new(name, path, ty);
    let mut history = field("history", "history", ScalarType::U32);
    history.count = 3;
    ProtobufLayout {
        message: "kapto.test.v1.JointState".to_owned(),
        descriptor_set: include_bytes!("../../testdata/protobuf/robot.binpb").to_vec(),
        fields: vec![
            field("sec", "header.stamp.sec", ScalarType::I64),
            field("frame", "header.frame_id", ScalarType::U8),
            field("second", "position[1]", ScalarType::F64),
            field("shoulder", "position[name=shoulder]", ScalarType::F64),
            field("effort", "effort[0]", ScalarType::F32),
            field("mode", "mode", ScalarType::U8),
            field("kp", "gains.kp", ScalarType::F64),
            field("raw", "raw", ScalarType::U8),
            field("offset", "offset", ScalarType::I32),
            field("counter", "counter", ScalarType::U64),
            field("enabled", "enabled", ScalarType::Bool),
            field("angle", "angle", ScalarType::F64),
            field("big", "big", ScalarType::U64),
            field("first_nanosec", "history[0].nanosec", ScalarType::U32),
            history,
        ],
    }
    .compile()
    .expect("the layout compiles")
});

fuzz_target!(|data: &[u8]| {
    let mut values = vec![Value::default(); DECODER.value_count()];
    let _ = DECODER.decode(data, &mut values);
});
