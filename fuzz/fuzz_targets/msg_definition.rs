// SPDX-License-Identifier: Apache-2.0

//! A message definition as an MCAP schema carries it, then a payload read
//! with it: the root type's name on the first line, its definition and
//! those of the types it uses after it, a zero byte, then the CDR.

#![no_main]

use kapto_decode::cdr::{MessageDefinitions, deserializer};
use kapto_fuzz::split_at_nul;
use libfuzzer_sys::fuzz_target;
use serde::Deserialize;
use serde::de::IgnoredAny;

fuzz_target!(|data: &[u8]| {
    let (text, payload) = split_at_nul(data);
    let Ok(text) = std::str::from_utf8(text) else {
        return;
    };
    let (root, definition) = text.split_once('\n').unwrap_or((text, ""));
    let Ok(definitions) = MessageDefinitions::parse(root, definition) else {
        return;
    };
    if let Ok(mut cdr) = deserializer(payload, &definitions) {
        let _ = IgnoredAny::deserialize(&mut cdr);
    }
});
