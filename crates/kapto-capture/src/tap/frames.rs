// SPDX-License-Identifier: Apache-2.0

//! Each camera's newest frame, handed from the writer thread to the tap
//! thread, which sends it whole to the viewer bridge's frame socket.
//!
//! A frame is far larger than a datagram, so frames have a Unix stream
//! socket of their own. Nothing here can make recording wait. The writer
//! copies a camera's newest frame into one of three slots set aside for it,
//! at most once per [`FrameConfig::min_interval`] and no more often than the
//! bridge last asked for, and only into a slot the tap thread has finished
//! with; otherwise the frame is skipped and counted. A frame the bridge
//! didn't ask for is never copied: with no page open, it wants only what
//! its history keeps, a few a second.
//! The tap thread writes one frame at a time, until a deadline it is given,
//! finishing a frame it wrote in part before starting another; meanwhile
//! only each camera's newest frame waits, and older ones are skipped and
//! counted. The third slot is what keeps the newest frame waiting while the
//! bridge is slow: one slot is being sent, one waits, and the writer fills
//! the last, which then takes the waiting one's place. With no bridge
//! listening, frames are counted as not sent.
//!
//! Each frame on the socket is a header and the message as recorded:
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 4 | `KFRM` |
//! | 4 | 1 | version, 2 |
//! | 5 | 1 | 0, reserved |
//! | 6 | 2 | stream index, `u16`, its position in the stream declarations |
//! | 8 | 8 | receive time, `u64` nanoseconds of the host's monotonic clock |
//! | 16 | 4 | the message's length `n`, `u32` |
//! | 20 | n | the message, exactly as received |
//!
//! The bridge says on the same connection how often it wants each camera's
//! frames, when it connects and whenever that changes, each camera in a
//! record of its own:
//!
//! | Offset | Bytes | Field |
//! |---|---|---|
//! | 0 | 4 | `KFRW` |
//! | 4 | 1 | version, 2 |
//! | 5 | 1 | 0, reserved |
//! | 6 | 2 | stream index, `u16` |
//! | 8 | 8 | the shortest time it wants between two frames, `u64` nanoseconds |
//!
//! The tap thread reads these without waiting and keeps each camera's
//! interval, never shorter than the cap, in an atomic the writer reads once
//! per batch. With no bridge connected, or before it has said, the cap
//! alone applies. A record of a stream that isn't a camera here is ignored.
//!
//! All numbers are little-endian. A reader that finds a version it doesn't
//! know can't tell where the next frame or record starts, and closes the
//! connection: a bridge and capture of different builds refuse each other.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use kapto_core::RecvTime;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::batch::{Batch, StreamIndex};

/// The first four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"KFRM";

/// The version this code writes, of frames and of the bridge's records.
pub const VERSION: u8 = 2;

/// The bytes before the message.
pub const HEADER: usize = 20;

/// The first four bytes of each record of how often the bridge wants a
/// camera's frames.
pub const WANTED_MAGIC: [u8; 4] = *b"KFRW";

/// The bytes of one such record.
pub const WANTED: usize = 16;

/// Slots per camera: one the tap thread is sending, one waiting, one the
/// writer fills.
const SLOTS: usize = 3;

/// How often the tap thread tries to reach a bridge that wasn't listening.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// How the frames' slots are sized and how often frames are offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameConfig {
    /// Each camera: its stream and the longest frame it sends.
    pub cameras: Vec<(StreamIndex, usize)>,
    /// The shortest time between two frames offered from one camera.
    pub min_interval: Duration,
}

impl FrameConfig {
    /// The memory the slots take.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.cameras
            .iter()
            .map(|(_, max_len)| SLOTS * (HEADER + max_len))
            .sum()
    }
}

/// One camera's frame: its header, then the message.
#[derive(Debug)]
struct Slot {
    bytes: Box<[u8]>,
    /// The header and message's length; 0 while empty.
    len: usize,
}

impl Slot {
    fn recv_time(&self) -> u64 {
        let mut time = [0; 8];
        time.copy_from_slice(&self.bytes[8..16]);
        u64::from_le_bytes(time)
    }
}

