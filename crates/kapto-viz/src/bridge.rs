// SPDX-License-Identifier: Apache-2.0

//! The viewer bridge: live samples in from the recording's tap, the
//! robot's poses and every decoded value out to the web UI's live view, as
//! a stream of frames, with what it streamed kept for pages looking back;
//! and cameras' frames in from the recording's frame socket, their
//! pictures out to the same pages.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kapto_core::clock::monotonic_now;
use kapto_core::{ClockSample, ErrorCode, KaptoError};
use kapto_robot::RobotModel;

use crate::camera_frames::{self, CameraStats, CameraTotals};
use crate::camera_history::CameraHistorySettings;
use crate::history::History;
use crate::live::LiveRobot;
use crate::marks::{Marks, watch_capture};
use crate::page_pictures::PageShare;
use crate::picture::CameraKind;
use crate::plots::Plots;
use crate::stream::{Frame, FrameStream, Opening, OpeningStream, STREAM_VERSION, StreamStats};
use crate::tap_reader::{TapReader, TapReaderStats};

/// How often the bridge reads the monotonic and wall clocks together.
const CLOCK_EVERY: Duration = Duration::from_secs(1);

/// Where and how the bridge runs.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeSettings {
    /// Address the stream is served on.
    pub bind: IpAddr,
    /// Port the robot's poses and plotted values are streamed on, to the
    /// web UI's live view.
    pub stream_port: u16,
    /// Port of Kapto's web UI, the only page the stream is sent to.
    pub ui_port: u16,
    /// The recording's live tap socket.
    pub tap_socket: PathBuf,
    /// The recording's frame socket, where cameras' frames come; none
    /// listened on without it, or without cameras.
    pub frame_socket: Option<PathBuf>,
    /// The streams that are cameras: each one's position among the streams
    /// and how its frames are laid out.
    pub cameras: Vec<(u16, CameraKind)>,
    /// The capture process's control socket, watched for events and
    /// episodes to show on the timeline; none shown without it.
    pub control_socket: Option<PathBuf>,
    /// How often the robot's pose is updated: a display's refresh interval.
    pub display_interval: Duration,
    /// How much of what it streamed the bridge keeps for pages looking
    /// back, in bytes. The oldest goes first.
    pub history_bytes: u64,
    /// How far back the bridge keeps what it streamed, for pages looking
    /// back.
    pub history_age: Duration,
    /// What it keeps of cameras' pictures for pages looking back; kept as
    /// long as `history_age`.
    pub camera_history: CameraHistorySettings,
    /// How many cameras' pictures a second each page is sent, shared among
    /// its cameras.
    pub page_share: PageShare,
}

impl BridgeSettings {
    /// The defaults for everything but the addresses and the socket.
    #[must_use]
    pub fn new(bind: IpAddr, (stream_port, ui_port): (u16, u16), tap_socket: PathBuf) -> Self {
        Self {
            bind,
            stream_port,
            ui_port,
            tap_socket,
            frame_socket: None,
            cameras: Vec::new(),
            control_socket: None,
            display_interval: Duration::from_millis(16),
            history_bytes: 64 * 1024 * 1024,
            history_age: Duration::from_secs(600),
            camera_history: CameraHistorySettings::default(),
            page_share: PageShare::default(),
        }
    }
}

/// A running bridge.
pub struct Bridge {
    stream: FrameStream,
    reader: TapReader,
    live: LiveRobot,
    display_interval: Duration,
    /// When the clocks were last read together.
    clock_read: Instant,
    /// Set when the bridge stops, so watching capture and reading its
    /// frames stop too.
    stop_watching: Arc<AtomicBool>,
    /// What became of cameras' frames, if any stream is a camera.
    cameras: Option<Arc<CameraStats>>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop_watching.store(true, Ordering::Relaxed);
    }
}

impl Bridge {
    /// Starts serving: the stream is up and the tap socket is listened on.
    /// A page is told the robot's links and each stream's values, then the
    /// robot's poses and the values as they arrive.
    ///
    /// # Errors
    ///
    /// `viewer.listen_failed` if the stream's port can't be listened on,
    /// `viewer.tap_socket_failed` if the tap socket can't be,
    /// `viewer.frame_socket_failed` if the frame socket can't be.
    pub fn start(
        settings: &BridgeSettings,
        robot: &RobotModel,
        live: LiveRobot,
        plots: &Plots,
    ) -> Result<Self, KaptoError> {
        let opening = Opening {
            version: STREAM_VERSION,
            links: robot.links.iter().map(|link| link.name.clone()).collect(),
            streams: OpeningStream::of(plots),
            history_s: settings.history_age.as_secs(),
            cameras: settings.cameras.iter().map(|(stream, _)| *stream).collect(),
        };
        let age_ns = i64::try_from(settings.history_age.as_nanos()).unwrap_or(i64::MAX);
        let history = History::new(
            age_ns,
            usize::try_from(settings.history_bytes).unwrap_or(usize::MAX),
        );
        let stream = FrameStream::start(
            (settings.bind, settings.stream_port, settings.ui_port),
            &opening,
            history,
            Marks::new(age_ns),
        )
        .map_err(|error| {
            listen_failed(
                SocketAddr::new(settings.bind, settings.stream_port),
                &error.to_string(),
            )
        })?;
        stream.keep_pictures_by(CameraHistorySettings {
            age_ns,
            ..settings.camera_history
        });
        stream.share_pictures_by(settings.page_share);
        let stop_watching = watch_marks(settings.control_socket.as_ref(), &stream)?;
        let reader = TapReader::bind(&settings.tap_socket).map_err(|error| {
            KaptoError::new(ErrorCode::ViewerTapSocketFailed)
                .detail(error.to_string())
                .context("socket", settings.tap_socket.display().to_string())
        })?;
        let cameras = match &settings.frame_socket {
            Some(path) if !settings.cameras.is_empty() => Some(
                camera_frames::listen(
                    path,
                    settings.cameras.clone(),
                    stream.photographer(),
                    Arc::clone(&stop_watching),
                )
                .map_err(|error| {
                    // The bridge won't start, so nothing else would stop
                    // watching capture.
                    stop_watching.store(true, Ordering::Relaxed);
                    KaptoError::new(ErrorCode::ViewerFrameSocketFailed)
                        .detail(error.to_string())
                        .context("socket", path.display().to_string())
                })?,
            ),
            _ => None,
        };
        let mut bridge = Self {
            stream,
            reader,
            live,
            display_interval: settings.display_interval,
            clock_read: Instant::now(),
            stop_watching,
            cameras,
        };
        // Show the robot at rest straight away.
        bridge.step()?;
        Ok(bridge)
    }

