# Cameras in the live view

## Context

A robot's cameras are recorded beside its other streams, each frame as
received, through rings of their own
([large messages](large-messages-in-rings-of-their-own.md)). The operator
also wants to see them live, beside the robot moving on its model: a
compressed frame is tens or hundreds of kilobytes (about 360 KB for a busy
1080p JPEG), a raw one up to several megabytes (900 KB for 640x480 RGB),
at 30 to 60 frames a second.

The live view's samples reach the viewer bridge through the capture
process's tap: datagrams on a Unix socket, one per sample, never waited on
([viewer](a-viewer-of-our-own.md)). A datagram is at most 64 KB, and a
Unix socket's buffer only a few hundred: no frame fits.

What has to hold:

- **Recording never waits for the live view**, and spends nothing on it
  per message: anything the live view needs is taken off the capture path,
  at the display's rate at most, and dropped and counted when the viewer
  is behind.
- **The page draws what it can.** A browser decodes JPEG itself; a raw
  frame is drawn only once turned into pixels, and sent as it is it costs
  about 28 MB/s for one 640x480 camera at 30 fps, too much for a page on
  another machine.
- **A frame is stored as received**: a ROS 2 camera's frame is a whole
  `sensor_msgs/msg/CompressedImage` or `Image` message, not the picture
  alone. Capture never parses it.
- **Memory is bounded and declared** when capture starts.

## Approach

**Capture hands each camera's newest frame to the bridge over a stream
socket of its own; the bridge takes the picture out, turns a raw one into
JPEG, and sends it to each page over the live view's WebSocket.**

- **Which streams are cameras.** A ROS 2 stream of type
  `sensor_msgs/msg/CompressedImage` or `sensor_msgs/msg/Image`, and a TCP
  stream that says so: `camera = { format = "jpeg" }`, or
  `camera = { format = "raw", width, height, encoding }` for raw frames
  of a fixed layout.
- **The writer offers the newest frame.** As it writes a batch, the writer
  thread copies a camera's newest frame into a slot set aside for that
  camera (two, of its `max_message_bytes`), at most
  `viewer.camera_max_fps` times a second (30 by default), and only into a
  slot the tap thread has finished with; otherwise the frame is skipped and
  counted. Nothing happens on the receivers, and the copies are bounded by
  the cap, not the camera's rate.
- **The tap thread sends it.** It writes each frame offered, with its
  stream and receive time, to the bridge's frame socket
  (`viewer.frame_socket`, beside the tap socket), a Unix stream socket,
  without blocking: a frame written in part is finished before another is
  started, and meanwhile only the newest frame of each camera waits; older
  ones are skipped and counted. With no bridge listening, frames are
  counted as not sent and the tap carries on.
- **The bridge takes the picture out.** It reads each frame as its stream
  declares it: a `CompressedImage`'s `format` and `data`, an `Image`'s
  size, encoding and pixels, or a TCP camera's bytes as declared. A raw
  frame in `rgb8`, `bgr8`, `mono8` or `rgba8`/`bgra8` is encoded as JPEG
  in the bridge, never in capture; another encoding, or a compressed
  format the browser can't draw, is counted and named in the live view
  rather than shown.
- **The page draws it.** Each picture goes to a page on the live view's
  WebSocket as a message of its own, beside the frames of poses and
  samples: the camera's position among the streams, when Kapto received
  the frame, its size, and the JPEG. The page decodes it with the
  browser's own decoder and draws the newest one per camera. As for every
  message to a page, each page has room for a few; a picture that doesn't
  fit is dropped for that page and counted, and the bridge never waits on
  a page.
- **Counted end to end.** Per camera: frames offered, skipped by the
  writer, skipped or not sent by the tap, encoded and sent by the bridge,
  dropped for a page. The live view's health shows them, as it shows
  every stream's.

## Consequences

- An operator sees each camera live beside the robot, at up to
  `viewer.camera_max_fps`, on any page, with the delay from its arrival
  shown as for the robot.
- Capture holds two frames of each camera's size beside its rings, counted
  in the memory it declares at start, and copies at most the cap's worth of
  frames a second, on the writer thread.
- A second socket between capture and the bridge, with a setting of its
  own; the tap thread writes to two sockets.
- The bridge depends on the JPEG encoder the synthetic camera already uses,
  and spends about 10 ms of one core per 640x480 raw frame on it, off the
  recording path.
- The viewer stream's specification gains a message kind for pictures. A
  page that doesn't know it skips it, so the stream's version stays.
- A TCP camera declares its frames' format in `streams.toml`; a ROS 2
  camera needs nothing beyond its type.
- The page shows raw cameras through JPEG, so the live view's pixels aren't
  exact; the recording keeps every frame as received.

## Alternatives considered

- **Shared memory slots.** Capture writes each camera's newest frame into a
  shared-memory file, framed by a sequence number before and after so a
  torn read is noticed, and the bridge reads the slots at its rate. No
  copy through the kernel, but `unsafe` memory-mapped code and a new
  dependency in capture, for frames that are copied at most a few dozen
  times a second.
- **The bridge subscribes itself.** The bridge takes ROS 2 cameras from the
  robot directly, beside capture. Nothing new in capture, but it works
  only for ROS 2 (a TCP camera sends to one receiver), and the middleware
  delivers every frame twice.
- **Larger datagrams.** A Unix datagram is bounded by its socket's buffer,
  which the operating system caps at a few hundred kilobytes unless the
  machine is reconfigured: a 1080p JPEG doesn't fit, a raw frame never.
- **Raw frames as they are**, drawn on a canvas, capped at a lower rate:
  exact pixels and no encoder in the bridge, but tens of MB/s to a page,
  too much over a network.
- **Encoding in capture.** Turning raw frames into JPEG where they arrive
  would put milliseconds of work per frame on the recording path.
