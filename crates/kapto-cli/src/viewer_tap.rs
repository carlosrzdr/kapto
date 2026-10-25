// SPDX-License-Identifier: Apache-2.0

//! The recording's live tap for the viewer: the `cap-tap` thread that
//! decodes every message the writer offers and, about sixty times a second,
//! sends each stream's newest values with their minimums and maximums since
//! the last send to the viewer's socket, keeping the newest values for
//! capture's status too. Between sends, it writes each camera's newest
//! frame to the viewer's frame socket. Lossy by design; see
//! `kapto_capture::tap`.

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kapto_capture::StreamIndex;
use kapto_capture::tap::frames::{FrameConfig, FrameSender, FrameStats, frames};
use kapto_capture::tap::{
    Decimator, LatestValues, TapConfig, TapDrain, TapFeed, TapSocket, TapStats, tap,
};
use kapto_config::StreamDeclaration;
use kapto_decode::{Decoder, Value};

/// How often samples are sent: a display's refresh rate.
const SEND_INTERVAL: Duration = Duration::from_millis(16);

/// The tap thread's work.
pub(crate) struct ViewerTap {
    drain: TapDrain,
    socket: TapSocket,
    /// Each stream's decoder; streams without one aren't sent.
    decoders: Vec<Option<Box<dyn Decoder>>>,
    /// Each stream's messages since the last send.
    decimator: Decimator,
    /// Each stream's newest values, as sent.
    latest: Arc<LatestValues>,
    /// Where the cameras' frames go, if any stream is a camera.
    frames: Option<FrameSender>,
}

/// What [`open`] prepares.
pub(crate) struct Opened {
    /// The writer's end.
    pub(crate) feed: TapFeed,
    /// The tap thread's work.
    pub(crate) tap: ViewerTap,
    /// Its counters.
    pub(crate) stats: Arc<TapStats>,
    /// Each stream's newest decoded values.
    pub(crate) latest: Arc<LatestValues>,
    /// The cameras' counters, if any stream is a camera.
    pub(crate) frames: Option<Arc<FrameStats>>,
}

/// Where and how often the cameras' frames are sent.
pub(crate) struct Cameras<'a> {
    /// Each camera's stream and largest frame.
    pub(crate) streams: &'a [(StreamIndex, usize)],
    /// The viewer's frame socket.
    pub(crate) socket: &'a Path,
    /// Frames a second, at most, of each camera.
    pub(crate) max_fps: u32,
}

/// Prepares the tap: the writer's end, the thread's work, the counters and
/// the newest values. Without a socket to send from, recording goes ahead
/// without a tap.
pub(crate) fn open(
    socket: &Path,
    declarations: &[StreamDeclaration],
    max_message_len: usize,
    cameras: &Cameras<'_>,
) -> Option<Opened> {
    let config = TapConfig {
        // The writer offers a few messages per stream between two sends;
        // this many per stream leaves room for the tap thread running late,
        // so the extremes it keeps rarely miss an offer.
        slots: (16 * declarations.len()).max(16),
        max_message_len,
        streams: declarations.len(),
    };
    let (mut feed, drain, stats) = tap(&config);
    let socket = match TapSocket::new(socket, Arc::clone(&stats)) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("warning: no live view: can't create a socket for the viewer: {error}\n");
            return None;
        }
    };
    let decoders = declarations
        .iter()
        .map(|declaration| {
            declaration
                .decoder
                .as_ref()
                .and_then(|decoder| decoder.compile().ok())
        })
        .collect();
    let latest = Arc::new(LatestValues::new(declarations.len()));
    let (sender, frame_stats) = if cameras.streams.is_empty() {
        (None, None)
    } else {
        let config = FrameConfig {
            cameras: cameras.streams.to_vec(),
            min_interval: Duration::from_secs(1) / cameras.max_fps.max(1),
        };
        eprintln!(
            "Live view: the newest frames of {}, up to {} a second each, through {} MiB set aside.",
            crate::counted(config.cameras.len(), "camera", "cameras"),
            cameras.max_fps,
            config.memory_bytes().div_ceil(1024 * 1024),
        );
        let (frame_feed, sender, frame_stats) = frames(&config, cameras.socket);
        feed.set_frames(frame_feed);
        (Some(sender), Some(frame_stats))
    };
    Some(Opened {
        feed,
        tap: ViewerTap {
            drain,
            socket,
            decoders,
            decimator: Decimator::new(declarations.len()),
            latest: Arc::clone(&latest),
            frames: sender,
        },
        stats,
        latest,
        frames: frame_stats,
    })
}

impl ViewerTap {
    /// Sends each stream's decoded values, and each camera's newest frame,
    /// until the writer has finished.
    pub(crate) fn run(mut self) {
        let mut values = Vec::new();
        let mut numbers = Vec::new();
        loop {
            let deadline = Instant::now() + SEND_INTERVAL;
            let finished = self.drain.is_finished();
            {
                let _span = kapto_core::span!("decode for the live view");
                let (decoders, decimator) = (&self.decoders, &mut self.decimator);
                self.drain.each(|stream, recv_time, bytes| {
                    let Some(Some(decoder)) = decoders.get(usize::from(stream)) else {
                        return;
                    };
                    values.resize(decoder.value_count(), Value::default());
                    // A message that doesn't decode isn't shown; recording it
                    // is the writer's job and already done.
                    if decoder.decode(bytes, &mut values).is_err() {
                        return;
                    }
                    numbers.clear();
                    // A value the message doesn't have is NaN: the decimator
                    // keeps the stream's last one instead.
                    numbers.extend(values.iter().map(|value| value.as_f64()));
                    decimator.add(stream, recv_time.as_nanos(), &numbers);
                });
            }
            {
                let _span = kapto_core::span!("send to the live view");
                let (socket, latest) = (&mut self.socket, &self.latest);
                self.decimator.take(|summary| {
                    socket.send(summary);
                    latest.set(usize::from(summary.stream), summary.values);
                });
            }
            // Frames get what is left of the interval; one the viewer hasn't
            // taken whole by then is finished next time.
            if let Some(frames) = &mut self.frames {
                let _span = kapto_core::span!("send frames to the live view");
                frames.send_until(deadline);
            }
            if finished {
                if let Some(frames) = self.frames.take() {
                    frames.finish();
                }
                return;
            }
            thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

/// The end-of-recording line about the live view.
pub(crate) fn summary(stats: &TapStats) -> String {
    format!(
        "Live view: {} samples sent to the viewer, {} dropped because it was slow, {} while no viewer listened, {} skipped",
        stats.sent(),
        stats.viewer_slow(),
        stats.no_viewer(),
        stats.skipped()
    )
}

/// The end-of-recording lines about each camera's frames, named by their
/// streams.
pub(crate) fn frames_summary(
    stats: &FrameStats,
    declarations: &[StreamDeclaration],
) -> Vec<String> {
    stats
        .totals()
        .iter()
        .map(|camera| {
            let name = declarations
                .get(usize::from(camera.stream))
                .map_or_else(|| camera.stream.to_string(), |declaration| declaration.id.to_string());
            format!(
                "Live view of {name}: {} of {} frames offered sent to the viewer, {} replaced by newer ones while it was slow, {} skipped, {} while no viewer listened",
                camera.sent, camera.offered, camera.superseded, camera.skipped, camera.not_sent
            )
        })
        .collect()
}
