# ROS 2 messages as CDR

`reference.json` holds messages serialised by ROS 2 itself
(`rclpy.serialization.serialize_message`, ROS 2 Jazzy, in the image named in
the file), for checking Kapto's CDR reader and `.msg` parser: for each, its
type, its definition as rosbag2 stores it in MCAP, its CDR bytes as hex and
its field values. They cover `sensor_msgs` JointState and Image,
`geometry_msgs` Pose, PoseStamped and TwistWithCovarianceStamped, and ROS
2's own `test_msgs`: every primitive type, fixed, bounded and unbounded
arrays, bounded strings, wide strings, empty and nested messages.

Each definition is the one rosbag2 itself wrote into an MCAP file, read
back from the file, and `rosbag2_encoding` says how rosbag2 stored it:
`ros2msg` for all but `test_msgs/WStrings`, which its package lists as an
`.idl` interface, so rosbag2 stores its IDL; that one's definition is put
together from its `.msg` file instead, which is what Kapto reads.
`msg_files` holds every `.msg` file the definitions are made of, as
installed, so Kapto putting a definition together from them can be checked
against rosbag2's.

ROS 2 doesn't clear the padding between aligned fields, so the CDR bytes
there change from one run to the next; a reader must ignore them.

Generated with `cargo xtask golden cdr` (Docker, network). Never edit it by
hand; regenerate it.
