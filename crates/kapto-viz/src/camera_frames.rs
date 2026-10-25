// SPDX-License-Identifier: Apache-2.0

//! Reading cameras' frames from the recording's frame socket, on a thread
//! of its own (`viz-cameras`): each frame's picture taken out, a raw one
//! encoded as JPEG, and handed to the stream for every page.
//!
//! The frames come one at a time over a Unix stream socket, in the format
//! `kapto_capture::tap::frames` writes. Encoding a raw picture is the slow
//! part, so it is done only when a page or the camera's history wants the
//! picture, as each frame is read: while it is, capture keeps only each
//! camera's newest frame waiting, so the pictures shown stay current
//! however slow the encoding is. One thread encodes for every camera: on a
//! machine shared with the recording and the browser, several encoding at
//! once each took three times as long.
//!
//! The thread also tells capture, on the same connection, how often it
//! wants each camera's frames: as often as the history keeps them with no
//! page open, as often as the page wanting most shows them otherwise, and
//! again whenever that changes. Capture then copies and sends no frame
//! nothing here would use.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::tap::frames::{HEADER, read_header, wanted_record};
use kapto_config::MAX_MESSAGE_BYTES;
use kapto_core::ClockSample;
use kapto_core::clock::monotonic_now;

use crate::picture::{CameraKind, PictureFormat, Pictures};
use crate::stream::{Photographer, picture_message};
use crate::tap_reader::prepare_socket_path;

/// How often the thread looks whether the bridge is stopping, while it
/// waits for capture to connect or to send.
const POLL: Duration = Duration::from_millis(100);
/// How often the monotonic and wall clocks are read together again.
const CLOCK_EVERY: Duration = Duration::from_secs(1);
/// The longest a camera's next frame is expected after its last: a camera
/// slower than this is wanted as if its next frame came then.
const LONGEST_EXPECTED: Duration = Duration::from_secs(1);

/// What the bridge has done with cameras' frames, readable from any
/// thread.
#[derive(Debug, Default)]
pub struct CameraStats {
    frames: AtomicU64,
    shown: AtomicU64,
    unshown: AtomicU64,
    unreadable: AtomicU64,
    not_encoded: AtomicU64,
}

/// A snapshot of [`CameraStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CameraTotals {
    /// Frames read from capture.
    pub frames: u64,
    /// Pictures handed to the pages.
    pub shown: u64,
    /// Frames whose picture can't be shown, each camera's reason told to
    /// the pages.
    pub unshown: u64,
    /// Connections dropped because what came wasn't a frame, or was one of
    /// a stream that isn't a camera.
    pub unreadable: u64,
    /// Raw frames not encoded: no page's share, nor the camera's history,
    /// wanted their picture.
    pub not_encoded: u64,
}

impl CameraStats {
    /// The counts so far.
    #[must_use]
    pub fn totals(&self) -> CameraTotals {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        CameraTotals {
            frames: read(&self.frames),
            shown: read(&self.shown),
            unshown: read(&self.unshown),
            unreadable: read(&self.unreadable),
            not_encoded: read(&self.not_encoded),
        }
    }
}

/// Listens on `path` for capture's frames of `cameras`, each a stream's
/// position and how its frames are laid out, and hands their pictures to
/// `photographer` until `stop` is set.
///
/// # Errors
///
/// `path` can't be listened on: too long, taken by a file that isn't a
/// socket, or in a folder that can't be made.
pub fn listen(
    path: &Path,
    cameras: Vec<(u16, CameraKind)>,
    photographer: Photographer,
    stop: Arc<AtomicBool>,
) -> io::Result<Arc<CameraStats>> {
    prepare_socket_path(path, "viewer.frame_socket")?;
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    let stats = Arc::new(CameraStats::default());
    let mut reader = Reader {
        cameras,
        last_frame: BTreeMap::new(),
        photographer,
        pictures: Pictures::default(),
        buffer: Vec::new(),
        told: Vec::new(),
        clock: ClockSample::now(),
        clock_read: Instant::now(),
        stats: Arc::clone(&stats),
        stop,
        path: path.to_path_buf(),
    };
    thread::Builder::new()
        .name("viz-cameras".into())
        .spawn(move || reader.run(&listener))?;
    Ok(stats)
}

struct Reader {
    cameras: Vec<(u16, CameraKind)>,
    /// When each camera's last frame was received, in nanoseconds of the
    /// monotonic clock.
    last_frame: BTreeMap<u16, u64>,
    photographer: Photographer,
    pictures: Pictures,
    /// The frame being read, reused.
    buffer: Vec<u8>,
    /// How often this connection's capture was last told the bridge wants
    /// each camera's frames, in the order of `cameras`.
    told: Vec<Option<Duration>>,
    clock: ClockSample,
    clock_read: Instant,
    stats: Arc<CameraStats>,
    stop: Arc<AtomicBool>,
    path: PathBuf,
}

