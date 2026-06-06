// SPDX-License-Identifier: Apache-2.0

//! The naming rules shared by stream IDs, role segments, decoded field names
//! and the kinds of events.

/// A name is lowercase ASCII letters, digits and underscores, starting with a
/// letter: safe in file names, MCAP topics, URLs and TOML keys without quoting.
#[must_use]
pub fn is_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A dotted name is names joined by dots, e.g. `grasp` or `subtask.reach`,
/// at most 64 characters: how the kinds of events are written.
#[must_use]
pub fn is_dotted_name(text: &str) -> bool {
    text.len() <= 64 && text.split('.').all(is_name)
}

#[cfg(test)]
mod tests {
    use super::{is_dotted_name, is_name};

    #[test]
    fn dotted_names_are_names_joined_by_dots() {
        for good in ["grasp", "subtask.reach", "kapto.stream_lost"] {
            assert!(is_dotted_name(good), "{good}");
        }
        for bad in ["", "Grasp", "grasp.", ".grasp", "a..b", "grasp-left"] {
            assert!(!is_dotted_name(bad), "{bad}");
        }
        assert!(!is_dotted_name(&"a".repeat(65)));
    }

    #[test]
    fn names_start_with_a_lowercase_letter() {
        assert!(is_name("arm"));
        assert!(is_name("a1_b2"));
        assert!(!is_name("1arm"));
        assert!(!is_name("_arm"));
        assert!(!is_name(""));
    }

    #[test]
    fn names_reject_uppercase_punctuation_and_spaces() {
        for bad in ["Arm", "arm-left", "arm.left", "arm left", "ärm"] {
            assert!(!is_name(bad), "{bad} should be rejected");
        }
    }
}
