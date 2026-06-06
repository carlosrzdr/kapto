// SPDX-License-Identifier: Apache-2.0

//! The message every stream sends.
//!
//! 76 bytes, little-endian, no padding:
//!
//! | Offset | Size | Type | Field |
//! |---|---|---|---|
//! | 0 | 4 | `u32` | sequence number, starting at 0, wrapping |
//! | 4 | 8 | `u64` | source time: the sender's wall clock, nanoseconds since the Unix epoch |
//! | 12 | 56 | 7 × `f64` | joint positions in radians |
//! | 68 | 8 | `f64` | gripper opening: how far each finger is from closed, in metres |
//!
//! The joints move together around a working pose, like an arm sweeping
//! across a table and reaching in and out, offset per stream, so a robot
//! model driven by them moves smoothly and believably.

use std::f64::consts::TAU;

/// Size of one message in bytes.
pub const SIZE: usize = 76;

/// Number of joint values in a message.
pub const JOINTS: usize = 7;

/// One message's contents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Counts messages on this stream, starting at 0 and wrapping.
    pub seq: u32,
    /// The sender's wall clock when the message was built, in nanoseconds
    /// since the Unix epoch.
    pub source_time_ns: u64,
    /// Joint positions in radians.
    pub joints: [f64; JOINTS],
    /// How far each finger is from closed, in metres.
    pub gripper: f64,
}

impl Sample {
    /// Writes the message into `out` in the documented layout.
    pub fn encode(&self, out: &mut [u8; SIZE]) {
        out[0..4].copy_from_slice(&self.seq.to_le_bytes());
        out[4..12].copy_from_slice(&self.source_time_ns.to_le_bytes());
        let (chunks, _) = out[12..68].as_chunks_mut::<8>();
        for (joint, chunk) in self.joints.iter().zip(chunks) {
            *chunk = joint.to_le_bytes();
        }
        out[68..76].copy_from_slice(&self.gripper.to_le_bytes());
    }

    /// Reads a message, or `None` if `bytes` isn't exactly one message long.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; SIZE] = bytes.try_into().ok()?;
        let seq = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let source_time_ns = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
        let mut joints = [0.0; JOINTS];
        let (chunks, _) = bytes[12..68].as_chunks::<8>();
        for (joint, chunk) in joints.iter_mut().zip(chunks) {
            *joint = f64::from_le_bytes(*chunk);
        }
        let gripper = f64::from_le_bytes(bytes[68..76].try_into().ok()?);
        Some(Self {
            seq,
            source_time_ns,
            joints,
            gripper,
        })
    }
}

/// The pose the arm works around: shoulder forward, elbow and wrist bent
/// forward so the gripper points down, about 30 cm above the floor in front
/// of the sample arm, as if over a table.
pub const READY_POSE: [f64; JOINTS] = [0.0, 0.6, 0.0, 1.5, 0.0, 1.0, 0.0];

/// Seconds for one sweep across the table and back.
const SWEEP_SECONDS: f64 = 12.0;

/// Joint positions of `stream` at `seconds` since the generator started: a
/// sweep of the base around [`READY_POSE`], reaching in and out twice per
/// sweep with the shoulder and elbow while the wrist keeps the gripper
/// pointing down, the forearm and wrist rolling a little. Each stream is a
/// little further along.
#[must_use]
pub fn joints_at(stream: u16, seconds: f64) -> [f64; JOINTS] {
    let sweep = TAU * seconds / SWEEP_SECONDS + f64::from(stream) * 0.7;
    let reach = (2.0 * sweep).sin();
    // Reaching out leans the shoulder forward and straightens the elbow;
    // the wrist takes up the difference, so the pitch angles keep adding up
    // to the same total and the gripper keeps pointing down.
    let motion = [
        0.9 * sweep.sin(),
        0.25 * reach,
        0.15 * (sweep + 1.0).sin(),
        -0.35 * reach,
        0.15 * (sweep - 0.5).sin(),
        0.1 * reach,
        0.8 * sweep.sin(),
    ];
    let mut joints = READY_POSE;
    for (joint, offset) in joints.iter_mut().zip(motion) {
        *joint += offset;
    }
    joints
}

