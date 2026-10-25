// SPDX-License-Identifier: Apache-2.0

//! What the fuzz targets share: cutting an input in two, and a file to
//! hand the readers that take a path.

use std::path::PathBuf;

/// `data` cut at its first zero byte: what comes before (a schema or a
/// definition, as text) and what comes after (a payload). Without a zero
/// byte, all of it is the first part.
pub fn split_at_nul(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|&byte| byte == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (data, &[]),
    }
}

/// A file in the temporary folder of this process's own, removed when
/// dropped. libFuzzer runs one input at a time per process, so one file
/// per target and process is enough.
pub struct TempFile(pub PathBuf);

impl TempFile {
    /// Writes `bytes` into the file named after `target`.
    ///
    /// # Panics
    ///
    /// If the temporary folder can't be written, which no input causes.
    pub fn with(target: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kapto-fuzz-{target}-{}.mcap",
            std::process::id()
        ));
        #[allow(clippy::expect_used, reason = "the fuzzing machine's fault, not the input's")]
        std::fs::write(&path, bytes).expect("the temporary folder can be written");
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
