// SPDX-License-Identifier: Apache-2.0

//! Any bytes, in a temporary file, opened as a recording whose `camera`
//! channel is read by its indexes, as a replay reads a camera's frames:
//! every message found read back, up to a few.

#![no_main]

use kapto_capture::seek::ChannelIndex;
use kapto_fuzz::TempFile;
use libfuzzer_sys::fuzz_target;

/// The most messages read back from one input.
const READS: usize = 16;

fuzz_target!(|data: &[u8]| {
    let file = TempFile::with("seek", data);
    let Ok(index) = ChannelIndex::open(&file.0, "camera") else {
        return;
    };
    let times: Vec<u64> = index.times().collect();
    assert_eq!(times.len(), index.len());
    assert!(times.is_sorted());
    if let Some(&last) = times.last() {
        assert_eq!(index.at(last), Some(index.len() - 1));
    }
    for n in 0..index.len().min(READS) {
        let _ = index.read(n);
    }
});
