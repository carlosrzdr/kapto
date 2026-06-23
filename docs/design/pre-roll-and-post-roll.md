# An episode holds the seconds before its start and after its stop

## Context

An episode used to hold exactly what arrived between Start and Stop. What
matters is often just before: an operator presses Start a moment late, a
trigger fires after the thing it reacts to has begun, a gripper slips and
the button is pressed once it has. Some streams matter just after: a
comment spoken when the episode is stopped, a robot settling. Capture
receives every stream all the time, recording or not, and between
episodes it lets go of what it receives; the seconds were there to keep.

What else holds:

- **The capture path never waits, and doesn't allocate, lock, call the
  system or log per message.** Keeping messages from before Start is a
  copy into memory set aside when capture starts, made by the writer
  thread, as kept messages are.
- **Memory is bounded and said at startup** (capture under 1 GiB at the
  reference load). A camera dominates: 5 s of a 30 fps camera whose
  frames may be 4 MiB is 600 MB at its largest; its real frames are
  usually far smaller.
- **Loss is never silent.** A message lost in the seconds kept before
  Start is as lost as one lost after.
- **A recording plays back as it was,** in ROS 2's player and Kapto's
  replay: every message at its own time, in order.
- **Payloads are stored exactly as received**, each with its receive time
  and its source time.

## Approach

**Pre-roll.** A stream may keep its last seconds before Start:
`pre_roll_s` on the stream in `streams.toml`, 0 by default. Capture sets
aside, when it starts, a ring of bytes per such stream: the stream's rate
times its seconds times its largest message, capped at
`capture.pre_roll_max_mib` (a deployment key, 64 MiB by default), and says
the total it set aside. The writer thread copies each message of the
stream into the ring as it takes the batch, between episodes and during
them alike, the oldest giving way: what the ring holds is the stream's
newest messages, as many seconds of them as fit. Messages are held with
their payload, receive time, source time and publisher's count, as
received.

When an episode starts, the writer writes each stream's held messages
from `pre_roll_s` before Start onwards, first, each at its own receive
time, then everything after as usual. A camera whose frames are larger
than expected holds fewer seconds than asked: the episode says how many
it holds.

Recording can begin after Start is asked: a Start waits while pre-flight
runs. Once a stream has a pre-roll, every other stream gets a ring of no
seconds, which holds only while a Start waits, from when it was asked,
sized for the pre-flight window and a second more. The episode then
starts every stream at Start, the pre-rolled ones earlier, and no stream
looks empty at its beginning. An episode without any pre-roll starts
when recording begins, as before, and sets nothing aside.

**Post-roll.** A stream may go on being recorded after Stop:
`post_roll_s` on the stream, 0 by default. At Stop, the streams without
one stop being written; the others go on until their own seconds have
passed, then the episode is finalised. Stop answers once it is, as now.

**Times.** The episode spans its data: `started_utc_ns` is its first
message's time, pre-roll included, and `ended_utc_ns` its last, post-roll
included, so a player starting at the episode's start plays the seconds
before Start at their own time and waits for nothing. When Start and Stop
were asked is kept beside them: `commanded_start_utc_ns` and
`commanded_stop_utc_ns`, optional, in `kapto.summary` and `episode.json`,
and drawn as marks on the replay's timeline. Kept messages are written at
the episode's start, as now.

**Counts.** A stream's `received`, `written`, `dropped_by_us` and
`lost_upstream` cover everything the recording holds, pre-roll and
post-roll included, so they add up the way they always have. Beside them,
optional `pre_roll` and `post_roll` say how many of the written came from
before Start and after Stop. Loss detection runs between episodes as
while recording. Each gap it finds goes with the batch it was found in,
from when the stream's batch before was received (or, for a silence, from
when it began) to that batch, and is held beside the stream's newest
message in it. A gap counts in the episode's `lost_upstream` for its share
of time within the episode's span: all of it once it began after the
episode's first message, in proportion if it began before, so a long
silence ending just before Start adds only what the episode holds of it.
A message the ring let go of to make room is not lost: it is older than
what the episode holds.

## Consequences

- An episode can start before its Start: a trigger or an operator who is
  a little late loses nothing they could have had, up to what memory was
  set aside for.
- The writer copies each message of a pre-rolled stream once more. A
  2 kHz joint stream costs a copy of its few dozen bytes per message; a
  camera a copy of each frame. Measured with the writer's benchmark and
  the system benchmark, and set down in `perf-budget.toml`.
- Memory grows by the rings set aside, said when capture starts and
  bounded by `capture.pre_roll_max_mib` per stream.
- `pre_roll_s` and `post_roll_s` are a stream's, in the package: a session
  profile or task can't change them, since the rings are set aside when
  capture starts.
- `kapto.summary` and `episode.json` gain optional fields only;
  readers that don't know them are unaffected. The episode's span now
  starts at its data, which for an episode without pre-roll is as before.
- Stop takes as long as the longest post-roll more to answer.

## Alternatives considered

- **Pre-roll counted apart from the episode's numbers**, as kept messages
  are with `carried`. The numbers would then not describe what the
  recording holds, and loss before Start would need a second set of
  counters.
- **The episode's span from Start to Stop**, with the pre-roll before it.
  True to the commands, but players that start at the episode's start
  would skip the seconds before it, and ROS 2's player would wait out the
  post-roll after its end.
- **One pre-roll for every stream, cameras left out.** Simpler, but the
  frames just before Start are often the ones wanted.
- **Rings sized for every stream's largest message.** A camera's would
  take hundreds of megabytes for frames that are usually a tenth of that;
  a byte ring holds what the frames really are.
- **Recording all the time and cutting episodes afterwards.** That is
  continuous mode (CAP-13): it needs a bounded segment buffer on disk and
  selection deciding what to keep, and it isn't what an operator pressing
  Start asks for.
