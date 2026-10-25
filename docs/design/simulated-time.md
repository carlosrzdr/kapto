# A simulation's time read from its clock, beside the two timestamps

## Context

A robot recorded from a simulation runs on the simulation's time, not the
machine's. The simulation can pause, be stepped one tick at a time, run
slower than real time when its scene is heavy, or several times faster
when nothing watches it, and a reset can take its time back to zero.
Recorded only by when Kapto received each message, such an episode looks
wrong: a pause shows as a silence, a simulation at half speed shows every
stream at half its declared rate, so pre-flight and the rate checks call
it slow, and a replay plays at the machine's pace rather than the
simulation's.

Simulators say what their time is in more than one way:

- **On ROS 2's `/clock`**, a `rosgraph_msgs/msg/Clock`, which nodes follow
  with `use_sim_time`: Isaac Sim, Gazebo through its bridge, MuJoCo
  through its ros2_control integration, CARLA's bridge and others. This
  is the common case.
- **On a topic of the simulation's own**, as a plain number of seconds,
  where a simulator doesn't publish `/clock` unless a script does.
- **As a field of its own messages**, for a simulation that sends over
  UDP or TCP without ROS: a learning environment, a physics engine driven
  from a script.

Their time can go back: Gazebo's and MuJoCo's resets take it to zero,
and so does Isaac Sim's when asked to; by default Isaac Sim keeps
counting across them.

What has to hold:

- **Two timestamps per message, never mixed or overwritten**: the receive
  time and the source's time as received. A simulation's time is neither.
  Over ROS 2, the source time is the publisher's timestamp, which the
  middleware sets from the publishing machine's clock even for a node on
  simulated time; the simulated time is in the message's header stamp,
  stored with the payload as received.
- **Nothing on the capture path per message.** Capture can't decode every
  message for its time.
- **A recording carries everything it was recorded with**, and what is
  computed after it is finalised lives beside it.
- **A real robot's recording doesn't change.**

## Approach

**The package says which value is the simulation's time, as a role.**
`mapping.toml` gives the role `time.simulated` to one decoded field of one
stream, as it gives `gripper.opening` to one:

```toml
[roles]
"time.simulated" = "clock.time"
```

The field is of a type the decoders gain for this, `time`: a ROS
`builtin_interfaces/msg/Time` (seconds and nanoseconds) read as one value,
or a number read as seconds, or as nanoseconds with `unit = "ns"`. So the
role points at `/clock`'s `clock`, at a topic of the simulation's own, or
at a field of a UDP or TCP message alike. Without the role, nothing below
happens.

**The stream is recorded like any other.** Its messages are stored as
received, with their two timestamps; nothing else in the recording
changes, and nothing is added to the MCAP: the clock stream's own channel
is the record of the simulation's time.

**Simulated time is a mapping from receive time, worked out from the
clock's messages.** Each message pairs the time Kapto received it with
the simulated time it carries; between two messages, simulated time is
taken to move as it did between them. Where it doesn't move, the
simulation was paused or waiting to be stepped. Where it goes back by
more than a tick, the simulation was reset: the mapping starts a new
segment there, rather than calling the clock broken. The mapping is worked
out wherever it is needed: by the worker once the episode is finalised,
by the server for a replay, by the viewer's bridge live from the newest
clock messages.

**`episode.json` says how the simulation ran**, in an optional
`simulated` object written once the episode is finalised: its simulated
start, end and duration, the ratio of simulated to real time, each pause
and each reset with when it happened. The schema only gains it.

**Rates are judged in simulated time.** With the role given, pre-flight's
rate check, the rate checks and the loss estimated from silences count a
stream's messages per simulated second against its `rate_hz`, and a
silence while the simulation is paused is no loss: a simulated sensor
publishes per simulated tick, whatever the simulation's speed. Kapto
keeping up with the real rate messages arrive at is still checked as
for any robot.

**Replay can play at the simulation's pace.** A replay of an episode with
a simulated clock has a *Simulated time* switch: the cursor moves through
simulated time, a pause passes as a still moment rather than a wait, the
segments between resets play one after the other with each reset marked,
and the time axis reads simulated seconds. Off, replay is as for any
episode.

**The live view shows the simulation's time** beside the receive time,
from the newest clock message, and says when it is paused.

## Consequences

- An episode from a slowed, stepped, paused, sped-up or reset simulation
  is counted and checked as it ran, and can be replayed as it ran, from
  Isaac Sim, Gazebo, MuJoCo, CARLA or a simulation sending over UDP alike.
- Everything recorded stays as received; the mapping can always be worked
  out again from the recording, and a better one later changes nothing
  stored.
- The decoders gain the `time` field type, with its own tests, round trip
  and fuzzing like the others.
- The checks that judge rates take the episode's mapping, and pick the
  clock a package says; the replay player has a second pace.
- Messages a ROS 2 tool plays back from an exported bag keep their timing
  as now; nodes on simulated time follow the `/clock` stream it holds.

## Alternatives considered

- **The simulation's time as each message's source time.** It would mean
  overwriting the timestamp the publisher gave, which is never done, and
  it loses the publisher's own clock. The header stamp already carries the
  simulated time where a message has one.
- **Each message's header stamp as its simulated time.** Reading it means
  decoding every message on the capture path, and not every message has a
  header: `/clock` itself doesn't, nor a UDP robot's. Comparing header
  stamps with the clock can be a check of its own later.
- **Simulated time as the MCAP's log time.** Every tool takes log time as
  when a message was logged, in order; a pause would put many messages at
  one moment, a reset would send time back, and a real robot recorded
  beside the simulation would fall out of order.
- **A stream key naming the clock stream, read only as
  `rosgraph_msgs/msg/Clock`.** Simpler, but it leaves out the simulators
  that don't publish `/clock` and those without ROS.
- **The mapping written into the recording when it is finalised.** It
  repeats what the clock's channel holds, and the recording is closed by
  then.
