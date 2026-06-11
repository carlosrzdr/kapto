# Cameras in replay

## Context

A recorded episode is watched again in the web UI's player: the robot and
its plots, from a replay view the worker makes once from the recording
([viewer](a-viewer-of-our-own.md)), on the episode's receive-time timeline.
A camera recorded beside them ([large messages](large-messages-in-rings-of-their-own.md))
should play there too, in step with the robot.

What has to hold:

- **Every frame can be seen.** The live view shows a camera at
  `viewer.camera_max_fps`; a replay is where an operator steps through
  what happened, so every recorded frame is there, one at a time if asked.
- **In step with the robot**: the frame shown is the camera's newest
  received at or before the cursor, on the same receive-time timeline.
- **A recording stays as it is**: frames are stored as received
  (a `CompressedImage`, an `Image`, a TCP camera's message with its length
  prefix), and nothing is added to an episode to replay it.
- **Memory stays bounded** however long the episode: an hour of two
  1080p cameras at 60 fps is about 150 GB of frames.
- **The page draws what it can**: JPEG or PNG, as in the live view.

## Approach

**The server reads a camera's frames straight from the recording, by its
MCAP indexes; the player asks for a camera's frame times once, then for
each frame it shows.**

- **Which streams are cameras** comes from the `streams.toml` the
  recording carries ([what a recording carries](recordings-carry-what-they-were-recorded-with.md)),
  as for the live view: a ROS 2 `CompressedImage` or `Image` topic, or a
  TCP stream with a `camera` table. The replay's opening lists them.
- **The frame times.** An MCAP recording ends with an index of its chunks,
  each with, per channel, the receive time and place of every message in
  it. The server reads those for the camera's channel, never a frame, and
  answers with every frame's receive time in order:
  `GET /api/episodes/{id}/cameras/{camera}/frames`. Kept for the episodes
  asked about last, in a cache of fixed size.
- **A frame.** `GET /api/episodes/{id}/cameras/{camera}/frames/{n}` reads
  the one chunk that holds frame `n` (about a megabyte), takes the frame
  out, and answers with its picture as the live view's bridge would: a
  compressed one as recorded, a raw one encoded as JPEG there. A file is
  read in pieces, never whole.
- **The player** shows, for each camera, the frame at or before the
  cursor: while paused, that one; while playing, it asks for the next only
  once the one before has come, so a slow link shows fewer frames, never
  late ones. Stepping moves the cursor to the camera's previous or next
  frame time, every frame recorded in turn.

## Consequences

- Every frame of an episode can be seen, as recorded for compressed ones,
  without a replay of its own: a camera plays as soon as the recording is
  finalised, before the worker has made the replay view.
- Opening a camera reads its chunks' indexes: tens of thousands of small
  reads for an hour of video, once per episode while it is cached.
- Each frame shown costs a chunk read and decompressed on the server, and a
  JPEG encoding for a raw camera; playing at 30 frames a second, a few
  percent of a core.
- An episode recorded before a stream was declared a camera doesn't play it.
- The API gains two endpoints and the replay's opening a field, both
  additive.

## Alternatives considered

- **Frames in the replay view.** The worker copies each camera's frames
  into the view as it makes it. One read per frame and the view's own
  format, but a second copy of every frame, as large as the recording,
  in a cache meant to stay small, and nothing to play until it is made.
- **Only the frames at the live view's cap**, decimated into the replay
  view: small, but not every frame, which is what replay is for.
- **The whole file mapped into memory** to read it with the MCAP library's
  own index functions: simpler code, but a mapping as large as the
  recording in the server, and its pages held while frames are read.
- **Video in the browser**, the frames re-encoded as a video stream:
  smooth playback, but a codec and its encoder on the server, and frames no
  longer exactly as recorded.
