// SPDX-License-Identifier: Apache-2.0

//! What capture declares for `cap-hash` and `cap-finalise` covers what they
//! allocate while one episode is closed and the next recorded, and
//! episodes waiting to be closed hold no `cap-hash` or its buffer.
//!
//! A counting allocator wraps the system allocator for this test binary.
//! It keeps, per thread, the bytes it allocated and hasn't yet freed, and
//! the most there ever were, which each thread reports as it ends. Stacks
//! aren't allocated through it: the declaration's arithmetic is tested
//! beside it, in `kapto_capture::threads`.

#![allow(
    unsafe_code,
    reason = "a global allocator can only be written with unsafe trait methods"
)]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use kapto_capture::finalise::{RECORDING_FILE, Staged};
use kapto_capture::threads::{SMALL_BYTES, ThreadMemory, Threads, named};
use kapto_capture::{
    Batch, ChannelSpec, Compression, HashBehind, LossCounters, Recording, WriterConfig,
};
use kapto_core::{ClockMap, ClockSample, EpisodeId, RecvTime};

struct CountingAllocator;

thread_local! {
    // `const` initialisers: reading them never allocates, so the allocator
    // can use them without recursing into itself.
    static ID: Cell<u64> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
    static HASH_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// What every `cap-hash` thread holds together, now.
static HASH_LIVE: AtomicIsize = AtomicIsize::new(0);

/// Adds `bytes` to what `cap-hash` threads hold, if this is one.
fn hash_thread_holds(bytes: isize) {
    if HASH_THREAD.try_with(Cell::get).unwrap_or(false) {
        HASH_LIVE.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// The next thread's number; 0 is a thread that couldn't be told apart.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The calling thread's number, given on its first allocation.
fn thread_id() -> u64 {
    ID.try_with(|id| {
        if id.get() == 0 {
            id.set(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        }
        id.get()
    })
    .unwrap_or(0)
}

fn grew(bytes: usize) {
    let bytes = isize::try_from(bytes).unwrap_or(isize::MAX);
    let _ = LIVE.try_with(|live| {
        live.set(live.get().saturating_add(bytes));
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(live.get())));
    });
    hash_thread_holds(bytes);
}

fn shrank(bytes: usize) {
    let bytes = isize::try_from(bytes).unwrap_or(isize::MAX);
    let _ = LIVE.try_with(|live| live.set(live.get().saturating_sub(bytes)));
    hash_thread_holds(-bytes);
}

/// Each allocation is preceded by the number of the thread that made it,
/// so freeing what another thread allocated (an episode's recording,
/// handed to `cap-finalise`) doesn't count against the one freeing it.
/// The header is a multiple of the alignment asked for, so what follows it
/// keeps that alignment.
fn with_header(layout: Layout) -> Option<(Layout, usize)> {
    let align = layout.align().max(16);
    let size = layout.size().checked_add(align)?;
    Some((Layout::from_size_align(size, align).ok()?, align))
}

#[allow(
    clippy::cast_ptr_alignment,
    reason = "the tag's place is 8-aligned: the allocation is at least 16-aligned and the header a multiple of 16"
)]
// SAFETY: every allocation is the system allocator's, `align` bytes larger
// and at least as aligned as asked; the pointer handed out is `align`
// bytes in, which keeps the alignment asked for and leaves the size asked
// for after it. Freeing goes back by the same `align` to the system
// allocator's pointer, with the layout it was allocated with.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some((outer, header)) = with_header(layout) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `outer` has a non-zero size, being larger than `header`.
        let base = unsafe { System.alloc(outer) };
        if base.is_null() {
            return base;
        }
        grew(layout.size());
        // SAFETY: `header` is at least 16 and within the allocation; the tag
        // goes in its last 8 bytes, 8-aligned as `base` and `header` are.
        unsafe {
            let inner = base.add(header);
            inner.cast::<u64>().sub(1).write(thread_id());
            inner
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some((outer, header)) = with_header(layout) else {
            return;
        };
        // SAFETY: `ptr` came from `alloc` with this layout, `header` bytes
        // past the system allocator's pointer, its tag just before it.
        unsafe {
            if ptr.cast::<u64>().sub(1).read() == thread_id() {
                shrank(layout.size());
            }
            System.dealloc(ptr.sub(header), outer);
        }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The most any `cap-hash` thread held at once.
static HASH_PEAK: AtomicUsize = AtomicUsize::new(0);
/// The most `cap-finalise` held at once.
static FINALISE_PEAK: AtomicUsize = AtomicUsize::new(0);
/// The `cap-hash` threads running, and the most there ever were.
static HASHING: AtomicUsize = AtomicUsize::new(0);
static MOST_HASHING: AtomicUsize = AtomicUsize::new(0);

/// Reports the thread's peak into its static when the thread ends, after
/// everything it allocated on the way has been counted.
struct ReportPeak {
    into: &'static AtomicUsize,
    hashing: bool,
}

impl Drop for ReportPeak {
    fn drop(&mut self) {
        let peak = PEAK.try_with(Cell::get).unwrap_or(0);
        self.into
            .fetch_max(usize::try_from(peak).unwrap_or(0), Ordering::Relaxed);
        if self.hashing {
            // What it still held goes with it.
            let live = LIVE.try_with(Cell::get).unwrap_or(0);
            HASH_LIVE.fetch_sub(live, Ordering::Relaxed);
            HASHING.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

thread_local! {
    static REPORT: Cell<Option<ReportPeak>> = const { Cell::new(None) };
}

/// Counts the calling thread from now, its peak reported into `into`.
fn count_into(into: &'static AtomicUsize, hashing: bool) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    if hashing {
        let now = HASHING.fetch_add(1, Ordering::Relaxed) + 1;
        MOST_HASHING.fetch_max(now, Ordering::Relaxed);
        HASH_THREAD.with(|flag| flag.set(true));
    }
    REPORT.with(|report| report.set(Some(ReportPeak { into, hashing })));
}

/// The tests share the counts, so they run one at a time, each from zero.
fn alone() -> std::sync::MutexGuard<'static, ()> {
    static ALONE: Mutex<()> = Mutex::new(());
    let guard = ALONE.lock().unwrap_or_else(PoisonError::into_inner);
    for count in [&HASH_PEAK, &FINALISE_PEAK, &HASHING, &MOST_HASHING] {
        count.store(0, Ordering::Relaxed);
    }
    HASH_LIVE.store(0, Ordering::Relaxed);
    guard
}

/// Waits up to 30 s for `done`.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(30), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// An output folder unique to this test, removed when dropped.
struct Out(PathBuf);

impl Out {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("kapto-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Bytes that don't compress, different for each `seed`.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// An episode being recorded, hashed behind its writer.
struct Episode {
    staged: Staged,
    recording: Recording,
    hash: HashBehind,
    clock: ClockMap,
}

fn start(out: &Path, id: &str) -> Episode {
    let staged = Staged::create(out, &id.parse::<EpisodeId>().unwrap()).unwrap();
    let clock = ClockMap::new(ClockSample::now());
    let streams = [(0, "camera".parse().unwrap())].map(ChannelSpec::from);
    let config = WriterConfig {
        compression: Compression::None,
        ..WriterConfig::default()
    };
    let mut recording =
        Recording::create(&staged.recording_path(), &streams, &clock, &config).unwrap();
    let (written, hash) =
        HashBehind::start(&staged.recording_path(), || count_into(&HASH_PEAK, true)).unwrap();
    recording.set_hash_behind(written);
    Episode {
        staged,
        recording,
        hash,
        clock,
    }
}

/// Writes 16 MiB of camera frames, put on disk every 2 MiB.
fn write(episode: &mut Episode, seed: u64) {
    let counters = LossCounters::new(1);
    let mut batch = Batch::new(4, 64 * 1024);
    for round in 0..64_u64 {
        batch.clear();
        for n in 0..4 {
            batch.push(0, &noise(seed + round * 4 + n, 64 * 1024));
        }
        batch.set_recv_time(RecvTime::from_nanos(round * 1_000_000));
        episode.recording.write(&batch, &counters).unwrap();
        if round % 8 == 7 {
            let due = RecvTime::from_nanos(u64::MAX / 128 * (round / 8 + 1));
            episode.recording.sync_if_due(due).unwrap();
        }
    }
}

/// What `cap-finalise` does with an episode: the file's index and summary,
/// `episode.json`, the rest of the recording hashed by `cap-hash`, the
/// manifest hashing every other file, and `_FINALISED`.
fn close(episode: Episode) -> PathBuf {
    let Episode {
        staged,
        recording,
        hash,
        clock,
    } = episode;
    recording.finish(&clock).unwrap();
    fs::write(staged.dir().join("episode.json"), "{}").unwrap();
    let hashed = vec![hash.finish().unwrap().entry(RECORDING_FILE)];
    staged.finalise_with(&hashed).unwrap().dir().to_path_buf()
}

/// What capture declares for each of the threads named `name`.
fn declared(name: &str) -> Threads {
    ThreadMemory::capture(1, true)
        .threads
        .into_iter()
        .find(|threads| threads.name == name)
        .unwrap()
}

#[test]
fn what_is_declared_for_cap_hash_and_cap_finalise_covers_an_episode_closing_while_the_next_records()
{
    let _alone = alone();
    let out = Out::new("thread-memory");
    let (closings, to_close) = mpsc::channel::<Episode>();
    let (go, held) = mpsc::channel::<()>();
    let (closed, done) = mpsc::channel();
    let finalise = named("cap-finalise")
        .spawn(move || {
            count_into(&FINALISE_PEAK, false);
            for episode in to_close {
                held.recv().unwrap();
                closed.send(close(episode)).unwrap();
            }
        })
        .unwrap();

    let mut first = start(&out.0, "20260305T192345.123Z-1a2b3c4d");
    write(&mut first, 0);
    closings.send(first).unwrap();
    // The next episode is recorded, its own `cap-hash` reading, before the
    // first is closed and while it is.
    let mut second = start(&out.0, "20260305T192346.123Z-1a2b3c4e");
    write(&mut second, 1_000);
    go.send(()).unwrap();
    write(&mut second, 2_000);
    closings.send(second).unwrap();
    go.send(()).unwrap();
    for _ in 0..2 {
        assert!(done.recv().unwrap().join("_FINALISED").exists());
    }
    drop(closings);
    finalise.join().unwrap();

    let hash = declared("cap-hash");
    assert_eq!(MOST_HASHING.load(Ordering::Relaxed), 2);
    assert!(MOST_HASHING.load(Ordering::Relaxed) <= hash.at_once);
    let hashed = HASH_PEAK.load(Ordering::Relaxed);
    // Its buffer is most of it, and was counted.
    assert!(hashed >= hash.buffer_bytes, "{hashed} bytes");
    assert!(hashed <= hash.buffer_bytes + SMALL_BYTES, "{hashed} bytes");

    let finalise = declared("cap-finalise");
    let finalised = FINALISE_PEAK.load(Ordering::Relaxed);
    assert!(finalised >= finalise.buffer_bytes, "{finalised} bytes");
    assert!(
        finalised <= finalise.buffer_bytes + SMALL_BYTES,
        "{finalised} bytes"
    );
}

#[test]
fn episodes_waiting_behind_the_one_being_closed_hold_no_cap_hash_or_its_buffer() {
    let _alone = alone();
    let out = Out::new("thread-memory-waiting");
    let (closings, to_close) = mpsc::channel::<Episode>();
    let (go, held) = mpsc::channel::<()>();
    let (closed, done) = mpsc::channel();
    let finalise = named("cap-finalise")
        .spawn(move || {
            count_into(&FINALISE_PEAK, false);
            for episode in to_close {
                held.recv().unwrap();
                closed.send(close(episode)).unwrap();
            }
        })
        .unwrap();
    let hash = declared("cap-hash");
    let one_buffer = isize::try_from(hash.buffer_bytes + SMALL_BYTES).unwrap();

    // The first is being closed, held there; the second and third wait
    // behind it, each stopped as capture stops one: set aside first.
    let ids = [
        "20260305T192345.123Z-1a2b3c4d",
        "20260305T192346.123Z-1a2b3c4e",
        "20260305T192347.123Z-1a2b3c4f",
    ];
    for (n, id) in (0_u64..).zip(ids) {
        let mut episode = start(&out.0, id);
        write(&mut episode, n * 1_000);
        assert!(HASHING.load(Ordering::Relaxed) <= hash.at_once);
        // While it records, only its own `cap-hash` runs, holding one
        // buffer: none for the episodes waiting.
        wait_until("the waiting episodes' cap-hash ended", || {
            HASHING.load(Ordering::Relaxed) == 1
        });
        let live = HASH_LIVE.load(Ordering::Relaxed);
        assert!(live <= one_buffer, "{live} bytes held by cap-hash");
        episode.hash.set_aside();
        closings.send(episode).unwrap();
    }
    wait_until("every waiting episode's cap-hash ended", || {
        HASHING.load(Ordering::Relaxed) == 0
    });
    let live = HASH_LIVE.load(Ordering::Relaxed);
    assert!(
        live <= 0,
        "{live} bytes held by cap-hash, nothing recording"
    );
    for _ in ids {
        go.send(()).unwrap();
        assert!(done.recv().unwrap().join("_FINALISED").exists());
    }
    drop(closings);
    finalise.join().unwrap();

    let most = MOST_HASHING.load(Ordering::Relaxed);
    assert!((1..=2).contains(&most), "{most} cap-hash at once");
    assert!(most <= hash.at_once);
    // The rest of each recording was read by `cap-finalise`, within what
    // is declared for it.
    let finalise = declared("cap-finalise");
    let finalised = FINALISE_PEAK.load(Ordering::Relaxed);
    assert!(finalised >= finalise.buffer_bytes, "{finalised} bytes");
    assert!(
        finalised <= finalise.buffer_bytes + SMALL_BYTES,
        "{finalised} bytes"
    );
}
