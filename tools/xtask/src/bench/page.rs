// SPDX-License-Identifier: Apache-2.0

//! A page of the live view for `bench viewer`: a WebSocket client on a
//! thread of its own that reads the bridge's frames, or stops reading, and
//! keeps each frame's time in Kapto, read from the frame itself.

use std::net::TcpStream;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// What the page does with its connection.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Mode {
    /// Reads every frame and keeps its delay.
    Measuring,
    /// Reads every frame.
    Reading,
    /// Reads nothing, as a page that hangs, so the bridge's queue for it
    /// fills.
    NotReading,
}

/// A page watching the live view.
pub(crate) struct Page {
    mode: Arc<AtomicU8>,
    delays_ms: Arc<Mutex<Vec<f64>>>,
}

impl Page {
    /// Connects to the bridge's stream on `port` once it listens, within ten
    /// seconds, and starts measuring.
    pub(crate) fn connect(port: &str) -> Result<Self, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match tungstenite::connect(format!("ws://127.0.0.1:{port}/")) {
                Ok((socket, _)) => break socket,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(format!("the page couldn't connect: {error}")),
            }
        };
        // Reads time out, so the page notices it should stop reading while
        // the bridge sends nothing.
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(|error| error.to_string())?;
        }
        let page = Self {
            mode: Arc::new(AtomicU8::new(Mode::Measuring as u8)),
            delays_ms: Arc::default(),
        };
        let (mode, delays_ms) = (Arc::clone(&page.mode), Arc::clone(&page.delays_ms));
        std::thread::Builder::new()
            .name("bench-page".to_owned())
            .spawn(move || read_frames(&mut socket, &mode, &delays_ms))
            .map_err(|error| error.to_string())?;
        Ok(page)
    }

    pub(crate) fn set(&self, mode: Mode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    /// The delays kept while measuring, in milliseconds.
    pub(crate) fn delays_ms(&self) -> Vec<f64> {
        self.delays_ms
            .lock()
            .map(|delays| delays.clone())
            .unwrap_or_default()
    }
}

/// Reads frames until the connection ends.
fn read_frames(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    mode: &AtomicU8,
    delays_ms: &Mutex<Vec<f64>>,
) {
    loop {
        let now = mode.load(Ordering::Relaxed);
        if now == Mode::NotReading as u8 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        match socket.read() {
            Ok(Message::Binary(frame)) if now == Mode::Measuring as u8 => {
                if let (Some(delay), Ok(mut delays)) = (frame_delay_ms(&frame), delays_ms.lock()) {
                    delays.push(delay);
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

/// A frame's time in Kapto: from when the newest message it shows was
/// received to when the bridge sent it, both written in the frame.
pub(crate) fn frame_delay_ms(frame: &[u8]) -> Option<f64> {
    if frame.get(0..4)? != b"KVFR" {
        return None;
    }
    let time = |at: usize| -> Option<i64> {
        Some(i64::from_le_bytes(frame.get(at..at + 8)?.try_into().ok()?))
    };
    let (received, sent) = (time(8)?, time(16)?);
    #[allow(
        clippy::cast_precision_loss,
        reason = "nanoseconds shown as milliseconds"
    )]
    Some((sent - received) as f64 / 1e6)
}

/// The median, 95th percentile and longest of some delays, in milliseconds.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Delays {
    pub(crate) p50: f64,
    pub(crate) p95: f64,
    pub(crate) max: f64,
}

/// `None` without any delays.
pub(crate) fn percentiles(mut delays: Vec<f64>) -> Option<Delays> {
    delays.sort_by(f64::total_cmp);
    let at = |fraction: f64| -> Option<f64> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an index into the sorted delays"
        )]
        let index = ((delays.len().checked_sub(1)?) as f64 * fraction).round() as usize;
        delays.get(index).copied()
    };
    Some(Delays {
        p50: at(0.50)?,
        p95: at(0.95)?,
        max: *delays.last()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frames_delay_is_from_its_newest_message_received_to_it_sent() {
        let mut frame = b"KVFR\x01\x00\x01\x00".to_vec();
        frame.extend(1_000_000_000_i64.to_le_bytes());
        frame.extend(1_002_500_000_i64.to_le_bytes());
        frame.extend([0; 8]);
        assert_eq!(frame_delay_ms(&frame), Some(2.5));
        assert_eq!(frame_delay_ms(&frame[..20]), None, "cut short");
        frame[0] = b'X';
        assert_eq!(frame_delay_ms(&frame), None, "not a frame");
    }

    #[test]
    fn percentiles_come_from_the_sorted_delays_and_none_without_any() {
        let delays = percentiles((1..=100).rev().map(f64::from).collect()).unwrap();
        assert_eq!(
            delays,
            Delays {
                p50: 51.0,
                p95: 95.0,
                max: 100.0
            }
        );
        assert!(percentiles(Vec::new()).is_none());
    }
}
