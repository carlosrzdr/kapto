# A stream's kept messages start each episode

## Context

Some messages are sent once and kept for whoever subscribes later. A ROS 2
publisher with `transient_local` durability keeps its last messages and
sends them to each new subscriber: the robot's fixed transforms on
`/tf_static` (where a camera is mounted, the lengths between frames), a
map, a robot's description. A subscriber that doesn't ask for them, or
that subscribed before an episode began, never sees them in the episode.

The capture process subscribes once, when it starts, and records episodes
on command for hours afterwards. A transient-local message arrives once,
just after it subscribes, so it is in no episode recorded later, although
the robot was in that state the whole time. That breaks what a recording
promises ([recordings carry what they were recorded with](recordings-carry-what-they-were-recorded-with.md)):
an episode should carry what is needed to understand it, and without the
fixed transforms its camera frames and tool positions can't be placed.
`kapto record`, which subscribes for each recording, gets them only
because it subscribes anew.

Such a stream also has no rate. It says nothing more once sent, so a
declared rate would be wrong in every place Kapto uses one: loss inferred
from a silence longer than the rate allows, the pre-flight check failing a
stream with nothing arriving, and the live view marking it silent.

What else holds:

- **The capture path never waits and doesn't allocate per message.**
  Keeping a message has to be a copy into memory set aside beforehand.
- **Payloads are stored as received**, and each message keeps its two
  timestamps, the receive time and the source's.
- **A recording plays back as it was.** ROS 2's player starts at the first
  message's time and waits between messages: a message whose receive time
  is an hour before the episode would hold every other message back an
  hour.

## Approach

Capture keeps the newest messages of each stream whose messages are kept
for late subscribers, and writes them at the start of every episode, as
that stream's first messages:

- **Which streams.** A ROS 2 stream declared with
  `durability = "transient_local"`: the robot says its messages stay true
  until replaced. Others aren't kept.
- **How many.** As many as the subscription's `depth`, newest last: each
  publisher on the topic keeps its own (several nodes may each publish
  their fixed transforms on `/tf_static`), and a new one replaces the
  oldest. Their memory, `depth` messages of the stream's
  `max_message_bytes`, is set aside when capture starts; keeping one is a
  copy, made by the writer thread as it writes the batch.
- **As received, at the episode's start.** Each kept message is written
  with its payload and publish time as received, and the episode's start
  as its log time, so the episode's messages start at its start and ROS
  2's tools play it as recorded. Its own receive time is kept beside it,
  in a metadata record `kapto.carried`: per stream, each message's receive
  time in UTC, in the order written. A message that arrives during the
  episode is written as every other, and kept for the next.
- **Counted apart.** A kept message written at the start isn't received
  again: `streams.<stream>.carried` counts them, in `kapto.summary` and
  `episode.json`, beside `received` and `written`.
- **No rate needed.** Such a stream may leave out `rate_hz`: no loss is
  inferred from its silences (its loss detection is `undetectable`, unless
  it has a sequence number); its buffers are sized for its `depth`; the
  pre-flight check passes it once a message is kept, even if nothing
  arrives in its window; the live view shows it kept rather than silent.

## Consequences

- Every episode carries the robot's fixed transforms and any other kept
  message, whenever it starts, and so does a recording of `kapto record`.
- The recording format has `kapto.carried` and the `carried` counts, as
  optional fields, so readers that don't know them are unaffected.
- A message kept from a publisher that has since gone is still written,
  as a late subscriber of the robot would still be sent it while the
  publisher ran. The receive time in `kapto.carried` says how old it is.
- Capture's writer copies each message of these streams once more. They
  are rare (`/tf_static` sends a few messages in a session); a fast
  transient-local stream costs a copy per message, measured with the
  writer's benchmark when built.
- `rate_hz` is optional for these streams only; every other stream
  still needs one to be recorded.

## Alternatives considered

- **Subscribe again at each episode's start**, so the publisher sends its
  kept messages anew. Less code in capture, but the messages are then
  lost whenever the publisher has gone or is slow to answer, and how fast
  they come depends on the middleware. Kapto already received them once.
- **Write them with their own receive time.** True to the timestamp, but
  the message would lie before the episode's start, and ROS 2's player
  would wait out the gap before playing the rest.
- **Only `kapto record`.** It already gets them; `serve`, how Kapto is
  run, would still record episodes without them.
