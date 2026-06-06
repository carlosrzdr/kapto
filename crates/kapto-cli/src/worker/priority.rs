// SPDX-License-Identifier: Apache-2.0

//! The worker's low priority: its jobs use the CPU and the disk only when
//! nothing else wants them, capture above all. Nice 10, and on Linux the
//! idle I/O class, so the disk serves the worker only when nobody else is
//! waiting for it.
//!
//! Linux keeps both per thread and gives a new thread its creator's, so
//! [`lower`] is called before the worker starts any thread.

#![allow(
    unsafe_code,
    reason = "ioprio_set is only reachable through libc's unsafe syscall function"
)]

use std::io;

/// The worker's nice value: lower priority than anything at the default 0.
pub(super) const NICE: i32 = 10;

/// Lowers the calling thread's CPU priority to [`NICE`] and, on Linux, its
/// I/O priority to the idle class.
///
/// # Errors
///
/// The operating system's reason if either couldn't be set.
pub(super) fn lower() -> io::Result<()> {
    rustix::process::setpriority_process(None, NICE)?;
    #[cfg(target_os = "linux")]
    idle_io()?;
    Ok(())
}

/// Moves the calling thread to the idle I/O class (`ioprio_set`, which
/// neither the standard library nor rustix offers).
#[cfg(target_os = "linux")]
fn idle_io() -> io::Result<()> {
    /// `which` names a process or thread; 0 is the caller.
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    /// Served only when no other I/O is waiting.
    const IOPRIO_CLASS_IDLE: libc::c_long = 3;
    const IOPRIO_CLASS_SHIFT: u32 = 13;
    // SAFETY: ioprio_set takes three integers and reads or writes no memory
    // of ours; it can only fail, which is checked below.
    let result = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            0,
            IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
