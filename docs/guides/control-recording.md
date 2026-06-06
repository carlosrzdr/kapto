# Control recording from your own programs

A robot program, a test rig or a script can start and stop Kapto's episodes
itself, for example to record each attempt at a task as one episode. It does
that through the HTTP API of `kapto serve`, the same one the web UI uses.
This guide shows the calls with curl, then a complete program in Python and
in Rust.

## Before you start

Kapto must be running. Without a robot, the demo is enough:

```text
cargo run --release --bin kapto -- demo
```

With a robot, start `kapto serve` with its package, as in the
[architecture overview](../concepts/architecture.md). Either way the API
answers on `http://127.0.0.1:7700/api`, on this machine only; the settings
`server.bind` and `server.port` change that (see the
[configuration reference](../reference/configuration.md)).

## The calls

| Call | What it does | Answer |
|---|---|---|
| `GET /api/version` | Kapto's version and the API's | `{"kapto": "0.0.0", "api": 1}` |
| `POST /api/episodes/start` | Starts recording an episode; answers once it is being recorded. `"waive"` lists the failed pre-flight checks to record despite (below) | `201` and `{"episode": "<id>"}` |
| `POST /api/episodes/stop` | Stops recording; answers once the episode is finalised, so its folder is complete | `{"episode": "<id>"}` |
| `POST /api/episodes/abort` | Stops recording and deletes the episode: for a failed attempt that isn't worth keeping | `{"episode": "<id>"}` |
| `POST /api/episodes/events` | Sends an event: something that happened, or the beginning, end or whole of a stretch. Recorded in the episode being recorded (below) | `201` and `{"event": {…}, "episode": "<id>"}` |
| `GET /api/event-kinds` | The kinds of events that may be sent, each with its name and colour | `{"kinds": […], "problems": […]}` |
| `GET /api/status` | The episode being recorded, if any, each stream's message counts, and pre-flight's latest run | see below |
| `POST /api/preflight` | Runs pre-flight again, between episodes; answers once it has ended, with each check's result | `{"passed": true, "checks": […], …}` |
| `GET /api/episodes` | The finalised episodes, newest first, each with whether the quality checks accepted it (`null` until checked) and how it was recovered if recording stopped without finishing it (`recovered`, `null` otherwise); `?accepted=false` lists only those a gate rejected | a list |
| `GET /api/episodes/<id>/qa` | What the quality checks found on an episode: whether it was accepted, the `qa_params_version` it was checked with, and each check's result | `{"accepted": true, "checks": […], …}` |
| `GET /api/episodes/<id>/statistics` | An episode's statistics, worked out with its quality checks: its duration, each stream's rate and loss, each joint's position, speed, acceleration, jerk and effort (count, extremes, mean, median, 90th and 99th percentiles), each gripper's cycles | `{"computed_utc_ns": …, "statistics": […]}` |

Requests that change something take a JSON body, `{}` for now, sent as
`Content-Type: application/json`. The [API reference](../reference/api.md)
lists every field.

## With curl

Check that Kapto answers, and which API version it speaks:

```sh
curl -fsS http://127.0.0.1:7700/api/version
```

Record an episode: start, look at the counts, stop.

```sh
curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7700/api/episodes/start
sleep 2
curl -fsS http://127.0.0.1:7700/api/status
curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7700/api/episodes/stop
```

The status tells the episode being recorded and, for each stream, the
messages received, written to the episode, dropped by Kapto and lost before
they reached it:

```text
{"capture":{"episode":"20260311T094107.512Z-3f9c2a1b","seconds":2,"streams":[{"stream":"arm_joints","received":4000,"written":4000,"dropped_by_us":0,"lost_upstream":0}],"values":[…],"preflight":{…}},"viewer":{"stream_port":9877}}
```

The episode just stopped is now first in the list of finalised episodes, and
its folder in the data directory is complete:

```sh
curl -fsS http://127.0.0.1:7700/api/episodes
```

To throw an attempt away instead, abort it: nothing of it is kept.

```sh
curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7700/api/episodes/start
curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7700/api/episodes/abort
```

## Pre-flight

Before recording, Kapto checks that this machine can record what the robot
sends: room and speed on the disk, and every declared stream arriving at
roughly its declared rate. It runs when the capture process starts, and
again when asked between episodes:

```sh
curl -fsS -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7700/api/preflight
```

