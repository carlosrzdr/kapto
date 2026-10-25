// SPDX-License-Identifier: Apache-2.0

//! JSON and MessagePack payloads decoded by path: where paths lead, how
//! values are converted, and what leaves a message undecoded. MessagePack
//! is checked against the same messages written as JSON.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use kapto_core::{ErrorCode, KaptoError};
use kapto_decode::{
    DecodeError, Decoder, PathDecoder, PathField, PathFormat, PathLayout, ScalarType, Value,
};
use proptest::prelude::*;

/// The largest message the layouts here are compiled for: a UDP datagram.
const LARGEST: usize = 65_507;

fn decoder(format: PathFormat, fields: Vec<PathField>) -> PathDecoder {
    PathLayout { format, fields }.compile(LARGEST).unwrap()
}

fn one(path: &str, ty: ScalarType) -> Vec<PathField> {
    vec![PathField::new("value", path, ty)]
}

/// `json` decoded as JSON, and as MessagePack after converting it, which
/// must agree.
fn decode(fields: &[PathField], json: &str) -> Result<Vec<Value>, DecodeError> {
    let message: serde_json::Value = serde_json::from_str(json).unwrap();
    let msgpack = rmp_serde::to_vec_named(&message).unwrap();
    let run = |format, payload: &[u8]| {
        let decoder = decoder(format, fields.to_vec());
        let mut out = vec![Value::default(); decoder.value_count()];
        decoder.decode(payload, &mut out).map(|()| out)
    };
    let from_json = run(PathFormat::Json, json.as_bytes());
    let from_msgpack = run(PathFormat::MsgPack, &msgpack);
    assert_eq!(
        from_json, from_msgpack,
        "JSON and MessagePack disagree on {json}"
    );
    from_json
}

