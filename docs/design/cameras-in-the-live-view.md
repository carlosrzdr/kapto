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
- **A page's link is shared.** A 1080p JPEG is about 360 KB: at 30 a
  second, five cameras send a page about 54 MB/s on the WebSocket that also
  carries the robot's frames, which then wait behind the pictures (564 ms
  at the 95th percentile, measured on one machine, against 150 ms allowed).
  More cameras must mean fewer pictures of each, not a later robot.
- **A frame is stored as received**: a ROS 2 camera's frame is a whole
  `sensor_msgs/msg/CompressedImage` or `Image` message, not the picture
  alone. Capture never parses it.
- **Memory is bounded and declared** when capture starts.
- **Looking back shows the cameras too.** The live view can be paused and
  looked back over the last `viewer.history_age` (ten minutes by default),
  as the bridge keeps the robot's poses and the plotted values for that
  long. The cameras should show the moment looked at, not carry on live.
  Ten minutes of two 1080p cameras at 60 fps as received is about 25 GB.

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
  camera (three, of its `max_message_bytes`), at most
  `viewer.camera_max_fps` times a second (30 by default), and only into a
  slot the tap thread has finished with; otherwise the frame is skipped and
  counted. Three slots keep the newest frame the one that waits while the
  bridge is slow: one is being sent, one waits, and the writer fills the
  third, which then takes the waiting one's place. With two, the writer
  would skip every newer frame, and the frame sent next would be as old
  as the bridge's delay. Nothing happens on the receivers, and the copies are bounded by
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
  in the bridge, never in capture. A ROS 2 depth image (`16UC1` or
  `mono16` in millimetres, `32FC1` in metres) is shown in grey, its
  nearest reading white and its farthest dark, scaled to that picture's
  own range, with no reading (0, or a float that isn't a positive
  distance) black. Another encoding, or a compressed
  format the browser can't draw, is counted and named in the live view
  rather than shown.
- **A raw frame is encoded only when its picture is wanted.** Encoding is
  the bridge's one costly step, about 17 ms a 1080p picture, so a raw
  frame is encoded whole only if a page's share of its camera would take
  a picture by the time the camera's next frame comes. If the camera's
  thinned history is due one, it is made from the frame's pixels, small
  first (below); a frame neither wants is counted as not encoded. With no
  page open, only the history's small pictures are made, and a page
  connecting is sent its first picture from the next frame. One thread encodes for
  every camera, as it reads: while it does, capture keeps only each
  camera's newest frame waiting, so the pictures shown stay current. A
  compressed picture costs nothing to pass on, and is passed on as it is
  read.
- **A page's cameras share its pictures.** The bridge sends each page at
  most `viewer.page_pictures` pictures a second across all its cameras (60
  by default), each camera an even share, never more than
  `viewer.camera_max_fps`: 30 each with two cameras, 12 each with five. A
  camera's picture is sent to a page once its share's interval has passed
  since that camera's last one to the page; until then only its newest
  waits, newer ones replacing it, so a page always gets the newest picture
  and never a backlog. Pictures held back this way are counted per camera
  as thinned for the page, apart from those dropped because the page was
  slow. The share is the bridge's, per page: capture's cap stays per
  camera, as it bounds the copies on the recording path, and two pages
  with different cameras open don't hold each other back.
- **The page draws it.** Each picture goes to a page on the live view's
  WebSocket as a message of its own, beside the frames of poses and
  samples: the camera's position among the streams, when Kapto received
  the frame, its size, and the JPEG. The page decodes it with the
  browser's own decoder, in a worker off its main thread, and draws the
  newest one per camera. As for every
  message to a page, each page has room for a few; a picture that doesn't
  fit is dropped for that page and counted, and the bridge never waits on
  a page.
- **Counted end to end.** Per camera: frames offered, skipped by the
  writer, skipped or not sent by the tap, encoded and sent by the bridge,
  dropped for a page. The live view's health shows them, as it shows
  every stream's.
- **The bridge keeps a thinned history of each camera.** Of the pictures
  it sends, it keeps one every `1 / viewer.camera_history_fps` seconds (5
  a second by default) per camera, made no wider than
  `viewer.camera_history_width` (640 pixels by default): a raw picture by
  keeping one pixel in so many across and down (three for 1080p), then
  encoding that, a ninth of the whole picture's cost; a compressed one by
  decoding it at a fraction of its size with the JPEG decoder's own
  scaling (a half, a quarter or an eighth, which costs a fraction of a
  full decode), then encoding it as JPEG again. It keeps them for `viewer.history_age`, within
  `viewer.camera_history` MiB for every camera together (128 by default),
  the oldest going first, so a budget too small for the whole window keeps
  a shorter one. A PNG picture isn't kept.
