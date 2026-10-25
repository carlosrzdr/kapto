// SPDX-License-Identifier: Apache-2.0

//! CDR read with `.msg` definitions, checked against messages ROS 2 itself
//! serialised (`testdata/cdr/reference.json`, from `cargo xtask golden
//! cdr`), and round-tripped through an encoder written here.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::path::Path;

use kapto_core::ErrorCode;
use kapto_decode::cdr::{
    Arity, Element, MessageDefinitions, Primitive, assemble, deserializer, msg_files_in,
};
use kapto_decode::{CdrLayout, DecodeError, Decoder, PathField, ScalarType, Value};
use proptest::prelude::*;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

/// One message ROS 2 serialised.
struct Reference {
    type_name: String,
    definition: String,
    cdr: Vec<u8>,
    values: Json,
}

fn references() -> Vec<(String, Reference)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/cdr/reference.json");
    let file: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    file["messages"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(case, message)| {
            let hex = message["cdr"].as_str().unwrap();
            let cdr = (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
                .collect();
            let reference = Reference {
                type_name: message["type"].as_str().unwrap().to_owned(),
                definition: message["definition"].as_str().unwrap().to_owned(),
                cdr,
                values: message["values"].clone(),
            };
            (case.clone(), reference)
        })
        .collect()
}

fn reference(case: &str) -> Reference {
    references()
        .into_iter()
        .find(|(name, _)| name == case)
        .unwrap()
        .1
}

fn read_all(definitions: &MessageDefinitions, cdr: &[u8]) -> Result<Json, String> {
    let mut deserializer = deserializer(cdr, definitions).map_err(|error| error.to_string())?;
    Json::deserialize(&mut deserializer).map_err(|error| error.to_string())
}

#[test]
fn every_message_ros_2_serialised_reads_back_as_its_values() {
    let references = references();
    assert!(references.len() >= 10);
    for (case, reference) in references {
        let definitions =
            MessageDefinitions::parse(&reference.type_name, &reference.definition).unwrap();
        let values = read_all(&definitions, &reference.cdr).unwrap();
        assert_eq!(values, reference.values, "{case}");
    }
}

#[test]
fn fields_are_found_by_path_with_the_same_conversions_as_json() {
    let joint_state = reference("joint_state");
    let layout = |fields| CdrLayout {
        type_name: joint_state.type_name.clone(),
        definition: joint_state.definition.clone(),
        fields,
    };
    let decoder = layout(vec![
        PathField::new("seconds", "header.stamp.sec", ScalarType::I64),
        PathField {
            count: 3,
            unit: Some("rad".to_owned()),
            ..PathField::new("position", "position", ScalarType::F64)
        },
        PathField::new("effort", "effort[2]", ScalarType::F32),
    ])
    .compile(65_507)
    .unwrap();
    let mut out = vec![Value::default(); decoder.value_count()];
    decoder.decode(&joint_state.cdr, &mut out).unwrap();
    assert_eq!(
        out,
        [
            Value::Int(1_773_066_812),
            Value::Float(0.5),
            Value::Float(-1.25),
            Value::Float(0.04),
            Value::Float(0.25)
        ]
    );

    let mut one = [Value::default()];
    let names = layout(vec![PathField::new("name", "name[0]", ScalarType::U8)])
        .compile(65_507)
        .unwrap();
    assert_eq!(
        names.decode(&joint_state.cdr, &mut one),
        Err(DecodeError::WrongType { field: 0 }),
        "text isn't a number"
    );
    let velocity = layout(vec![PathField::new("v", "velocity[0]", ScalarType::F64)])
        .compile(65_507)
        .unwrap();
    assert_eq!(
        velocity.decode(&joint_state.cdr, &mut one),
        Err(DecodeError::Missing { field: 0 }),
        "velocity is empty"
    );
    assert_eq!(
        velocity.decode(&joint_state.cdr[..20], &mut one),
        Err(DecodeError::Malformed),
        "cut short"
    );
}

