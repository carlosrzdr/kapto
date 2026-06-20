// SPDX-License-Identifier: Apache-2.0

//! Telling apart a topic's publishers by the IDs their messages carry, so
//! each publisher's count of its messages can be followed on its own.

/// How many publishers of one topic are told apart by their counts: a
/// message from any further one has none, and its stream's loss is
/// counted by its rate.
const PUBLISHERS: usize = 8;

/// A topic's publishers, by the ID the middleware gives each, in the order
/// first heard; made with the subscription, so taking never allocates.
#[derive(Default)]
pub(super) struct Publishers {
    ids: [[u8; 16]; PUBLISHERS],
    known: usize,
}

impl Publishers {
    /// The publisher with `id`'s place, a new one taking the next; `None`
    /// once [`PUBLISHERS`] are known.
    pub(super) fn index(&mut self, id: &[u8; 16]) -> Option<u8> {
        let place = match self.ids[..self.known].iter().position(|known| known == id) {
            Some(place) => place,
            None if self.known < PUBLISHERS => {
                self.ids[self.known] = *id;
                self.known += 1;
                self.known - 1
            }
            None => return None,
        };
        u8::try_from(place).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_publisher_keeps_the_place_it_was_first_heard_in_until_there_are_too_many() {
        let mut publishers = Publishers::default();
        let id = |byte: u8| [byte; 16];
        assert_eq!(publishers.index(&id(7)), Some(0));
        assert_eq!(publishers.index(&id(3)), Some(1));
        assert_eq!(publishers.index(&id(7)), Some(0));
        for byte in 10..16 {
            assert!(publishers.index(&id(byte)).is_some());
        }
        assert_eq!(publishers.index(&id(99)), None, "a ninth isn't told apart");
        assert_eq!(publishers.index(&id(3)), Some(1));
    }
}
