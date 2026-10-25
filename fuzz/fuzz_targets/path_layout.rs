// SPDX-License-Identifier: Apache-2.0

//! Fields found by path, as a recording's schema stores them, then a JSON
//! or MessagePack payload decoded with them: the first byte picks the
//! format, the schema follows up to a zero byte, the payload after it.

#![no_main]

use kapto_decode::{Decoder, PathFormat, PathLayout, Value};
use kapto_fuzz::split_at_nul;
use libfuzzer_sys::fuzz_target;

/// The most values decoded: a schema may declare arrays of any length,
/// and room for every value is made before decoding.
const MAX_VALUES: usize = 1 << 16;

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
    let Ok(decoder) = layout.compile() else {
        return;
    };
    let count = decoder
        .fields()
        .iter()
        .try_fold(0_usize, |sum, field| sum.checked_add(field.count));
    let Some(count) = count.filter(|&count| count <= MAX_VALUES) else {
        return;
    };
    let mut values = vec![Value::default(); count];
    let _ = decoder.decode(payload, &mut values);
});
