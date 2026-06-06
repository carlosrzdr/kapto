# Sample arm reference poses

`fk_reference.json` holds the link poses of the sample arm in
`packages/arm7/urdf/arm7.urdf` for eight joint configurations, computed by
ROS 2's robot_state_publisher with `cargo xtask golden fk`. Never edit it by
hand; regenerate it.
