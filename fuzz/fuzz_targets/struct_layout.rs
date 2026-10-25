// SPDX-License-Identifier: Apache-2.0

//! A struct layout as a recording's schema stores it, then a payload
//! decoded with it, cut at the first zero byte. A layout that compiles
//! decodes any payload at least as long as it needs, and no shorter one.

#![no_main]

use kapto_decode::{Decoder, StructLayout, Value};
use kapto_fuzz::split_at_nul;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (json, payload) = split_at_nul(data);
    let Ok(json) = std::str::from_utf8(json) else {
        return;
    };
    let Ok(layout) = StructLayout::from_schema_json(json) else {
        return;
    };
    let Ok(decoder) = layout.compile() else {
        return;
    };
    let mut values = vec![Value::default(); decoder.value_count()];
    let decoded = decoder.decode(payload, &mut values);
    assert_eq!(
        decoded.is_ok(),
        payload.len() >= decoder.min_payload_len(),
        "{decoded:?}"
    );
});
