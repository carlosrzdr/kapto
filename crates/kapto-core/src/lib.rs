// SPDX-License-Identifier: Apache-2.0

//! Shared types used by every Kapto crate: identifiers, timestamps, roles and errors with cause codes.

pub mod clock;
pub mod episode;
pub mod error;
pub mod hex;
pub mod name;
pub mod role;
pub mod stream;
pub mod time;

pub use clock::{ClockMap, ClockSample, ClockStep};
pub use episode::EpisodeId;
pub use error::{ErrorCode, KaptoError, ResultExt};
pub use hex::hex;
pub use role::Role;
pub use stream::StreamId;
pub use time::{RecvTime, SourceTime, utc_iso8601};
