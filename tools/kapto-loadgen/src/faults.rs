// SPDX-License-Identifier: Apache-2.0

//! The synthetic robot misbehaving on demand, so a demonstration can show
//! what Kapto finds and where: a joint that freezes and jumps back, a wrist
//! that vibrates, a burst of messages lost on the way. Each fault lasts a
//! set time from the tick it starts on, then the robot is itself again.

use std::f64::consts::TAU;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::payload::{JOINTS, joints_at};

/// The joint that freezes: the shoulder, which moves the whole arm.
pub const FROZEN_JOINT: usize = 1;
/// How far off its real position the frozen reading sticks, in radians: a
/// reading stuck at a wrong value, which jumps when it sticks and again
/// when it comes back, wherever in its motion the joint is.
pub const STUCK_OFFSET_RAD: f64 = 0.3;
/// The joint that vibrates: the wrist's pitch.
pub const VIBRATING_JOINT: usize = 5;
/// How far and how fast it vibrates: small, quick and plain to see.
const VIBRATION_RAD: f64 = 0.04;
const VIBRATION_HZ: f64 = 25.0;

/// One way the robot misbehaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// [`FROZEN_JOINT`]'s reading sticks, [`STUCK_OFFSET_RAD`] off where
    /// the joint was, while the others move, then jumps back to where the
    /// joint really is.
    FreezeThenJump,
    /// [`VIBRATING_JOINT`] shakes on top of its motion.
    Vibrate,
    /// Nothing is sent; the sequence numbers still count on, so the gap
    /// shows as messages lost on the way.
    Drop,
}

/// The fault going on, if any.
#[derive(Clone, Copy, Debug)]
struct Active {
    fault: Fault,
    /// When it started and ends, in seconds since the generator started;
    /// `None` until the next tick starts it.
    from: Option<f64>,
    lasts: f64,
}

/// What the generator does about faults: shared between whoever starts
/// them and the sending loop, which asks once a tick.
#[derive(Debug, Default)]
pub struct Faults {
    active: Mutex<Option<Active>>,
}

impl Faults {
    /// No fault, until one is started.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts `fault` on the next tick, for `lasts`; one already going on
    /// is ended.
    pub fn start(&self, fault: Fault, lasts: Duration) {
        *self.lock() = Some(Active {
            fault,
            from: None,
            lasts: lasts.as_secs_f64(),
        });
    }

    /// Whether a fault is going on or about to start.
    #[must_use]
    pub fn active(&self) -> bool {
        self.lock().is_some()
    }

    /// Changes the joints `stream` sends at `seconds` as the fault going on
    /// says. Returns whether to send them at all.
    pub fn shape(&self, stream: u16, seconds: f64, joints: &mut [f64; JOINTS]) -> bool {
        let mut active = self.lock();
        let Some(going) = active.as_mut() else {
            return true;
        };
        let from = *going.from.get_or_insert(seconds);
        if seconds >= from + going.lasts {
            *active = None;
            return true;
        }
        match going.fault {
            Fault::FreezeThenJump => {
                joints[FROZEN_JOINT] = joints_at(stream, from)[FROZEN_JOINT] + STUCK_OFFSET_RAD;
                true
            }
            Fault::Vibrate => {
                joints[VIBRATING_JOINT] +=
                    VIBRATION_RAD * (TAU * VIBRATION_HZ * (seconds - from)).sin();
                true
            }
            Fault::Drop => false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Active>> {
        self.active.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `faults` makes stream 0 send at each of `times`.
    fn sent(faults: &Faults, times: &[f64]) -> Vec<Option<[f64; JOINTS]>> {
        times
            .iter()
            .map(|&seconds| {
                let mut joints = joints_at(0, seconds);
                faults.shape(0, seconds, &mut joints).then_some(joints)
            })
            .collect()
    }

    #[test]
    fn without_a_fault_the_robot_sends_what_it_always_does() {
        let faults = Faults::new();
        assert_eq!(sent(&faults, &[1.0]), [Some(joints_at(0, 1.0))]);
        assert!(!faults.active());
    }

    #[test]
    fn a_frozen_joint_holds_its_reading_while_the_others_move_then_jumps_back() {
        let faults = Faults::new();
        faults.start(Fault::FreezeThenJump, Duration::from_secs(2));
        let shown = sent(&faults, &[3.0, 4.0, 4.99, 5.0]);
        let held = joints_at(0, 3.0)[FROZEN_JOINT] + STUCK_OFFSET_RAD;
        for during in &shown[..3] {
            assert_eq!(during.unwrap()[FROZEN_JOINT], held);
        }
        // The others moved meanwhile.
        assert_ne!(shown[2].unwrap()[0], shown[0].unwrap()[0]);
        // Over: where the joint really is, a jump from the held reading.
        assert_eq!(shown[3], Some(joints_at(0, 5.0)));
        assert!(!faults.active());
    }

    #[test]
    fn a_vibrating_wrist_shakes_around_its_motion_and_a_drop_sends_nothing() {
        let faults = Faults::new();
        faults.start(Fault::Vibrate, Duration::from_secs(1));
        let at = 2.01;
        let shaken = sent(&faults, &[2.0, at])[1].unwrap();
        let off = shaken[VIBRATING_JOINT] - joints_at(0, at)[VIBRATING_JOINT];
        assert!(
            off.abs() > 0.01 && off.abs() <= VIBRATION_RAD + 1e-12,
            "{off}"
        );

        faults.start(Fault::Drop, Duration::from_millis(500));
        assert_eq!(
            sent(&faults, &[6.0, 6.4, 6.5]),
            [None, None, Some(joints_at(0, 6.5))]
        );
    }
}
