// SPDX-License-Identifier: Apache-2.0

//! Roles: what a stream or field means, independent of how a robot names it.

use std::fmt;
use std::str::FromStr;

use crate::error::{ErrorCode, KaptoError};
use crate::name::is_name;

/// What a piece of data is for, e.g. `arm.left.joints`, `gripper.left` or
/// `camera.head.rgb`.
///
/// Robot packages map their own field and topic names to roles, so checks,
/// layouts and exports work the same for every robot. A role is two or more
/// dot-separated segments, each lowercase letters, digits and underscores
/// starting with a letter.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Role(String);

impl Role {
    /// The longest accepted role, in characters.
    pub const MAX_LEN: usize = 128;

    /// The role as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The dot-separated segments, e.g. `["arm", "left", "joints"]`.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

impl FromStr for Role {
    type Err = KaptoError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let valid = text.len() <= Self::MAX_LEN
            && text.split('.').count() >= 2
            && text.split('.').all(is_name);
        if valid {
            Ok(Self(text.to_owned()))
        } else {
            Err(KaptoError::new(ErrorCode::RoleInvalidName).detail(format!("`{text}`")))
        }
    }
}

impl fmt::Display for Role {
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
        fn valid_roles_round_trip_through_text(
            segments in prop::collection::vec("[a-z][a-z0-9_]{0,15}", 2..6)
        ) {
            let text = segments.join(".");
            let role: Role = text.parse().unwrap();
            prop_assert_eq!(role.to_string(), text);
            prop_assert_eq!(role.segments().collect::<Vec<_>>(), segments);
        }
    }

    #[test]
    fn invalid_roles_are_rejected_with_their_code() {
        let too_long = format!("arm.{}", "a".repeat(Role::MAX_LEN));
        for bad in [
            "",
            "arm",
            "arm.",
            ".arm",
            "arm..left",
            "Arm.left",
            "arm.left-joints",
            "arm.1left",
            too_long.as_str(),
        ] {
            let error = bad.parse::<Role>().unwrap_err();
            assert_eq!(error.code(), ErrorCode::RoleInvalidName, "{bad}");
        }
    }
}