/// How far each finger opens, in metres.
pub const GRIPPER_OPEN: f64 = 0.03;

/// The gripper opening of `stream` at `seconds`, moving with
/// [`joints_at`]: open while the arm reaches out, closing as it reaches in,
/// as if it picked something up on each pass.
#[must_use]
pub fn gripper_at(stream: u16, seconds: f64) -> f64 {
    let sweep = TAU * seconds / SWEEP_SECONDS + f64::from(stream) * 0.7;
    GRIPPER_OPEN * 0.5 * (1.0 + (2.0 * sweep).sin())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn fields_are_at_their_documented_offsets() {
        let sample = Sample {
            seq: 0x0102_0304,
            source_time_ns: 0x1112_1314_1516_1718,
            joints: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -2.5],
            gripper: 0.02,
        };
        let mut bytes = [0; SIZE];
        sample.encode(&mut bytes);
        assert_eq!(bytes[0..4], [0x04, 0x03, 0x02, 0x01]);
        assert_eq!(bytes[4..12], 0x1112_1314_1516_1718_u64.to_le_bytes());
        assert_eq!(bytes[12..20], 1.0_f64.to_le_bytes());
        assert_eq!(bytes[60..68], (-2.5_f64).to_le_bytes());
        assert_eq!(bytes[68..76], 0.02_f64.to_le_bytes());
    }

    #[test]
    fn messages_of_the_wrong_length_are_rejected() {
        assert_eq!(Sample::decode(&[0; SIZE - 1]), None);
        assert_eq!(Sample::decode(&[0; SIZE + 1]), None);
    }

    #[test]
    fn joints_stay_within_one_radian_of_the_ready_pose() {
        for step in 0..10_000 {
            let seconds = f64::from(step) * 0.01;
            for (joint, ready) in joints_at(3, seconds).into_iter().zip(READY_POSE) {
                assert!((joint - ready).abs() <= 1.0, "{joint} at {seconds} s");
            }
        }
    }

    #[test]
    fn joints_move_smoothly_enough_for_a_real_arm() {
        // At most 1.5 rad/s, well within the sample arm's 2 rad/s.
        for step in 0..12_000 {
            let seconds = f64::from(step) * 0.001;
            let now = joints_at(0, seconds);
            let next = joints_at(0, seconds + 0.001);
            for (a, b) in now.into_iter().zip(next) {
                assert!((b - a).abs() / 0.001 <= 1.5, "{a} → {b} at {seconds} s");
            }
        }
    }

    #[test]
    fn the_gripper_closes_as_the_arm_reaches_in_and_opens_as_it_reaches_out() {
        let opening: Vec<f64> = (0..1_200)
            .map(|step| gripper_at(0, f64::from(step) * 0.01))
            .collect();
        assert!(opening.iter().all(|o| (0.0..=GRIPPER_OPEN).contains(o)));
        assert!(opening.iter().any(|o| *o < 0.001), "it closes");
        assert!(
            opening.iter().any(|o| *o > GRIPPER_OPEN - 0.001),
            "it opens"
        );
    }

    #[test]
    fn streams_are_phase_shifted() {
        assert_ne!(joints_at(0, 1.0), joints_at(1, 1.0));
    }

    proptest! {
        #[test]
        fn messages_round_trip(seq: u32, source_time_ns: u64, joints: [f64; JOINTS], gripper: f64) {
            let sample = Sample { seq, source_time_ns, joints, gripper };
            let mut bytes = [0; SIZE];
            sample.encode(&mut bytes);
            let decoded = Sample::decode(&bytes).unwrap();
            // Compare bit patterns so NaN joint values round-trip too.
            prop_assert_eq!(decoded.seq, seq);
            prop_assert_eq!(decoded.source_time_ns, source_time_ns);
            prop_assert_eq!(decoded.joints.map(f64::to_bits), joints.map(f64::to_bits));
        }
    }
}
