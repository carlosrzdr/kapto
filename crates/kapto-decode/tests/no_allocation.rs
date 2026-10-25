// SPDX-License-Identifier: Apache-2.0

//! Decoding a payload never allocates: it will run for messages on the
//! recording path, where allocation per message isn't allowed.
//!
//! A counting allocator wraps the system allocator for this test binary. It
//! counts per thread, so tests running in parallel don't count each other's
//! allocations.

#![allow(
    unsafe_code,
    reason = "a global allocator can only be written with unsafe trait methods"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use kapto_decode::{
    CdrLayout, Decoder, FieldSpec, PathField, PathFormat, PathLayout, ScalarType, StructLayout,
    Value,
};

struct CountingAllocator;

thread_local! {
    // A `const` initialiser: reading it never allocates, so the allocator can
    // use it without recursing into itself.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

// SAFETY: every method delegates to the system allocator with the arguments
// it was given, which upholds `GlobalAlloc`'s contract; counting has no effect
// on the memory handed out.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn decoding_payloads_never_allocates() {
    let decoder = StructLayout {
        fields: vec![
            FieldSpec::new("sequence", ScalarType::U32, 0),
            FieldSpec::new("source_time", ScalarType::U64, 4),
            FieldSpec {
                count: 7,
                scale: Some(0.5),
                ..FieldSpec::new("joint_positions", ScalarType::F64, 12)
            },
            FieldSpec::new("sent", ScalarType::Time, 68),
            FieldSpec {
                unit: Some("ns".to_owned()),
                ..FieldSpec::new("sent_ns", ScalarType::Time, 76)
            },
        ],
    }
    .compile()
    .unwrap();
    let mut out = vec![Value::default(); decoder.value_count()];
    let payload = [3_u8; 84];
    let short = [3_u8; 10];

    let before = allocations();
    for _ in 0..10_000 {
        let whole = decoder.decode(&payload, &mut out);
        let cut_off = decoder.decode(&short, &mut out);
        assert!(whole.is_ok() && cut_off.is_err());
    }
    assert_eq!(allocations() - before, 0);
}

#[test]
fn decoding_json_and_msgpack_by_path_allocates_nothing_when_it_succeeds() {
    let layout = |format| PathLayout {
        format,
        fields: vec![
            PathField::new("sequence", "header.seq", ScalarType::U32),
            PathField {
                count: 3,
                ..PathField::new("position", "joints.position", ScalarType::F64)
            },
            PathField::new("mode", "status.flags[1]", ScalarType::Bool),
            PathField::new("b", "joints.position[names=b]", ScalarType::F64),
            PathField::new("stamp", "header.stamp", ScalarType::Time),
            PathField::new("elapsed", "status.elapsed", ScalarType::Time),
        ],
    };
    let json = br#"{"header":{"seq":7,"frame":"base","stamp":{"sec":12,"nanosec":5}},"joints":{"names":["a","b","c"],"position":[0.1,0.2,0.3]},"status":{"flags":[false,true],"elapsed":1.5}}"#;
    let message: serde_json::Value = serde_json::from_slice(json).unwrap();
    let msgpack = rmp_serde::to_vec_named(&message).unwrap();
    for (format, payload) in [
        (PathFormat::Json, json.as_slice()),
        (PathFormat::MsgPack, msgpack.as_slice()),
    ] {
        let decoder = layout(format).compile(65_507).unwrap();
        let mut out = vec![Value::default(); decoder.value_count()];
        let before = allocations();
        for _ in 0..1_000 {
            decoder.decode(payload, &mut out).unwrap();
        }
        assert_eq!(allocations() - before, 0, "{format:?}");
        assert_eq!(out[0], Value::UInt(7));
        assert_eq!(out[4], Value::Bool(true));
        assert_eq!(out[5], Value::Float(0.2), "found by name");
        assert_eq!(
            out[6],
            Value::Time(12_000_000_005),
            "a map of seconds and nanoseconds"
        );
        assert_eq!(out[7], Value::Time(1_500_000_000), "a number of seconds");
    }
}

#[test]
fn decoding_cdr_by_path_allocates_nothing_when_it_succeeds() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/cdr/reference.json");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let joint_state = &file["messages"]["joint_state"];
    let hex = joint_state["cdr"].as_str().unwrap();
    let cdr: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect();
    let decoder = CdrLayout {
        type_name: joint_state["type"].as_str().unwrap().to_owned(),
        definition: joint_state["definition"].as_str().unwrap().to_owned(),
        fields: vec![
            PathField::new("seconds", "header.stamp.sec", ScalarType::I64),
            PathField {
                count: 3,
                ..PathField::new("position", "position", ScalarType::F64)
            },
            PathField::new("effort", "effort[2]", ScalarType::F32),
            PathField::new("shoulder", "position[name=joint_2]", ScalarType::F64),
        ],
    }
    .compile(65_507)
    .unwrap();
    let mut out = vec![Value::default(); decoder.value_count()];
    let before = allocations();
    for _ in 0..1_000 {
        decoder.decode(&cdr, &mut out).unwrap();
    }
    assert_eq!(allocations() - before, 0);
    assert_eq!(out[1], Value::Float(0.5));
}