It answers once it has watched the streams for `capture.preflight_window`
(five seconds by default), with each check: its `code`, whether it
`passed`, what was `measured` and `needed`, and, for a check that failed,
what to do about it (`remedy`). `/api/status` carries the same latest run
as `capture.preflight`.

```text
{"ran_at":"2026-03-15T20:00:00.000Z","window_ms":5000,"passed":false,"checks":[…,{"code":"preflight.stream_silent","summary":"A declared stream sent nothing","remedy":"Check that the robot is on…","stream":"arm_joints","passed":false,"measured":"0 messages in 5.0 s, 0/s","needed":"messages arriving","waived":false}]}
```

While a check of the latest run failed, starting an episode is refused with
`capture.preflight_failed`. Fix what failed and run pre-flight again, or
record anyway by waiving each failed check by its code:
`{"waive": ["preflight.stream_silent"]}` as the start's body. A failed check
not waived still refuses, and the episode keeps the run, saying which checks
were waived. With `capture.preflight = "warn"` in `kapto.toml`, a failed
check never refuses a start.

## Marking what happened

An event marks what happened while the robot was recorded, so episodes and
stretches can be found by it later: a grasp beginning and ending, a slip,
an operator stepping in. During an episode it is recorded in it, on the
same clock as the messages; between episodes it is only told to whoever
watches.

```sh
curl -fsS -X POST -H 'Content-Type: application/json' \
  -d '{"kind": "intervention", "phase": "begin", "id": "i1", "text": "box slipped"}' \
  http://127.0.0.1:7700/api/episodes/events
curl -fsS -X POST -H 'Content-Type: application/json' \
  -d '{"kind": "intervention", "phase": "end", "id": "i1"}' \
  http://127.0.0.1:7700/api/episodes/events
```

- `phase` is `moment` (the default), `begin` or `end` of a stretch, paired
  by their `id`, or `stretch` for a whole one told afterwards, with its end
  in `until_utc_ns`.
- `at_utc_ns` sets when it happened; without it, it is when capture takes
  it.
- `text` and `attributes` say more about it.

