// SPDX-License-Identifier: Apache-2.0

//! Stream identifiers.

use std::fmt;
use std::str::FromStr;

use crate::error::{ErrorCode, KaptoError};
use crate::name::is_name;

/// The name of one input stream as declared in a robot package, e.g.
/// `arm_left_joints`.
///
/// It is Kapto's own name for the stream, independent of where the data comes
/// from (a UDP port, a TCP connection or a ROS 2 topic).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(String);

impl StreamId {
    /// The longest accepted stream ID, in characters.
    pub const MAX_LEN: usize = 64;

    /// The ID as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for StreamId {
    type Err = KaptoError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if is_name(text) && text.len() <= Self::MAX_LEN {
            Ok(Self(text.to_owned()))
        } else {
            Err(KaptoError::new(ErrorCode::StreamInvalidId).detail(format!("`{text}`")))
        }
    }
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn valid_stream_ids_round_trip_through_text(text in "[a-z][a-z0-9_]{0,63}") {
            let id: StreamId = text.parse().unwrap();
            prop_assert_eq!(id.to_string(), text);
        }
    }

    #[test]
    fn invalid_stream_ids_are_rejected_with_their_code() {
        let too_long = "a".repeat(StreamId::MAX_LEN + 1);
        for bad in ["", "Arm", "arm-left", "arm.left", "1arm", too_long.as_str()] {
            let error = bad.parse::<StreamId>().unwrap_err();
            assert_eq!(error.code(), ErrorCode::StreamInvalidId, "{bad}");
        }
    }
}
