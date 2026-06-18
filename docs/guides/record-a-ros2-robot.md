# Record a ROS 2 robot

Kapto subscribes to a ROS 2 robot's topics as one of the robot's own nodes
would, and records, shows and checks them as it does any other stream. This
guide declares a robot's topics in a robot package, runs Kapto beside the
robot, and reads what it recorded back with ROS 2's own tools. The sample
package [`packages/arm7_ros2`](../../packages/arm7_ros2/README.md) is the
example throughout: a seven-joint arm publishing its joint states on
`/joint_states`.

## Before you start

Only the `kapto-ros2` image's Kapto speaks ROS 2. Build it from the
repository:

```sh
docker compose build kapto-ros2
```

Kapto has to be on the robot's ROS 2 network, as the robot's nodes are:

- **The same network.** Run the container with `--network host` on the
  robot's computer, or on one its nodes can reach.
- **The same domain.** Pass the robot's `ROS_DOMAIN_ID` to the container
  (`-e ROS_DOMAIN_ID=42`) if it isn't the default, 0.
- **The same middleware.** The image has Fast DDS (the default), Cyclone
  DDS and Zenoh. Pass the robot's `RMW_IMPLEMENTATION` if it uses another
  than Fast DDS, e.g. `-e RMW_IMPLEMENTATION=rmw_cyclonedds_cpp`. With
  Zenoh, Kapto finds the robot's topics through its Zenoh router, as the
  robot's nodes do: `localhost:7447` unless told otherwise, e.g.
  `-e ZENOH_CONFIG_OVERRIDE='connect/endpoints=["tcp/192.168.1.20:7447"]'`.
  Pre-flight fails with `transport.ros2_router_unreachable` when nothing
  answers there. A router started after Kapto, or restarted while it
  records, is found again on its own, and recording carries on. Recording
  the same robot over Zenoh costs Kapto more than over DDS: with two
  1080p cameras beside sixteen 2 kHz topics, about 0.74 of a core against
  0.55 to 0.60, measured on a laptop, partly because the machine runs
  slower under Zenoh's own load. Zenoh's shared memory
  (`transport/shared_memory/enabled=true`) didn't change that. The image has Fast DDS take messages over UDP only
  (`FASTDDS_BUILTIN_TRANSPORTS=UDPv4`): nodes on the same machine would
  otherwise send through shared memory, which doesn't reach into the
  container, and Kapto would see the topics but receive nothing. To use
  shared memory instead, give Kapto Fast DDS's default transports back
  (`-e FASTDDS_BUILTIN_TRANSPORTS=DEFAULT`), the host's shared memory
  (`--ipc host`) and the same user as the robot's nodes
  (`--user <uid>:<gid>`; with another user Fast DDS delivers nothing),
  with `/data` on a folder that user can write.
- **The message types.** Every type Kapto subscribes with must be installed
  in the image, as it must be for any node: the standard ones are. For a
  robot's own message package, build an image on top of `kapto-ros2:dev`
  with the package built and sourced; without it, subscribing fails with
  `transport.ros2.type_unavailable`, naming the type.

## Find the topics

List what the robot publishes, with each topic's type:

```sh
docker run --rm --network host kapto-ros2:dev topics
```

## Declare them in a package

A robot package declares each topic as a stream in its `streams.toml`,
with the topic's name, its type and how to subscribe:

```toml
[streams.arm_joints]
rate_hz = 2000

[streams.arm_joints.ros2]
topic = "/joint_states"
type = "sensor_msgs/msg/JointState"
qos = { reliability = "reliable", depth = 1000 }
```

`qos` is optional. By default Kapto subscribes best effort, keeping the last
10 messages, which matches every publisher; subscribe reliably when the
robot publishes reliably and nothing should be lost between it and Kapto,
and raise `depth` for a fast topic. `rate_hz` lets Kapto notice silences:
`JointState` has no sequence number, so a lost message can't be counted
exactly.

Kapto stores every message exactly as it arrived, serialised as ROS 2 does.
To draw the robot and check it, it also needs values out of the messages:
a decoder of `kind = "cdr"` finds them by path through the message, as its
type's definition lays it out.

```toml
[streams.arm_joints.decoder]
kind = "cdr"

[[streams.arm_joints.decoder.fields]]
name = "joint_positions"
type = "f64"
path = "position"
count = 7
unit = "rad"

[[streams.arm_joints.decoder.fields]]
name = "gripper"
type = "f64"
path = "position[7]"
unit = "m"
```

The type's definition comes from `.msg` files. Copy them into a `msg/`
folder beside `streams.toml`, laid out as ROS 2 installs them under
`share/`, the type's own and that of every type it uses:

```text
msg/sensor_msgs/msg/JointState.msg
msg/std_msgs/msg/Header.msg
msg/builtin_interfaces/msg/Time.msg
```

