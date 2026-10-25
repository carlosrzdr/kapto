// SPDX-License-Identifier: Apache-2.0

//! A page's share of cameras' pictures. The bridge sends each page at most
//! so many pictures a second across all its cameras, each an even share no
//! more than each camera's own cap, so more cameras mean fewer pictures of
//! each rather than the robot's frames waiting behind them on the page's
//! connection. Until a camera's share allows another picture only its
//! newest waits, the page always getting the newest and never a backlog.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use tungstenite::Bytes;

/// How many pictures a page is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageShare {
    /// Pictures a second for all of a page's cameras together.
    pub pictures_per_second: u32,
    /// Pictures a second, at most, of any one camera.
    pub camera_max_fps: u32,
}

impl Default for PageShare {
    fn default() -> Self {
        Self {
            pictures_per_second: 60,
            camera_max_fps: 30,
        }
    }
}

impl PageShare {
    /// The time between two pictures of one camera to a page that shows
    /// `cameras`: an even share, never above the camera's own cap.
    #[must_use]
    pub fn interval(self, cameras: usize) -> Duration {
        let cameras = u32::try_from(cameras.max(1)).unwrap_or(u32::MAX);
        let each = (self.pictures_per_second / cameras)
            .min(self.camera_max_fps)
            .max(1);
        Duration::from_secs(1) / each
    }
}

/// What became of a picture offered to a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    /// It waits for its camera's share; nothing was waiting before.
    Waits,
    /// It replaced a picture of its camera waiting for its share: thinned.
    Thinned,
    /// It replaced a picture already due that the page hadn't taken: the
    /// page is slow, and missed it.
    Dropped,
}

/// One page's pictures waiting for their camera's share.
#[derive(Debug)]
pub struct Waiting {
    interval: Duration,
    newest: BTreeMap<u16, Bytes>,
    /// When each camera's last picture was taken for the page.
    sent: BTreeMap<u16, Instant>,
}

impl Waiting {
    /// Nothing waiting, each camera's pictures `interval` apart.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            newest: BTreeMap::new(),
            sent: BTreeMap::new(),
        }
    }

    /// Whether camera `stream` may have its next picture taken at `now`.
    fn due(&self, stream: u16, now: Instant) -> bool {
        self.sent
            .get(&stream)
            .is_none_or(|sent| now.saturating_duration_since(*sent) >= self.interval)
    }

    /// Keeps `picture` as camera `stream`'s newest at `now`, in place of any
    /// waiting.
    pub fn offer(&mut self, stream: u16, picture: Bytes, now: Instant) -> Offered {
        match self.newest.insert(stream, picture) {
            None => Offered::Waits,
            Some(_) if self.due(stream, now) => Offered::Dropped,
            Some(_) => Offered::Thinned,
        }
    }

    /// The pictures whose camera's share allows one at `now`, taken.
    pub fn take_due(&mut self, now: Instant) -> Vec<Bytes> {
        let due: Vec<u16> = self
            .newest
            .keys()
            .copied()
            .filter(|stream| self.due(*stream, now))
            .collect();
        due.into_iter()
            .filter_map(|stream| {
                self.sent.insert(stream, now);
                self.newest.remove(&stream)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pages_pictures_are_shared_evenly_among_its_cameras_within_each_cap() {
        let share = PageShare::default();
        let fps = |cameras| 1.0 / share.interval(cameras).as_secs_f64();
        assert!((fps(1) - 30.0).abs() < 0.01, "one camera keeps its own cap");
        assert!((fps(2) - 30.0).abs() < 0.01);
        assert!((fps(5) - 12.0).abs() < 0.01);
        assert!((fps(0) - 30.0).abs() < 0.01);
        // Never none: a page with more cameras than pictures still sees each.
        assert!((fps(100) - 1.0).abs() < 0.01);
    }

    #[test]
    fn only_each_cameras_newest_waits_for_its_share_and_is_sent_when_due() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut waiting = Waiting::new(Duration::from_millis(100));
        let picture = |n: u8| Bytes::from(vec![n]);

        // The first picture of a camera goes at once.
        assert_eq!(waiting.offer(1, picture(1), at(0)), Offered::Waits);
        assert_eq!(waiting.take_due(at(0)), [picture(1)]);
        // Within its interval, newer pictures replace each other: thinned.
        assert_eq!(waiting.offer(1, picture(2), at(30)), Offered::Waits);
        assert_eq!(waiting.offer(1, picture(3), at(60)), Offered::Thinned);
        assert_eq!(waiting.take_due(at(90)), Vec::<Bytes>::new());
        // Once due, the newest goes, even if no newer one comes: a camera
        // that stops still shows its last picture.
        assert_eq!(waiting.take_due(at(100)), [picture(3)]);
        assert_eq!(waiting.take_due(at(500)), Vec::<Bytes>::new());

        // Another camera has its own share.
        assert_eq!(waiting.offer(2, picture(20), at(110)), Offered::Waits);
        assert_eq!(waiting.take_due(at(110)), [picture(20)]);

        // A picture due but not taken, as a slow page leaves it, is dropped
        // when the next replaces it.
        assert_eq!(waiting.offer(1, picture(4), at(250)), Offered::Waits);
        assert_eq!(waiting.offer(1, picture(5), at(260)), Offered::Dropped);
        assert_eq!(waiting.take_due(at(270)), [picture(5)]);
    }
}
