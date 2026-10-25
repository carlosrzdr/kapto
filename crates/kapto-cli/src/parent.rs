// SPDX-License-Identifier: Apache-2.0

//! Ending with serve. The viewer bridge and the worker are of no use
//! without the serve that started them, so they end with it, even when it
//! is killed outright and can't stop them: left running, they would hold
//! their ports and sockets, and a serve started again would start a second
//! of each beside them. Capture ends with it too, but only once no episode
//! is recorded: it outlives serve to finish the episode it records (see
//! [`gone`]).
//!
//! Serve passes its process ID (`--parent`); a process started without it
//! ends only when told to.

use std::io;

use rustix::process::{Signal, getpid, getppid, kill_process};

/// Gets the calling process SIGTERM once process `parent` has ended, as
/// SIGTERM from serve would: the worker then gives back the job it is
/// doing, and the bridge ends at once.
///
/// # Errors
///
/// The operating system's reason if it can't arrange that.
pub(crate) fn end_with(parent: u32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    rustix::process::set_parent_process_death_signal(Some(Signal::TERM))?;
    // Already gone before the request was made: nothing would send it.
    if gone(parent) {
        return terminate();
    }
    // Elsewhere, nothing sends it: look once a second.
    #[cfg(not(target_os = "linux"))]
    std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(move || {
            while !gone(parent) {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            let _ = terminate();
        })?;
    Ok(())
}

/// Whether process `parent`, which started this one, has ended: this one
/// was then handed to another parent. A system call, cheap enough to ask a
/// few times a second, never per message.
pub(crate) fn gone(parent: u32) -> bool {
    getppid().is_none_or(|pid| pid.as_raw_nonzero().get().cast_unsigned() != parent)
}

fn terminate() -> io::Result<()> {
    kill_process(getpid(), Signal::TERM).map_err(io::Error::from)
}
