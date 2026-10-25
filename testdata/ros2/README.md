# A ROS 2 message package of the tests' own

`kapto_test_msgs` defines one message type, `kapto_test_msgs/msg/Gripper`,
that no ROS 2 installation has, as a robot's own message package would.
Written for Kapto's tests, not taken from elsewhere.

`cargo xtask ros2` builds it with colcon in the ROS 2 image, into
`target/ros2/overlays/kapto_test_msgs`, and mounts that folder read-only at
`/overlays/kapto_test_msgs` for the tests, as a robot's built package is
mounted into the `kapto-ros2` image. The tests then record a topic of its
type, and check that the same topic is refused without the overlay.