The kind must be one of `GET /api/event-kinds`: those of `events.toml` in
the configuration folder, or `note`, `problem` and `intervention` without
it, and each task's subtasks as `subtask.<name>` (see
[Event kinds](../reference/profiles.md#event-kinds-eventstoml)). The same
from a shell, on the machine running Kapto:

```sh
cargo run --bin kapto -- episode event --kind intervention --begin i1 --text "box slipped"
cargo run --bin kapto -- episode event --kind intervention --end i1
cargo run --bin kapto -- episode event --kind problem --last 10
```

`--last 10` marks the last ten seconds as a whole stretch.

## When Kapto refuses

A refused or failed request answers with an HTTP error status and a JSON body
with the cause `code`, a `message` and a `remedy`. Stopping when nothing is
being recorded, for example:

```sh
curl -sS -X POST -H 'Content-Type: application/json' -d '{}' \
  http://127.0.0.1:7700/api/episodes/stop
```

answers `409 Conflict` with:

```text
{"code":"capture.not_recording","message":"No episode is being recorded","remedy":"Start an episode first. If it was started with a duration, it may already have ended."}
```

Decide what to do from the `code`; the message and remedy are for people and
may be reworded. The [error codes](../reference/error-codes.md) page lists
every code. The ones a recording program meets most:

| Code | Status | Meaning |
|---|---|---|
| `capture.already_recording` | 409 | An episode is already being recorded, perhaps started from the web UI; or pre-flight was asked for while one is |
| `capture.not_recording` | 409 | Stop or abort with no episode being recorded |
| `capture.preflight_failed` | 409 | A check of pre-flight's latest run failed: this machine may not record what the robot sends; the message names each failed check, which the start can waive |
| `events.unknown_kind` | 409 | An event's kind isn't one that may be sent; the message lists those that may |
| `capture.event_invalid` | 400 | An event isn't well formed, e.g. a beginning without an ID |
| `capture.control_unreachable` | 503 | The capture process isn't answering; `kapto serve` restarts it |

## In Python

This program records a three-second episode and prints each stream's counts.
It uses only Python's standard library, so it runs on the robot's computer as
it is:

```python
"""Records a three-second episode through Kapto's API."""

import json
import time
import urllib.error
import urllib.request

API = "http://127.0.0.1:7700/api"


def call(method, path):
    """Sends one request; returns the answer's JSON, or raises with Kapto's cause code."""
    body = b"{}" if method == "POST" else None
    request = urllib.request.Request(
        API + path, data=body, method=method, headers={"Content-Type": "application/json"}
    )
    try:
        # Stopping answers once the episode is finalised, which takes a moment.
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        problem = json.load(error)
        raise RuntimeError(f"{problem['code']}: {problem['message']} {problem['remedy']}") from None


if call("GET", "/version")["api"] != 1:
    raise SystemExit("this program knows API version 1")

started = call("POST", "/episodes/start")
print("Recording", started["episode"])
time.sleep(3)

for stream in call("GET", "/status")["capture"]["streams"]:
    print(f"{stream['stream']}: {stream['received']} received, {stream['written']} written")

stopped = call("POST", "/episodes/stop")
print("Recorded episode", stopped["episode"])
```

With the [`requests`](https://requests.readthedocs.io/) package, `call` is
`requests.request(method, API + path, json={} if method == "POST" else None)`
followed by a check of the status code.

## In Rust

The same program in Rust, with [`reqwest`](https://docs.rs/reqwest) (features
`blocking` and `json`) and `serde_json`:

```rust
//! Records a three-second episode through Kapto's API and prints what each
//! stream received. Anything Kapto refuses ends it with the cause code.

use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{Value, json};

/// Where `kapto serve` answers.
const API: &str = "http://127.0.0.1:7700/api";

type Error = Box<dyn std::error::Error>;

fn main() -> Result<(), Error> {
    let episode = record(API, Duration::from_secs(3))?;
    println!("Recorded episode {episode}");
    Ok(())
}

/// Records one episode lasting `length` and returns its ID.
pub fn record(api: &str, length: Duration) -> Result<String, Error> {
    // Stopping answers once the episode is finalised, which takes a moment.
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let get = |path: &str| answer(client.get(format!("{api}{path}")));
    let post = |path: &str| answer(client.post(format!("{api}{path}")).json(&json!({})));

    let version = get("/version")?;
    if version["api"] != 1 {
        return Err(format!("this program knows API version 1, not {}", version["api"]).into());
    }

    let started = post("/episodes/start")?;
    let episode = started["episode"].as_str().unwrap_or_default();
    println!("Recording {episode}");
    std::thread::sleep(length);

    let streams = get("/status")?["capture"]["streams"].take();
    for stream in streams.as_array().unwrap_or(&Vec::new()) {
        println!(
            "{}: {} received, {} written",
            stream["stream"].as_str().unwrap_or_default(),
            stream["received"],
            stream["written"]
        );
    }

    let stopped = post("/episodes/stop")?;
    Ok(stopped["episode"].as_str().unwrap_or_default().to_owned())
}

/// Sends a request: the answer's JSON, or an error with Kapto's cause code.
fn answer(request: RequestBuilder) -> Result<Value, Error> {
    let response = request.send()?;
    let succeeded = response.status().is_success();
    let body: Value = response.json()?;
    if succeeded {
        Ok(body)
    } else {
        Err(format!("{}: {} {}", body["code"], body["message"], body["remedy"]).into())
    }
}
```

## Watching without asking

Asking `/api/status` now and then is enough for most programs. To hear about
changes as they happen, open the WebSocket `ws://127.0.0.1:7700/api/events`.
Each text message is one JSON event:

| `event` | Sent | Other fields |
|---|---|---|
| `status` | At once, then at least once a second | `status`: what `/api/status` answers |
| `episode_started` | When an episode starts, from any client | `episode` |
| `episode_ended` | When it ends | `episode`, and `finalised`: `false` if it was aborted or failed |
| `episode_recovered` | When an episode that recording left unfinished, by a crash, a kill or a power cut, has been recovered and listed | `episode` |
| `capture_unreachable` | Once, when the capture process stops answering | `error`: code, message and remedy |

Episodes started or stopped elsewhere, from the web UI or `kapto episode`,
are reported the same way.

## What stays stable

- Within one API version, the number `api` in `/api/version`, the API only
  gains endpoints, fields and kinds of event. Ignore fields and events you
  don't know, and your program keeps working as Kapto is updated. Every
  change is checked for this before it is merged.
- Error codes are never renamed or removed; new ones are added.
- A new API version may change anything. Check the version when your
  program starts, as the examples do, so a mismatch fails clearly instead of
  half-way through a recording.
