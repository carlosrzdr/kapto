// SPDX-License-Identifier: Apache-2.0

//! The robot moving: joint values from live samples, link poses from
//! forward kinematics, one pose per link.

use kapto_capture::tap::wire::TapSample;
use kapto_core::ClockSample;
use kapto_robot::{JointMap, Kinematics, RobotModel, Transform};

/// The robot's current joint values, and where each comes from.
pub struct LiveRobot {
    kinematics: Kinematics,
    /// For each stream, the `(joint, value)` pairs it sets: the joint's
    /// place in [`Kinematics::joint_names`] and the value's place in the
    /// stream's decoded values.
    sources: Vec<Vec<(usize, usize)>>,
    values: Vec<f64>,
    poses: Vec<Transform>,
    /// Maps receive times, read on the monotonic clock, to wall-clock time.
    clock: ClockSample,
    /// The newest receive time of the samples applied since the last log,
    /// or none if nothing changed.
    pending: Option<u64>,
}

impl LiveRobot {
    /// The robot at rest, moved by the joints `map` maps. Receive times
    /// are shown in wall-clock time, with the offset between the clocks
    /// in `clock`. The first [`LiveRobot::advance`] gives the robot at
    /// rest, at the time `clock` was read, so a page has it before any
    /// sample arrives.
    #[must_use]
    pub fn new(
        robot: &RobotModel,
        kinematics: Kinematics,
        map: &JointMap,
        streams: usize,
        clock: ClockSample,
    ) -> Self {
        let mut sources = vec![Vec::new(); streams];
        for (joint, mapped) in map.joints().iter().enumerate() {
            if let Some(source) = &mapped.position
                && let Some(stream) = sources.get_mut(source.stream)
            {
                stream.push((joint, source.value));
            }
        }
        let values = vec![0.0; kinematics.joint_names().len()];
        Self {
            sources,
            values,
            poses: vec![Transform::IDENTITY; robot.links.len()],
            kinematics,
            clock,
            pending: Some(clock.mono.as_nanos()),
        }
    }

    /// Takes the joint values `sample` carries. Returns whether it moved
    /// the robot: samples of streams that set no joint don't.
    pub fn apply(&mut self, sample: &TapSample) -> bool {
        let Some(sources) = self.sources.get(usize::from(sample.stream)) else {
            return false;
        };
        let mut applied = false;
        for &(joint, value) in sources {
            // A value the message didn't have (NaN) leaves the joint where
            // it was.
            if let Some(&value) = sample.values.get(value)
                && !value.is_nan()
            {
                self.values[joint] = value;
                applied = true;
            }
        }
        if applied {
            self.pending = Some(self.pending.unwrap_or(0).max(sample.recv_time_ns));
        }
        applied
    }

    /// The current joint values, in [`Kinematics::joint_names`] order.
    #[must_use]
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    /// Works out every link's pose if a sample moved the robot since the
    /// last time, and returns when the newest such sample was received, on
    /// the monotonic clock; `None` if nothing moved.
    pub fn advance(&mut self) -> Option<u64> {
        let received = self.pending.take()?;
        // The values and pose buffers are sized from the same kinematics,
        // so this can't fail.
        self.kinematics
            .poses(&self.values, &[], &Transform::IDENTITY, &mut self.poses)
            .ok()?;
        Some(received)
    }

    /// Every link's pose, relative to the root link, in the robot
    /// description's link order, as [`LiveRobot::advance`] last worked
    /// them out.
    #[must_use]
    pub fn poses(&self) -> &[Transform] {
        &self.poses
    }

    /// Maps receive times, read on the monotonic clock, to wall-clock time.
    #[must_use]
    pub fn clock(&self) -> ClockSample {
        self.clock
    }

    /// Maps receive times from now on with `clock`, read again.
    pub fn set_clock(&mut self, clock: ClockSample) {
        self.clock = clock;
    }
}
