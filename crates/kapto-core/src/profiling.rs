// SPDX-License-Identifier: Apache-2.0

//! Spans marking work for `cargo xtask profile trace`: a batch through
//! each stage of capture, a job in the worker. They exist only with the
//! `profiling` feature, which release builds never have; without it
//! [`span!`](crate::span) is nothing at all, so the hot path is as without
//! it.

#[cfg(feature = "profiling")]
pub use tracing;

/// What [`span!`](crate::span) gives without the `profiling` feature:
/// nothing, ended with nothing.
#[derive(Debug)]
pub struct Off;

/// Marks the work from here to the end of the scope as `name`, with
/// `tracing`'s fields, e.g. `let _span = span!("write", messages = 64);`.
#[cfg(feature = "profiling")]
#[macro_export]
macro_rules! span {
    ($name:literal $(, $($fields:tt)*)?) => {
        $crate::profiling::tracing::info_span!($name $(, $($fields)*)?).entered()
    };
}

/// Marks the work from here to the end of the scope as `name`; without the
/// `profiling` feature, nothing: its fields aren't even evaluated.
#[cfg(not(feature = "profiling"))]
#[macro_export]
macro_rules! span {
    ($name:literal $(, $($fields:tt)*)?) => {
        $crate::profiling::Off
    };
}

/// Marks `future`, from when it is first polled until it is dropped, as
/// `name` with `tracing`'s fields: for work that waits, as an upload's
/// parts do.
#[cfg(feature = "profiling")]
#[macro_export]
macro_rules! instrument {
    ($future:expr, $name:literal $(, $($fields:tt)*)?) => {
        $crate::profiling::tracing::Instrument::instrument(
            $future,
            $crate::profiling::tracing::info_span!($name $(, $($fields)*)?),
        )
    };
}

/// Marks `future` as `name`; without the `profiling` feature, `future`
/// itself, its fields not evaluated.
#[cfg(not(feature = "profiling"))]
#[macro_export]
macro_rules! instrument {
    ($future:expr, $name:literal $(, $($fields:tt)*)?) => {
        $future
    };
}
