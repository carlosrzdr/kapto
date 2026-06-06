# arm7_ros2: the sample arm on ROS 2

The sample seven-joint arm of [`packages/arm7`](../arm7/README.md), as a
ROS 2 robot: it publishes its joint states as `sensor_msgs/msg/JointState`
on `/joint_states`, at 2 kHz, as the synthetic robot in `tools/kapto-loadgen`
does with `--transport ros2`. It is the example of
[Record a ROS 2 robot](../../docs/guides/record-a-ros2-robot.md).

| File | What it says |
|---|---|
| `package.toml` | What the package is: its ID `arm7_ros2`, version and name, and where the URDF is |
| `urdf/arm7.urdf` | The arm, the same as `packages/arm7`'s |
| `streams.toml` | What the robot publishes: one stream, `arm_joints`, the topic `/joint_states`, subscribed reliably, with its positions read by path |
| `mapping.toml` | Which value drives which joint, as in `packages/arm7` |
| `qa.toml` | Which quality checks run, as in `packages/arm7` |
| `msg/` | The `.msg` files of `JointState` and the types it uses, copied from ROS 2 Jazzy's installation (Apache-2.0), so the package decodes without ROS 2 |

Each message names the joints `joint_1` to `joint_7`, then `gripper`, and
gives their positions in that order: the seven joint angles in radians,
then the gripper opening in metres. Its header's stamp is when the robot
sent it.
