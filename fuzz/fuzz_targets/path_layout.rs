// SPDX-License-Identifier: Apache-2.0

//! Fields found by path, as a recording's schema stores them, then a JSON
//! or MessagePack payload decoded with them: the first byte picks the
//! format, the schema follows up to a zero byte, the payload after it.
//! The layout is compiled for messages as long as the payload, so any
//! `count` the schema gives is checked against that bound.

#![no_main]

use kapto_decode::{Decoder, PathFormat, PathLayout, Value};
use kapto_fuzz::split_at_nul;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let format = PathFormat::ALL[usize::from(pick) % PathFormat::ALL.len()];
    let (json, payload) = split_at_nul(rest);
    let Ok(json) = std::str::from_utf8(json) else {
        return;
    };
    let Ok(layout) = PathLayout::from_schema_json(format, json) else {
        return;
    };
    let Ok(decoder) = layout.compile(payload.len()) else {
        return;
    };
    // Every value takes at least a byte, so a layout that compiles never
    // needs more values than the message has bytes.
    assert!(decoder.value_count() <= payload.len());
    let mut values = vec![Value::default(); decoder.value_count()];
    let _ = decoder.decode(payload, &mut values);
});
