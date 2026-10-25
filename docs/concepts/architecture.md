# Architecture

This page shows the parts of Kapto that exist today. It is updated whenever
that changes.

## What exists

Kapto is a single binary, `kapto`, built from a Cargo workspace. It records
UDP, TCP and ROS 2 streams into finalised episodes, shows them live and in
replay in its web UI, checks each episode, and uploads it to the storage
configured. Selection (what is worth keeping) doesn't exist yet.

```
kapto (binary, crates/kapto-cli)
 │  commands: config, record, capture, episode, package, profile, task,
 │            decode, demo, showcase, serve, viz-bridge, worker, topics,
 │            export, upload
 │  flags for every setting
 ▼
kapto-server ─── supervisor, REST/WebSocket API, the embedded web UI
kapto-capture ── rings, loss accounting, the MCAP writer, finalisation
kapto-transport  UDP, TCP and ROS 2 receivers
kapto-viz ────── the live view's bridge and replays
kapto-qa ─────── checks, statistics and anomaly detectors
kapto-upload ─── uploads through object_store
kapto-robot ──── robot packages, URDF, meshes, forward kinematics
kapto-decode ─── payloads into named values; camera frames
kapto-store ──── kapto.db, episode.json, manifests
kapto-config ─── kapto.toml, environment variables and flags, each
 │                setting's source; streams, mappings, quality settings
 ▼
kapto-core ───── shared types: error codes and reports, episode and
                  stream IDs, roles, receive and source timestamps,
                  reading the host's clocks and mapping them to UTC
```

The recording path starts with a **transport** that receives messages, and
the **ring** that carries them from a receiver thread to the writer thread.

`kapto-transport` defines what every input provides (the `Transport` trait)
and its first implementation, **UDP**: one receiver thread listens on any
number of ports (IPv4 or IPv6, unicast or multicast), waits on all of them at
once, and reads every waiting datagram straight into the free slots of a
batch, many per system call on Linux. Each port feeds one stream. A datagram
too long for a slot is discarded and counted against its stream as dropped,
never stored cut short. One from a source outside the port's allow-list
(`allowed_sources`) isn't the robot's: it is discarded and counted apart, as
the stream's `rejected` in `episode.json` and in the capture process's line
each second.

For TCP, which delivers a byte stream rather than messages, a **deframer**
cuts messages out of the bytes: by a length field in each message's header,
by a delimiter such as a newline, or by a fixed size. A message longer than
the stream allows is skipped and counted, and the deframer keeps its place,
so the messages after it come out whole.

The **TCP** transport carries one stream over one connection, opened either
way round: Kapto connects to the robot, or listens for the robot to connect.
A lost connection is counted against its stream as a disconnect, the
partial message it cut off is dropped, and Kapto connects again, waiting a
little longer after each failure in a row (with some randomness, so many
collectors don't retry in step). A robot that reconnects while its old
connection still looks open has restarted, and the new connection replaces
the old one. Each stream's disconnects are shown live and written to
`episode.json`.

The receiver thread runs the same loop for every transport: it fills one
batch until the batch is full or its first message has waited a couple of
milliseconds, counts it, and sends it to the writer. Each ring is sized to
absorb a two-second writer stall in batches handed over, not only in
messages: a stream slower than a batch waits sends nearly every message in
a batch of its own, so its ring gets a batch per message. If the writer
still fills three quarters of a ring, or anything is dropped, the next
phase's ring for that input is twice as large, up to sixteen times its
planned size, and capture says so. Receiving, counting and sending allocate
no memory once running.

```
UDP ports ──▶ receiver thread                              writer thread
               fills a Batch ──▶ [ full batches  ]──▶ takes it, writes it
                     ▲                                         │
                     └─────────── [ empty batches ]◀───────────┘  returned automatically
```

A fixed set of batches (fixed blocks of memory, allocated once) circulates
between two lock-free queues, so passing data along never allocates or
waits. If the writer falls behind and no empty batch is left, the receiver
keeps reading, discards the batch it was filling, and counts every discarded
message against its stream: recording never waits, and loss is never silent.

Every message of every stream is accounted for by five counters, readable
live from any thread:

| Counter | Counts |
|---|---|
| `received` | Messages that reached Kapto |
| `decoded` | Received messages whose live fields, such as the sequence number, could be read |
| `written` | Received messages stored in the recording |
| `dropped_by_us` | Received messages Kapto discarded, e.g. because the writer fell behind |
| `lost_upstream` | Messages the source sent that never reached Kapto |

The receiver thread updates them once per batch. `lost_upstream` comes from
the sequence numbers a source writes into its messages, if it does: numbers
skipped count as lost, a number arriving late takes one back, repeats count
nothing, and the count follows numbers that wrap around or restart from zero.
For a stream without sequence numbers but with a declared rate, it is
estimated from silences lasting several periods, each counted once the
messages it only delayed have had time to arrive (a second, or the stream's
`loss_settle_ms`), and when its receiver ends. Silences are measured on the
messages' sending times when they carry them (ROS 2's source timestamps),
which a delay on the way doesn't move, and otherwise on their receive times;
a sending time that jumps, as a source's clock being set does, starts the
stream's count afresh. Over a ROS 2 middleware that gives each message
its publisher's count of it (Fast DDS and Zenoh do, Cyclone DDS doesn't),
a stream's loss is counted by those counts instead, each publisher's on its
own: every number skipped is a message lost on the way, however short the
gap, which a rate can't tell from jitter, and a publisher pausing loses
nothing; `episode.json` names such a stream's detection `publisher_count`.
What a publisher sent before Kapto's subscription matched it can't be
counted: its first number seen tells nothing of that. A stream with neither
can't tell its upstream loss, and its counter stays at zero.

The writer thread stores what it takes from the rings in an **MCAP**
recording, the format of ROS 2 bags, readable with the `mcap` command-line
tool and Foxglove:

- before the first message, two metadata records that say what the
  recording is, so the file alone says so even if it is cut off:
  `kapto.episode` (its ID, Kapto's version, the robot package, profile
  and task) and `kapto.capture` (how it is written and buffered; see
  [Recording format](../reference/recording.md)), and the robot package's
  files as attachments, as they were when capture loaded the package
  (meshes and textures are left out, each named with its SHA-256, unless
  `capture.attach_meshes` is on);
