// SPDX-License-Identifier: Apache-2.0

//! The recording path: batch buffers, loss accounting, the MCAP writer and episode finalisation.

pub mod batch;
#[cfg(unix)]
pub mod control;
#[cfg(unix)]
pub mod disk;
pub mod durability;
pub mod finalise;
pub mod kept;
pub mod loss;
pub mod pre_roll;
pub mod preflight;
pub mod reader;
pub mod recover;
pub mod ring;
pub mod salvage;
pub mod seek;
#[cfg(unix)]
pub mod tap;
pub mod writer;

pub use batch::{Batch, Publication, StreamIndex};
pub use durability::DurabilityStats;
pub use kept::{Kept, KeptHeld, KeptStream};
pub use loss::{Accounting, LossCounters, LossDetection, StreamTotals};
pub use pre_roll::{HeldMessage, PreRoll, PreRollStream};
pub use recover::{RecoveredEpisode, recover_interrupted};
pub use ring::{Drainer, Filler, Received, RingConfig, RingStats, Sent, ring};
pub use salvage::{Salvaged, salvage};
pub use writer::{ChannelSchema, ChannelSpec, Compression, Recording, WriterConfig, discard};
