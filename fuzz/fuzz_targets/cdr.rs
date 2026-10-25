// SPDX-License-Identifier: Apache-2.0

//! Any bytes read as CDR, as a ROS 2 topic of one of the types in
//! testdata/cdr would carry them: the first byte picks the type, the rest
//! is the payload. Every value is walked, built as a JSON value and
//! skipped unbuilt, as decoding a path skips what it doesn't follow.

#![no_main]

use std::sync::LazyLock;

use kapto_decode::cdr::{MessageDefinitions, deserializer};
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use serde::de::IgnoredAny;

/// The definitions of the reference messages ROS 2 serialised.
static TYPES: LazyLock<Vec<MessageDefinitions>> = LazyLock::new(|| {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("../../testdata/cdr/reference.json"))
            .expect("testdata/cdr/reference.json is JSON");
    reference["messages"]
        .as_object()
        .expect("it has messages")
        .values()
        .map(|message| {
            let (Some(name), Some(text)) =
                (message["type"].as_str(), message["definition"].as_str())
            else {
                panic!("a message without its type or definition");
            };
            MessageDefinitions::parse(name, text).expect("ROS 2's definitions parse")
        })
        .collect()
});

fuzz_target!(|data: &[u8]| {
    let Some((&pick, payload)) = data.split_first() else {
        return;
    };
    let definitions = &TYPES[usize::from(pick) % TYPES.len()];
    if let Ok(mut cdr) = deserializer(payload, definitions) {
        let _ = IgnoredAny::deserialize(&mut cdr);
    }
    if let Ok(mut cdr) = deserializer(payload, definitions) {
        let _ = serde_json::Value::deserialize(&mut cdr);
    }
});