#[test]
fn paths_lead_through_maps_and_arrays() {
    let message =
        r#"{"header":{"seq":7},"arms":[{"joints":[0.5,1.5]},{"joints":[2.5,3.5]}],"x.y":1}"#;
    let fields = vec![
        PathField::new("sequence", "header.seq", ScalarType::U32),
        PathField::new("second", "arms[1].joints[0]", ScalarType::F64),
        PathField {
            count: 2,
            ..PathField::new("first", "arms[0].joints", ScalarType::F64)
        },
    ];
    assert_eq!(
        decode(&fields, message).unwrap(),
        [
            Value::UInt(7),
            Value::Float(2.5),
            Value::Float(0.5),
            Value::Float(1.5)
        ]
    );
    // The whole message may be an array.
    assert_eq!(
        decode(&one("[1].id", ScalarType::I8), r#"[{"id":1},{"id":-2}]"#).unwrap(),
        [Value::Int(-2)]
    );
    // An array field takes the first elements of a longer array.
    let first_two = vec![PathField {
        count: 2,
        ..PathField::new("value", "a", ScalarType::U8)
    }];
    assert_eq!(
        decode(&first_two, r#"{"a":[1,2,3]}"#).unwrap(),
        [Value::UInt(1), Value::UInt(2)]
    );
}

#[test]
fn integers_take_whole_numbers_in_range_and_flags_as_one_and_zero() {
    let cases = [
        (ScalarType::U8, "255", Some(Value::UInt(255))),
        (ScalarType::U8, "256", None),
        (ScalarType::U8, "-1", None),
        (ScalarType::U16, "3.0", Some(Value::UInt(3))),
        (ScalarType::U32, "3.5", None),
        (
            ScalarType::U64,
            "18446744073709551615",
            Some(Value::UInt(u64::MAX)),
        ),
        (ScalarType::I8, "-128", Some(Value::Int(-128))),
        (ScalarType::I8, "128", None),
        (ScalarType::I16, "-2.0", Some(Value::Int(-2))),
        (ScalarType::I64, "9223372036854775808", None),
        (ScalarType::I32, "true", Some(Value::Int(1))),
        (ScalarType::U32, "false", Some(Value::UInt(0))),
    ];
    for (ty, number, expected) in cases {
        let decoded = decode(&one("v", ty), &format!(r#"{{"v":{number}}}"#));
        match expected {
            Some(value) => assert_eq!(decoded, Ok(vec![value]), "{ty:?} from {number}"),
            None => assert_eq!(
                decoded,
                Err(DecodeError::WrongType { field: 0 }),
                "{ty:?} from {number}"
            ),
        }
    }
}

#[test]
fn floats_take_any_number_and_f32_is_rounded_to_32_bits() {
    let float = |ty, number: &str| decode(&one("v", ty), &format!(r#"{{"v":{number}}}"#));
    assert_eq!(float(ScalarType::F64, "0.1"), Ok(vec![Value::Float(0.1)]));
    assert_eq!(float(ScalarType::F64, "-3"), Ok(vec![Value::Float(-3.0)]));
    assert_eq!(float(ScalarType::F64, "true"), Ok(vec![Value::Float(1.0)]));
    assert_eq!(
        float(ScalarType::F32, "0.1"),
        Ok(vec![Value::Float(f64::from(0.1_f32))])
    );
}

#[test]
fn flags_take_true_and_false_and_numbers_as_nonzero() {
    let flag = |text: &str| decode(&one("v", ScalarType::Bool), &format!(r#"{{"v":{text}}}"#));
    assert_eq!(flag("true"), Ok(vec![Value::Bool(true)]));
    assert_eq!(flag("false"), Ok(vec![Value::Bool(false)]));
    assert_eq!(flag("0"), Ok(vec![Value::Bool(false)]));
    assert_eq!(flag("-2.5"), Ok(vec![Value::Bool(true)]));
}

#[test]
fn a_scale_turns_values_into_floats_in_the_declared_unit() {
    let fields = vec![PathField {
        scale: Some(0.001),
        unit: Some("rad".to_owned()),
        ..PathField::new("value", "v", ScalarType::I32)
    }];
    assert_eq!(
        decode(&fields, r#"{"v":-1500}"#),
        Ok(vec![Value::Float(-1.5)])
    );
}

#[test]
fn text_null_and_containers_are_not_numbers() {
    for value in [r#""3""#, "null", "[3]", r#"{"a":3}"#] {
        assert_eq!(
            decode(&one("v", ScalarType::F64), &format!(r#"{{"v":{value}}}"#)),
            Err(DecodeError::WrongType { field: 0 }),
            "{value}"
        );
    }
    let array = vec![PathField {
        count: 2,
        ..PathField::new("value", "v", ScalarType::F64)
    }];
    assert_eq!(
        decode(&array, r#"{"v":[1,"x"]}"#),
        Err(DecodeError::WrongType { field: 0 })
    );
    assert_eq!(
        decode(&array, r#"{"v":1}"#),
        Err(DecodeError::WrongType { field: 0 })
    );
}

#[test]
fn a_path_not_in_the_message_leaves_its_field_missing_and_the_others_decoded() {
    let fields = vec![
        PathField::new("present", "a", ScalarType::U8),
        PathField::new("absent", "b.c", ScalarType::U8),
    ];
    for message in [
        r#"{"a":1}"#,
        r#"{"a":1,"b":{}}"#,
        r#"{"a":1,"b":[1]}"#,
        r#"{"a":1,"b":"c"}"#,
        r#"{"a":1,"b":null}"#,
    ] {
        assert_eq!(
            decode(&fields, message),
            Ok(vec![Value::UInt(1), Value::Missing]),
            "{message}"
        );
    }
    // With none of its fields, a message isn't decoded, naming the first.
    assert_eq!(
        decode(&fields, r#"{"x":1}"#),
        Err(DecodeError::Missing { field: 0 })
    );
    assert_eq!(
        decode(&one("v[2]", ScalarType::U8), r#"{"v":[1,2]}"#),
        Err(DecodeError::Missing { field: 0 })
    );
    let too_short = vec![PathField {
        count: 3,
        ..PathField::new("value", "v", ScalarType::U8)
    }];
    assert_eq!(
        decode(&too_short, r#"{"v":[1,2]}"#),
        Err(DecodeError::Missing { field: 0 })
    );
}

#[test]
fn a_payload_that_isnt_in_its_format_is_malformed() {
    let json = decoder(PathFormat::Json, one("v", ScalarType::U8));
    let msgpack = decoder(PathFormat::MsgPack, one("v", ScalarType::U8));
    let mut out = [Value::default()];
    for payload in [&b"{\"v\":1"[..], b"{\"v\":1} trailing", b"\xff\xfe", b""] {
        assert_eq!(
            json.decode(payload, &mut out),
            Err(DecodeError::Malformed),
            "{payload:?}"
        );
    }
    // A map of one entry whose value is cut off.
    assert_eq!(
        msgpack.decode(&[0x81, 0xa1, b'v'], &mut out),
        Err(DecodeError::Malformed)
    );
}

#[test]
fn numeric_keys_in_msgpack_maps_match_their_digits() {
    let decoder = decoder(PathFormat::MsgPack, one("joints.3", ScalarType::U8));
    // {"joints": {3: 9}}
    let payload = [
        0x81, 0xa6, b'j', b'o', b'i', b'n', b't', b's', 0x81, 0x03, 0x09,
    ];
    let mut out = [Value::default()];
    decoder.decode(&payload, &mut out).unwrap();
    assert_eq!(out, [Value::UInt(9)]);
}

fn layout_problems(fields: Vec<PathField>) -> Vec<String> {
    PathLayout {
        format: PathFormat::Json,
        fields,
    }
    .compile(LARGEST)
    .unwrap_err()
    .iter()
    .map(|error| {
        assert_eq!(error.code(), ErrorCode::DecoderInvalidLayout);
        error.message()
    })
    .collect()
}

#[test]
fn an_element_is_found_by_the_name_beside_it_in_whatever_order_they_come() {
    let fields = vec![
        PathField::new("shoulder", "arm.position[name=shoulder]", ScalarType::F64),
        PathField::new("elbow", "arm.position[name=elbow]", ScalarType::F64),
    ];
    let in_order = r#"{"arm":{"name":["shoulder","elbow"],"position":[0.5,1.5]}}"#;
    let swapped = r#"{"arm":{"position":[1.5,0.5],"name":["elbow","shoulder"]}}"#;
    for message in [in_order, swapped] {
        assert_eq!(
            decode(&fields, message).unwrap(),
            [Value::Float(0.5), Value::Float(1.5)],
            "{message}"
        );
    }
}

#[test]
fn a_message_without_the_name_or_with_too_few_values_is_missing_the_field() {
    let fields = one("position[name=elbow]", ScalarType::F64);
    for message in [
        r#"{"name":["shoulder"],"position":[0.5]}"#,
        r#"{"name":["shoulder","elbow"],"position":[0.5]}"#,
        r#"{"position":[0.5]}"#,
        r#"{"name":"elbow","position":[0.5]}"#,
    ] {
        assert_eq!(
            decode(&fields, message),
            Err(DecodeError::Missing { field: 0 }),
            "{message}"
        );
    }
}

#[test]
fn a_name_is_looked_up_once_per_path_for_one_value_after_an_arrays_key() {
    let cases = [
        (
            "position[name=]",
            "looking up by name is `[<array>=<name>]`",
        ),
        (
            "position[=elbow]",
            "looking up by name is `[<array>=<name>]`",
        ),
        (
            "[name=elbow]",
            "in something other than an array named by its key",
        ),
        (
            "a[0][name=elbow]",
            "in something other than an array named by its key",
        ),
        ("a[name=x].b[name=y]", "looks up by name more than once"),
    ];
    for (path, problem) in cases {
        let messages = layout_problems(one(path, ScalarType::F64));
        assert_eq!(messages.len(), 1, "{path}: {messages:?}");
        assert!(messages[0].contains(problem), "{path}: {}", messages[0]);
    }
    let several = PathField {
        count: 2,
        ..PathField::new("joints", "position[name=elbow]", ScalarType::F64)
    };
    let messages = layout_problems(vec![several]);
    assert!(
        messages[0].contains("declare a field for each name"),
        "{messages:?}"
    );
}

#[test]
fn every_invalid_path_is_reported_with_what_is_wrong() {
    let cases = [
        ("", "the path is empty"),
        (".a", "`.a` has an empty key"),
        ("a..b", "`a..b` has an empty key"),
        ("a.", "`a.` ends with a `.`"),
        ("a[1", "`a[1` has a `[` without its `]`"),
        ("a[x]", "`a[x]` has `[x]`; an index is a whole number"),
        ("a]", "`a]` has a `]` without its `[`"),
        ("a[0]b", "`a[0]b` needs a `.` before each key"),
    ];
    for (path, problem) in cases {
        let messages = layout_problems(one(path, ScalarType::U8));
        assert_eq!(messages.len(), 1, "{path}: {messages:?}");
        assert!(messages[0].ends_with(problem), "{path}: {}", messages[0]);
    }
}

#[test]
fn every_layout_problem_is_reported_with_its_field() {
    let errors = PathLayout {
        format: PathFormat::MsgPack,
        fields: vec![
            PathField::new("Bad", "a", ScalarType::U8),
            PathField::new("twice", "b", ScalarType::U8),
            PathField::new("twice", "c", ScalarType::U8),
            PathField {
                count: 0,
                ..PathField::new("empty", "d", ScalarType::U8)
            },
            PathField {
                scale: Some(2.0),
                ..PathField::new("flag", "e", ScalarType::Bool)
            },
            PathField {
                scale: Some(f64::NAN),
                ..PathField::new("nan", "f", ScalarType::F64)
            },
            PathField {
                unit: Some(String::new()),
                ..PathField::new("no_unit", "g", ScalarType::U8)
            },
        ],
    }
    .compile(LARGEST)
    .unwrap_err();
    let fields: Vec<(String, String)> = errors
        .iter()
        .map(|error: &KaptoError| {
            let context = |key: &str| {
                error
                    .context_entries()
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.clone())
                    .unwrap()
            };
            (context("field"), context("position"))
        })
        .collect();
    let expected: Vec<(String, String)> = [
        ("Bad", "1"),
        ("twice", "3"),
        ("empty", "4"),
        ("flag", "5"),
        ("nan", "6"),
        ("no_unit", "7"),
    ]
    .iter()
    .map(|(field, position)| ((*field).to_owned(), (*position).to_owned()))
    .collect();
    assert_eq!(fields, expected);
    assert!(layout_problems(Vec::new())[0].ends_with("the layout has no fields"));
}

fn array(name: &str, count: usize) -> PathField {
    PathField {
        count,
        ..PathField::new(name, name, ScalarType::U8)
    }
}

/// The one error compiling `fields` for messages of `max` bytes gives, as
/// its code, field and position.
fn too_many(fields: Vec<PathField>, max: usize) -> (ErrorCode, String, String) {
    let errors = PathLayout {
        format: PathFormat::Json,
        fields,
    }
    .compile(max)
    .unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let context = |key: &str| {
        let entries = errors[0].context_entries();
        let found = entries.iter().find(|(name, _)| *name == key);
        found.map(|(_, value)| value.clone()).unwrap()
    };
    (errors[0].code(), context("field"), context("position"))
}

#[test]
fn a_layout_with_as_many_values_as_its_messages_have_bytes_compiles() {
    let decoder = PathLayout {
        format: PathFormat::Json,
        fields: vec![array("a", 4), array("b", 6)],
    }
    .compile(10)
    .unwrap();
    assert_eq!(decoder.value_count(), 10);
}

#[test]
fn a_layout_with_more_values_than_its_messages_have_bytes_is_refused_at_the_field_past_it() {
    assert_eq!(
        too_many(vec![array("a", 4), array("b", 7), array("c", 1)], 10),
        (
            ErrorCode::DecoderTooManyValues,
            "b".to_owned(),
            "2".to_owned()
        )
    );
    // One field alone too large for the stream, which would otherwise make
    // room for that many values.
    assert_eq!(
        too_many(vec![array("huge", 1 << 40)], LARGEST).0,
        ErrorCode::DecoderTooManyValues
    );
}

#[test]
fn counts_whose_sum_overflows_are_refused_rather_than_wrapping() {
    let (code, field, position) = too_many(vec![array("a", usize::MAX), array("b", 2)], usize::MAX);
    assert_eq!(code, ErrorCode::DecoderTooManyValues);
    assert_eq!((field.as_str(), position.as_str()), ("b", "2"));
}

proptest! {
    /// Whatever arrives, decoding answers: it never panics or hangs.
    #[test]
    fn any_bytes_decode_or_are_refused(payload in proptest::collection::vec(any::<u8>(), 0..256)) {
        let fields = vec![
            PathField::new("a", "a.b[1]", ScalarType::F64),
            PathField { count: 2, ..PathField::new("c", "c", ScalarType::I16) },
        ];
        for format in PathFormat::ALL {
            let decoder = decoder(format, fields.clone());
            let mut out = vec![Value::default(); decoder.value_count()];
            let _ = decoder.decode(&payload, &mut out);
        }
    }

    /// A number written into a message comes back as written, from JSON and
    /// from MessagePack alike.
    #[test]
    fn numbers_come_back_as_written(
        unsigned in any::<u64>(),
        signed in any::<i64>(),
        float in -1e300_f64..1e300,
        flag in any::<bool>(),
        filler in "[a-z]{0,8}",
    ) {
        let message = serde_json::json!({
            "filler": filler,
            "data": {"u": unsigned, "list": [{"skip": [1, {"x": null}]}, {"i": signed}], "f": float},
            "flag": flag,
        });
        let fields = vec![
            PathField::new("u", "data.u", ScalarType::U64),
            PathField::new("i", "data.list[1].i", ScalarType::I64),
            PathField::new("f", "data.f", ScalarType::F64),
            PathField::new("flag", "flag", ScalarType::Bool),
        ];
        let decoded = decode(&fields, &message.to_string()).unwrap();
        prop_assert_eq!(
            decoded,
            vec![Value::UInt(unsigned), Value::Int(signed), Value::Float(float), Value::Bool(flag)]
        );
    }
}
