# Four processes from one binary; threads for high-rate work

## Context

Recording must continue no matter what else happens: the web UI hanging, the
viewer crashing, a slow check, a stalled upload. At the same time Kapto does
very different kinds of work: tens of thousands of small messages per second
in the recording path, a handful of web and API connections, CPU-heavy checks
after each episode, and long network uploads.

Each kind of work suits a different concurrency model. Async runtimes are
good for many mostly idle connections, but in a path handling 32,000 messages
a second they add scheduling jitter, move work between threads, and allocate
inside futures.

## Approach

Kapto runs as four long-lived processes, all started from the one `kapto`
binary:

| Process | Does | Runs as |
|---|---|---|
| server | Supervises the others; API and web UI; the only writer of the runtime database | async, small runtime |
| capture | Receives, timestamps and records data | one thread per input, one writer thread, one stats thread; normal or real-time priority |
| viewer bridge | Turns a lossy copy of live data into Rerun views | threads, slightly lowered priority |
| worker | Checks, statistics, relevance selection, labelling, export, upload | low priority; thread pool for CPU work, small async runtime for uploads |

- The server restarts the viewer bridge and the worker if they crash. It never
  restarts capture during a recording. If the server dies, capture keeps
  recording and finishes the current episode.
- Control messages travel over Unix sockets. Live data reaches the viewer
  bridge and the worker over non-blocking datagrams that are dropped, and
  counted, when the reader is slow, so a slow reader can never slow capture.
- Python plugins run as separate, time-limited processes.
- Every thread is named, so system tools and profilers show where time goes.

## Consequences

- A crash or hang outside capture can't stop a recording.
- Heavy work can't take CPU from capture: it runs at low priority and pauses
  while an episode is recording.
- Capture code stays plain and synchronous; the crates it uses never depend
  on an async runtime.
- Communication between processes needs a small, versioned message protocol.
- One binary to build and ship, despite four processes.

## Alternatives considered

- **One process, everything async:** simplest to write, but any blocking or
  crashing code takes recording down with it, and async adds jitter to the
  recording path.
- **One process with threads:** better latency, but still no isolation from
  crashes.
- **Separate programs:** the same isolation, but several artifacts to build,
  version and ship together.