impl Reader {
    /// Takes capture's connections one after another until the bridge
    /// stops, then removes the socket file.
    fn run(&mut self, listener: &UnixListener) {
        while !self.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((connection, _)) => self.read(connection),
                Err(_) => thread::sleep(POLL),
            }
        }
        // Nothing to do about a failure: the next run replaces the file.
        let _ = std::fs::remove_file(&self.path);
    }

    /// Reads frames from one connection until capture closes it, sends
    /// something that isn't a frame, or the bridge stops.
    fn read(&mut self, mut connection: UnixStream) {
        if connection.set_nonblocking(false).is_err()
            || connection.set_read_timeout(Some(POLL)).is_err()
            || connection.set_write_timeout(Some(POLL)).is_err()
        {
            return;
        }
        self.told.clear();
        self.told.resize(self.cameras.len(), None);
        if !self.tell(&mut connection) {
            return;
        }
        let mut header = [0; HEADER];
        loop {
            if !self.fill(&mut connection, &mut header) {
                return;
            }
            let Some(frame) = read_header(&header) else {
                self.stats.unreadable.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let kind = self
                .cameras
                .iter()
                .find(|(stream, _)| *stream == frame.stream)
                .map(|(_, kind)| *kind);
            let too_long =
                u64::try_from(frame.len).unwrap_or(u64::MAX) > u64::from(MAX_MESSAGE_BYTES);
            let Some(kind) = kind.filter(|_| !too_long) else {
                self.stats.unreadable.fetch_add(1, Ordering::Relaxed);
                return;
            };
            let mut buffer = std::mem::take(&mut self.buffer);
            buffer.resize(frame.len, 0);
            let read = self.fill(&mut connection, &mut buffer);
            if read {
                self.stats.frames.fetch_add(1, Ordering::Relaxed);
                self.hand(frame.stream, kind, frame.recv_time.as_nanos(), &buffer);
            }
            self.buffer = buffer;
            if !read || !self.tell(&mut connection) {
                return;
            }
        }
    }

    /// Tells capture how often the bridge now wants each camera's frames,
    /// if that changed since it was last told. `false` if capture has gone.
    fn tell(&mut self, connection: &mut UnixStream) -> bool {
        let wanted = self.photographer.wanted_interval();
        for ((stream, _), told) in self.cameras.iter().zip(&mut self.told) {
            if *told == Some(wanted) {
                continue;
            }
            if connection
                .write_all(&wanted_record(*stream, wanted))
                .is_err()
            {
                return false;
            }
            *told = Some(wanted);
        }
        true
    }

    /// Shows camera `stream`'s frame received at `recv_time_ns`: a
    /// compressed one always, as passing it on costs nothing; a raw one only
    /// if a page's share wants its picture, as encoding it is costly, and
    /// for looking back, if one is due, made small from its pixels.
    fn hand(&mut self, stream: u16, kind: CameraKind, recv_time_ns: u64, frame: &[u8]) {
        if !matches!(kind, CameraKind::Raw { .. } | CameraKind::Image) {
            self.show(stream, kind, recv_time_ns, frame);
            return;
        }
        // Its next frame is expected as long after as this one came after
        // the last.
        let last = self.last_frame.insert(stream, recv_time_ns);
        let ahead = last.map_or(Duration::ZERO, |last| {
            Duration::from_nanos(recv_time_ns.saturating_sub(last)).min(LONGEST_EXPECTED)
        });
        let received = i64::try_from(recv_time_ns)
            .unwrap_or(i64::MAX)
            .saturating_add(self.clock.offset_ns());
        let page = self.photographer.page_wants(stream, ahead);
        let look_back = self.photographer.look_back_due(stream, received);
        if page {
            self.show(stream, kind, recv_time_ns, frame);
        }
        let Some(width) = look_back else {
            if !page {
                self.stats.not_encoded.fetch_add(1, Ordering::Relaxed);
            }
            return;
        };
        match self.pictures.small(&kind, frame, width) {
            Ok(Some(small)) => self.photographer.keep(stream, received, &small),
            Err(why) if !page => {
                self.photographer.unshown(stream, Some(&why.to_string()));
                self.stats.unshown.fetch_add(1, Ordering::Relaxed);
            }
            // Told already, as a page wanted it.
            Ok(None) | Err(_) => {}
        }
    }

    /// Fills `into` from `connection`, waiting as long as it takes unless
    /// the bridge stops, and telling capture meanwhile when the frames
    /// wanted change. `false` if it closed or the bridge is stopping.
    fn fill(&mut self, connection: &mut UnixStream, into: &mut [u8]) -> bool {
        let mut filled = 0;
        while filled < into.len() {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            match connection.read(&mut into[filled..]) {
                Ok(0) => return false,
                Ok(read) => filled += read,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    // A page that opens while a camera's frames come only
                    // for the history is told within a poll.
                    if !self.tell(connection) {
                        return false;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }

    /// Hands the picture in `frame`, camera `stream`'s received at
    /// `recv_time_ns` of the monotonic clock, to the pages, or tells them
    /// why it can't be shown.
    fn show(&mut self, stream: u16, kind: CameraKind, recv_time_ns: u64, frame: &[u8]) {
        // The wall clock moves against the monotonic one when the machine
        // sleeps or the time is set, so they are read together now and then.
        if self.clock_read.elapsed() >= CLOCK_EVERY {
            self.clock = ClockSample::now();
            self.clock_read = Instant::now();
        }
        let offset = self.clock.offset_ns();
        let utc = |mono: u64| {
            i64::try_from(mono)
                .unwrap_or(i64::MAX)
                .saturating_add(offset)
        };
        match self.pictures.picture(&kind, frame) {
            Ok(picture) => {
                let sent = utc(monotonic_now().as_nanos());
                let message = picture_message(stream, &picture, utc(recv_time_ns), sent);
                self.photographer.picture(stream, &message);
                // A PNG isn't kept: cameras send JPEG. A raw picture is
                // kept made small from its pixels instead.
                if picture.format == PictureFormat::Jpeg && picture.size.is_none() {
                    self.photographer
                        .remember(stream, utc(recv_time_ns), picture.bytes);
                }
                self.photographer.unshown(stream, None);
                self.stats.shown.fetch_add(1, Ordering::Relaxed);
            }
            Err(why) => {
                self.photographer.unshown(stream, Some(&why.to_string()));
                self.stats.unshown.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
