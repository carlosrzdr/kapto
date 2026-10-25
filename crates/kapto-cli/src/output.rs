// SPDX-License-Identifier: Apache-2.0

//! What a command prints on stdout.
//!
//! `println!` panics when stdout can't be written, as when it is piped into
//! a reader that has gone (`kapto config show | head -1`). Printing goes
//! through [`out!`] and [`outln!`] instead: once stdout has gone, the rest
//! is dropped, the command carries on, and [`stdout_gone`] tells `run` to
//! end it with a failure status. Diagnostics go to stderr with
//! `kapto_core::say!`.

use std::fmt;
use std::io::{self, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};

static STDOUT_GONE: AtomicBool = AtomicBool::new(false);

/// Writes `text` to stdout, or drops it once stdout can't be written.
pub(crate) fn print(text: fmt::Arguments<'_>) {
    if STDOUT_GONE.load(Ordering::Relaxed) {
        return;
    }
    let mut stdout = io::stdout().lock();
    // Flushed now: a failure found only at exit couldn't change the status.
    if stdout
        .write_fmt(text)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        STDOUT_GONE.store(true, Ordering::Relaxed);
    }
}

/// Whether something printed on stdout couldn't be written.
pub(crate) fn stdout_gone() -> bool {
    STDOUT_GONE.load(Ordering::Relaxed)
}

/// Like `print!`, without panicking when stdout has gone.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::output::print(::std::format_args!($($arg)*))
    };
}

/// Like `println!`, without panicking when stdout has gone.
macro_rules! outln {
    () => {
        $crate::output::print(::std::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::output::print(::std::format_args!("{}\n", ::std::format_args!($($arg)*)))
    };
}

pub(crate) use {out, outln};
