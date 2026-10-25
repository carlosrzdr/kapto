// SPDX-License-Identifier: Apache-2.0

//! What a process says to the people watching it, on stderr.
//!
//! `eprintln!` panics when stderr can't be written, as when the reader of
//! the pipe it goes to has gone (`kapto serve 2>&1 | tee log` whose `tee`
//! was stopped, or a closed terminal). Capture prints a line every second
//! while recording, so a dying log reader would end a recording. What a
//! process says therefore goes through [`say!`](crate::say), which drops a
//! line it can't write and carries on.

use std::fmt;
use std::io::{self, Write as _};

/// Writes `line` and a newline to stderr, dropping it when stderr can't be
/// written.
pub fn say(line: fmt::Arguments<'_>) {
    // The line is what failed: there's nowhere left to report it.
    let _ = writeln!(io::stderr().lock(), "{line}");
}

/// Like `eprintln!`, but a line that can't be written is dropped instead
/// of panicking.
#[macro_export]
macro_rules! say {
    () => {
        $crate::output::say(::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::output::say(::std::format_args!($($arg)*))
    };
}