#[test]
fn a_joint_is_found_by_name_in_a_joint_state_from_any_of_the_publishers_on_its_topic() {
    let joint_state = reference("joint_state");
    let decoder = CdrLayout {
        type_name: joint_state.type_name.clone(),
        definition: joint_state.definition.clone(),
        fields: vec![PathField::new(
            "gripper",
            "position[name=gripper]",
            ScalarType::F64,
        )],
    }
    .compile(65_507)
    .unwrap();
    let mut one = [Value::default()];
    decoder.decode(&joint_state.cdr, &mut one).unwrap();
    assert_eq!(one, [Value::Float(0.04)], "as ROS 2 serialised it");

    // Two drivers on one topic: the gripper's alone, then the arm's alone.
    let definitions =
        MessageDefinitions::parse(&joint_state.type_name, &joint_state.definition).unwrap();
    let message = |names: &[&str], positions: &[f64]| {
        let value = serde_json::json!({
            "header": {"stamp": {"sec": 1, "nanosec": 0}, "frame_id": ""},
            "name": names, "position": positions, "velocity": [], "effort": [],
        });
        encode(&definitions, &value, true)
    };
    let gripper = message(&["gripper"], &[0.02]);
    let arm = message(&["joint_1", "joint_2"], &[0.5, -1.25]);
    decoder.decode(&gripper, &mut one).unwrap();
    assert_eq!(one, [Value::Float(0.02)]);
    assert_eq!(
        decoder.decode(&arm, &mut one),
        Err(DecodeError::Missing { field: 0 }),
        "never another joint's value"
    );

    // One stream naming joints of both: each message gives its own.
    let both = CdrLayout {
        type_name: joint_state.type_name.clone(),
        definition: joint_state.definition.clone(),
        fields: vec![
            PathField::new("joint_1", "position[name=joint_1]", ScalarType::F64),
            PathField::new("gripper", "position[name=gripper]", ScalarType::F64),
        ],
    }
    .compile(65_507)
    .unwrap();
    let mut two = [Value::default(); 2];
    both.decode(&gripper, &mut two).unwrap();
    assert_eq!(two, [Value::Missing, Value::Float(0.02)]);
    both.decode(&arm, &mut two).unwrap();
    assert_eq!(two, [Value::Float(0.5), Value::Missing]);
}

#[test]
fn a_message_cut_short_anywhere_is_refused_without_panicking() {
    for (case, reference) in references() {
        let definitions =
            MessageDefinitions::parse(&reference.type_name, &reference.definition).unwrap();
        let whole = read_all(&definitions, &reference.cdr).unwrap();
        for end in 0..reference.cdr.len() {
            // Bytes after the message are ignored, so a cut may still hold
            // the whole message; then it must read the same.
            if let Ok(values) = read_all(&definitions, &reference.cdr[..end]) {
                assert_eq!(values, whole, "{case} cut at {end}");
            }
        }
    }
}

#[test]
fn an_unknown_encapsulation_is_refused() {
    let pose = reference("pose");
    let definitions = MessageDefinitions::parse(&pose.type_name, &pose.definition).unwrap();
    let mut cdr = pose.cdr.clone();
    cdr[1] = 0x07;
    assert!(
        read_all(&definitions, &cdr)
            .unwrap_err()
            .contains("isn't plain CDR")
    );
}

/// Writes `value`, a message of `type_name`, as CDR in either byte order.
struct Encoder<'a> {
    definitions: &'a MessageDefinitions,
    little: bool,
    out: Vec<u8>,
}

