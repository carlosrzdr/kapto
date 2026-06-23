# Capture control protocol

How a running `kapto capture` is told to start, stop and abort episodes, and asked
what it is doing. `kapto episode` and `kapto serve` speak it. This page is
a specification: requests and answers only gain fields.

## Connection

- A Unix stream socket, `capture.control_socket` (by default `capture.sock`
  in the data directory). Anyone who may open the socket file may control
  capture, so keep it in a folder only Kapto's user can reach.
- Each request and each answer is one line of JSON, ending with a newline.
  A request is at most 4096 bytes.
- The first request on a connection is `hello` with the protocol version,
  1. The answer gives the capture process's protocol and Kapto versions.
  Another version, a request that isn't JSON, or any other request first
  is answered with an error and the connection is closed. Whatever else the
  client already sent is read and discarded first, so it reads the error
  and then the end of the connection, never a reset.
- After `hello`, requests are answered one at a time, in order.

## Requests

| Request | Answer |
|---|---|
| `{"command":"hello","version":1}` | `{"hello":{"version":1,"kapto":"0.1.0"}}` |
| `{"command":"status"}` | `{"status":{"episode":null,"seconds":12,"streams":[…],"preflight":{…}}}`, `preflight` once a run has ended |
| `{"command":"start"}`, `{"command":"start","duration_s":30}`, with `"profile"`, `"task"` and `"waive"` (below) | `{"started":{"episode":"<id>"}}` once it is recording; while pre-flight runs, once it has ended |
| `{"command":"stop"}` | `{"stopped":{"episode":"<id>","folder":"<path>"}}` once the episode is finalised |
| `{"command":"abort"}` | `{"aborted":{"episode":"<id>"}}` once the episode is deleted |
| `{"command":"supervise"}` | `"supervised"` at once |
| `{"command":"watch"}` | `"watching"` at once, then an announcement line for each episode recovered when capture started, and each time an episode starts or ends |
| `{"command":"preflight"}` | `{"preflight":{"ran_at":"<UTC>","window_ms":5000,"checks":[…]}}` once pre-flight has run |
| `{"command":"event","kind":"grasp","phase":"begin","id":"g1","source":"cli"}` (below) | `{"noted":{"event":{…},"episode":"<id>"}}` at once; `episode` is `null` between episodes |

`start` may carry the session profile and the task the episode is recorded
with, as copies that the episode keeps in `episode.json`:
`"profile":{"package":"arm7","profile":{"id":"lab_ana","operator":"Ana",…}}`
and `"task":{"id":"pick_and_place","subtasks":[…],…}`, each with the fields
`episode.json` gives it. Whoever asks reads the files (`kapto episode
start --profile … --task …` and the API do); the capture process never
reads the configuration folder. A profile whose `package` isn't the robot
package capture records is refused with `profile.unavailable`.

`status` reports the episode being recorded (`null` while none is), the
seconds since it started (or since receiving started while none is), and
each stream's `received`, `written`, `dropped_by_us` and `lost_upstream`
over that time. A stream the live view has decoded also has `values`, its
newest decoded values (field after field, array elements in order), and
`values_age_ms`, how long ago they were decoded; they are sampled for
display about sixty times a second, not taken from every message. A camera
also has `live_view`: what became of its frames on their way to the live
view since the capture process started, as `offered` (its newest, at most
`viewer.camera_max_fps` a second), `skipped` (the live view held every
slot), `superseded` (replaced by a newer frame before it could be sent),
`not_sent` (no viewer bridge listening) and `sent`.

`stop`, and the end of a start's `duration_s`, end each stream's messages
at once, except those of a stream with a post-roll (`post_roll_s`), which
goes on being recorded for its seconds. The episode is finalised after the
longest post-roll, so `stop` answers that much later. A message received
after its stream ended is neither written nor counted.

`abort` ends the episode at once, ignoring post-rolls, then deletes it instead of finalising
it: it was never moved out of staging, so nothing ever took it for an
episode. A capture process older than `abort` answers it with
`capture.control_invalid_request`.

`supervise` makes the connection a supervisor's: `kapto serve` sends it on a
connection it keeps open, and again every second as a sign of life. When
the last supervisor's connection closes, because serve stopped or crashed,
an episode that was being recorded is finalised once
`capture.control_timeout` has passed, unless a supervisor connects again
first. Episodes started after the supervisor left, and a capture process
that never had one, are left alone. It is answered by the connection
itself, never waiting on recording.

`watch` makes the connection a watcher's: after `"watching"` it receives
one line for each episode starting and ending, as it happens, however
short the episode and whoever started it:

```text
{"announced":{"event":"episode_started","episode":"<id>"}}
{"announced":{"event":"episode_ended","episode":"<id>","finalised":true}}
```

