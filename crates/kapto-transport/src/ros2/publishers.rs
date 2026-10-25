// SPDX-License-Identifier: Apache-2.0

//! Telling apart a topic's publishers by the IDs their messages carry, so
//! each publisher's count of its messages can be followed on its own.

use kapto_capture::loss::PUBLISHERS;

/// A topic's publishers, by the ID the middleware gives each, in the place
/// each was given; made with the subscription, so taking never allocates.
#[derive(Default)]
pub(super) struct Publishers {
    ids: [[u8; 16]; PUBLISHERS],
    /// When each place's publisher was last heard, by `heard`'s count.
    last: [u64; PUBLISHERS],
    known: usize,
    /// Messages taken so far, counting when each publisher was last heard.
    heard: u64,
}

impl Publishers {
    /// The publisher with `id`'s place, and whether it took another's, a new
    /// one taking the next free, or
    /// once [`PUBLISHERS`] are known, the place of the one heard from least
    /// recently: most likely one that restarted under a new ID and is gone.
    /// Its count starting again from 1 there is taken as a restart, not as
    /// loss, so the topic keeps being counted by its publishers' numbers
    /// however often they restart.
    pub(super) fn index(&mut self, id: &[u8; 16]) -> Option<(u8, bool)> {
        self.heard += 1;
        let (place, replaced) = match self.ids[..self.known].iter().position(|known| known == id) {
            Some(place) => (place, false),
            None if self.known < PUBLISHERS => {
                self.known += 1;
                (self.known - 1, false)
            }
            None => ((0..PUBLISHERS).min_by_key(|&place| self.last[place])?, true),
        };
        self.ids[place] = *id;
        self.last[place] = self.heard;
        u8::try_from(place).ok().map(|place| (place, replaced))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_publisher_keeps_its_place_and_a_new_one_once_all_are_taken_takes_the_least_recently_heard()
     {
        let mut publishers = Publishers::default();
        let id = |byte: u8| [byte; 16];
        assert_eq!(publishers.index(&id(7)), Some((0, false)));
        assert_eq!(publishers.index(&id(3)), Some((1, false)));
        assert_eq!(publishers.index(&id(7)), Some((0, false)));
        for byte in 10..16 {
            assert_eq!(
                publishers.index(&id(byte)).map(|(_, replaced)| replaced),
                Some(false)
            );
        }
        // Every place taken: 3 is heard from again, so 7 is the least recent.
        assert_eq!(publishers.index(&id(3)), Some((1, false)));
        assert_eq!(
            publishers.index(&id(99)),
            Some((0, true)),
            "a ninth takes 7's place, and says so"
        );
        assert_eq!(publishers.index(&id(99)), Some((0, false)));
        assert_eq!(
            publishers.index(&id(3)),
            Some((1, false)),
            "the others keep theirs"
        );
    }

    #[test]
    fn publishers_restarting_again_and_again_are_always_told_apart() {
        // Each restart a new ID: never left without a place.
        let mut publishers = Publishers::default();
        for restart in 0..100_u8 {
            assert!(
                publishers.index(&[restart; 16]).is_some(),
                "restart {restart}"
            );
        }
    }
}
