# Replay view

What the web UI's player needs to show a recorded episode: the robot's
poses and each decoded value's minimum and maximum, at the display rate and
at coarser steps, with the events the recording carries and the robot it
was recorded with, as the page draws it. It is made from the
recording, from what the recording carries, and kept in the replay cache,
from which it can go at any time and is made again when next asked for. It
is never part of an episode. This page is a specification: the footer only
gains fields, and the layout of the records changes only with the view's
version.

## Steps

The finest step is as long as the time between two poses on screen
(`viewer.display_rate`, 60 a second by default); each next step is four
times as long, up to the first step one record of which spans the whole
episode. Steps start at the episode's first message, so each record of a
step covers exactly the records of the step below that fall in it.

A record covers one part of its step that holds at least one message: the
robot's newest pose in it, if the robot moved, and, for each stream with
messages in it, their count, the times of the oldest and newest, and each
value's minimum and maximum. A spike shows at every step: it is the minimum
or maximum of the record that covers it. A message received a little before
the one before it, as streams written in batches can be, is counted in the
record being made.

To show a stretch in a number of points, a reader takes the coarsest step
that still gives that many records over the stretch, starting from the last
index entry at or before the stretch's start, and merges its records into
that many parts as the viewer stream merges a
[look back](viewer-stream.md#looking-back). `GET
/api/episodes/{id}/replay/window?from=&to=&points=` does so and answers
with the look back's binary form (question number 0), its span the
episode's first and last message of any stream, shown or not (a camera's
frames included); the events are those
`/api/episodes/{id}/events` lists. `GET /api/episodes/{id}/replay/opening`
gives the footer's span, `links` and `streams`, which name what a stretch
holds, as the viewer stream's opening message does. `GET
/api/episodes/{id}/replay/scene` gives the footer's `scene` and
`/api/episodes/{id}/replay/meshes/{n}` each of its meshes, as
`/api/robot/scene` and its meshes give the robot loaded now; a recording
without a robot is answered with `viewer.replay_without_robot`, a mesh
that couldn't be read with `robot.mesh_unreadable`. The opening also lists
the episode's `cameras` by name; a camera's frames don't come from the
replay view but from the recording itself, by its indexes, and need no
replay made: `GET /api/episodes/{id}/cameras/{camera}/frames` gives every
frame's receive time in order, and `…/frames/{n}` frame `n` as a picture
(JPEG as recorded, or a raw frame encoded as JPEG; `viewer.picture_unshown`
with the reason if it can't be shown, `viewer.camera_not_found` for a
camera or frame the episode doesn't have). A stretch outside the episode, no
points or more than 10 000 are refused with `api.invalid_request`; a
replay not made yet with `viewer.replay_not_ready`, and a view that can't
be read with `viewer.replay_unreadable`.

## Layout

Little-endian throughout. Times are UTC nanoseconds since the Unix epoch,
signed. Fields aren't aligned.

| Bytes | Holds |
|---|---|
| 4 | `KVRV` |
| 2 | The view's version: 1. A view of another version is made again |
| 2 | Zero |
| … | Each step's records, finest step first, each step's in time order |
| … | Each of the robot's meshes, in the scene's numbering, in the binary form of the [viewer scene](viewer-scene.md#a-mesh) |
| … | The footer: JSON, below |
| 8 | Where the footer begins in the file |
| 4 | The footer's length in bytes |
| 4 | `KVRV` |

Each record is laid out as a frame of the [viewer stream](viewer-stream.md)
lays out its poses and samples:

| Bytes | Holds |
|---|---|
| 8 | When the newest message it covers was received |
| 4 | Links with a pose, `L`: every link, or 0 if the robot didn't move in it |
| 4 | Samples, `S`: the streams with messages in it |
| 28 `L` | Each link's newest pose relative to the root link, in the footer's `links` order: x, y and z in metres, then a unit quaternion x, y, z, w, as 32-bit floats |

Then each sample:

| Bytes | Holds |
|---|---|
| 2 | The stream's position in the footer's `streams` |
| 2 | Zero |
| 4 | How many messages it covers |
| 4 | Values, `V` |
| 8 | When the oldest of them was received |
| 8 | When the newest was received |
| 16 `V` | Each value's minimum then maximum over those messages, as 64-bit floats, in the footer's order |

## The footer

```json
{
  "version": 1,
  "start_utc_ns": 1760000000000000000,
  "end_utc_ns": 1760000013000000000,
  "links": ["base_link", "link_1"],
  "streams": [
    {
      "name": "arm_joints",
      "values": [
        {"name": "sequence", "plotted": false},
        {"name": "joint_positions[0]", "plotted": true}
      ]
    }
  ],
  "messages": 2600,
  "events": [
    {"kind": "intervention", "at_utc_ns": 1760000002000000000, "phase": "begin", "id": "i1", "source": "cli"}
  ],
  "steps": [
    {"step_ns": 16666666, "records": 780, "offset": 8, "index": [[1760000000000000000, 8]]}
  ],
  "scene": {"name": "arm7", "root": "base_link", "links": [], "meshes": 1},
  "meshes": [[64008, 2412]]
}
```

- `start_utc_ns` and `end_utc_ns`: when the first and last message were
  received.
- `links` and `streams`: as the viewer stream's opening message names them.
- `messages`: how many messages the records cover, once at each step.
- `events`: the events the recording carries, as its
  [`kapto.events` channel](recording.md#events-kaptoevents) holds them.
- `steps`: finest first. `step_ns` is how long each part is; `records` how
  many records it has; `offset` where its first record begins; `index` the
  time and offset of its first record and every 64th after it.
- `scene`: the robot the episode was recorded with, as
  [`/api/robot/scene`](viewer-scene.md#the-scene) gives the one loaded now,
  from the robot description the recording carries, or the local package's
  if it has the same ID and content; absent if the recording carries no
  robot.
- `meshes`: where each of the scene's meshes begins in the file and its
  length in bytes, in the scene's numbering. A mesh the recording doesn't
  carry, and no package with the same ID and content has, isn't in the
  scene; one that couldn't be read has length 0. Absent with `scene`.