/// Creates the frames' path: the writer thread's end, the tap thread's end,
/// and its counters.
#[must_use]
pub fn frames(config: &FrameConfig, socket: &Path) -> (FrameFeed, FrameSender, Arc<FrameStats>) {
    let stats = Arc::new(FrameStats {
        cameras: config
            .cameras
            .iter()
            .map(|(stream, _)| CameraCounts {
                stream: *stream,
                ..CameraCounts::default()
            })
            .collect(),
    });
    let streams = config
        .cameras
        .iter()
        .map(|(stream, _)| usize::from(*stream) + 1);
    let mut camera_of = vec![usize::MAX; streams.max().unwrap_or(0)];
    let cap = u64::try_from(config.min_interval.as_nanos()).unwrap_or(u64::MAX);
    let intervals: Arc<[AtomicU64]> = config.cameras.iter().map(|_| AtomicU64::new(cap)).collect();
    let mut feeds = Vec::with_capacity(config.cameras.len());
    let mut sends = Vec::with_capacity(config.cameras.len());
    for (camera, (stream, max_len)) in config.cameras.iter().enumerate() {
        camera_of[usize::from(*stream)] = camera;
        let (full_tx, full_rx) = RingBuffer::new(SLOTS);
        let (mut free_tx, free_rx) = RingBuffer::new(SLOTS);
        for _ in 0..SLOTS {
            // Can't fail: the queue holds every slot there is.
            let _ = free_tx.push(Slot {
                bytes: vec![0; HEADER + max_len].into_boxed_slice(),
                len: 0,
            });
        }
        feeds.push(FeedCamera {
            full: full_tx,
            free: free_rx,
            due: 0,
            interval: cap,
            newest: usize::MAX,
        });
        sends.push(SendCamera {
            full: full_rx,
            free: free_tx,
            waiting: None,
        });
    }
    let feed = FrameFeed {
        camera_of: camera_of.into_boxed_slice(),
        cameras: feeds.into_boxed_slice(),
        intervals: Arc::clone(&intervals),
        stats: Arc::clone(&stats),
    };
    let sender = FrameSender {
        cameras: sends.into_boxed_slice(),
        sending: None,
        path: socket.to_path_buf(),
        stream: None,
        last_attempt: None,
        intervals,
        cap,
        heard: [0; WANTED],
        heard_len: 0,
        stats: Arc::clone(&stats),
    };
    (feed, sender, stats)
}

/// The writer thread's end.
#[derive(Debug)]
pub struct FrameFeed {
    /// Per stream, its camera's position in the config, or `usize::MAX`.
    camera_of: Box<[usize]>,
    cameras: Box<[FeedCamera]>,
    /// Each camera's shortest time between two offers, in nanoseconds: the
    /// cap, or longer if the bridge wants fewer. Set by the tap thread.
    intervals: Arc<[AtomicU64]>,
    stats: Arc<FrameStats>,
}

#[derive(Debug)]
struct FeedCamera {
    full: Producer<Slot>,
    free: Consumer<Slot>,
    /// When the next frame is due, in nanoseconds of the monotonic clock;
    /// 0 before the first.
    due: u64,
    /// The shortest time between two offers `due` was set by.
    interval: u64,
    /// The position of its newest frame in the current batch.
    newest: usize,
}

impl FeedCamera {
    /// Offers from now on no more often than every `interval`.
    fn follow(&mut self, interval: u64) {
        if interval == self.interval {
            return;
        }
        // Due that long after the last frame offered: a page that opens
        // has its frames from the next, not after the history's interval.
        if self.due != 0 {
            self.due = self
                .due
                .saturating_sub(self.interval)
                .saturating_add(interval);
        }
        self.interval = interval;
    }

    /// Whether its newest frame in the batch received at `now` is due.
    /// Frames a little early still count as due, so a camera at twice the
    /// interval's rate with some jitter is offered at that rate, not at a
    /// third of its own.
    fn due(&self, now: u64) -> bool {
        self.newest != usize::MAX && now.saturating_add(self.interval / 8) >= self.due
    }
}

