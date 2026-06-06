# ROS 2 topics through ROS 2's own client library, taken as received

## Context

Kapto receives UDP and TCP streams. Most robots that run ROS 2 publish what
matters on topics instead: joint states, transforms, a teleoperation
device's buttons. Kapto should subscribe to them as it receives a UDP
stream, with the same guarantees:

- **Payloads stored as received** ([mcap recordings stored as received](mcap-recordings-stored-as-received.md)).
  A ROS 2 message arrives serialised as CDR. It should go into the recording
  exactly so, with its `.msg` definition as the channel's schema, the way
  rosbag2 writes MCAP, so ROS 2's own tools read the recording. Kapto already
  decodes CDR with its `.msg` definitions, checked against messages ROS 2
  serialised itself, so it never needs the message as a typed value.
- **No allocation, lock, syscall or log per message on the hot path**, in
  Kapto's own code. Messages go straight into the free slots of a capture
  batch, as the UDP and TCP transports do.
- **Two timestamps per message.** Kapto's receive time, and the source's
  time as received. ROS 2 hands each message over with the publisher's
  timestamp in its message info.
- **Whatever middleware the robot uses.** ROS 2 talks over a middleware
  chosen per machine (`RMW_IMPLEMENTATION`): Fast DDS by default, Cyclone
  DDS, or Zenoh (`rmw_zenoh`, packaged since Jazzy).
  Kapto must see the topics the robot's own nodes see.
- **The slim image stays free of ROS.** Robots that only speak UDP or TCP
  shouldn't carry ROS 2's libraries, and building Kapto shouldn't need them
  unless ROS 2 is wanted.

Three Rust clients exist:

| | `r2r` 0.9.7 | `ros2-client` 0.11.1 | `rclrs` 0.8.0 |
|---|---|---|---|
| How | Bindings to ROS 2's C library (rcl), generated at build time | Pure Rust, over its own DDS (RustDDS) | Bindings to rcl |
| Middleware | Any the machine has, as every ROS 2 node | DDS (Fast DDS, Cyclone interoperate); Zenoh experimental | Any |
| Messages still serialised | Yes: `subscribe_raw`, and rcl's `rcl_take_serialized_message` underneath | No: messages are deserialised with serde | No |
| Needs ROS 2 to build | Yes: a sourced installation and libclang | No | Yes |
| Licence | MIT and Apache-2.0 | Apache-2.0 | Apache-2.0 |
| Maintained | Regular releases | Active | The crates.io release names no repository |

`r2r`'s `subscribe_raw` takes each message with `rcl_take_serialized_message`
into a buffer it keeps, then copies it into a new `Vec<u8>` for each
message and drops the message info, so the publisher's timestamp is lost.
Used as it is, it would allocate per message and lose a timestamp.

## Approach

Kapto subscribes to ROS 2 topics through rcl, with `r2r`, behind the cargo
feature `ros2` of `kapto-transport` (and `kapto-cli`, which passes it on):

- **`r2r` lists the topics** and their types (`kapto topics`,
  `get_topic_names_and_types`); its build generates no message types Kapto
  doesn't use (`IDL_PACKAGE_FILTER`). Its node keeps its rcl handles to itself, so
  Kapto can't take messages from `r2r`'s subscriptions; and `r2r`
  subscribes, even to raw messages, only to types it generated code for
  when it was built, which a robot's own message types never are. Kapto
  subscribes with a node of its own instead, made through the same rcl
  functions (`r2r_rcl`, `r2r`'s bindings, at its version), and loads each
  type's support library by name when it subscribes
  (`lib<package>__rosidl_typesupport_c.so`, which ROS 2 installs for every
  message package), as rosbag2 does: any type installed where Kapto runs
  can be subscribed to, the robot's own included. Kapto has its own QoS
  types, and puts each type's definition together from the `.msg` files
  itself, the package's first, then the installation's.
- **Kapto takes each message itself**, through rcl's C functions that
  `r2r` already binds (`r2r_rcl`). It waits on its own wait set with the
  transport's timeout, and takes with `rcl_take_serialized_message` into a
  serialised-message buffer per subscription, kept for the transport's life.
  It copies the bytes into the batch's free slot and reads the publisher's
  timestamp from the message info as the source time. Nothing is allocated
  per message in Kapto's code; rcl and the middleware below it manage their
  own memory. The calls are `unsafe`, in one module that allows it, each
  with its `// SAFETY:` reason.
- **One ROS 2 client per process.** `r2r` sets up
  ROS 2's logging for the whole process when its context starts, and a node
  of Kapto's own starting at the same time in another thread crashed Fast
  DDS. Listing topics is a command of its own, and the capture process uses
  Kapto's node alone.
- **The transport is a `Transport` like UDP's**, on a capture thread of its
  own (`cap-rx-ros2`). It counts a message too large for its slot as
  truncated, and stores it no differently. Nothing of `r2r` or rcl appears
  in Kapto's traits.
- **Only the ROS 2 image has the feature.** `kapto-ros2` is built on the
  pinned ROS 2 base with the feature, linking the installation's libraries;
  `kapto` is built without it and has no ROS 2 code.

## Consequences

- A ROS 2 robot's topics are recorded with the same loss accounting, live
  view and checks as a UDP robot's, and the recording opens in ROS 2's tools.
  Whatever middleware the robot's nodes use, Kapto sees what they see.
- **Building with the feature needs ROS 2**, so it is built and checked only
  in Docker, on the ROS 2 base image. The checks on the host (`cargo xtask
  ci`) don't use `--all-features`, which would need ROS 2 there, and name
  the other features instead; a `cargo xtask` command checks, lints and tests
  the feature in the ROS 2 image, and the image build runs it.
- **Kapto has `unsafe` code of its own**, small and in one module:
  taking a message, the message info and the wait set. It is reviewed by a
  second person, as every `unsafe` block is; Miri can't run it, as it calls
  C, so the transport's tests run against real ROS 2 nodes in the image.
- **Every message type a topic is subscribed with must be installed where
  Kapto runs**, as for the robot's own nodes: the standard ones come with the
  ROS 2 image, and a robot's own message package is built and sourced there.
  A missing one is refused when the topic is subscribed to, naming it
  (`transport.ros2.type_unavailable`).
- `r2r` follows ROS 2's releases. Moving to a new ROS 2 distribution means
  moving the base image and, if needed, `r2r`, in their own pull request.
- The middleware's own memory and threads are outside Kapto's control:
  a slow or bursty network can make the middleware drop messages before rcl
  hands them over. Kapto counts what it can see (a sequence gap, when the
  stream has one), like any loss upstream.

## Alternatives considered

- **`r2r`'s `subscribe_raw` as it is.** The least code, but a new `Vec` per
  message, against the rule for the capture path, and no publisher
  timestamp. Taking the message ourselves costs about one module.
- **`ros2-client`.** Needs nothing of ROS 2 to build, which would keep one
  image. But it speaks DDS only (its Zenoh support is experimental), so
  Kapto wouldn't see a robot whose nodes use `rmw_zenoh`, and it hands over
  messages deserialised through serde, where Kapto needs the bytes as
  received.
- **`rclrs`.** Bindings to rcl as well, but no subscription to serialised
  messages, and its crates.io release names no repository to follow.
- **Kapto's own DDS or Zenoh participant.** No ROS 2 needed at all, but
  every middleware's discovery, QoS matching and wire format to implement
  and keep up with: far beyond what Kapto should own.
- **Recording ROS 2 with rosbag2 and importing the bags.** Already planned as
  an importer, for what was recorded elsewhere. It isn't live: no live view,
  no checks while recording, and loss counted by another tool.