- **A page asks for the one nearest the moment it looks at.** Paused or
  looking back, a page asks, per camera, for the picture kept nearest a
  time, and is answered with that picture, when Kapto received its frame,
  and its size, or that none is kept. It asks again as the moment changes,
  one question per camera at a time, the newest moment replacing a
  question not yet asked. Back to live, it shows the newest picture again.

## Consequences

- An operator sees each camera live beside the robot, at up to
  `viewer.camera_max_fps`, fewer the more cameras a page shows (up to
  `viewer.page_pictures` a second for all of them), on any page, with the
  delay from its arrival on hover, and in its caption once it is long, as
  for the robot, which stays within the live view's budget however many
  cameras there are.
- A new deployment setting, `viewer.page_pictures`, and a count per camera
  of pictures thinned for a page, beside those dropped.
- Capture holds three frames of each camera's size beside its rings, counted
  in the memory it declares at start, and copies at most the cap's worth of
  frames a second, on the writer thread.
- A second socket between capture and the bridge, with a setting of its
  own; the tap thread writes to two sockets.
- The bridge depends on the JPEG encoder the synthetic camera already uses,
  and spends about 17 ms of a core per 1080p raw picture on it, off the
  recording path: only as many pictures as its pages' shares and the
  cameras' histories take, on one thread, so raw cameras take at most a
  core of the bridge. Five raw 1080p cameras shown on a page fill it, at
  about the page's share; with no page open it reads them all and encodes
  about one frame in five, for the histories. A thread per camera was
  tried: on a laptop shared with the recording and the browser, each
  picture then took three times as long, the bridge 3.7 cores, and the
  page's delay rose from 20 ms to 114 ms, for no more pictures.
- The viewer stream's specification gains a message kind for pictures. A
  page that doesn't know it skips it, so the stream's version stays.
- A TCP camera declares its frames' format in `streams.toml`; a ROS 2
  camera needs nothing beyond its type.
- The page shows raw cameras through JPEG, so the live view's pixels aren't
  exact; the recording keeps every frame as received.
- Looking back, the cameras show the moment looked at, at up to
  `viewer.camera_history_fps` and `viewer.camera_history_width`: enough to
  see what the robot saw, not every frame, which a replay of the recording
  shows. Two 1080p cameras at the defaults keep about 30 KB a picture, 10
  pictures a second: 180 MB for ten minutes, so the default budget keeps
  about seven of them.
- The bridge makes 5 small pictures a second per camera for looking back,
  a few milliseconds each, off the recording path, and depends on a JPEG
  decoder as well as its encoder for compressed cameras. A raw picture
  made small by leaving pixels out, without blending, can show jagged
  edges; it is for finding a moment, which a replay shows whole. Five raw
  1080p cameras with no page open take the bridge 0.20 cores, reading
  their frames and making these, where encoding each whole first took
  0.85.
- The viewer stream gains a question and its answer; a page that doesn't
  ask is sent nothing more.

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
- **Two slots, and stale frames dropped.** The tap thread frees a waiting
  frame older than one interval rather than send it, so the writer can
  fill the slot with a newer one. No third frame of memory, but a camera
  that pauses or stops would never show its last frame.
- **Encoding in capture.** Turning raw frames into JPEG where they arrive
  would put milliseconds of work per frame on the recording path.
- **Each page keeps the pictures it was sent.** Nothing new in the bridge,
  but a page could look back only over what it saw since it opened, and
  ten minutes of full-size pictures don't fit in a browser's memory; made
  smaller in the page, every page would do the bridge's work again.
- **Looking back in the recording.** While an episode is recorded, its
  frames are on disk, every one exact, and replay already reads them. But
  between episodes nothing is recorded, and looking back is mostly done
  then, to see what just happened.
- **`viewer.camera_max_fps` as the total.** One setting fewer, but it
  caps the copies capture's writer makes per camera, on the recording path;
  as a total split among cameras, adding a camera would change what
  capture does for the others, and the link that needs sharing is the
  page's, not capture's.
- **Pictures shrunk to the width each page shows.** The bridge decodes
  each picture at a fraction of its size and encodes it again for each
  page, as it already does for the history, sending 10 to 20 times fewer
  bytes; but at 30 pictures a second per camera that is real CPU in the
  bridge. Worth measuring once the shared rate is in place, not instead
  of it.
- **WebRTC for what the live view shows.** Each camera a video track
  (about 4 to 10 Mbit/s for 1080p) with congestion control, the robot's
  frames on an unordered data channel, lossy as every tap to the viewer is
  while the recording keeps every frame as received; counts, status and
  looking back staying on the WebSocket. It needs a video encoder in the
  bridge (hardware, or a large dependency) and a WebRTC stack, and reaches
  only pages on the robot's network, as Kapto makes no outbound connection
  for STUN or TURN. Kept for when many cameras must be watched over slow
  links.
- **Kept as received, at a lower rate.** No decoding or encoding in the
  bridge, but a 1080p picture is about twelve times the size of one 640
  pixels wide, so the same budget would keep less than a minute.
