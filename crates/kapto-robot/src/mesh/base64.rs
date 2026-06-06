// SPDX-License-Identifier: Apache-2.0

//! Decoding base64, for glTF buffers embedded as `data:` URIs.

/// The bytes `text` encodes in standard base64 (`A–Z a–z 0–9 + /`, padded
/// with `=` or not); `None` if it isn't base64.
pub(super) fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=').as_bytes();
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut group = 0_u32;
        for (index, &symbol) in chunk.iter().enumerate() {
            group |= u32::from(value(symbol)?) << (18 - 6 * index);
        }
        let [_, first, second, third] = group.to_be_bytes();
        bytes.extend_from_slice(&[first, second, third][..chunk.len() - 1]);
    }
    Some(bytes)
}

fn value(symbol: u8) -> Option<u8> {
    match symbol {
        b'A'..=b'Z' => Some(symbol - b'A'),
        b'a'..=b'z' => Some(symbol - b'a' + 26),
        b'0'..=b'9' => Some(symbol - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_examples_of_the_base64_standard() {
        // The test vectors of RFC 4648, section 10.
        for (text, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(
                decode(encoded).as_deref(),
                Some(text.as_bytes()),
                "{encoded}"
            );
        }
        assert_eq!(decode("Zm9vYg").as_deref(), Some(&b"foob"[..]));
    }

    #[test]
    fn text_that_is_not_base64_is_refused() {
        assert_eq!(decode("Zm9v!"), None);
        assert_eq!(decode("Z"), None);
    }
}