    /// What the bridge has read from the tap so far.
    #[must_use]
    pub fn tap_stats(&self) -> TapReaderStats {
        self.reader.stats()
    }

    /// What became of cameras' frames so far, if any stream is a camera.
    #[must_use]
    pub fn camera_totals(&self) -> Option<CameraTotals> {
        self.cameras.as_ref().map(|stats| stats.totals())
    }

    /// Where the robot's poses and plotted values are streamed.
    #[must_use]
    pub fn stream_address(&self) -> SocketAddr {
        self.stream.address()
    }

    /// How the stream to the web UI's live view has fared so far.
    #[must_use]
    pub fn stream_stats(&self) -> StreamStats {
        self.stream.stats()
    }

    /// Updates the pages as samples arrive from the tap, until `stop` is
    /// set. It waits on the tap itself, not on a timer of its own, so what
    /// arrives goes out at once; the tap paces it at the display's rate.
    ///
    /// # Errors
    ///
    /// `viewer.tap_socket_failed` if the tap socket stops working.
    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), KaptoError> {
        while !stop.load(Ordering::Relaxed) {
            self.update(Some(self.display_interval))?;
        }
        Ok(())
    }

    /// Takes every sample waiting on the tap: keeps the newest value of
    /// each joint, works out the robot's pose once if it moved, and streams
    /// all of it to the web UI's pages as one frame. Returns how many
    /// samples were read.
    ///
    /// # Errors
    ///
    /// As [`Bridge::run`].
    pub fn step(&mut self) -> Result<usize, KaptoError> {
        self.update(None)
    }

    /// [`Bridge::step`], first waiting up to `wait` for a sample if given.
    fn update(&mut self, wait: Option<Duration>) -> Result<usize, KaptoError> {
        // The wall clock moves against the monotonic one when the machine
        // sleeps (the monotonic clock stops) or the time is set: read both
        // again now and then, as capture does, so receive times stay on
        // the same clock as events and the page.
        if self.clock_read.elapsed() >= CLOCK_EVERY {
            let clock = ClockSample::now();
            self.live.set_clock(clock);
            self.clock_read = Instant::now();
        }
        let live = &mut self.live;
        let offset = live.clock().offset_ns();
        let mut frame = Frame::default();
        let mut each = |sample: &_| {
            live.apply(sample);
            frame.sample(sample, offset);
        };
        let read = match wait {
            Some(wait) => self.reader.drain_waiting(wait, &mut each),
            None => self.reader.drain(&mut each),
        };
        let samples = read.map_err(|error| {
            KaptoError::new(ErrorCode::ViewerTapSocketFailed)
                .detail(error.to_string())
                .context("socket", self.reader.path().display().to_string())
        })?;
        let moved = self.live.advance();
        if moved.is_some() {
            frame.poses(self.live.poses());
        }
        if !frame.is_empty() {
            let utc = |mono: u64| {
                i64::try_from(mono)
                    .unwrap_or(i64::MAX)
                    .saturating_add(offset)
            };
            let sent = utc(monotonic_now().as_nanos());
            let received = moved.map_or(i64::MIN, utc);
            let (frame, record) = frame.finish(received, sent);
            self.stream.send(&frame, record);
        }
        Ok(samples)
    }
}

/// Has the events and episodes capture announces on `control_socket`, if
/// given, told to the pages of `stream`. Returns what stops watching.
fn watch_marks(
    control_socket: Option<&PathBuf>,
    stream: &FrameStream,
) -> Result<Arc<AtomicBool>, KaptoError> {
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(socket) = control_socket {
        let marker = stream.marker();
        watch_capture(socket.clone(), Arc::clone(&stop), move |mark| {
            marker.mark(mark);
        })
        .map_err(|error| {
            KaptoError::new(ErrorCode::CaptureThreadFailed).detail(error.to_string())
        })?;
    }
    Ok(stop)
}

fn listen_failed(address: SocketAddr, reason: &str) -> KaptoError {
    KaptoError::new(ErrorCode::ViewerListenFailed)
        .detail(reason.to_owned())
        .context("address", address.to_string())
}