impl Encoder<'_> {
    fn align(&mut self, size: usize) {
        while !(self.out.len() - 4).is_multiple_of(size) {
            self.out.push(0);
        }
    }

    fn bytes<const N: usize>(&mut self, little: [u8; N], big: [u8; N]) {
        self.align(N);
        self.out.extend(if self.little { little } else { big });
    }

    fn u32(&mut self, value: u32) {
        self.bytes(value.to_le_bytes(), value.to_be_bytes());
    }

    fn message(&mut self, type_name: &str, value: &Json) {
        let fields = self.definitions.fields(type_name).unwrap();
        if fields.is_empty() {
            self.out.push(0);
        }
        for field in fields {
            let value = &value[&field.name];
            match field.arity {
                Arity::One => self.element(&field.element, value),
                Arity::Fixed(_) | Arity::Sequence(_) => {
                    let items = value.as_array().unwrap();
                    if let Arity::Sequence(_) = field.arity {
                        self.u32(u32::try_from(items.len()).unwrap());
                    }
                    for item in items {
                        self.element(&field.element, item);
                    }
                }
            }
        }
    }

    fn element(&mut self, element: &Element, value: &Json) {
        let primitive = match element {
            Element::Message(name) => return self.message(name, value),
            Element::Primitive(primitive) => *primitive,
        };
        let int = || value.as_i64().unwrap();
        let uint = || value.as_u64().unwrap();
        match primitive {
            Primitive::Bool => self.out.push(u8::from(value.as_bool().unwrap())),
            Primitive::Byte | Primitive::Char | Primitive::Uint8 => {
                self.out.push(u8::try_from(uint()).unwrap());
            }
            Primitive::Int8 => self.out.push(i8::try_from(int()).unwrap().to_ne_bytes()[0]),
            Primitive::Int16 => {
                let v = i16::try_from(int()).unwrap();
                self.bytes(v.to_le_bytes(), v.to_be_bytes());
            }
            Primitive::Uint16 => {
                let v = u16::try_from(uint()).unwrap();
                self.bytes(v.to_le_bytes(), v.to_be_bytes());
            }
            Primitive::Int32 => {
                let v = i32::try_from(int()).unwrap();
                self.bytes(v.to_le_bytes(), v.to_be_bytes());
            }
            Primitive::Uint32 => self.u32(u32::try_from(uint()).unwrap()),
            Primitive::Int64 => self.bytes(int().to_le_bytes(), int().to_be_bytes()),
            Primitive::Uint64 => self.bytes(uint().to_le_bytes(), uint().to_be_bytes()),
            Primitive::Float32 => {
                #[allow(clippy::cast_possible_truncation, reason = "generated as f32")]
                let v = value.as_f64().unwrap() as f32;
                self.bytes(v.to_le_bytes(), v.to_be_bytes());
            }
            Primitive::Float64 => {
                let v = value.as_f64().unwrap();
                self.bytes(v.to_le_bytes(), v.to_be_bytes());
            }
            Primitive::String => {
                let text = value.as_str().unwrap();
                self.u32(u32::try_from(text.len() + 1).unwrap());
                self.out.extend(text.as_bytes());
                self.out.push(0);
            }
            Primitive::WString => {
                let text = value.as_str().unwrap();
                self.u32(u32::try_from(text.chars().count()).unwrap());
                for character in text.chars() {
                    self.u32(u32::from(character));
                }
            }
        }
    }
}

fn encode(definitions: &MessageDefinitions, value: &Json, little: bool) -> Vec<u8> {
    let mut encoder = Encoder {
        definitions,
        little,
        out: vec![0, u8::from(little), 0, 0],
    };
    encoder.message(definitions.root(), value);
    encoder.out
}

const SAMPLE: &str = "\
bool flag
byte b
int8 i8v
uint16 u16v
float64 time
int32[3] counts
float32[] gains
string label
wstring note
string<=8[<=2] tags
kapto_test/Point[] path
kapto_test/Empty nothing
uint64 last
================================================================================
MSG: kapto_test/Point
int16 x
float64 y
================================================================================
MSG: kapto_test/Empty
";

fn sample() -> impl Strategy<Value = Json> {
    let point = (any::<i16>(), -1e9_f64..1e9).prop_map(|(x, y)| json!({"x": x, "y": y}));
    (
        (
            any::<bool>(),
            any::<u8>(),
            any::<i8>(),
            any::<u16>(),
            -1e12_f64..1e12,
        ),
        proptest::array::uniform3(any::<i32>()),
        proptest::collection::vec(-1e6_f32..1e6, 0..4),
        (
            "[a-z ]{0,12}",
            "[a-zé]{0,5}",
            proptest::collection::vec("[a-z]{0,8}", 0..=2),
        ),
        proptest::collection::vec(point, 0..3),
        any::<u64>(),
    )
        .prop_map(
            |((flag, b, i8v, u16v, time), counts, gains, (label, note, tags), path, last)| {
                let mut message = Map::new();
                message.insert("flag".into(), json!(flag));
                message.insert("b".into(), json!(b));
                message.insert("i8v".into(), json!(i8v));
                message.insert("u16v".into(), json!(u16v));
                message.insert("time".into(), json!(time));
                message.insert("counts".into(), json!(counts));
                let gains: Vec<f64> = gains.into_iter().map(f64::from).collect();
                message.insert("gains".into(), json!(gains));
                message.insert("label".into(), json!(label));
                message.insert("note".into(), json!(note));
                message.insert("tags".into(), json!(tags));
                message.insert("path".into(), json!(path));
                message.insert("nothing".into(), json!({}));
                message.insert("last".into(), json!(last));
                Json::Object(message)
            },
        )
}

