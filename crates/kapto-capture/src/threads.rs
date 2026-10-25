// SPDX-License-Identifier: Apache-2.0

//! Capture's threads, and the memory they take, declared when capture
//! starts beside its rings and pre-roll.
//!
//! Every thread capture starts is made by [`named`], with a stack of
//! [`STACK_BYTES`] rather than the default, which `RUST_MIN_STACK` can
//! change, so what is declared for its stack is what it reserves. Each is
//! counted with the buffer it holds for itself, and [`SMALL_BYTES`] for
//! what it allocates as it goes: its name, a message, a line read from a
//! connection, the records it writes.
//!
//! Not counted: the process's main thread, whose stack the operating
//! system sets; the threads rcl and the ROS 2 middleware start, and
//! whatever they allocate; and memory counted where it is set aside (the
//! rings, the pre-roll, kept messages, the live view's frames). Nor what
//! grows with what a thread is given rather than with the thread: the
//! writer's chunks as the MCAP library builds and compresses them, a chunk
//! read back to recover an episode, and an episode's index and
//! `episode.json`, larger the longer the episode and the more streams it
//! has, beyond [`SMALL_BYTES`].
//!
//! A receiver's own buffer is counted at its bound, known at start: a TCP
//! input's framing buffer, room for its stream's `max_message_bytes`, a
//! delimiter and one read, is allocated whole when the input opens rather
//! than grown, its pages taken by the system as bytes first reach them.

use std::thread;

/// The stack every capture thread is started with: Rust's own default,
/// set on each thread so that `RUST_MIN_STACK` can't change it.
pub const STACK_BYTES: usize = 2 * 1024 * 1024;

/// What a thread may allocate as it goes, beside its own buffer.
pub const SMALL_BYTES: usize = 64 * 1024;

/// A thread named `name`, with a stack of [`STACK_BYTES`].
pub fn named(name: impl Into<String>) -> thread::Builder {
    thread::Builder::new()
        .name(name.into())
        .stack_size(STACK_BYTES)
}

/// The threads of one name capture runs at most at once, and the buffer
/// each holds for itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Threads {
    /// Their name, or its start, as in `cap-rx-*`.
    pub name: &'static str,
    /// How many can run at once.
    pub at_once: usize,
    /// The buffer each holds for itself, in bytes.
    pub buffer_bytes: usize,
}

impl Threads {
    /// What one of them takes, in bytes: its stack, its buffer and
    /// [`SMALL_BYTES`].
    #[must_use]
    pub fn each_bytes(&self) -> usize {
        STACK_BYTES + self.buffer_bytes + SMALL_BYTES
    }
}

/// What `kapto capture`'s threads take at most at once, besides its main
/// thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadMemory {
    /// Each name's threads.
    pub threads: Vec<Threads>,
}

impl ThreadMemory {
    /// Capture's threads, with a receiver thread for each of `receivers`,
    /// the buffer it holds for itself (a TCP input's framing buffer, 0 for
    /// others), the live view's `cap-tap` if it runs, and a
    /// `cap-control-conn` for each of the `control_connections` answered at
    /// once (`capture.control_max_connections`), whether or not they are
    /// open.
    ///
    /// Two `cap-hash`: the episode being recorded, and the one just
    /// stopped until it is set aside and ends at its next read
    /// ([`crate::hash_behind`]). An episode waiting to be closed, behind
    /// another or not, keeps no thread or buffer, only its digest and its
    /// open file. `cap-finalise` closes one episode at a time, reading the
    /// rest of its recording and then the manifest's other files with one
    /// buffer each, one after the other. Pre-flight's and recovery's
    /// threads are counted though they run only for a while.
    #[must_use]
    pub fn capture(receivers: &[usize], live_view: bool, control_connections: usize) -> Self {
        let one = one_of;
        let mut threads = vec![
            one("cap-control", 0),
            one("cap-control-tell", 0),
            Threads {
                name: "cap-control-conn",
                at_once: control_connections,
                buffer_bytes: 0,
            },
            one("cap-recover", 0),
            one("cap-preflight", crate::preflight::WRITE_BLOCK_BYTES),
            one("cap-preflight-router", 0),
        ];
        threads.extend(receivers.iter().map(|&buffer| one("cap-rx-*", buffer)));
        threads.extend([
            one("cap-writer", 0),
            Threads {
                name: "cap-hash",
                at_once: 2,
                buffer_bytes: crate::hash_behind::CHUNK,
            },
            one(
                "cap-finalise",
                crate::hash_behind::CHUNK.max(kapto_store::manifest::HASH_BUFFER_BYTES),
            ),
        ]);
        if live_view {
            threads.push(one("cap-tap", 0));
        }
        Self { threads }
    }