impl FrameFeed {
    /// Offers each camera's newest frame in `batch`, if one is due. Never
    /// allocates, locks or waits: with no free slot, the frame is skipped
    /// and counted.
    pub fn offer(&mut self, batch: &Batch) {
        let now = batch.recv_time().as_nanos();
        let mut any = false;
        for (position, (stream, _)) in batch.messages().enumerate() {
            if let Some(&camera) = self.camera_of.get(usize::from(stream))
                && let Some(camera) = self.cameras.get_mut(camera)
            {
                camera.newest = position;
                any = true;
            }
        }
        if !any {
            return;
        }
        for (camera, interval) in self.cameras.iter_mut().zip(self.intervals.iter()) {
            camera.follow(interval.load(Ordering::Relaxed));
        }
        // Most batches have no frame due: they cost one look at each
        // message, not two.
        if !self.cameras.iter().any(|camera| camera.due(now)) {
            for camera in &mut self.cameras {
                camera.newest = usize::MAX;
            }
            return;
        }
        for (position, (stream, message)) in batch.messages().enumerate() {
            let Some(&index) = self.camera_of.get(usize::from(stream)) else {
                continue;
            };
            let Some(camera) = self.cameras.get_mut(index) else {
                continue;
            };
            if camera.newest != position {
                continue;
            }
            let due = camera.due(now);
            camera.newest = usize::MAX;
            if !due {
                continue;
            }
            // The next one is due an interval after this one was, so the
            // interval holds over time; after a pause, it starts again from
            // now.
            camera.due = if now > camera.due.saturating_add(camera.interval) {
                now.saturating_add(camera.interval)
            } else {
                camera.due.saturating_add(camera.interval)
            };
            let counts = &self.stats.cameras[index];
            counts.offered.fetch_add(1, Ordering::Relaxed);
            let Ok(mut slot) = camera.free.pop() else {
                counts.skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let len = HEADER + message.len();
            let (Some(bytes), Ok(message_len)) =
                (slot.bytes.get_mut(..len), u32::try_from(message.len()))
            else {
                counts.skipped.fetch_add(1, Ordering::Relaxed);
                // An empty slot goes back through the tap thread, which
                // frees it.
                let _ = camera.full.push(Slot { len: 0, ..slot });
                continue;
            };
            bytes[..4].copy_from_slice(&MAGIC);
            bytes[4] = VERSION;
            bytes[5] = 0;
            bytes[6..8].copy_from_slice(&stream.to_le_bytes());
            bytes[8..16].copy_from_slice(&now.to_le_bytes());
            bytes[16..20].copy_from_slice(&message_len.to_le_bytes());
            bytes[HEADER..].copy_from_slice(message);
            slot.len = len;
            // Can't fail: the queue holds every slot there is.
            let _ = camera.full.push(slot);
        }
    }
}

/// The tap thread's end: takes the frames offered and writes them to the
/// bridge's frame socket.
#[derive(Debug)]
pub struct FrameSender {
    cameras: Box<[SendCamera]>,
    /// The frame being written: its camera, its slot and how much of it
    /// is written.
    sending: Option<(usize, Slot, usize)>,
    path: PathBuf,
    stream: Option<UnixStream>,
    last_attempt: Option<Instant>,
    /// Each camera's interval, as the writer reads it.
    intervals: Arc<[AtomicU64]>,
    /// The shortest interval, `viewer.camera_max_fps`'s, in nanoseconds.
    cap: u64,
    /// A record from the bridge read in part, and how much of it.
    heard: [u8; WANTED],
    heard_len: usize,
    stats: Arc<FrameStats>,
}

#[derive(Debug)]
struct SendCamera {
    full: Consumer<Slot>,
    free: Producer<Slot>,
    /// Its newest frame not yet started.
    waiting: Option<Slot>,
}

impl SendCamera {
    fn free(&mut self, slot: Slot) {
        // Can't fail: the queue holds every slot there is.
        let _ = self.free.push(Slot { len: 0, ..slot });
    }
}

impl FrameSender {
    /// Writes frames to the bridge until each camera's newest is sent or
    /// `deadline` passes. A frame the bridge hasn't taken in full by then
    /// is finished next time, before any other.
    pub fn send_until(&mut self, deadline: Instant) {
        self.hear();
        loop {
            self.take_offered();
            if self.sending.is_none() {
                let Some(next) = self.oldest_waiting() else {
                    return;
                };
                if let Some(slot) = self.cameras[next].waiting.take() {
                    self.sending = Some((next, slot, 0));
                }
            }
            let Some((camera, slot, written)) = self.sending.take() else {
                return;
            };
            let Some(stream) = self.connected() else {
                self.stats.cameras[camera]
                    .not_sent
                    .fetch_add(1, Ordering::Relaxed);
                self.cameras[camera].free(slot);
                continue;
            };
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                self.sending = Some((camera, slot, written));
                return;
            };
            match write(stream, &slot.bytes[written..slot.len], left) {
                Ok(count) if written + count == slot.len => {
                    self.stats.cameras[camera]
                        .sent
                        .fetch_add(1, Ordering::Relaxed);
                    self.cameras[camera].free(slot);
                }
                Ok(count) => self.sending = Some((camera, slot, written + count)),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    self.sending = Some((camera, slot, written));
                    return;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    self.sending = Some((camera, slot, written));
                }
                // The bridge has gone: the frame went with the connection.
                Err(_) => {
                    self.stats.cameras[camera]
                        .not_sent
                        .fetch_add(1, Ordering::Relaxed);
                    self.cameras[camera].free(slot);
                    self.disconnect();
                }
            }
        }
    }

    /// Reads, without waiting, what the bridge has said about how often it
    /// wants each camera's frames. Something that isn't such a record ends
    /// the connection.
    fn hear(&mut self) {
        let mut bytes = [0; 4 * WANTED];
        while let Some(stream) = &self.stream {
            let understood = match receive(stream, &mut bytes) {
                // Closed by the bridge.
                Ok(0) => false,
                Ok(read) => self.heard(bytes.get(..read).unwrap_or_default()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => true,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(_) => false,
            };
            if !understood {
                self.disconnect();
            }
        }
    }

    /// Takes `bytes` read from the bridge, each record whole setting its
    /// camera's interval. `false` if one isn't a record of this version.
    fn heard(&mut self, bytes: &[u8]) -> bool {
        for &byte in bytes {
            let Some(at) = self.heard.get_mut(self.heard_len) else {
                return false;
            };
            *at = byte;
            self.heard_len += 1;
            if self.heard_len < WANTED {
                continue;
            }
            self.heard_len = 0;
            let Some((stream, interval)) = read_wanted(&self.heard) else {
                return false;
            };
            let camera = self.stats.cameras.iter().position(|c| c.stream == stream);
            if let Some(wanted) = camera.and_then(|camera| self.intervals.get(camera)) {
                let interval = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
                wanted.store(interval.max(self.cap), Ordering::Relaxed);
            }
        }
        true
    }

    /// Lets go of the connection to the bridge: a frame written in part is
    /// counted as not sent, and each camera's frames are offered at the cap
    /// again until a bridge says otherwise.
    fn disconnect(&mut self) {
        self.stream = None;
        self.heard_len = 0;
        for interval in self.intervals.iter() {
            interval.store(self.cap, Ordering::Relaxed);
        }
        if let Some((camera, slot, written)) = self.sending.take() {
            if written == 0 {
                // Nothing of it went: it can still go whole on the next.
                self.sending = Some((camera, slot, written));
            } else {
                self.stats.cameras[camera]
                    .not_sent
                    .fetch_add(1, Ordering::Relaxed);
                self.cameras[camera].free(slot);
            }
        }
    }

    /// Stops sending: the frame being written and every frame still
    /// waiting are counted as not sent.
    pub fn finish(mut self) {
        self.take_offered();
        if let Some((camera, _, _)) = self.sending.take() {
            self.stats.cameras[camera]
                .not_sent
                .fetch_add(1, Ordering::Relaxed);
        }
        for (camera, counts) in self.cameras.iter_mut().zip(&self.stats.cameras) {
            if camera.waiting.take().is_some() {
                counts.not_sent.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Whether the writer's end is gone and every frame it offered was
    /// taken.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.sending.is_none()
            && self.cameras.iter().all(|camera| {
                camera.waiting.is_none() && camera.full.is_abandoned() && camera.full.is_empty()
            })
    }

    /// Keeps only each camera's newest frame offered, skipping older ones.
    fn take_offered(&mut self) {
        for (camera, counts) in self.cameras.iter_mut().zip(&self.stats.cameras) {
            while let Ok(slot) = camera.full.pop() {
                if slot.len == 0 {
                    camera.free(slot);
                    continue;
                }
                if let Some(older) = camera.waiting.replace(slot) {
                    counts.superseded.fetch_add(1, Ordering::Relaxed);
                    camera.free(older);
                }
            }
        }
    }

    /// The camera whose waiting frame was received first.
    fn oldest_waiting(&self) -> Option<usize> {
        let waiting = self.cameras.iter().enumerate();
        waiting
            .filter_map(|(camera, state)| Some((camera, state.waiting.as_ref()?.recv_time())))
            .min_by_key(|(_, recv_time)| *recv_time)
            .map(|(camera, _)| camera)
    }

    /// The connection to the bridge, made if there is none and the last
    /// try wasn't too recent.
    fn connected(&mut self) -> Option<&UnixStream> {
        if self.stream.is_none() {
            let now = Instant::now();
            let recent = self
                .last_attempt
                .is_some_and(|last| now.duration_since(last) < RECONNECT_INTERVAL);
            if recent {
                return None;
            }
            self.last_attempt = Some(now);
            self.stream = connect(&self.path).ok();
        }
        self.stream.as_ref()
    }
}

/// Connects to the bridge without waiting: a bridge whose queue of
/// connections is full counts as not listening.
#[cfg(target_os = "linux")]
fn connect(path: &Path) -> io::Result<UnixStream> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let socket = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&socket, &SocketAddrUnix::new(path)?)?;
    let stream = UnixStream::from(socket);
    // Writes wait, but only as long as each one's timeout.
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(not(target_os = "linux"))]
fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Reads what the bridge has sent into `bytes`, without waiting.
#[cfg(target_os = "linux")]
fn receive(stream: &UnixStream, bytes: &mut [u8]) -> io::Result<usize> {
    let (read, _) = rustix::net::recv(stream, bytes, rustix::net::RecvFlags::DONTWAIT)?;
    Ok(read)
}

