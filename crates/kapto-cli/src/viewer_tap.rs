// SPDX-License-Identifier: Apache-2.0

//! The recording's live tap for the viewer: the `cap-tap` thread that
//! decodes every message the writer offers and, about sixty times a second,
//! sends each stream's newest values with their minimums and maximums since
//! the last send to the viewer's socket, keeping the newest values for
//! capture's status too. Lossy by design; see `kapto_capture::tap`.

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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
}

/// Prepares the tap: the writer's end, the thread's work, the counters and
/// the newest values. Without a socket to send from, recording goes ahead
/// without a tap.
pub(crate) fn open(
    socket: &Path,
    declarations: &[StreamDeclaration],
    max_message_len: usize,
) -> Option<Opened> {
    let config = TapConfig {
        // The writer offers a few messages per stream between two sends;
        // this many per stream leaves room for the tap thread running late,
        // so the extremes it keeps rarely miss an offer.
        slots: (16 * declarations.len()).max(16),
        max_message_len,
        streams: declarations.len(),
    };
    let (feed, drain, stats) = tap(&config);
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
    Some(Opened {
        feed,
        tap: ViewerTap {
            drain,
            socket,
            decoders,
            decimator: Decimator::new(declarations.len()),
            latest: Arc::clone(&latest),
        },
        stats,
        latest,
    })
}

impl ViewerTap {
    /// Sends each stream's decoded values until the writer has finished.
    pub(crate) fn run(mut self) {
        let mut values = Vec::new();
        let mut numbers = Vec::new();
        loop {
            let finished = self.drain.is_finished();
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
            let (socket, latest) = (&mut self.socket, &self.latest);
            self.decimator.take(|summary| {
                socket.send(summary);
                latest.set(usize::from(summary.stream), summary.values);
            });
            if finished {
                return;
            }
            thread::sleep(SEND_INTERVAL);
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