- one channel per stream, named after the stream, with each payload stored
  byte for byte as received;
- each message's time is when Kapto received it, mapped to UTC through the
  episode's clock map, which is itself stored in the file as a
  `kapto.clock_map` metadata record;
- when recording ends, before the file is closed, a `kapto.summary` record
  with the same counts as `episode.json`, so a recording without it is
  recognisably unfinished;
- messages are written in compressed chunks, as `capture.compression`
  says: lz4 by default, zstd, or none, which spares the CPU recompressing
  a camera's JPEG frames, since a chunk holds every stream's messages and
  is compressed as a whole. Every half second the writer closes the current chunk and makes
  the operating system put the file on disk, so a power cut loses at most
  about half a second, and a file cut off part-way is still readable up to
  its last complete chunk. How long messages actually waited to be on disk
  is recorded in `episode.json`, along with how full the buffers got;
- if the disk fills up or fails, the writer keeps taking batches so
  receivers never wait, and counts every message it couldn't store as
  `dropped_by_us`. To keep that from happening, the capture process reads
  the disk's free space once a second (never per message): below
  `capture.disk_warn_below` it warns, and below `capture.disk_stop_below`
  it finalises the episode cleanly and starts no other, so a recording is
  never cut off by a full disk. `/api/storage` reports both limits, and the
  web UI counts the time left down to the second one.