#[cfg(not(target_os = "linux"))]
fn receive(stream: &UnixStream, bytes: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;
    stream.set_nonblocking(true)?;
    let mut reading = stream;
    let read = reading.read(bytes);
    stream.set_nonblocking(false)?;
    read
}

/// Writes as much of `bytes` as the bridge takes within `timeout`.
fn write(stream: &UnixStream, bytes: &[u8], timeout: Duration) -> io::Result<usize> {
    // A timeout of zero would mean none at all.
    stream.set_write_timeout(Some(timeout.max(Duration::from_micros(1))))?;
    #[cfg(target_os = "linux")]
    {
        // Not `SIGPIPE` if the bridge has gone, whatever the process does
        // with the signal.
        Ok(rustix::net::send(
            stream,
            bytes,
            rustix::net::SendFlags::NOSIGNAL,
        )?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::io::Write;
        let mut stream = stream;
        stream.write(bytes)
    }
}

/// A frame's header, as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    /// The stream's position in the stream declarations.
    pub stream: StreamIndex,
    /// When the frame was received.
    pub recv_time: RecvTime,
    /// The length of the message that follows.
    pub len: usize,
}

/// Reads a frame's header; `None` if it isn't one of this version.
#[must_use]
pub fn read_header(header: &[u8; HEADER]) -> Option<FrameHeader> {
    if header[..4] != MAGIC || header[4] != VERSION {
        return None;
    }
    let mut time = [0; 8];
    time.copy_from_slice(&header[8..16]);
    let mut len = [0; 4];
    len.copy_from_slice(&header[16..20]);
    Some(FrameHeader {
        stream: u16::from_le_bytes([header[6], header[7]]),
        recv_time: RecvTime::from_nanos(u64::from_le_bytes(time)),
        len: usize::try_from(u32::from_le_bytes(len)).ok()?,
    })
}

