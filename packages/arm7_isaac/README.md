# arm7_isaac: the sample arm in Isaac Sim

The sample seven-joint arm of [`packages/arm7`](../arm7/README.md),
simulated in Isaac Sim and published over its ROS 2 bridge, with a camera
beside it and one on its wrist. It is the example of
[Record a robot in Isaac Sim](../../docs/guides/record-a-robot-in-isaac-sim.md).

| File | What it says |
|---|---|
| `package.toml` | What the package is: its ID `arm7_isaac`, version and name, and where the URDF is |
| `urdf/arm7.urdf` | The arm, the same as `packages/arm7`'s, and what Isaac Sim imports |
| `streams.toml` | What the simulation publishes: the joint states on `/joint_states`, each joint found by its name, the simulation's clock on `/clock`, and the two cameras' raw pictures |
| `mapping.toml` | Which value drives which joint, as in `packages/arm7` |
| `msg/` | The `.msg` files of `JointState`, `Image`, `Clock` and the types they use, copied from ROS 2 Jazzy's installation (Apache-2.0), so the package decodes without ROS 2 |
| `sim/arm7_sim.py` | The simulation, run headless with Isaac Sim's own Python: the arm imported from the URDF, three blocks on the ground, the two cameras, and the graph publishing it all |

The script bends the arm over the blocks, its tool pointing down, and
sweeps it across them, so that the wrist camera looks down past the
fingers at them. It publishes once a simulated frame, 60 a second: the
joints' positions, in radians, then the gripper's opening in metres,
named `joint_1` to `joint_7` and `gripper` in the order Isaac Sim gives
its joints; and every other frame a picture from each camera, `rgb8`, 960
by 540 beside the arm and 640 by 480 on the wrist. Its options
(`--help`) change the cameras' sizes, how often they take pictures, how
long it runs and the joints' drive gains.
