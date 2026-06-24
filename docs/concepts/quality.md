# Quality, statistics and anomalies

After an episode is recorded, the worker checks it: it runs the **quality
checks**, works out the episode's **statistics**, and runs the **anomaly
detectors**, which compare those statistics with other episodes. Everything
is set in the robot package's `qa.toml`, and read from the copy the
recording carries, so an episode is always checked with the settings it was
recorded with. The results are in the web UI under **Episodes**, and in the
API (`/api/episodes/{id}/qa`, `/api/episodes/{id}/statistics`).

- A **gate** is deterministic and structural: an episode with a failed gate
  isn't accepted.
- An **advisory** only flags. Every anomaly detector is advisory: nothing it
  finds can reject an episode.

## The checks

| Check | Kind | Passes when |
|---|---|---|
| `streams.complete` | gate | Kapto dropped no more than `max_dropped` messages from any stream (0 by default). Messages lost before they reached Kapto are the robot's or the network's, and don't count. |
| `streams.present` | gate | Every critical stream has messages in the episode, and every one named is in it. Name them with `critical` (e.g. `["joints", "wrist_camera"]`); with none named, every stream is, but those kept for late subscribers, which may send nothing while an episode is recorded. A camera unplugged or a topic never published leaves the episode without what it was recorded for. |
| `streams.empty` | advisory | Every other stream has messages; a kept stream counts those it started with. |
| `episode.short` | gate | The episode lasted at least `min_seconds` (2 s), from its first message to its end: shorter, it can hardly hold a task. |
| `episode.long` | advisory | It lasted no longer than `max_seconds` (600 s): one far longer was likely left recording. |
| `motion.jump` | advisory | No joint moved faster than `max_speed` (20 rad/s or m/s by default) between two messages: no motor turns that fast, so a reading that does jumped (a glitch, a wrapped angle, a sensor catching up after it froze). Each jump is named with where it happened. |
| `motion.frozen` | advisory | No joint's reading stayed exactly the same for `max_seconds` (1 s) right after it moved at `moving_speed` (0.05) or more, while another joint moved: stopping dead mid-motion is what a reading that stopped updating does. A joint slowing down to rest gets slower than `moving_speed` first, and isn't flagged; nor is a robot of one joint. |
| `motion.chatter` | advisory | No gripper (each role ending in `opening`) closed more than `max_closings` times (3) within `window_seconds` (2 s), counted with the same hysteresis as its cycles: chattering, from a loose grasp, a controller hunting, or a reading flickering. |
| `motion.idle` | advisory | The robot, every joint slower than `moving_speed` (0.05), stood still for no more than `max_ratio` (half) of the episode, from its first joint position to its last, counting spans of `min_seconds` (2 s) or more: an episode mostly still may hold little of the task. |

Checks that find something in part of an episode name where, as time
ranges, shown with their result.

## Roles: what the statistics are about

Statistics don't use a stream's or a field's name; they use **roles**, what
a value is for, from the package's `mapping.toml`:

- Each mapped joint plays `joints.<joint>.position`, and `.velocity` and
  `.effort` if those are mapped.
- `[roles]` names other values, e.g. `"gripper.opening" = "arm_joints.gripper"`.

A role mapped to a whole array rather than one value has no statistics yet.

## The statistics