/// The bridge's record saying it wants camera `stream`'s frames no more
/// often than every `interval`.
#[must_use]
pub fn wanted_record(stream: StreamIndex, interval: Duration) -> [u8; WANTED] {
    let mut record = [0; WANTED];
    record[..4].copy_from_slice(&WANTED_MAGIC);
    record[4] = VERSION;
    record[6..8].copy_from_slice(&stream.to_le_bytes());
    let interval = u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX);
    record[8..].copy_from_slice(&interval.to_le_bytes());
    record
}

/// Reads one of the bridge's records: the camera's stream and how often it
/// wants its frames; `None` if it isn't one of this version.
#[must_use]
pub fn read_wanted(record: &[u8; WANTED]) -> Option<(StreamIndex, Duration)> {
    if record[..4] != WANTED_MAGIC || record[4] != VERSION {
        return None;
    }
    let mut interval = [0; 8];
    interval.copy_from_slice(&record[8..]);
    Some((
        u16::from_le_bytes([record[6], record[7]]),
        Duration::from_nanos(u64::from_le_bytes(interval)),
    ))
}

/// What happened to each camera's frames, readable from any thread.
#[derive(Debug)]
pub struct FrameStats {
    cameras: Box<[CameraCounts]>,
}

#[derive(Debug, Default)]
struct CameraCounts {
    stream: StreamIndex,
    offered: AtomicU64,
    skipped: AtomicU64,
    superseded: AtomicU64,
    not_sent: AtomicU64,
    sent: AtomicU64,
}