    /// How many threads are counted.
    #[must_use]
    pub fn count(&self) -> usize {
        self.threads.iter().map(|threads| threads.at_once).sum()
    }

    /// What the receiver threads hold for themselves, in bytes: their TCP
    /// inputs' framing buffers.
    #[must_use]
    pub fn receiver_buffer_bytes(&self) -> usize {
        self.threads
            .iter()
            .filter(|threads| threads.name == "cap-rx-*")
            .map(|threads| threads.at_once * threads.buffer_bytes)
            .sum()
    }

    /// What they take together, in bytes.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.threads
            .iter()
            .map(|threads| threads.at_once * threads.each_bytes())
            .sum()
    }

    /// How many control connections are counted.
    #[must_use]
    pub fn control_connections(&self) -> usize {
        self.threads
            .iter()
            .filter(|threads| threads.name == "cap-control-conn")
            .map(|threads| threads.at_once)
            .sum()
    }
}

/// One thread named `name`, holding `buffer_bytes` for itself.
fn one_of(name: &'static str, buffer_bytes: usize) -> Threads {
    Threads {
        name,
        at_once: 1,
        buffer_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_total_is_each_threads_stack_and_buffers() {
        let memory = ThreadMemory::capture(&[0; 3], true, 0);
        // Control 2, recovery 1, pre-flight 2, receivers 3, the writer,
        // cap-hash twice, cap-finalise and the live view's tap.
        assert_eq!(memory.count(), 13);
        let buffers = crate::preflight::WRITE_BLOCK_BYTES
            + 2 * crate::hash_behind::CHUNK
            + kapto_store::manifest::HASH_BUFFER_BYTES;
        assert_eq!(
            memory.total_bytes(),
            13 * (STACK_BYTES + SMALL_BYTES) + buffers
        );
    }

    #[test]
    fn both_episodes_cap_hash_are_counted_with_their_buffers() {
        let memory = ThreadMemory::capture(&[0], false, 16);
        let hash = memory
            .threads
            .iter()
            .find(|threads| threads.name == "cap-hash")
            .map(|threads| threads.at_once * threads.each_bytes());
        assert_eq!(hash, Some(2 * (STACK_BYTES + 256 * 1024 + SMALL_BYTES)));
    }

    #[test]
    fn each_receivers_own_buffer_is_counted_once_with_its_thread() {
        let udp_only = ThreadMemory::capture(&[0], false, 16);
        // UDP's thread and two TCP inputs, one of a 32 MiB camera.
        let memory = ThreadMemory::capture(&[0, 4096, 32 << 20], false, 16);
        assert_eq!(memory.count(), udp_only.count() + 2);
        assert_eq!(memory.receiver_buffer_bytes(), 4096 + (32 << 20));
        assert_eq!(
            memory.total_bytes() - udp_only.total_bytes(),
            2 * (STACK_BYTES + SMALL_BYTES) + 4096 + (32 << 20)
        );
    }

    #[test]
    fn the_control_connections_answered_at_once_are_counted_whether_open_or_not() {
        let none = ThreadMemory::capture(&[0], false, 0);
        let sixteen = ThreadMemory::capture(&[0], false, 16);
        assert_eq!(sixteen.control_connections(), 16);
        assert_eq!(sixteen.count(), none.count() + 16);
        assert_eq!(
            sixteen.total_bytes() - none.total_bytes(),
            16 * (STACK_BYTES + SMALL_BYTES)
        );
    }

    #[test]
    fn without_a_live_view_its_tap_thread_isnt_counted() {
        let with = ThreadMemory::capture(&[0; 2], true, 16);
        let without = ThreadMemory::capture(&[0; 2], false, 16);
        assert_eq!(with.count(), without.count() + 1);
        assert_eq!(
            with.total_bytes() - without.total_bytes(),
            STACK_BYTES + SMALL_BYTES
        );
    }
}
