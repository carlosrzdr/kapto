// SPDX-License-Identifier: Apache-2.0

//! Any bytes read as a datagram of the live tap, and as the header of a
//! camera frame on its socket. A datagram read is written back exactly as
//! it came, but for the reserved byte, which is written as 0.

#![no_main]

use kapto_capture::tap::frames::{self, read_header};
use kapto_capture::tap::wire::{Summary, decode, encode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(header) = data.first_chunk::<{ frames::HEADER }>() {
        let _ = read_header(header);
    }
    let Ok(sample) = decode(data) else {
        return;
    };
    let mut written = Vec::new();
    encode(
        &Summary {
            stream: sample.stream,
            first_recv_time_ns: sample.first_recv_time_ns,
            recv_time_ns: sample.recv_time_ns,
            messages: sample.messages,
            values: &sample.values,
            minimums: &sample.minimums,
            maximums: &sample.maximums,
        },
        &mut written,
    );
    let mut expected = data.to_vec();
    expected[5] = 0;
    assert_eq!(written, expected);
});
