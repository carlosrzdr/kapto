// SPDX-License-Identifier: Apache-2.0

//! Each camera's newest frame, handed from the writer thread to the tap
//! thread, which sends it whole to the viewer bridge's frame socket.
//!
//! A frame is far larger than a datagram, so frames have a Unix stream
//! socket of their own. Nothing here can make recording wait. The writer
//! copies a camera's newest frame into one of three slots set aside for it,
//! at most once per [`FrameConfig::min_interval`], and only into a slot the
//! tap thread has finished with; otherwise the frame is skipped and counted.
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
//! | 4 | 1 | version, 1 |
//! | 5 | 1 | 0, reserved |
//! | 6 | 2 | stream index, `u16`, its position in the stream declarations |
//! | 8 | 8 | receive time, `u64` nanoseconds of the host's monotonic clock |
//! | 16 | 4 | the message's length `n`, `u32` |
//! | 20 | n | the message, exactly as received |
//!
//! All numbers are little-endian. A reader that finds a version it doesn't
//! know can't tell where the next frame starts, and closes the connection.

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

/// The version this code writes.
pub const VERSION: u8 = 1;

/// The bytes before the message.
pub const HEADER: usize = 20;

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
        interval: u64::try_from(config.min_interval.as_nanos()).unwrap_or(u64::MAX),
        stats: Arc::clone(&stats),
    };
    let sender = FrameSender {
        cameras: sends.into_boxed_slice(),
        sending: None,
        path: socket.to_path_buf(),
        stream: None,
        last_attempt: None,
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
    /// The shortest time between two offers, in nanoseconds.
    interval: u64,
    stats: Arc<FrameStats>,
}

#[derive(Debug)]
struct FeedCamera {
    full: Producer<Slot>,
    free: Consumer<Slot>,
    /// When the next frame is due, in nanoseconds of the monotonic clock.
    due: u64,
    /// The position of its newest frame in the current batch.
    newest: usize,
}

impl FrameFeed {
    /// Offers each camera's newest frame in `batch`, if one is due. Never
    /// allocates or waits: with no free slot, the frame is skipped and
    /// counted.
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
            camera.newest = usize::MAX;
            // Frames a little early still count as due, so a camera at twice
            // the cap with some jitter is offered at the cap, not at a third
            // of its rate.
            if now.saturating_add(self.interval / 8) < camera.due {
                continue;
            }
            // The next one is due an interval after this one was, so the cap
            // holds over time; after a pause, it starts again from now.
            camera.due = if now > camera.due.saturating_add(self.interval) {
                now.saturating_add(self.interval)
            } else {
                camera.due.saturating_add(self.interval)
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
                    self.stream = None;
                    self.stats.cameras[camera]
                        .not_sent
                        .fetch_add(1, Ordering::Relaxed);
                    self.cameras[camera].free(slot);
                }
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
    /// Frames the writer offered, at most one per interval.
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

    const CAP: Duration = Duration::from_nanos(1_000_000_000 / 30);

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