Each is a **value** (one number per episode) or a **distribution** (many
numbers, kept as described under [Distributions](#distributions)). Times are
receive times, when Kapto got each message.

| Statistic | Kind | Unit | How it is worked out |
|---|---|---|---|
| `episode.duration_s` | value | s | End time minus start time. |
| `streams.<stream>.rate_hz` | value | Hz | Messages received ÷ duration. |
| `streams.<stream>.lost_upstream_ratio` | value | 0–1 | Messages lost before reaching Kapto ÷ (received + lost): the robot's or the network's loss. |
| `streams.<stream>.dropped_ratio` | value | 0–1 | Messages Kapto received but couldn't write ÷ received. |
| `joints.<joint>.position` | distribution | rad or m | Every position the joint was at: its range of motion. |
| `joints.<joint>.speed` | distribution | rad/s or m/s | The size of its velocity: the mapped velocity if there is one, otherwise how much the position changed between consecutive messages ÷ the time between them. |
| `joints.<joint>.acceleration` | distribution | rad/s² or m/s² | The size of how much the velocity changed between consecutive messages ÷ the time between them. |
| `joints.<joint>.jerk` | distribution | rad/s³ or m/s³ | The same, one step further: how fast the acceleration changed. |
| `joints.<joint>.effort` | distribution | N·m or N | Every effort it reported. |
| `<role>.cycles` for each role ending in `opening` | value | count | How many times it closed after being open, e.g. `gripper.opening.cycles`. |

Notes on how they are worked out:

- **Rates of change** (speed, acceleration, jerk) are worked out from
  consecutive messages. Two messages received at the same moment are
  skipped, since their time apart is zero. Each derivative amplifies the
  sensor's noise, so jerk on a real robot is mostly noise; it is still
  useful for comparing episodes, where the noise is alike, and shaking or
  chattering shows in it long before it shows in the speed.
- **Cycles** use the opening's own range in the episode: above 70 % of it
  counts as open, below 30 % as closed, and between the two it keeps what
  it was, so jitter around one level isn't counted. A closing counts only
  after the opening was open.

### Distributions

A distribution keeps how many values there were, the smallest and largest,
their sum (so their mean), and a sketch of their percentiles: values are
counted in buckets whose bounds grow by a fixed ratio, so **every
percentile is within 1 % of a value at that rank**, whatever their range.
Values closer to zero than 10⁻¹² count as zero, and values that aren't
numbers are counted apart. Distributions of several episodes can be merged
into one without reading the recordings again.

The web UI shows each distribution's median, 90th and 99th percentiles and
largest value; the API gives its count, extremes, mean, median, 90th and
99th percentiles.

### Choosing the statistics

Every statistic is kept by default. `[statistics]` in `qa.toml` turns some
off, and some of those back on:

```toml
qa_params_version = "1"

[statistics]
exclude = ["streams.*.lost_upstream_ratio", "joints.wrist_*.jerk"]
include = ["streams.camera.lost_upstream_ratio"]
```

`*` stands for any part of one name between dots, never for a dot. A
statistic turned off isn't kept, shown or compared. A pattern that names
none of an episode's statistics is reported in the worker's log.

## The anomaly detectors

Both compare **measures** of the statistics: a value as it is, and a
distribution by its 1st, 50th, 90th and 99th percentiles (`<name>.p1`,
`.p50`, `.p90`, `.p99`), so going further than usual either way shows.

### Unusual for its session: `anomalies.statistics`

A **session** is the episodes recorded with the same robot package,
profile and task (each the same content, or all without one) on the same
day, in the machine's time zone. They should be alike.

For each measure of the episode, the detector takes the same measure of
the session's other checked episodes (the newest `max_episodes`, at least
`min_episodes`) and works out:

- their **median**, and their **spread**: 1.4826 × the median of how far
  each is from the median (the MAD), which reads as a standard deviation
  for normally distributed values, and isn't moved by a few unusual
  episodes. The spread is never taken as less than 1 % of the median, the
  statistics' own error, so episodes that are nearly the same aren't
  flagged for a rounding;
- the episode's **score**: how far its measure is from the median, in
  spreads.

A measure scored beyond `threshold` is flagged; the result names the five
furthest out, with their values, and records every flagged measure's
score. A session with fewer than `min_episodes` others is compared with
nothing, and the result says so.

### Unlike its package and task: `anomalies.autoencoder`

An **autoencoder** learns which statistics usually go together, from the
accepted episodes of the robot package and task, so it catches an episode
whose statistics are each ordinary but whose mix isn't: a short episode
with as many gripper cycles as the long ones. `model` chooses which:

- `"linear"` (the default): principal components, which learn
  straight-line relations ("these move together in proportion"), in one
  step, in milliseconds.
- `"neural"`: a small neural network, which learns curved relations too,
  such as effort growing with the square of speed, in seconds.

Both are checked the same way, below.

#### The linear model

**Learning** it, in one step:

1. **Standardise**: each measure has its mean taken away and is divided by
   its standard deviation (never less than 1 % of its mean), so measures in
   different units count alike. It learns only the measures every one of
   its episodes has.
2. **Decompose**: the principal components of the standardised measures
   (a singular value decomposition): the directions along which episodes
   vary, the most first.
3. **Keep** the fewest components that explain `variance` of the spread.
   They are what the model can express; anything else it can't.
4. **Learn what is usual**: each episode it learned from is reconstructed
   by a model learned without it; the median and spread (as above) of
   those errors set how far is far. The spread is never less than the
   error every measure being off by 1 % would make.

#### The neural model

```text
measures → neurons (tanh) → code → neurons (tanh) → measures
```

Each layer weighs every output of the one before; tanh bends what passes
through the two hidden layers, of `neurons` each (16 by default), which is
what lets it learn curves. The **code** in the middle is a quarter as many
numbers as there are measures, from 1 to 8: everything the model keeps of
an episode has to pass through it.

**Learning** it: standardised as for the linear model, it starts from
weights drawn with a fixed seed, so the same episodes always give the same
model, and adjusts them by gradient descent (Adam) to reconstruct every
episode at once, pass after pass, until the error stops falling (300
passes without falling by a ten-thousandth) or after 10 000. What is usual
comes from five more models, each learned without a fifth of the episodes
and reconstructing them, as for the linear model.

If learning takes longer than `train_seconds` (60 by default) it stops:
the previous model stays in use, and each result says so. On a laptop it
takes about 0.5 s for 20 episodes of 30 statistics, 3 s for 200, and 11 s
for 200 of 300 statistics; the length of the episodes doesn't enter.

#### Checking an episode

Its standardised measures are passed through the model, and the
**reconstruction error** is the mean squared difference between them and
what comes out, in standard deviations. Above the median error plus
`threshold` spreads, the episode is flagged, and the result names the
measures accounting for at least a tenth of the error, each with its value
and the value the model expected. A measure the episode lacks is counted
and taken as usual.

**When it learns**: once the package and task have `min_episodes`
accepted, checked episodes, and again after each `retrain_after` new ones,
from the newest `max_episodes`. A model is kept in `kapto.db` as the next
version of that package and task's models and is never replaced; each
result names the version it came from. Until there is one, the result says
how many more episodes it needs. Choosing another `model` learns one of
that kind at the next episode. The linear model takes about 30 ms for
200 episodes of 30 statistics, and up to about 3 s for 300 statistics.

It never learns from an episode a gate rejected. It does learn from
episodes it flagged, which are still accepted, so a lasting change (new
hardware, a different way of doing the task) becomes the new usual instead
of being flagged forever.

## Settings

Every key of `qa.toml`, with its default and range, is in the
[package reference](../reference/package.md). For example, with each
detector's defaults:

```toml
qa_params_version = "1"

[streams.complete]
max_dropped = 0

[anomalies.statistics]
threshold = 3.5
min_episodes = 5
max_episodes = 200

[anomalies.autoencoder]
model = "linear"
neurons = 16
train_seconds = 60
threshold = 3.5
min_episodes = 20
max_episodes = 200
variance = 0.9
retrain_after = 10
```