`finalised` is `false` for an episode that was aborted or failed.

When it starts, the capture process finalises each episode that recording
left unfinished, by a crash, a kill or a power cut
([episodes on disk](../concepts/episodes.md#when-recording-stops-without-finishing)),
and announces each one:

```text
{"announced":{"event":"episode_recovered","episode":"<id>"}}
```

Every watcher hears of each recovered episode, those connecting later
included: one connecting after it was recovered gets it right after
`"watching"`, before anything else. A client older than
`episode_recovered` skips a line it doesn't know. The connection receives
nothing but announcements from then on, and what it sends is ignored. Announcements are written by a thread of their own, never by
recording: a watcher that doesn't take one within a second is
disconnected. A capture process older than `watch` answers it with
`capture.control_invalid_request`.

`event` says something happened, as the [recording format](recording.md#events-kaptoevents)
gives an event: `kind`, `source`, and optionally `phase` (`moment`,
`begin`, `end`, or `stretch` for a whole stretch told at once), `id`
(pairing a beginning with its end), `text`, `attributes`, `at_utc_ns`, its
moment in UTC nanoseconds, which is when the capture process takes it if
left out, and `until_utc_ns`, when a whole stretch ended. During an episode it is
written into the recording, between the streams' batches; between
episodes there is no recording to keep it in. Either way the answer gives
the event as kept, and every watcher is told:

```text
{"announced":{"event":"event_noted","happened":{"kind":"grasp",…},"episode":"<id>"}}
```

An event whose kind isn't lowercase words joined by dots, or begins
`kapto.` (Kapto's own), or comes from the source `kapto`, or is a
beginning or an end without an ID, is refused with `capture.event_invalid`.

`preflight` checks whether this machine can record what the robot sends.
The capture process runs it once when it starts and again for each
`preflight` asked between episodes (one asked while a run goes on is
answered with that run): it watches the streams for
`capture.preflight_window` while it writes and flushes a 32 MiB test file
in the data directory, then answers with every check:

```text
{"code":"preflight.disk_too_small","passed":true,"measured":"121.9 GiB free","needed":"2.8 GiB free: 10 minutes of recording at 1.4 MiB/s, above the 2.0 GiB recording stops at","waived":false}
{"code":"preflight.stream_silent","stream":"joints","passed":false,"measured":"0 messages in 5.0 s, 0/s","needed":"messages arriving","waived":false}
```

`code` is the cause code the check fails with, so it names the check:
`preflight.disk_too_small` (room for `capture.preflight_disk_time` of
recording above `capture.disk_stop_below`), `preflight.disk_too_slow` (the
test file written at least as fast as the streams send), and, for each
stream, `preflight.stream_silent` or `preflight.stream_slow` (at least
90 % of its declared rate). The streams' rate is an upper bound: each
stream's declared or measured rate, whichever is higher, times its largest
message. A `start` asked while pre-flight runs waits until it has ended.
When a stream has a pre-roll (`pre_roll_s`), the episode still starts at
the `start`: every other stream is held from when it was asked, so what
arrived while it waited is in the episode.

Each `start` is checked against pre-flight's latest run. While a check of
it failed, the start is refused with `capture.preflight_failed`, naming
each failed check, unless it waives each one by its code:
`"waive":["preflight.stream_silent"]`. With `capture.preflight = "warn"`
it starts anyway. Either way the episode keeps the run, in `episode.json`
(`preflight`) and in its recording (the `kapto.preflight` record), each
check saying whether it passed and whether it was waived. A capture
process older than `waive` ignores it.

`status` carries the latest run as `preflight` once one has ended; while
an episode is recorded, the run that episode keeps, its waived checks
marked. A capture process older than that leaves it out.
`preflight` is refused with `capture.already_recording` while an episode is
recorded, since its test file would compete with the recording's writes. A
capture process older than `preflight` answers it with
`capture.control_invalid_request`.

## Errors

A refused or failed request is answered with
`{"error":{"code":"<cause code>","message":"<summary and specifics>"}}`. The
codes are listed in [Error codes](error-codes.md); the ones specific to
control are:

| Code | When |
|---|---|
| `capture.control_invalid_request` | Not JSON, not a known request, longer than 4096 bytes, or not `hello` first |
| `capture.control_version_mismatch` | `hello` with another protocol version |
| `capture.already_recording` | `start` or `preflight` while an episode is being recorded |
| `capture.preflight_failed` | `start` while a check of pre-flight's latest run failed, not waived |
| `capture.not_recording` | `stop` or `abort` while none is |
| `capture.event_invalid` | `event` with a malformed kind, ID or text, or a kind or source of Kapto's own |
