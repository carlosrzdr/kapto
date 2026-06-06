# Humanoid test robot

`humanoid.urdf` was written for Kapto's tests: a pelvis root with two legs,
a torso, a head, and two arms whose grippers have a mimicking finger, all
made of primitive shapes. Apache-2.0, like the rest of Kapto.

`fk_reference.json` holds its link poses for eight joint configurations,
computed by ROS 2's robot_state_publisher with `cargo xtask golden fk`.
Never edit it by hand; regenerate it.
