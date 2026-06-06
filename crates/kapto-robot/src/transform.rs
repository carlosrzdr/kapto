// SPDX-License-Identifier: Apache-2.0

//! Rigid transforms: a rotation and a translation, the only geometry forward
//! kinematics needs. The arithmetic is `nalgebra`'s; the type is Kapto's, so
//! callers never depend on how it is computed.

use nalgebra::{Isometry3, Point3, Translation3, Unit, UnitQuaternion, Vector3};

use crate::model::Pose;

/// A rotation followed by a translation, taking points from a child frame
/// into its parent frame: `p_parent = rotation × p_child + translation`.
/// Metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform(Isometry3<f64>);

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// No rotation, no translation.
    pub const IDENTITY: Self = Self(Isometry3::from_parts(
        Translation3::new(0.0, 0.0, 0.0),
        UnitQuaternion::new_unchecked(nalgebra::Quaternion::new(1.0, 0.0, 0.0, 0.0)),
    ));

    /// A rotation as a row-major 3×3 matrix, then a translation in metres.
    /// The matrix must be a rotation: orthonormal, with determinant 1.
    #[must_use]
    pub fn new(rotation: [[f64; 3]; 3], translation: [f64; 3]) -> Self {
        let matrix = nalgebra::Matrix3::from_row_slice(rotation.as_flattened());
        let rotation = nalgebra::Rotation3::from_matrix_unchecked(matrix);
        Self(Isometry3::from_parts(
            Translation3::from(translation),
            UnitQuaternion::from_rotation_matrix(&rotation),
        ))
    }

    /// A URDF origin: translate by `xyz`, rotate by roll about x, then pitch
    /// about y, then yaw about z, all about the fixed axes, which makes the
    /// rotation `Rz(yaw) × Ry(pitch) × Rx(roll)`.
    #[must_use]
    pub fn from_pose(pose: &Pose) -> Self {
        let [roll, pitch, yaw] = pose.rpy;
        Self(Isometry3::from_parts(
            Translation3::from(pose.xyz),
            UnitQuaternion::from_euler_angles(roll, pitch, yaw),
        ))
    }

    /// A turn of `angle` radians about the unit vector `axis`.
    #[must_use]
    pub fn rotation_about(axis: [f64; 3], angle: f64) -> Self {
        let axis = Unit::new_unchecked(Vector3::from(axis));
        Self(Isometry3::from_parts(
            Translation3::identity(),
            UnitQuaternion::from_axis_angle(&axis, angle),
        ))
    }

    /// A move of `distance` metres along the unit vector `axis`.
    #[must_use]
    pub fn translation_along(axis: [f64; 3], distance: f64) -> Self {
        Self(Isometry3::translation(
            axis[0] * distance,
            axis[1] * distance,
            axis[2] * distance,
        ))
    }

    /// `self` then `child`: the transform from `child`'s frame into the frame
    /// `self` maps into.
    #[must_use]
    pub fn then(&self, child: &Self) -> Self {
        Self(self.0 * child.0)
    }

    /// Where the point `point` of the child frame lies in the parent frame.
    #[must_use]
    pub fn apply(&self, point: [f64; 3]) -> [f64; 3] {
        self.0.transform_point(&Point3::from(point)).coords.into()
    }

    /// The translation, in metres.
    #[must_use]
    pub fn translation(&self) -> [f64; 3] {
        self.0.translation.vector.into()
    }

    /// The rotation as a row-major 3×3 matrix.
    #[must_use]
    pub fn rotation(&self) -> [[f64; 3]; 3] {
        let matrix = self.0.rotation.to_rotation_matrix();
        [0, 1, 2].map(|row| [0, 1, 2].map(|column| matrix[(row, column)]))
    }

    /// The rotation as a unit quaternion `[x, y, z, w]` with `w ≥ 0`.
    #[must_use]
    pub fn quaternion(&self) -> [f64; 4] {
        let q = self.0.rotation.coords;
        let q = [q.x, q.y, q.z, q.w];
        if q[3] < 0.0 { q.map(|v| -v) } else { q }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_PI_2;

    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-12)
    }

    #[test]
    fn a_quarter_yaw_turns_x_into_y() {
        let yaw = Transform::from_pose(&Pose {
            xyz: [1.0, 2.0, 3.0],
            rpy: [0.0, 0.0, FRAC_PI_2],
        });
        assert!(close(yaw.apply([1.0, 0.0, 0.0]), [1.0, 3.0, 3.0]));
    }

    #[test]
    fn roll_pitch_yaw_matches_three_turns_about_the_fixed_axes() {
        let [roll, pitch, yaw] = [0.3, -0.7, 1.9];
        let pose = Transform::from_pose(&Pose {
            xyz: [0.0; 3],
            rpy: [roll, pitch, yaw],
        });
        let turns = Transform::rotation_about([0.0, 0.0, 1.0], yaw)
            .then(&Transform::rotation_about([0.0, 1.0, 0.0], pitch))
            .then(&Transform::rotation_about([1.0, 0.0, 0.0], roll));
        for point in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            assert!(close(pose.apply(point), turns.apply(point)));
        }
    }

    #[test]
    fn quaternions_of_known_turns() {
        let half = std::f64::consts::FRAC_1_SQRT_2;
        let cases = [
            (Transform::IDENTITY, [0.0, 0.0, 0.0, 1.0]),
            (
                Transform::rotation_about([0.0, 0.0, 1.0], FRAC_PI_2),
                [0.0, 0.0, half, half],
            ),
            (
                Transform::rotation_about([1.0, 0.0, 0.0], std::f64::consts::PI),
                [1.0, 0.0, 0.0, 0.0],
            ),
            (
                Transform::rotation_about([0.0, 1.0, 0.0], -FRAC_PI_2),
                [0.0, -half, 0.0, half],
            ),
        ];
        for (transform, expected) in cases {
            let q = transform.quaternion();
            assert!(
                q.iter().zip(&expected).all(|(a, b)| (a - b).abs() < 1e-12),
                "{q:?}"
            );
        }
    }
}
