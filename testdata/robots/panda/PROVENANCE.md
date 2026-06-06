# Franka Emika Panda URDF

- **File:** `panda.urdf`, unchanged.
- **Source:** <https://github.com/Gepetto/example-robot-data>, file
  `robots/panda_description/urdf/panda.urdf`, branch `master`; SHA-256
  `63792d2679f22c4e41cda2c4d9d903644e628970ad0656b9eec8485113941555`.
- **Licence:** BSD-3-Clause, see `LICENSE` in this folder. The file was
  generated from the `franka_description` package of `franka_ros`
  (Apache-2.0).
- **Why it is here:** a real robot description Kapto didn't write: seven
  revolute arm joints, fixed joints, a gripper whose second finger mimics
  the first, and meshes referred to with `package://` paths.
- **`fk_reference.json`:** link poses for eight joint configurations,
  computed by ROS 2's robot_state_publisher with `cargo xtask golden fk`.
  Never edit it by hand; regenerate it.
- **Not included:** the meshes. Tests check that each missing mesh is
  reported rather than failing the load.
