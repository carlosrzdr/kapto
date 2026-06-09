// SPDX-License-Identifier: Apache-2.0

//! What the web UI's viewer shows: the robot live and its decoded values,
//! streamed by the viewer bridge with the last minutes kept for looking
//! back, and recorded episodes as replay views to watch again
//! ([`write_replay`]). Link poses are computed with Kapto's own forward
//! kinematics.

#[cfg(unix)]
mod bridge;
#[cfg(unix)]
pub mod history;
#[cfg(unix)]
pub mod live;
#[cfg(unix)]
pub mod marks;
pub mod page_scene;
pub mod picture;
#[cfg(unix)]
pub mod plots;
#[cfg(unix)]
pub mod replay;
#[cfg(unix)]
pub mod stream;
#[cfg(unix)]
pub mod tap_reader;
#[cfg(unix)]
pub mod view;

#[cfg(unix)]
pub use bridge::{Bridge, BridgeSettings};
#[cfg(unix)]
pub use live::LiveRobot;
#[cfg(unix)]
pub use plots::{Plots, StreamFields};
#[cfg(unix)]
pub use replay::{DrawnRobots, ReplaySettings, Replayed, write_replay};
#[cfg(unix)]
pub use tap_reader::{TapReader, TapReaderStats};
