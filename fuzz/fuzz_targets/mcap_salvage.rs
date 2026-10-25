// SPDX-License-Identifier: Apache-2.0

//! Any bytes salvaged as a recording cut off part-way, in a temporary
//! file. A salvaged recording is whole: salvaging it again changes nothing
//! and keeps the same messages.

#![no_main]

use kapto_capture::salvage;
use kapto_fuzz::TempFile;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let file = TempFile::with("salvage", data);
    let Ok(first) = salvage(&file.0) else {
        return;
    };
    let again = salvage(&file.0).expect("a salvaged recording can be read");
    assert!(again.already_whole, "{first:?}, then {again:?}");
    assert_eq!(again.messages, first.messages);
    assert_eq!(again.last_log_time, first.last_log_time);
});
