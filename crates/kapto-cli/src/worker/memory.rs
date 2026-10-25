// SPDX-License-Identifier: Apache-2.0

//! Memory a job freed, given back to the system after each job. glibc's
//! allocator keeps what a thread frees for its next allocations, so a
//! worker that once checked or replayed a long episode would hold that much
//! while it waits, which is most of the time: over a hundred MiB after an
//! hour of episodes of a robot with two cameras, all of it free.

#![allow(
    unsafe_code,
    reason = "malloc_trim is only reachable through libc's unsafe binding"
)]

/// Hands back to the system the memory the allocator holds free, on Linux
/// with glibc; elsewhere nothing.
pub(super) fn give_back() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim takes a size and touches only the allocator's own
    // memory, under the allocator's own locks, so any thread may call it at
    // any time; its result only says whether anything was given back.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This process's resident anonymous memory, in MiB.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn resident_mib() -> f64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let kib: f64 = status
            .lines()
            .find_map(|line| line.strip_prefix("RssAnon:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse().ok())
            .unwrap();
        kib / 1024.0
    }

    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn memory_a_job_freed_is_given_back_to_the_system() {
        std::thread::spawn(|| {
            // Pieces small enough to come from the thread's own arena, every
            // 50th kept so the arena can't shrink by itself.
            let mut pieces: Vec<Vec<u8>> = (0..2_000).map(|_| vec![1_u8; 64 * 1024]).collect();
            let kept: Vec<Vec<u8>> = pieces.iter_mut().step_by(50).map(std::mem::take).collect();
            drop(pieces);
            let held = resident_mib();
            give_back();
            let given = held - resident_mib();
            drop(kept);
            assert!(given > 80.0, "only {given:.0} MiB of about 120 given back");
        })
        .join()
        .unwrap();
    }
}