A file missing from the package is looked for in the ROS 2 installation
Kapto runs in, but carrying them makes the package decode the same
anywhere, also on a machine without ROS 2. `mapping.toml` says which value
drives which joint, as for any robot; the
[configuration guide](../concepts/configuration.md) has the details, and
`kapto package lint` checks the whole package:

```sh
cargo run --bin kapto -- package lint packages/arm7_ros2
```

Read by position this way, the order of `name` has to stay the same; the
sample arm's does. When a robot's order can change, find each joint by its
name instead, one field per joint:

```toml
[[streams.arm_joints.decoder.fields]]
name = "shoulder"
type = "f64"
path = "position[name=shoulder_joint]"
unit = "rad"
```

The field is the element of `position` where `name` holds `shoulder_joint`;
a message without that joint leaves it out rather than giving it another
joint's value. So one stream can name the joints of several drivers that
share a topic, such as an arm's and a gripper's on `/joint_states`: each
message gives the joints it has, and the live view keeps the others where
they were. Each field found by name reads the message once more, so
declare by position what keeps its order.

## Record the robot's transforms

A ROS 2 robot says where its frames are on two topics: `/tf` for those
that move, published all the time, and `/tf_static` for fixed ones, such
as where a camera is mounted, published once when the robot starts. The
fixed ones are kept for nodes that subscribe later, but only for a
subscriber that asks for them, so declare `/tf_static` with
`durability = "transient_local"`, and reliably, as robot_state_publisher
sends it:

```toml
[streams.tf]
rate_hz = 50
ros2 = { topic = "/tf", type = "tf2_msgs/msg/TFMessage" }

[streams.tf_static]
ros2 = { topic = "/tf_static", type = "tf2_msgs/msg/TFMessage", qos = { reliability = "reliable", durability = "transient_local" } }
```

Kapto keeps the newest fixed transforms it was sent, and every episode
starts with them, however long after the robot started it begins: each is
written at the episode's start, and the episode's details count it as
*kept*. `/tf_static` has no `rate_hz`: it says nothing more once sent, and
that isn't a silence, so the live view shows it as kept and pre-flight
only asks that one has arrived since Kapto started. Neither topic needs a
decoder to be recorded; both are stored as received and play back with
`ros2 bag play`.

Kapto subscribes to the message types its image has installed: the
standard ones (`std_msgs`, `sensor_msgs`, `geometry_msgs`, `nav_msgs`,
`tf2_msgs` and the others of ROS 2's common interfaces). `kapto topics`
lists a topic of any type, but recording one of a robot's own types isn't
possible yet.

## Run Kapto beside the robot

Mount the packages folder, and a volume for the recordings, and run
`serve` in the image, on the host's network:

```sh
docker run --rm --network host -v kapto-data:/data -v "$PWD/packages:/packages:ro" \
  -e KAPTO_SERVER_BIND=127.0.0.1 kapto-ros2:dev serve \
  --robot /packages/arm7_ros2/urdf/arm7.urdf \
  --streams /packages/arm7_ros2/streams.toml \
  --mapping /packages/arm7_ros2/mapping.toml
```

Open <http://127.0.0.1:7700>: the arm moves as the robot publishes, and
the Record button records an episode. Each episode is checked and can be
replayed as any other. `KAPTO_SERVER_BIND` keeps the web UI to the
machine itself; leave it out to reach it from elsewhere.

Without a robot, the synthetic one publishes the sample arm's joint states
at 2 kHz. It is built with ROS 2 in the image the checks use, into
`target/ros2/` as `cargo xtask ros2` builds:

```sh
docker build --target ros2-dev --tag kapto-ros2-dev --file docker/Dockerfile .
docker run --rm --network host --user "$(id -u):$(id -g)" -e HOME=/tmp \
  -e CARGO_HOME=/src/target/ros2/cargo -e CARGO_TARGET_DIR=/src/target/ros2 \
  -v "$PWD:/src" -w /src kapto-ros2-dev \
  bash -c '. /opt/ros/$ROS_DISTRO/setup.sh && cargo run --release -p kapto-loadgen --features ros2 -- --transport ros2'
```

## Read it back with ROS 2's tools

An episode's ROS 2 topics are stored as rosbag2 stores them, so
`ros2 bag info` and Foxglove read its `recording.mcap` as it is. To play
it back on the robot's topics, export a bag of the topics alone, here
episode `20260322T161545.701Z-5f729d2c` (its ID is in the web UI and the
API) into the current folder:

```sh
docker run --rm --user "$(id -u):$(id -g)" -v kapto-data:/data:ro -v "$PWD:/out" \
  kapto-ros2:dev export rosbag2 /data/20260322T161545.701Z-5f729d2c --output /out/episode.mcap
ros2 bag play episode.mcap
```