Before recording, **pre-flight** checks that this machine can record what
the robot sends: room on the disk for `capture.preflight_disk_time` of
recording, a written-and-flushed test file at least as fast as the streams
send, and every declared stream arriving at roughly its declared rate. The
capture process runs it once when it starts, watching the streams for
`capture.preflight_window` while a `cap-preflight` thread writes the test
file, and again whenever asked (`kapto episode preflight`, or `POST
/api/preflight`), but never during an episode, whose writes the test would
slow. A start asked for while it runs waits for it. Each check names the
cause code it fails with, and `/api/status` carries the latest run. While a
check of it failed, a start is refused with `capture.preflight_failed`
unless it waives each failed check (`kapto episode start --waive <code>`,
or `"waive"` in the API's start), or `capture.preflight` is `warn`; the
episode keeps the run, saying what failed and what was waived, in
`episode.json` and in its recording.

**Events** say what happened at a moment or over a stretch: an action
began or ended, an operator noted something. A client sends one on the
control socket (`event`); the capture process checks it, tells every
watcher, and during an episode hands it to `cap-writer` over a queue of
its own, which writes it into the recording on the `kapto.events` channel
between the streams' batches, so no stream's messages wait on it. Every
recording has that channel, and the reader keeps it, like every channel
named `kapto.…`, out of what decoding, the checks, replays and recovery
read as streams ([recording format](../reference/recording.md#events-kaptoevents)).

**Kept messages** hold for every episode. A stream whose messages are
kept for late subscribers (a ROS 2 topic subscribed to as
`transient_local`, such as `/tf_static`) may send them once, while
capture idles between episodes. `cap-writer` keeps the newest of each
such stream as it takes the batches, in every phase, in memory set aside
when capture starts (`kapto_capture::Kept`), and each episode starts with
them, written at its start, with when they were received in the
`kapto.carried` record ([recording format](../reference/recording.md#kaptocarried)).

**Pre-roll** holds a stream's last seconds for the next episode
([design](../design/pre-roll-and-post-roll.md)). `cap-writer` copies each
message of a stream with `pre_roll_s` into a byte ring set aside when
capture starts (`kapto_capture::PreRoll`), in every phase, the oldest
giving way. Capture notes when each Start is asked; the episode then
starts with each such stream's messages from its seconds before that up
to when recording began, each at its own receive time, and spans its
data from the first of them. Every other stream gets a ring too, held
only while a Start waits, from when it was asked: an episode with a
pre-roll starts every stream at its Start, however long recording took
to begin. The receiver threads' loss detection records each gap it finds
in the batch it was found in (`kapto_capture::Gap`); the pre-roll holds
it with the messages, and the episode counts each gap's share of time
within its span as lost.

**Post-roll** records a stream on after Stop. Capture notes when Stop is
asked (`kapto_capture::PostRoll`); each receiver thread reads it once per
batch and stamps it into the batch, and both its loss accounting and
`cap-writer` leave out a stream's messages from after its `post_roll_s`,
so the counts and the recording agree. The episode is finalised once the
longest post-roll has passed, on a thread of its own, `cap-finalise`
([design](../design/closing-an-episode-beside-receiving.md)): capture
hands it the open recording and starts the next idle phase at once, so
receiving and the live view go on while the file's index is written and
synced, `episode.json` written, the folder moved and every byte hashed for
the manifest. The thread runs at the worker's lower priority and closes
one episode at a time, in the order they stopped; capture answers each
Stop, and announces each end, once its episode is finalised. A Start
meanwhile is accepted, and SIGTERM waits for every episode handed over.

Each recording is written in a staging folder, then moved, listed in a
manifest with every file's checksum, and marked `_FINALISED` last, so a
partial recording never looks finished. [Episodes on disk](episodes.md)
describes the steps and the rule for when an episode is complete. The
process writing an episode holds its folder locked until it is finalised;
the lock goes when the process does, however it ends. So an unfinished
folder nobody holds was left by a crash, a kill or a power cut, and the
next `kapto record`, or `kapto capture` on a `cap-recover` thread while it
receives, recovers it: the recording salvaged to its last intact part,
`episode.json` written from what it carries and marked `recovered`, and
the episode finalised. Capture announces each one to every watcher, those
connecting later included, so serve lists it and queues its checks.
`episode.json` also names the robot package the streams were declared in,
when their `streams.toml` is the one a `package.toml` in the same folder
names: its ID, version and content hash as recording started, so an
episode can always be matched to exactly the files it was recorded with.
Streams declared outside a package are still recorded, and the episode is
`unchecked`.

`kapto-decode` turns payloads into named values. A stream can declare its
message layout in `streams.toml`; the recorder stores that layout in the
recording (as the MCAP channel's schema), so every recording can be decoded
later: `kapto decode` reads a recording, piece by piece so its size doesn't
matter, and prints each stream's values as a table or CSV. While
recording, only the live view's tap decodes, off the recording path
(below).
A **struct layout** describes a message with a fixed binary layout: each
field at a byte offset, with a type, byte order, optional array length,
scale and unit. A layout is checked once (every problem reported with its
field) and compiled into one read per value, so decoding a message is a
short loop that never allocates. **JSON and MessagePack** messages declare
each field's path through the message instead (`joints.position[2]`), read
through serde without building the message, with one set of conversion
rules for both. **CDR**, as ROS 2 serialises messages, is read with the
message's `.msg` definitions as rosbag2 stores them, through the same path
follower; it is checked against messages ROS 2 itself serialised
(`cargo xtask golden cdr`), and is how a ROS 2 topic's messages are decoded. **Protobuf** is
read with a descriptor set and found by path the same way, checked against
messages encoded by buf; no stream declares it yet either. Payloads are
always recorded exactly as received; decoding only reads them.

A robot is described by a **robot package**: a folder with a
`package.toml` (its ID, version and where its URDF is), the URDF and
meshes, `streams.toml` and `mapping.toml`. `kapto-robot` loads packages
from the folders of `paths.packages`, and identifies a version of each by
a SHA-256 over all its files, in a form `sha256sum` can check
(`kapto package list`, `kapto package show`; see the
[package format](../reference/package.md)). `kapto package lint` loads a
package the way Kapto does to run it and reports every problem at once,
each with its cause code, file and line, and what to do: errors that stop
Kapto using the package, such as a mapped joint the robot doesn't have, and
warnings, such as a missing mesh or a joint without a velocity limit.

`kapto-robot` loads a robot's URDF into Kapto's own robot model: its links,
the joints between them (with their axes, limits and mimicked joints) and
the shapes each link is drawn with. The joints must form one tree from a
single root link, as an arm on a base or a humanoid from its pelvis does;
every way they don't is reported at once. Mesh and texture files are found
the way ROS finds them (`package://` paths by folder name or by the name
a folder's `package.xml` declares, through links too, relative paths,
extra search folders), and a missing one is listed rather than stopping
the load. `kapto package import` makes a package of a robot's own
description: the URDF with only the files it names, laid out so each is
found as written, a description in xacro expanded to URDF first
(`xacro-rs`); Kapto only ever runs from URDF.
Mesh files in STL, OBJ, COLLADA (`.dae`) and glTF (`.gltf`, `.glb`) load
into one triangle-mesh type with colours and normals. A COLLADA file in
millimetres is scaled to metres. Meshes stand as rviz shows them: COLLADA's
`<up_axis>` is left alone, and glTF, Y up by its specification, is turned
into the Z up of robot descriptions. glTF buffers are read from the file
itself or next to it, never downloaded.
Forward kinematics turns joint values into every link's pose. The tree is
walked once when the robot is loaded, with mimic joints resolved to the
joint they copy, so computing poses is a single pass that never allocates;
a batch form computes many samples at once. Its results are checked against
ROS 2's robot_state_publisher (`cargo xtask golden fk`). A package's
`mapping.toml` says which decoded value drives which joint; it is checked
against the robot and its streams and turned into where each joint's value
sits among a stream's decoded values. The viewer bridge and the quality
checks, below, use them; recording doesn't.

`kapto-select`, `kapto-py` and `kapto-wasm` exist but are empty: each has
a one-line description in its `Cargo.toml` of what it will hold.

It is also packaged as two container images built from one Dockerfile
(`docker/Dockerfile`): `kapto`, on a slim Debian base, and `kapto-ros2`, on a
ROS 2 base image. The ROS 2 image's Kapto is built with the `ros2` feature,
linking ROS 2's own client library (rcl) through `r2r`, so it joins the
robot's ROS 2 network as the robot's nodes do, over whichever middleware
`RMW_IMPLEMENTATION` names: the image has Fast DDS (the default), Cyclone
DDS and Zenoh ([the ROS 2 design](../design/ros-2-through-rcl.md)). It
lists the topics there (`kapto topics`) and records them, cameras included.
The slim image's Kapto has no ROS 2 code.
Building with the feature needs ROS 2 and libclang, so it is only built
and checked in the ROS 2 image (`cargo xtask ros2`). The node is torn down
in a few lines of C++ (`teardown.cpp`): a C++ middleware such as Zenoh's
may throw as it closes, and an exception that reached Rust would abort
Kapto rather than let the receiver end.

For testing without a robot, `tools/kapto-loadgen` is a synthetic robot: it
sends any number of streams at a fixed rate (2 kHz by default), each
message carrying a sequence number, a timestamp and seven joint positions,
and reports how punctually it kept to the schedule. Streams go over UDP, or
over TCP with each message after its length in 4 bytes, the generator
either connecting to Kapto or listening for it (`--transport`). Built with
ROS 2 (its `ros2` feature, in the ROS 2 image), it publishes each stream as
`sensor_msgs/msg/JointState` on `/joint_states`, then `/joint_states_1` and
on (`--transport ros2`), serialised by ROS 2 itself; with
`--wait-for-subscribers` it waits for every topic to have a subscriber
before its first message, so none is published before it can be
received. With `--camera 640x480` each stream is a camera instead: a
second of JPEG frames of a moving picture, encoded once and played in a
loop, each frame a message over TCP or a `sensor_msgs/msg/CompressedImage`
on `/camera/image_raw/compressed` over ROS 2; with `--raw` too, the same
pictures' pixels in `rgb8`, uncompressed, a `sensor_msgs/msg/Image` on
`/camera/image_raw` over ROS 2. `packages/arm7` is the
robot those joint positions belong to: a seven-joint arm described in URDF,
with a `streams.toml` that declares the synthetic robot's stream and its
message layout. `packages/arm7_ros2` is the same arm as a ROS 2 robot: its
joint states on `/joint_states`, as the synthetic robot publishes them
with `--transport ros2`.

## See it working

```sh
cargo run --quiet --bin kapto -- config show
cargo run --quiet --bin kapto -- config validate --port 0     # an invalid value, reported with a remedy
docker compose build && docker run --rm kapto:dev
cargo run --release -p kapto-loadgen -- --streams 16 --duration 5
```

While recording, a **live tap** feeds the viewer without ever slowing the
recording down. Once per batch the writer thread copies each stream's newest
message into one of a few preallocated slots; with none free, it skips the
message and counts it. A `cap-tap` thread decodes every message offered
with its stream's declared layout and, about sixty times a second, sends
one small datagram per stream to the viewer's Unix socket (setting
`viewer.tap_socket`): the newest values, with each value's minimum and
maximum since the last datagram, so however fast a stream is the viewer
gets at most sixty a second and still sees a spike that lasted one batch.
Sending never waits: a sample the viewer is too slow
to take, or that no viewer is there to take, is dropped and counted. The
counts are printed live once a viewer is reading, and at the end. The
tap thread also keeps each stream's newest values, which capture's
`status` answers with, so the web UI can show where each joint is.

A camera's frames are far larger than a datagram, so they take a path of
their own ([cameras in the live view](../design/cameras-in-the-live-view.md)).
A stream is a camera when it is a ROS 2 topic of a camera's type or a TCP
stream with a `camera` table. As it offers each batch, the writer copies
each camera's newest frame into one of three slots set aside for it, at
most `viewer.camera_max_fps` times a second (30 by default); the batches
between only look for a frame. In what is left of each interval between
its sends of samples, `cap-tap` writes the frames to the viewer's frame
socket (`viewer.frame_socket`), a Unix stream socket, as a short header and
the message as recorded: one frame at a time, finishing a frame cut off by
the interval's end before any other, keeping only each camera's newest
waiting. Frames the viewer is too slow for are replaced by newer ones and
counted; with no viewer listening, they are counted as not sent, and the
counts are printed live once a viewer takes a frame, and per camera at the
end. The bridge's `viz-cameras` thread reads them: it takes each picture
out (a `CompressedImage`'s, an `Image`'s, a TCP camera's as declared),
encodes a raw one as JPEG there, never in capture, and sends it to each
page as a message of its own beside the frames, with each camera's newest
kept for a page that connects. A raw frame is encoded only when a page's
share of its camera wants its picture; one the camera's thinned history
is due is made small from its pixels first. Each page gets at most
`viewer.page_pictures` pictures a second (60 by default) across its
cameras, each an even share no more than `viewer.camera_max_fps`: until a
camera's share allows its next, only its newest waits, so with more
cameras each is shown less often while the robot's frames, on the same
connection, aren't held up; pictures held back are counted apart from
those a slow page missed. A frame that can't be shown, such as an
encoding the bridge doesn't encode, is counted and named to the page. The
live view shows each camera's newest picture above the plots, with how long
it took to reach the page: decoded by the browser in a worker off the
page's main thread, one at a time per camera, so a slow browser shows
fewer pictures, never older ones; a camera that can't be shown says why
instead.

In replay, a camera's frames are read from the recording itself, not from
the replay view ([cameras in replay](../design/cameras-in-replay.md)): the
server finds every frame's receive time in the recording's MCAP indexes,
without reading a frame, keeps them for the cameras asked about last, and
reads one frame by reading only the chunk that holds it, encoding a raw one
as JPEG as the bridge does. The player shows each camera's frame at the
moment shown, asking for the next only once the one before has come, and
steps through every frame recorded, holding the replay where each is.

The **viewer bridge**, `kapto viz-bridge`, reads that socket and streams
the robot moving to the web UI's live view, Kapto's own viewer
([the viewer's design](../design/a-viewer-of-our-own.md)). It is a process of
its own: if it stops, stalls or is never started, recording carries on
unchanged.

```
kapto record ──cap-tap──▶ tap.sock ──▶ kapto viz-bridge ──▶ browser
                 (lossy, min/max)      viz-tap: newest     frames on
                                       joint values, FK,   viewer.stream_port
                                       ≤ 60 poses/s
```

When it starts, the bridge loads the robot (`kapto-robot`). Then, as
samples arrive on the socket (about sixty times a second, as the tap sends
them), it takes every sample waiting, keeps each joint's newest value
through the package's `mapping.toml`, and computes every link's pose with
Kapto's forward kinematics. The poses and each sample's extremes go, as
they arrive, to the live view: one binary frame per wake-up over a
WebSocket on `viewer.stream_port`, to pages of the web UI only, each with
room for a few frames and missing those it has no room for, so a slow page
never holds the bridge up ([viewer stream](../reference/viewer-stream.md)).
The page plots each decoded value with its minimum and maximum since the
sample before, in a tab per stream beside the robot; a stream's sequence
number and its clocks aren't plotted, as they only ever climb. The bridge
also keeps what it streamed for the last ten minutes
(`viewer.history_age`, within `viewer.history` MiB), in blocks of a
second, and a page can ask for any stretch of it merged to the points it
can draw, each value's minimum and maximum kept, so the operator can look
back at something odd; whatever the page keeps, its memory stays bounded
however long it is open. It also watches the capture process's control
socket, like any other client, for the events capture takes and each
episode's start and end, keeps them for as long, and tells every page all
it keeps when it connects, then each new one as it comes, for the
timeline. What surrounds the robot, meshes (glTF, STL, OBJ) and simple
shapes an operator places around it in the live view, is the page's alone:
kept in that browser's IndexedDB and never sent to Kapto. The robot's links
and shapes for it come from `/api/robot/scene`
([viewer scene](../reference/viewer-scene.md)), with how far the robot
reaches, which serve works out once at start, sampling joint values within
their limits through the same forward kinematics, so the page frames the
robot once and it stays in view as it moves. The view is arranged by the
layout `/api/robot` gives, the package's own or the one made from its
roles: columns of panels (the robot, a camera, a plot of fields or roles,
the streams' counts, the marks), one under another below 70rem; the
robot's 3D view is made once and moved to the panel that shows it. An
operator arranges their own view with *Arrange*: kept in that browser for
the package, before every other layout, until saved as the package's
default or reset. A
replay is arranged by the layout its recording carries, or one made from
what the recording declares, which the worker stores in the replay view. The page loads nothing from
elsewhere and its Content-Security-Policy lets it reach only Kapto's API
and the bridge's stream, so the view works offline and nothing leaves the
machine.

A recorded episode can be watched again in the web UI. The worker makes
its replay as a `replay` job, in one pass over the recording, into the
**replay cache** in the data directory: the episode's
[replay view](../reference/replay-view.md), `.cache/replay/<episode
ID>.view`, the robot's poses and each value's extremes at the display rate
and coarser steps, with the recording's events and the robot it was
recorded with, as the page draws it, all from what the recording carries,
from which the web UI's player shows it. The worker keeps the
shapes of the robot it last drew, with every mesh read, for the next
replay of an episode recorded with the same package (the same content
hash, which covers its meshes), so it reads no mesh again. It is outside
the episode's folder, so the episode stays as its manifest lists it; a
replay is made once its view is there, of the version this Kapto writes,
so one of another version is made again when asked for. It is written
beside its name and renamed once complete. The job waits
while an episode is recorded like any job, and isn't made while the disk has
less free space than `capture.disk_warn_below` (`viewer.replay_disk_low`).
Anything in the cache can go, and Kapto empties it itself: `wrk-sched`
trims it when the worker starts, after each replay is made and every minute
meanwhile, removing unfinished replays, replays of episodes that are gone,
every replay when the disk has less free space than
`capture.disk_warn_below`, replays unopened for `viewer.replay_cache_age`
days, and then, while the cache is over `viewer.replay_cache_size`, the
least recently opened, never the one opened last. A replay's modification
time is when it was last opened. A replay is asked for through the API
(`POST /api/episodes/{id}/replay`), which queues the job unless the replay
is made, queued or being made, and again if it failed or left the cache;
`GET /api/episodes/{id}/replay` says where it is, with what it lacks once
made. `/api/episodes/{id}/replay/window?from=&to=&points=` sends a stretch
of it from its view, marking it opened (`/replay/opening` names the links and
values a stretch holds, and `/replay/scene` and `/replay/meshes/{n}` the
robot it was recorded with), merged into the points asked for at
the coarsest step that has them, in the form the viewer stream answers a
look back with, so the page's player draws a replay as it draws the last
minutes. In the web UI, an episode's **Replay** asks for it, says it is
being prepared until it is made, then the live view replays it: it reads
the replay's opening, then asks for the stretch it shows as it plays, at
the points its plots have, with play, pause, speed and scrubbing on its
timeline, which draws the episode's events and every moment a check found.
Meanwhile the episode's button reads "Preparing…", then "Replaying", and
can't be pressed; the page's header and the timeline offer **Back to live**
(Esc), and marking events isn't offered, since marks go to what is recorded
now. A replay plays from its start and stops on its last moment; the bar
above it shows "Playing 0:04 of 0:09", "Paused at", or "Episode over" with
**Play again**. Every time range a check found is listed under its result
with **watch**, which opens the episode's replay a few seconds before that
moment and plays on; with the replay already shown it moves the same replay
there. A moment outside the episode plays it from its start. A replay draws
the robot it was recorded with, from `/replay/scene` and its meshes, and
**Back to live** the one Kapto runs again; each is loaded once and kept
while it can be shown again, and the camera is framed again only for a
robot that reaches elsewhere. A replay recorded without a robot shows its
values alone and says so.
[The viewer's design](../design/a-viewer-of-our-own.md) explains why replays
are Kapto's own.

To follow a message all the way: record the synthetic robot. `kapto record`
runs as one process: `cap-rx-udp-0` receives every UDP stream,
`cap-rx-tcp-0`, `cap-rx-tcp-1`, … each receive one TCP stream, every one of
them into its own ring; `cap-writer` writes all the rings into the
recording, and the main thread prints a live line once a second. The streams file declares each stream's port and rate;
[Getting started](../getting-started/README.md#first-recording) has one for
the synthetic robot.

```sh
cargo run --release --bin kapto -- record --streams streams.toml --duration 10 --data-dir /tmp/kapto &
cargo run --release -p kapto-loadgen -- --streams 4 --duration 5
```

To watch the sample arm move, run `kapto serve` (below), send the
synthetic robot's stream and open <http://127.0.0.1:7700>; the arm moves
whether or not an episode is being recorded.

`kapto capture` is the same recording path as a long-running **capture
process**: it opens the streams' inputs once and keeps receiving, feeding
the live view, whether or not an episode is being recorded. Episodes start
and stop on command, over a Unix socket (`capture.control_socket`), with
the [capture control protocol](../reference/capture-control.md);
`kapto episode start`, `stop`, `abort` and `status` speak it. An aborted
episode is ended like a stopped one and then deleted from staging, so
nothing of it is kept. Between episodes,
the writer thread only offers each batch to the live view and discards it.
Each episode gets fresh rings and counters, so its counts cover exactly
the messages it holds. When an episode starts, the writer thread stops
taking batches at once and hands the idle rings over with what is left in
them: the episode starts with those batches, counted as its own, so
nothing that arrived while the threads changed over is lost. Loss
detection goes on from the idle phase, so a gap across the start is
found, and counts for the share of its time after the episode's first
message. The inputs stay open in between, and the
operating system's socket buffers hold the few milliseconds until the new
receivers read them. SIGINT or SIGTERM finalises
the episode being recorded and ends the process.

```sh
cargo run --release --bin kapto -- capture --streams packages/arm7/streams.toml --data-dir /tmp/kapto &
cargo run --release -p kapto-loadgen -- --streams 1 --duration 30 &
cargo run --bin kapto -- episode start --data-dir /tmp/kapto
cargo run --bin kapto -- episode status --data-dir /tmp/kapto
cargo run --bin kapto -- episode stop --data-dir /tmp/kapto
```

`kapto serve` runs Kapto as its processes, all from the same binary: it
starts `kapto capture`, `kapto viz-bridge` and `kapto worker` as children
and keeps them running. A child that exits is started again after a second, then after
waits that double up to 30 s while it keeps failing, and from a second
again once it has run for a minute (`supervisor.restart_delay`,
`restart_max_delay`, `stable_after`). Ctrl-C, SIGINT or SIGTERM stops them
all: each gets SIGTERM, capture up to 30 s to finalise an episode
(`capture.stop_grace`), the bridge 5 s (`viewer.stop_grace`), the worker 5 s
(it stops at once), and whatever is left is killed. The robot's files are checked before anything starts,
and `serve` confirms that capture answers on its control socket in the
same protocol version. Children write straight to `serve`'s own output
rather than through it, so if `serve` itself dies, capture keeps recording
and can still be stopped with `kapto episode stop`. `serve` also keeps a
connection to capture open (`srv-watch`) and sends a sign of life on it
every second; if `serve` is gone for a minute (`capture.control_timeout`)
during an episode, capture finalises the episode, so nothing recorded
waits on a server that may not return, and keeps receiving until it does.
The viewer bridge and the worker, on the other hand, end with `serve`, even
when it is killed outright: of no use without it, they would otherwise hold
their ports and sockets, and a `serve` started again would start a second of
each. `serve` passes them its process ID, and Linux sends them SIGTERM once
it has gone (elsewhere they look once a second); the worker gives back the
job it was doing.
Once a second `srv-stats` reads each process's CPU time, memory and
threads from `/proc` (Linux only), and the supervisor's events say which
runs, since when, and how each one that ended last ended. `srv-announce`
keeps one more connection to capture on which it asked to `watch`: capture
tells it each episode starting and ending as it happens (written by its own
`cap-control-tell` thread, so recording never waits on it), and serve
passes that on to the API's event streams.

Serve keeps its **runtime state** in SQLite, in `kapto.db` in the data
directory: for now the episode index, from which the API lists episodes
without reading every `episode.json`. One thread, `srv-db`, owns the only
connection that writes; everything else sends it commands, and readers
open read-only connections of their own. The database holds no settings
and never the only copy of anything about a recording: when serve starts it
rebuilds the index from the episode folders, so episodes copied in or
deleted while it was stopped are seen, and deleting `kapto.db` loses
nothing. While it runs, `srv-index` adds each episode capture announces
finalised, and rebuilds the index if it missed announcements. The
database also holds the **job queue**: work on an episode after it was
recorded, done one job at a time and oldest first, each kind at most once
per episode; a job that was running when Kapto stopped is queued again
when serve starts, until it has been started `worker.max_attempts` times. Each finalised episode gets a
`verify` job, which checks its files against its manifest, an `events`
job, which reads its events back from its recording into the episode index
(`episode_events`, kept apart from the episodes so a rebuild of the index
keeps them; an episode whose events were never read, e.g. one copied in,
gets one when serve starts), and then a `qa`
job, which runs the **quality checks** on it ([Quality, statistics and
anomalies](quality.md) explains every check, statistic and detector). Each
check is a **gate**,
deterministic and structural, or an **advisory**, which only flags; an
episode is accepted unless a gate failed, so nothing advisory can reject
it. For now there is one check, the gate `streams.complete`: every message
that reached Kapto was written. The checks are made from the `qa.toml` the
episode's recording carries, the settings it was recorded with, never the
package as it is now; a recording without one is checked with every
check's defaults. Checks read an episode's values by role
(`kapto_qa::roles`): the recording is decoded with the layouts stored in
it, and each value named with the `mapping.toml` it carries, so checks
never see a stream's name. A check that can't run counts as failed, saying why. Serve hands the
jobs out on `worker.sock` in the data directory (`srv-jobs`), one line of
JSON each way: the worker says `hello`, asks for the `next` job and reports
each one `finished`, done or failed with its cause code, with what the
checks found for a `qa` job, the events an `events` job read, and what a job couldn't do in full though it was
done (a `replay` without its robot or some of its meshes, each with its
cause code, kept with the job in `kapto.db`), or `released` if it stopped
it part-way. Serve
records the checks' results in `kapto.db`, one row per run and one per
check's result, before it marks the job done. The same job works out the
episode's statistics by role (its duration, each stream's rate and loss,
each joint's position, speed, acceleration, jerk and effort, each
gripper's cycles; all of them unless the `[statistics]` of its `qa.toml`
leaves some out), sent and stored with
the results: distributions that pool, so a session's come from its
episodes'. Then the **anomaly detectors** the `qa.toml` turns on compare
the episode with the other checked episodes of its **session**, those
recorded with the same robot package, profile and task on the same day,
whose statistics the worker reads from `kapto.db` with a read-only
connection: `anomalies.statistics` flags each statistic far from the
session's median, in robust standard deviations of its spread; and
`anomalies.autoencoder` learns
which statistics go together from the accepted episodes of the robot
package and task (principal components, from one decomposition) and flags
an episode whose mix it can't reconstruct. The worker learns a model once
there are enough of those episodes, and again after a few more, and sends
it to serve with the results, which keeps it in `kapto.db` as the next
version of that package and task's models, never replacing one; each
result names its model's version. What a detector finds is always
advisory, a result beside the checks' that never rejects the episode. **`kapto worker`**
does them: `wrk-sched` takes each job from the socket, connecting again
whenever serve can't be reached, and `wrk-cpu-0` does it, one at a time.
Both run at low priority, nice 10 and (on Linux) the idle I/O class, so
jobs use the CPU and the disk only when capture and the rest don't. While
capture records an episode, the worker waits (`worker.pause_while_recording`,
on by default): `wrk-watch` asks capture once a second whether it is
recording, `wrk-sched` starts no job meanwhile, and a job being done waits
at its next step, e.g. between two chunks of a file it checks. Capture's
answer that it is recording holds the worker for three seconds at most, so
a watch held up anywhere (capture not answering, a connection stuck) never
holds the worker up with it. Stopped with
SIGTERM (as serve does when it stops), the worker stops a job at its next
step and gives it back (`released`): queued again without counting as an
attempt, so only a worker that dies uses up `worker.max_attempts`. A
job the worker was doing when it stopped stays running in the queue and is
done again when serve next starts.

**Uploads** take a lane of their own. With at least one
`[[upload.destination]]`, serve queues an episode's `upload` job once it
qualifies for a destination as that destination's `upload.when` says (its
files verified, or also clean, or its checks accepting it), hands it out
with the destinations it goes to then, and when an upload job ends queues
it again for one it came to qualify for meanwhile. An upload that failed
is tried again by itself once its destination's `upload.retry_failed_after`
has passed since it failed (an hour by default), and only if the episode
still qualifies: serve queues it again when the lane next asks for an
upload, and hands out a failed destination only once its wait is over.
Serve also decides which episodes may be deleted here, as
`upload.delete_local` says (`kapto_server::retention`): at most every ten
seconds, as a lane asks for work, it queues a `delete` job for each episode
verified on `upload.min_verified_destinations` destinations (`after_upload`),
or, while free space is below `capture.disk_warn_below`, for the oldest of
them only (`when_disk_low`); it hands one out only if the episode is still
verified on enough and no other job of it is waiting or being done, and
records the deletion, its audit entry in `kapto.db` keeping the episode's
summary, only once the worker reports the episode set aside, refusing one
not verified on enough destinations (`store.deletion_refused`); an upload
job ending looks at once, so `after_upload` deletes right after it.
`wrk-sched` does `delete` jobs itself, even while an episode is recorded,
since they give a recording the disk is low for room: the retention executor
(`kapto worker`'s `retention` module), the only code that removes raw data,
renames the episode's folder aside as `.deleting-<id>`, out of the episodes,
reports it with its `episode.json`, and removes it only once serve has
recorded the deletion. A crash, or serve not answering, leaves the folder
aside, never half an episode looking whole, and the worker finishes it the
next time it reaches serve; one serve refuses is put back. `wrk-upload` takes only
those jobs, on a connection of
its own, while `wrk-sched` takes every other kind: a long upload never
holds up checking the next episode. The lane runs a small tokio runtime
(helper threads `wrk-upload-io`), started after the worker lowers its
priority, so uploads share the worker's nice 10 and idle I/O class. For each
destination it works out the episode's key from the destination's prefix and
layout, asks serve to start the upload (serve answers with the key the upload
was first given and how far it got, kept in `kapto.db`), and puts each file
and the manifest up, checking each, then the completeness marker `_SUCCESS`
last (`kapto_upload::episode`). Each step is reported back and kept, so an
upload carries on after a restart; an episode counts as uploaded only once
its marker is written. While capture records an episode the lane takes no
upload, and one going up waits before its next file
(`upload.pause_while_recording`, on by default); stopped with SIGTERM, it
lets the file going up finish and gives the job back, the upload carrying
on from the next file. If the lane's thread ends by itself, by a bug or a
runtime it couldn't start, the worker stops with `worker.lane_ended` and
serve starts it again, rather than it running on without uploads. Each
destination's store is set up from its `url`,
`endpoint` and `region` and the provider's settings in the worker's
environment (`kapto_upload::stores`): S3 and S3-compatible storage check
each file's SHA-256 as they take it, and cloud storage takes a large file in
parts that survive a restart: `upload.parallel_parts` of them at once to
begin with, one more for each round of requests that succeed, up to
`upload.max_in_flight`, and half as many when the store throttles
(`kapto_upload::throttle`, told from `object_store`'s error text: a `429`,
a `503`, S3's `SlowDown`), the throttled request tried again after a wait
without counting as an attempt; each part read from the disk only when it may
go, so memory stays at that many parts, and each kept by its number as it
comes back, in whatever order. A file that would need more parts than the
store takes (10 000 on S3 and GCS, 50 000 on Azure) goes up in larger
ones, and an upload carried on cuts a file as it began. With `upload.bandwidth` set, each request
that sends data waits until those before it would have taken at the cap
(`kapto_upload::pace`): `object_store` sends a request's body as fast as
the link takes it, so the cap is kept between requests, and parts are made
about one second at the cap, never below the store's smallest part, so
each leaves as a burst of about a second. A store never tries a request again by
itself; the engine does, as `[upload]` says. `cargo xtask storage` checks
it all against a RustFS server, the one `compose.yaml`'s `storage` profile
starts with a bucket `kapto` for [trying uploads](../guides/try-uploads.md),
with the network to it cut in the middle of an upload by a proxy in the
test. Until Kapto's first release the
schema changes in place, at version 1 and without migrations: a database
whose tables differ from it is deleted and made again.

```
kapto serve ─┬─ kapto capture      (cap-rx-*, cap-writer, cap-tap, cap-finalise, cap-control, cap-control-tell, cap-preflight)
  srv-api    ├─ kapto viz-bridge   (viz-stream, viz-stream-page, viz-marks)
             └─ kapto worker       (wrk-sched, wrk-cpu-0, wrk-watch, wrk-upload) ── worker.sock
  srv-watch, srv-stats, srv-announce, srv-index, srv-jobs ── worker.sock
  srv-db ── kapto.db
```

`kapto serve` also serves the **API** on `server.bind`:`server.port`
(`http://127.0.0.1:7700/api` by default): Kapto's version, what capture is
doing, the robot and its streams as serve loaded them (`/api/robot`: each
joint with its limits and the decoded value that drives it, each stream
with its input, rate and fields, each role with the value that plays it,
and the layout the live view is arranged
by: the default saved for the package (`PUT /api/layout` writes
`layouts/<package id>.toml` in the configuration folder, `DELETE` removes
it), else the package's `layout.toml`, else one made from its roles,
cameras and fields, each panel with what it names that the package lacks), what each process is doing and costs
(`/api/processes`, which answers even while capture doesn't), the
settings it runs with and where each came from (`/api/settings`), how much room the data directory
has and what its episodes take (`/api/storage`), the finalised episodes
and each one in full (`/api/episodes/{id}`: every stream's counts, its
files, buffers, how quickly it reached the disk, the wall clock) and its
replay, and starting, stopping and aborting episodes. `/api/events` is a WebSocket that tells when an episode starts
or ends as capture announces it, however short, and sends the status once
a second, asking capture for it four times a second for as long as a
client listens (which also catches an episode whose announcement was
missed while capture restarted). The API is the only part of Kapto
that uses an async runtime (`tokio`, two `srv-api` threads); it turns each
request about recording into an order on capture's control socket.
Requests that change something need a JSON body, so a web page from
another site can't send them as a form; while the API listens only on
this machine it only answers requests addressed to `localhost` or a
loopback address, so a page can't reach it by DNS rebinding either. The
endpoints are in the [API reference](../reference/api.md), with the OpenAPI
document, which `cargo xtask api-diff` checks for changes that could break
a client. [Control recording from your own programs](../guides/control-recording.md)
shows how a robot program uses it.

The same address serves the **web UI**: the robot moving in the embedded
viewer and a Record/Stop button, with five tabs beside them. **Live** has
pre-flight's latest run, each failed check with what to do about it, a
*Run again* button and *Record anyway*, which starts an episode waiving
exactly the checks that failed; each stream's rate (marked when it falls silent or well below the rate
its `streams.toml` declares) and counts and the free disk space with how long it
lasts at the rate an episode is being written; **Episodes** the latest
episodes, each with whether the quality checks accepted it, filtered by
that if asked, and shown in full when clicked, with each check's result,
its statistics and its uploads: each destination's, with the URI of each
object there to copy; one deleted here, verified elsewhere, is marked as
stored only remotely, its results still shown and its replay not; **Uploads** each destination's uploaded, uploading,
failed and throttled counts with a *Test* button that writes, reads back and
removes a probe there, the episodes waiting, each upload going with how fast
it goes and how long it has left (from the bytes and the times the worker
reported), each failed one with why and *Retry now*, and a warning while
`upload.marker` is off; **Robot** its joints with where
each is now against its range and what drives it, and its declared
streams; **System** each process with its state, CPU, memory, threads and
restarts, and the settings with the ones changed from their defaults
marked. A tab holding a problem (drops, a failed pre-flight check, a
robot warning, a failed upload, a process waiting to restart) is marked. It is Svelte and TypeScript ([its design](../design/web-ui-stack.md)),
built with Node.js in Docker (`cargo xtask ui build`, or the image build)
and embedded in the binary; a binary built without it serves a page saying
how to add it. The UI asks for `/api/status`, `/api/processes`,
`/api/storage` and `/api/uploads` once a second, listens on `/api/events` so an episode
ended elsewhere (`kapto episode`, another program) is listed at once, checks
`/api/version` when it loads, and uses API types generated from the OpenAPI
document. Its page policy lets it reach only its own origin and the viewer
bridge's stream, and frame nothing. `cargo xtask ui e2e` runs its flows in a real browser (Playwright, in
Docker) against `kapto demo`, configured to upload to a folder and to one
that can't be made: recording an episode from the page, and the
page with the capture process gone; every request the page makes must stay
on this machine. `cargo xtask ui screenshots` runs the same
way and saves a picture of each tab and state into `target/screenshots/`.

```sh
curl -s http://127.0.0.1:7700/api/status
curl -s -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:7700/api/episodes/start
curl -s -X POST -H 'Content-Type: application/json' -d '{}' http://127.0.0.1:7700/api/episodes/stop
curl -s http://127.0.0.1:7700/api/episodes
```

```sh
cargo run --release --bin kapto -- serve --robot packages/arm7/urdf/arm7.urdf --streams packages/arm7/streams.toml --mapping packages/arm7/mapping.toml --data-dir /tmp/kapto &
cargo run --release -p kapto-loadgen -- --streams 1 --duration 60
```

`kapto demo` is `kapto serve` with a robot to show: it writes the sample
arm (built into the binary) to the data directory, runs Kapto with it, and
sends the arm's joint states from a synthetic robot thread (`demo-robot`)
over UDP on the same machine, and a wrist camera's JPEG pictures from
another (`demo-camera`, 640x480 at 30 a second) over TCP on the port after
the robot's, declared a camera in the arm's `streams.toml` there.

```sh
cargo run --release --bin kapto -- demo
```

`cargo xtask bench system` does the same at full load (16 streams at 2 kHz
for 60 s, any number of them over TCP with `--tcp`, and cameras sending
1080p JPEG frames at 60 fps beside them with `--cameras`), measures the recording
process's CPU per thread and its memory from outside, and reports whether
each performance target was met.
`cargo xtask bench headroom` records twice that load, and `cargo xtask bench
capacity` finds how many streams the machine records completely.
`cargo xtask bench viewer` records the same load while a viewer bridge
streams to a page, is stopped, has its page stop reading and is killed,
and checks that recording loses nothing and uses no more CPU for any of
it; it also reports how long each frame took from its newest message's
arrival to being sent. `cargo xtask bench browser` runs `kapto serve` at
that load and watches the live view in Chromium, drawing with the
machine's graphics card, for ten minutes: how long each frame took from
its newest message's arrival to being drawn, and whether the page's
memory stays flat.
`cargo xtask bench ros2` records the same load as ROS 2 topics, one
`sensor_msgs/msg/JointState` topic per stream, in the ROS 2 image, while
the live view's bridge moves the arm with the first, and holds it to the
same targets, with recording and the bridge together within two cores;
with `--cameras` it publishes 1080p JPEG frames at 60 fps beside them as
`sensor_msgs/msg/CompressedImage` topics.
`cargo xtask bench upload` uploads an episode to the RustFS server of the
`storage` profile, as the worker does, for each part size and number of
parts at once, and reports how fast each went, with its CPU and memory;
with `--while-recording` it records the reference load while episodes go
up as the worker uploads them, and checks that recording doesn't notice.

## Where it is going

Kapto will run as four processes from the same binary: a server (API, web UI,
supervision), a capture process that never waits on anything else, a viewer
bridge, and a low-priority worker for heavy jobs. The supervision, a first
API and web UI, capture, the viewer bridge and a first worker exist. The reasoning is in
[the design of processes and threads](../design/process-and-thread-model.md).
