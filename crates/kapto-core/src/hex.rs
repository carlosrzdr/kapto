// SPDX-License-Identifier: Apache-2.0

//! Bytes as lowercase hexadecimal, as `sha256sum` writes a digest.

use std::fmt::Write as _;

/// `bytes` as lowercase hexadecimal, two digits each.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_byte_is_two_lowercase_digits() {
        assert_eq!(hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
        assert_eq!(hex(&[]), "");
    }
}
