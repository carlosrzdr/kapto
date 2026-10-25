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

/// How long a picture may wait due, untaken, before its page is called
/// slow: a page's thread looks for pictures between frames, or every few
/// tens of milliseconds without them.
const SLOW: Duration = Duration::from_millis(100);

/// What became of a picture offered to a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offered {
    /// It waits for its camera's share; nothing was waiting before.
    Waits,
    /// It replaced a picture of its camera waiting for its share: thinned.
    Thinned,
    /// It replaced a picture that had been due for longer than a page
    /// takes to look: the page is slow, and missed it.
    Dropped,
}

/// One page's pictures waiting for their camera's share.
#[derive(Debug)]
pub struct Waiting {
    interval: Duration,
    /// Each camera's newest, with when it was offered.
    newest: BTreeMap<u16, (Bytes, Instant)>,
    /// When each camera's next picture is due, once one was taken.
    due: BTreeMap<u16, Instant>,
}

impl Waiting {
    /// Nothing waiting, each camera's pictures `interval` apart.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            newest: BTreeMap::new(),
            due: BTreeMap::new(),
        }
    }

    /// The time between two pictures of one camera.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// From when camera `stream` may have its next picture taken: at once
    /// if none was yet.
    fn due_from(&self, stream: u16) -> Option<Instant> {
        self.due.get(&stream).copied()
    }

    /// Whether camera `stream` may have its next picture taken at `now`.
    /// A picture a little early counts as due: capture offers frames as
    /// often as the share and no more, each up to an eighth of an interval
    /// early, and a page that waited for each would miss one now and then.
    fn due(&self, stream: u16, now: Instant) -> bool {
        self.due_from(stream)
            .is_none_or(|due| now + self.interval / 8 >= due)
    }

    /// Whether camera `stream`'s share would take a picture offered now
    /// by `by`, when its next frame is expected: if not, a picture made of
    /// this frame would only be replaced by the next.
    #[must_use]
    pub fn wants(&self, stream: u16, by: Instant) -> bool {
        self.due(stream, by)
    }

    /// Keeps `picture` as camera `stream`'s newest at `now`, in place of any
    /// waiting.
    pub fn offer(&mut self, stream: u16, picture: Bytes, now: Instant) -> Offered {
        let due_from = self.due_from(stream);
        match self.newest.insert(stream, (picture, now)) {
            None => Offered::Waits,
            Some((_, offered)) => {
                // Due once offered and its camera's share allowed one.
                let due = due_from.map_or(offered, |due| due.max(offered));
                if now.saturating_duration_since(due) > SLOW {
                    Offered::Dropped
                } else {
                    Offered::Thinned
                }
            }
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
                // The next is due an interval after this one was, so the
                // share holds over time however late the page looks; after
                // a pause, an interval from now.
                let next = match self.due.get(&stream) {
                    Some(due) if now <= *due + self.interval => *due + self.interval,
                    _ => now + self.interval,
                };
                self.due.insert(stream, next);
                self.newest.remove(&stream).map(|(picture, _)| picture)
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
        assert_eq!(waiting.take_due(at(80)), Vec::<Bytes>::new());
        // Once due, the newest goes, even if no newer one comes: a camera
        // that stops still shows its last picture.
        assert_eq!(waiting.take_due(at(100)), [picture(3)]);
        assert_eq!(waiting.take_due(at(500)), Vec::<Bytes>::new());

        // Another camera has its own share.
        assert_eq!(waiting.offer(2, picture(20), at(110)), Offered::Waits);
        assert_eq!(waiting.take_due(at(110)), [picture(20)]);

        // A picture due and not yet taken is thinned if the next comes
        // before a page would have looked, and dropped if after: the page
        // is slow.
        assert_eq!(waiting.offer(1, picture(4), at(250)), Offered::Waits);
        assert_eq!(waiting.offer(1, picture(5), at(260)), Offered::Thinned);
        assert_eq!(waiting.offer(1, picture(6), at(400)), Offered::Dropped);
        assert_eq!(waiting.take_due(at(410)), [picture(6)]);
    }

    #[test]
    fn a_camera_offered_at_its_share_with_jitter_is_sent_every_picture() {
        let start = Instant::now();
        let mut waiting = Waiting::new(Duration::from_millis(100));
        let mut sent = 0;
        // Ten a second, each up to 12 ms early or late, as capture offers
        // them at the share's rate, the page looking 0 to 30 ms later.
        for picture in 0..100_u64 {
            let jitter = [0, 12, 5, 0, 7][usize::try_from(picture % 5).unwrap_or(0)];
            let offered = start + Duration::from_millis(picture * 100 + jitter);
            assert_ne!(
                waiting.offer(1, Bytes::from_static(b"p"), offered),
                Offered::Thinned,
                "picture {picture}"
            );
            let looked = offered + Duration::from_millis((picture * 7) % 31);
            sent += waiting.take_due(looked).len();
        }
        assert!(sent >= 99, "{sent} sent");
    }

    #[test]
    fn a_camera_is_wanted_only_if_its_share_takes_a_picture_before_its_next_frame() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut waiting = Waiting::new(Duration::from_millis(100));
        // Never sent: wanted at once.
        assert!(waiting.wants(1, at(0)));
        waiting.offer(1, Bytes::from_static(b"a"), at(0));
        waiting.take_due(at(0));
        // Next due at 100 ms: a frame at 50 ms whose next comes at 80 ms
        // isn't, one whose next comes at 110 ms is.
        assert!(!waiting.wants(1, at(80)));
        assert!(waiting.wants(1, at(110)));
        // Another camera has its own share.
        assert!(waiting.wants(2, at(10)));
    }
}