/// One camera's counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameTotals {
    /// The camera's stream.
    pub stream: StreamIndex,
    /// Frames the writer offered, at most one per interval: the cap's, or
    /// the longer one the bridge asked for. Frames between are neither
    /// offered nor counted, as nothing was to use them.
    pub offered: u64,
    /// Offered frames the writer skipped because the tap thread held every
    /// slot, or that were longer than a slot.
    pub skipped: u64,
    /// Frames the tap thread skipped because a newer one came before it
    /// could start them.
    pub superseded: u64,
    /// Frames not sent because no bridge was listening, or it went away
    /// during one.
    pub not_sent: u64,
    /// Frames the bridge took whole.
    pub sent: u64,
}

impl FrameStats {
    /// Each camera's counts, in the order they were configured.
    #[must_use]
    pub fn totals(&self) -> Vec<FrameTotals> {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        self.cameras
            .iter()
            .map(|counts| FrameTotals {
                stream: counts.stream,
                offered: read(&counts.offered),
                skipped: read(&counts.skipped),
                superseded: read(&counts.superseded),
                not_sent: read(&counts.not_sent),
                sent: read(&counts.sent),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP_NS: u64 = 1_000_000_000 / 30;
    const CAP: Duration = Duration::from_nanos(CAP_NS);

    fn config(cameras: &[(StreamIndex, usize)]) -> FrameConfig {
        FrameConfig {
            cameras: cameras.to_vec(),
            min_interval: CAP,
        }
    }

    fn batch(recv_time_ns: u64, messages: &[(StreamIndex, &[u8])]) -> Batch {
        let mut batch = Batch::new(8, 64);
        for (stream, message) in messages {
            assert!(batch.push(*stream, message));
        }
        batch.set_recv_time(RecvTime::from_nanos(recv_time_ns));
        batch
    }

    /// Every frame waiting in the tap thread, by camera.
    fn waiting(sender: &mut FrameSender) -> Vec<Option<(StreamIndex, u64, Vec<u8>)>> {
        sender.take_offered();
        sender
            .cameras
            .iter()
            .map(|camera| {
                let slot = camera.waiting.as_ref()?;
                let header = read_header(slot.bytes[..HEADER].try_into().ok()?)?;
                let message = slot.bytes[HEADER..slot.len].to_vec();
                Some((header.stream, header.recv_time.as_nanos(), message))
            })
            .collect()
    }

    #[test]
    fn each_cameras_newest_frame_in_a_batch_is_offered_and_other_streams_are_not() {
        let (mut feed, mut sender, stats) = frames(&config(&[(1, 8), (3, 8)]), Path::new("x"));
        feed.offer(&batch(
            5,
            &[(1, b"a1"), (0, b"joints"), (1, b"a2"), (3, b"b1")],
        ));
        assert_eq!(
            waiting(&mut sender),
            [Some((1, 5, b"a2".to_vec())), Some((3, 5, b"b1".to_vec()))]
        );
        let totals = stats.totals();
        assert_eq!((totals[0].stream, totals[0].offered), (1, 1));
        assert_eq!((totals[1].stream, totals[1].offered), (3, 1));
        feed.offer(&batch(6, &[(0, b"joints")]));
        assert_eq!(stats.totals()[0].offered, 1);
    }

    #[test]
    fn a_camera_faster_than_the_cap_is_offered_at_the_cap_despite_jitter() {
        let (mut feed, mut sender, stats) = frames(&config(&[(0, 8)]), Path::new("x"));
        // Two seconds of a camera at 120 frames a second, each frame up to
        // a millisecond early or late.
        for frame in 0..240_u64 {
            let jitter = [0, 1_000_000, 2_000_000][usize::try_from(frame % 3).unwrap()];
            feed.offer(&batch(frame * 8_333_333 + jitter, &[(0, b"f")]));
            sender.take_offered();
            let _ = sender.cameras[0]
                .waiting
                .take()
                .map(|slot| sender.cameras[0].free(slot));
        }
        let offered = stats.totals()[0].offered;
        assert!((59..=61).contains(&offered), "{offered} offered");
        assert_eq!(stats.totals()[0].skipped, 0);
    }

    #[test]
    fn a_camera_at_twice_the_cap_is_offered_every_other_frame() {
        let (mut feed, mut sender, stats) = frames(&config(&[(0, 8)]), Path::new("x"));
        for frame in 0..120_u64 {
            // Sixty frames a second, those falling due a little early.
            let early = if frame % 2 == 0 && frame > 0 {
                1_000_000
            } else {
                0
            };
            feed.offer(&batch(frame * 16_666_667 - early, &[(0, b"f")]));
            sender.take_offered();
            let _ = sender.cameras[0]
                .waiting
                .take()
                .map(|slot| sender.cameras[0].free(slot));
        }
        assert_eq!(stats.totals()[0].offered, 60);
    }

    #[test]
    fn with_every_slot_held_by_the_tap_thread_frames_are_skipped_and_counted() {
        let (mut feed, mut sender, stats) = frames(&config(&[(0, 8)]), Path::new("x"));
        for frame in 0..5_u64 {
            feed.offer(&batch(frame * 100_000_000, &[(0, b"f")]));
        }
        let totals = stats.totals()[0];
        assert_eq!((totals.offered, totals.skipped), (5, 2));
        // The tap thread keeps the newest of the three and frees the others.
        assert_eq!(
            waiting(&mut sender),
            [Some((0, 200_000_000, b"f".to_vec()))]
        );
        assert_eq!(stats.totals()[0].superseded, 2);
        feed.offer(&batch(500_000_000, &[(0, b"g")]));
        assert_eq!(stats.totals()[0].skipped, 2);
    }

    #[test]
    fn a_frame_longer_than_its_slot_is_skipped_and_counted() {
        let (mut feed, mut sender, stats) = frames(&config(&[(0, 2)]), Path::new("x"));
        feed.offer(&batch(0, &[(0, b"long")]));
        assert_eq!(waiting(&mut sender), [None]);
        assert_eq!(stats.totals()[0].skipped, 1);
        feed.offer(&batch(100_000_000, &[(0, b"ok")]));
        assert_eq!(
            waiting(&mut sender),
            [Some((0, 100_000_000, b"ok".to_vec()))]
        );
    }

    #[test]
    fn the_slots_memory_is_three_frames_and_headers_per_camera() {
        assert_eq!(
            config(&[(0, 100), (1, 1000)]).memory_bytes(),
            3 * 120 + 3 * 1020
        );
    }

    /// Offers a frame of camera 0 every `period_ns` from `from_ns` until
    /// `to_ns`, the tap thread taking each, and says how many were offered.
    fn offer_every(
        (feed, sender): (&mut FrameFeed, &mut FrameSender),
        period_ns: u64,
        (from_ns, to_ns): (u64, u64),
    ) -> u64 {
        let before = sender.stats.totals()[0].offered;
        let mut at = from_ns;
        while at < to_ns {
            feed.offer(&batch(at, &[(0, b"f")]));
            sender.take_offered();
            let _ = sender.cameras[0]
                .waiting
                .take()
                .map(|slot| sender.cameras[0].free(slot));
            at += period_ns;
        }
        sender.stats.totals()[0].offered - before
    }

    const SECOND: u64 = 1_000_000_000;
    /// A camera at 30 frames a second.
    const PERIOD: u64 = SECOND / 30;

    #[test]
    fn the_writer_offers_frames_as_often_as_the_bridge_last_asked() {
        let (mut feed, mut sender, _) = frames(&config(&[(0, 8)]), Path::new("x"));
        // Before the bridge says anything, the cap: 30 a second.
        let offered = offer_every((&mut feed, &mut sender), PERIOD, (0, SECOND));
        assert!((29..=31).contains(&offered), "{offered}");
        // Five a second, as for the history with no page open.
        assert!(sender.heard(&wanted_record(0, Duration::from_millis(200))));
        let offered = offer_every((&mut feed, &mut sender), PERIOD, (SECOND, 3 * SECOND));
        assert!((9..=11).contains(&offered), "{offered}");
        // A page opens and wants 12 a second: from the next frame.
        assert!(sender.heard(&wanted_record(0, Duration::from_secs(1) / 12)));
        let offered = offer_every((&mut feed, &mut sender), PERIOD, (3 * SECOND, 5 * SECOND));
        assert!((23..=25).contains(&offered), "{offered}");
        // More than the cap allows is the cap.
        assert!(sender.heard(&wanted_record(0, Duration::ZERO)));
        let offered = offer_every((&mut feed, &mut sender), PERIOD, (5 * SECOND, 6 * SECOND));
        assert!((29..=31).contains(&offered), "{offered}");
        assert_eq!(sender.stats.totals()[0].skipped, 0);
    }

    #[test]
    fn a_page_opening_has_the_next_frame_not_one_a_history_interval_later() {
        let (mut feed, mut sender, _) = frames(&config(&[(0, 8)]), Path::new("x"));
        assert!(sender.heard(&wanted_record(0, Duration::from_millis(200))));
        // Offered at 0; the next would be due at 200 ms.
        assert_eq!(offer_every((&mut feed, &mut sender), PERIOD, (0, 1)), 1);
        assert!(sender.heard(&wanted_record(0, CAP)));
        assert_eq!(
            offer_every((&mut feed, &mut sender), PERIOD, (PERIOD, PERIOD + 1)),
            1
        );
    }

    #[test]
    fn with_the_bridge_gone_the_cap_alone_applies_again() {
        let (mut feed, mut sender, _) = frames(&config(&[(0, 8)]), Path::new("x"));
        assert!(sender.heard(&wanted_record(0, Duration::from_millis(200))));
        let offered = offer_every((&mut feed, &mut sender), PERIOD, (0, SECOND));
        assert!((4..=6).contains(&offered), "{offered}");
        sender.disconnect();
        let offered = offer_every((&mut feed, &mut sender), PERIOD, (SECOND, 2 * SECOND));
        assert!((29..=31).contains(&offered), "{offered}");
    }

    #[test]
    fn a_record_of_a_stream_that_isnt_a_camera_here_is_ignored() {
        let (_, mut sender, _) = frames(&config(&[(2, 8)]), Path::new("x"));
        assert!(sender.heard(&wanted_record(5, Duration::from_secs(1))));
        assert_eq!(sender.intervals[0].load(Ordering::Relaxed), CAP_NS);
    }

    proptest::proptest! {
        /// However the bridge's records are cut up by the reads, each sets
        /// its camera's interval once whole.
        #[test]
        fn records_split_across_reads_are_read_whole(
            wanted_ms in proptest::collection::vec((0_u16..3, 0_u64..2_000), 1..8),
            cuts in proptest::collection::vec(1_usize..40, 0..20),
        ) {
            let (_, mut sender, _) = frames(&config(&[(0, 8), (1, 8), (2, 8)]), Path::new("x"));
            let bytes: Vec<u8> = wanted_ms
                .iter()
                .flat_map(|(stream, ms)| wanted_record(*stream, Duration::from_millis(*ms)))
                .collect();
            let mut rest = &bytes[..];
            for cut in cuts.iter().chain(std::iter::repeat(&usize::MAX)) {
                if rest.is_empty() {
                    break;
                }
                let (read, left) = rest.split_at((*cut).min(rest.len()));
                proptest::prop_assert!(sender.heard(read));
                rest = left;
            }
            for camera in 0..3_u16 {
                let expected = wanted_ms
                    .iter()
                    .rev()
                    .find(|(stream, _)| *stream == camera)
                    .map_or(CAP.as_nanos(), |(_, ms)| {
                        Duration::from_millis(*ms).max(CAP).as_nanos()
                    });
                let interval = sender.intervals[usize::from(camera)].load(Ordering::Relaxed);
                proptest::prop_assert_eq!(u128::from(interval), expected);
            }
        }

        /// Bytes that aren't records are refused, never panicked on, and
        /// never set an interval shorter than the cap.
        #[test]
        fn anything_else_from_the_bridge_is_refused_without_panicking(
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..100),
        ) {
            let (_, mut sender, _) = frames(&config(&[(0, 8), (1, 8)]), Path::new("x"));
            let understood = sender.heard(&bytes);
            let records_ok = bytes
                .as_chunks::<WANTED>()
                .0
                .iter()
                .all(|record| read_wanted(record).is_some());
            proptest::prop_assert_eq!(understood, records_ok);
            for interval in sender.intervals.iter() {
                proptest::prop_assert!(interval.load(Ordering::Relaxed) >= CAP_NS);
            }
        }
    }

    #[test]
    fn a_wanted_record_is_read_back_and_one_of_another_version_is_not() {
        let mut record = wanted_record(7, Duration::from_millis(200));
        assert_eq!(read_wanted(&record), Some((7, Duration::from_millis(200))));
        record[4] = VERSION + 1;
        assert_eq!(read_wanted(&record), None);
        assert_eq!(read_wanted(&[0; WANTED]), None);
    }

    #[test]
    fn a_header_of_another_version_is_not_read() {
        let mut header = [0; HEADER];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = VERSION;
        header[16] = 3;
        assert_eq!(read_header(&header).map(|header| header.len), Some(3));
        header[4] = VERSION + 1;
        assert_eq!(read_header(&header), None);
    }
}