proptest! {
    /// Any message of the sample type, written in either byte order,
    /// reads back as written.
    #[test]
    fn messages_round_trip_in_either_byte_order(message in sample(), little in any::<bool>()) {
        let definitions = MessageDefinitions::parse("kapto_test/Sample", SAMPLE).unwrap();
        let cdr = encode(&definitions, &message, little);
        prop_assert_eq!(read_all(&definitions, &cdr).unwrap(), message);
    }

    /// Whatever arrives, reading answers: it never panics or hangs.
    #[test]
    fn any_bytes_read_or_are_refused(bytes in proptest::collection::vec(any::<u8>(), 0..300)) {
        let definitions = MessageDefinitions::parse("kapto_test/Sample", SAMPLE).unwrap();
        let mut cdr = vec![0, 1, 0, 0];
        cdr.extend(bytes);
        let _ = read_all(&definitions, &cdr);
    }
}

/// The fixture as JSON: each message, with the encoding rosbag2 stored its
/// definition in, and every `.msg` file the definitions are made of, by
/// `package/Type`.
fn fixture() -> Json {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/cdr/reference.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn msg_files(fixture: &Json) -> impl FnMut(&str, &str) -> Option<String> + '_ {
    |package, name| {
        fixture["msg_files"][format!("{package}/{name}")]
            .as_str()
            .map(str::to_owned)
    }
}

#[test]
fn every_definition_rosbag2_wrote_is_put_together_from_its_msg_files_the_same_way() {
    let fixture = fixture();
    let mut checked = 0;
    for (case, message) in fixture["messages"].as_object().unwrap() {
        // rosbag2 stores a type its package lists as an `.idl` interface as
        // IDL; Kapto reads `.msg` files only.
        if message["rosbag2_encoding"] != "ros2msg" {
            continue;
        }
        let type_name = message["type"].as_str().unwrap();
        let assembled = assemble(type_name, &mut msg_files(&fixture)).unwrap();
        assert_eq!(assembled, message["definition"].as_str().unwrap(), "{case}");
        checked += 1;
    }
    assert!(checked >= 12, "{checked}");
}

#[test]
fn a_type_without_its_msg_file_is_named_with_the_type_that_uses_it() {
    let fixture = fixture();
    let mut files = msg_files(&fixture);
    let mut without_time =
        |package: &str, name: &str| (name != "Time").then(|| files(package, name)).flatten();
    let error = assemble("sensor_msgs/msg/JointState", &mut without_time).unwrap_err();
    assert_eq!(error.code(), ErrorCode::DecoderMsgNotFound);
    assert!(
        error
            .to_string()
            .contains("builtin_interfaces/msg/Time.msg"),
        "{error}"
    );
    assert_eq!(
        error.context_entries(),
        [("used by", "std_msgs/Header".to_owned())]
    );
    let error = assemble("nobody/msg/Nothing", &mut msg_files(&fixture)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::DecoderMsgNotFound);
    assert_eq!(error.context_entries(), []);
}

#[test]
fn msg_files_are_found_in_folders_laid_out_as_ros_2_installs_them() {
    let root = std::env::temp_dir().join(format!("kapto-msg-files-{}", std::process::id()));
    let (first, second) = (root.join("first"), root.join("second"));
    for (folder, text) in [(&first, "int32 a\n"), (&second, "int64 a\n")] {
        let msg = folder.join("pkg/msg");
        std::fs::create_dir_all(&msg).unwrap();
        std::fs::write(msg.join("Value.msg"), text).unwrap();
    }
    std::fs::write(second.join("pkg/msg/Other.msg"), "pkg/Value value\n").unwrap();
    let folders = [first, second];
    let mut found = msg_files_in(&folders);
    assert_eq!(
        found("pkg", "Value").as_deref(),
        Some("int32 a\n"),
        "the first folder wins"
    );
    assert_eq!(found("pkg", "Missing"), None);
    let text = assemble("pkg/msg/Other", &mut found).unwrap();
    assert!(text.ends_with("MSG: pkg/Value\nint32 a\n"), "{text}");
    std::fs::remove_dir_all(root).unwrap();
}
