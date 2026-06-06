# Large messages go through rings of their own

## Context

Capture takes messages into batches: blocks of memory allocated once, with
room for a fixed number of messages, each in a slot of the same size, the
largest message the batch's streams declare (`max_message_bytes`, 1 KiB by
default). A receiver fills one batch at a time and hands it to the writer
through a ring of such batches, sized when capture starts for the longest
the writer may stall at the receiver's rate. Nothing on that path
allocates, locks or logs per message. Each input has one receiver and one
ring: every UDP stream shares the UDP one, each TCP stream has its own,
and every ROS 2 topic shares the ROS 2 one.

So far no message has been larger than a UDP datagram (65,507 bytes), the
limit `max_message_bytes` takes. Cameras change that. A compressed frame
is tens or hundreds of KiB, a raw 640×480 RGB frame 900 KiB, a raw 1080p
one 6 MiB, at 30 to 90 frames a second. They arrive over TCP or as ROS 2
topics (`sensor_msgs/msg/Image`, `CompressedImage`), never over UDP.

With one slot size per ring, a ROS 2 camera topic would make every ROS 2
slot the size of a frame: the joint states' ring, 64 messages a batch and
about a thousand batches at the reference load, would need 64 GiB for
1 MiB slots. The 2 kHz streams have to keep their small slots.

What else holds:

- **No allocation, lock or syscall per message** on the capture path,
  frames included.
- **Loss is never silent**: a frame dropped because the writer is behind,
  or larger than declared, is counted for its stream.
- **Memory is bounded and declared** when capture starts, and stays within
  1 GiB at the reference load, cameras included.
- **The batch layout stays as it is**: the UDP receiver hands every free
  slot of a batch to the kernel at once, which needs slot `i` at byte
  `i × max_message_len`.

## Approach

A stream declaring `max_message_bytes` above 65,507 bytes is a large
stream, and its messages go through a ring of its own:

- **Its own ring and batches.** Slots of its `max_message_bytes`, and as
  many slots a batch as arrive in the receiver's batch wait at its rate,
  at least one: one frame a batch for a camera. The ring has as many
  batches as the writer's longest stall takes at its rate, plus the two in
  use, as every ring: 62 batches for a 30 fps camera and a 2 s stall,
  62 MiB with 1 MiB frames.
- **A receiver of its own.** A TCP stream already has a receiver and a
  ring of its own, which take its size. A large ROS 2 topic gets a
  receiver thread of its own (`cap-rx-ros2-1`, `-2`, …), with a node of
  its own (`kapto_<stream>`) taking only that topic, as
  the [ROS 2 design](ros-2-through-rcl.md)'s receiver takes the small topics:
  copying a frame never holds up the joint states, and the `Transport`
  trait, which fills one batch at a time, is the same for every
  transport.
- **Everything else unchanged.** The writer takes batches from every ring
  as it does from several inputs; loss accounting, the stream's
  counters, drops when the writer is behind and a message larger than
  declared (counted as dropped on arrival, its stream named) work per
  stream as before.
- **A limit.** `max_message_bytes` goes up to 32 MiB, which a raw 4K RGB
  frame (24 MiB) fits, for TCP and ROS 2 streams; UDP streams keep 65,507.
- **Declared memory.** Each ring's memory is set aside when capture starts
  and counted in what it declares, so a camera's cost shows before an
  episode is recorded.

## Consequences

- Cameras can be recorded beside the 2 kHz streams without growing their
  buffers, at memory that grows with each camera's rate and frame size,
  known when capture starts.
- Each camera topic costs a thread and a middleware node more: a robot
  sees Kapto as several nodes, `kapto` and one per camera.
- The writer's allocations, made per chunk of the recording, come with
  each frame when a frame fills a chunk (1 MiB by default): a few a
  frame, at a camera's rate, on the writer thread, never on a receiver.
- A camera with a large declared size and a high rate costs that much
  memory even when its frames are small; the declared size is the
  operator's to set, as for every stream.
- Frames reach the live view by a path of their own, designed with the
  cameras in the live view: the live tap's datagrams are far smaller than
  a frame.
- `max_message_bytes` is a 32-bit value in the configuration.

## Alternatives considered

- **One frame per buffer from a shared pool.** Each large message takes a
  buffer of the largest size from one pool and is handed to the writer on
  its own. Simpler to size across cameras, and a hand-off per frame costs
  nothing at 30–90 fps, but it is a second way through capture beside
  batches, with its own hand-off, accounting and tests, for what a ring of
  one-slot batches already does.
- **Packed batches.** Messages laid end to end in one block, so one ring
  takes any mix of sizes. The most general, but it changes the batch
  layout the UDP receiver relies on to receive many datagrams in one call,
  and every stage that reads batches.
- **One slot size per input.** Impossible for ROS 2
  topics, whose cameras and joint states share an input.
- **One ROS 2 receiver filling several rings.** One thread and node for
  every topic, but the `Transport` trait would take a set of batches
  instead of one, for every transport, and a frame's copy would delay the
  small topics taken after it.
