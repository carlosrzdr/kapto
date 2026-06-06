# Viewer stream

How the viewer bridge streams the robot's motion and plotted values to a
page as they arrive: a WebSocket on `viewer.stream_port` (9877 by default;
`/api/status` gives it as `viewer.stream_port`). The robot itself, its links
and shapes, comes from the [viewer scene](viewer-scene.md). This page is a
specification: the opening message only gains fields, and frames change
only with the stream's version.

## Connecting

Connect to `ws://<host>:<viewer.stream_port>/`. Only Kapto's own web UI may
connect from a browser: a page whose `Origin` isn't the web UI's, on the
same host at `server.port`, is refused with `403 Forbidden`, so another
site can't watch the robot through the browser of someone who visits it. A
client that isn't a page, and sends no `Origin`, may connect.

A page may ask for a stretch of the last minutes ([Looking back](#looking-back));
the bridge ignores anything else it sends. Besides frames, the bridge tells
a page what happened, as text ([What happened](#what-happened)); a page
ignores text it doesn't know. At most 16 pages watch at once;
more are refused.

## The opening message

The first message is text: JSON naming what the frames carry.

```json
{
  "version": 1,
  "links": ["base_link", "link_1", "link_2"],
  "streams": [
    {
      "name": "arm_joints",
      "values": [
        {"name": "sequence", "plotted": false},
        {"name": "source_time", "plotted": false},
        {"name": "joint_positions[0]", "plotted": true}
      ]
    }
  ],
  "history_s": 600
}
```

- `version` is the stream's version: 1. A page that reads another closes
  the connection.
- `links` are the robot's links in the order a frame gives their poses: the
  robot description's, as the scene lists them.
- `streams` are in declaration order, a frame naming each by its position
  here; `values` are each stream's decoded values in decoding order, an
  array's as `field[index]`, with whether it is plotted. Sequence numbers
  and clocks, which only ever climb, aren't.
- `history_s` is how far back, in seconds, a page can look
  (`viewer.history_age`); less if `viewer.history` fills first.

## Frames

Every later message is binary: one frame. A frame is sent as soon as
samples arrive from the capture process's tap, which sends a sample of each
stream about 60 times a second, however fast the stream itself is. The
first frame after the opening message holds the robot's newest pose, so a
page shows the robot before it next moves.

Little-endian throughout. Times are UTC nanoseconds since the Unix epoch.
Fields aren't aligned: read them with a `DataView`.

| Bytes | Holds |
|---|---|
| 4 | `KVFR` |
| 2 | The stream's version: 1 |
| 2 | Flags: bit 0, it holds poses |
| 8 | When Kapto received the newest message the frame shows (signed) |
| 8 | When the bridge sent the frame, on the same clock: the difference is the time it spent in Kapto |
| 4 | Links with a pose, `L`: every link, or 0 if the robot didn't move |
| 4 | Samples, `S` |
| 28 `L` | Each link's pose relative to the root link, in the opening message's order: x, y and z in metres, then a unit quaternion x, y, z, w, as 32-bit floats |

Then each sample, summarising a stream's messages since its last sample:

| Bytes | Holds |
|---|---|
| 2 | The stream's position in the opening message's `streams` |
| 2 | Zero |
| 4 | How many messages it summarises |
| 4 | Values, `V` |
| 8 | When the oldest of them was received (signed) |
| 8 | When the newest was received (signed) |
| 16 `V` | Each value's minimum then maximum over those messages, as 64-bit floats, in the opening message's order |

A spike between two samples still shows: it is the minimum or maximum of
the sample that covers it.

## Looking back

The bridge keeps what it streamed for the last `viewer.history_age`
seconds, at most `viewer.history` MiB, the oldest going first. A page asks
for a stretch as text:

```json
{"history": {"id": 7, "from": 1772700400000000000, "to": 1772700460000000000, "points": 600}}
```

`from` and `to` are UTC nanoseconds, both included; `points`, from 1 to
10 000, is how many parts at most the stretch is merged into, each the same length:
usually the number of pixels it is drawn across. `id` is the page's own,
given back with the answer.

The answer is binary, little-endian, read with a `DataView` like a frame:

| Bytes | Holds |
|---|---|
| 4 | `KVHI` |
| 2 | The stream's version: 1 |
| 2 | Zero |
| 4 | The question's `id` |
| 8 | The oldest time kept, whatever was asked (signed; 0 if nothing is) |
| 8 | The newest time kept |
| 4 | Links, `L` |
| 4 | Poses, `P`: at most one per part, the newest in it, for the parts where the robot moved |
| (8 + 28 `L`) `P` | Each pose's time (signed), then each link's pose as in a frame |
| 4 | Streams with samples in the stretch, `M` |

Then, for each of those streams:

| Bytes | Holds |
|---|---|
| 2 | The stream's position in the opening message's `streams` |
| 2 | Zero |
| 4 | Values, `V` |
| 4 | Parts with samples, `B`, in time order |

and for each part:

| Bytes | Holds |
|---|---|
| 8 | When the oldest message in it was received (signed) |
| 8 | When the newest was |
| 4 | How many messages it covers |
| 4 | Zero |
| 16 `V` | Each value's minimum then maximum over them, as 64-bit floats |

A spike still shows however far the stretch is merged: it is the minimum or
maximum of the part that covers it. A question that can't be answered is
answered with text, `{"error": {"id": 7, "message": "…"}}`, `id` being
`null` if the question couldn't be read.

## What happened

The bridge watches the capture process for every event it takes, recorded
in an episode or not, and for each episode's start and end, and keeps them
for as long as its history (at most 10 000). Right after the opening
message, a page is told every one kept, oldest first; then each new one as
the bridge hears of it. Each time, as text:

```json
{"marks": [
  {"episode_started": {"episode": "20260316T164807.714Z-aa56e38a", "at_utc_ns": 1772700400000000000}},
  {"noted": {"event": {"kind": "intervention", "at_utc_ns": 1772700402000000000, "phase": "begin", "id": "i1", "source": "api", "text": "box slipped"}, "episode": "20260316T164807.714Z-aa56e38a"}},
  {"episode_ended": {"episode": "20260316T164807.714Z-aa56e38a", "at_utc_ns": 1772700409000000000, "finalised": true}}
]}
```

- `noted`: an event as capture kept it, as the
  [recording](recording.md#events-kaptoevents) holds it, and the episode it
  was recorded in, `null` between episodes.
- `episode_started` and `episode_ended`: when the bridge heard of them;
  `finalised` is `false` for an episode aborted or failed.

Times are UTC nanoseconds, too large for a JavaScript number to hold
exactly; read as one, they are still right to well under a millisecond.
A page has room for 256 waiting; a page that stopped reading misses the
rest, and is told them all again when it connects again.

## Losing frames

The stream is lossy, like the tap it comes from. Each page has room for a
few frames; a frame that doesn't fit is dropped for that page and counted,
and the bridge never waits on a page. A page that stops reading for a few
seconds is let go. Recording is never affected by any of it.
