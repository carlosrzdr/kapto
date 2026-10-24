# An episode is closed on a thread of its own while capture receives again

## Context

When an episode ends, after the post-roll of each stream that has one,
capture closes it: it writes the summary record and the MCAP file's index
and summary, syncs the file, writes `episode.json`, moves the folder out of
staging, lists every file with its SHA-256 in the manifest, and writes
`_FINALISED` last. Only then does it answer Stop and say the episode ended.

All of this runs on the thread that drives capture's phases, between the
episode's phase and the next idle one. While it runs, no phase is running:
the receivers are stopped and nothing feeds the live view. The work grows
with the episode: the manifest reads every byte back to hash it. With two
1080p cameras at 60 fps, 20 seconds of recording is about 1 GB, and
closing it takes about 2.5 s on the maintainer's laptop:

- the live view stops for that long, then jumps, which an operator sees
  after every episode;
- what the robot sends meanwhile waits in the operating system's socket
  buffers and the middleware's queues, which drop what doesn't fit (a
  UDP socket's buffer at 2 kHz, a ROS 2 topic's `KeepLast` depth). None of
  it belongs to an episode, so nothing recorded is lost, but the pre-roll
  of an episode started right after misses that gap;
- an episode can't start until the last one is closed.

What has to hold:

- **A partial recording never looks finished**: stage, rename, manifest,
  `_FINALISED` last, in that order, and recovery from a crash at any step
  as today.
- **Stop is answered once the episode is finalised**, with its folder, and
  the episode's end is announced as finalised only then: the server and
  the worker act on that.
- **Shutting down finishes what was started**: SIGTERM finalises the episode
  being recorded, and one being closed is finished before the process
  exits.
- **Every thread is named**, and capture's threads are counted in its
  declared memory.

## Approach

**Capture hands the closing of an episode to a thread of its own,
`cap-finalise`, and starts the next idle phase at once; Stop is answered,
and the episode's end announced, when that thread is done.**

- **Handing over.** When the episode's phase ends, the writer thread hands
  back the open recording as today. The capture thread then gives it, with
  what the summary and `episode.json` need (the counts, buffers and clock
  as the phase ended) and the staged folder, to `cap-finalise`, and starts
  the idle phase: receivers and the live view carry on within
  milliseconds.
- **Closing.** `cap-finalise` does what the capture thread does today, in
  the same order: summary record, the file's index and summary and sync,
  `episode.json`, rename, manifest, `_FINALISED`. It runs at the worker's
  lower priority, so reading the file back to hash it doesn't take a core
  from receiving. It sends what came of it, the folder or the error, over
  a channel.
- **Answering.** The idle phase's loop, which already answers control
  requests between batches, takes that result when it comes: it answers
  the Stop it holds and announces the episode's end, finalised or not.
- **One at a time.** A Start while an episode is being closed is
  accepted: the new episode has a folder of its own. Its Stop, or one
  more, waits for the closing before it to finish before its own is
  handed over, so at most one episode is closed at a time and their ends
  are announced in order.
- **Status.** Capture's status says which episode is being closed, and
  the page shows it beside the live view, which goes on.
- **Shutting down.** SIGTERM finalises the episode being recorded through
  the same thread, then waits for it before the process exits.
- **A crash while closing** leaves the episode in staging or moved without
  its marker, which recovery handles as it does today.

## Consequences

- The live view and receiving carry on through the end of an episode;
  an episode can start right after the last one stopped, with its
  pre-roll complete.
- One more thread in capture, idle but for a few seconds after each
  episode, holding the recording's open file and the summary's inputs
  meanwhile; nothing is copied.
- Closing an episode competes with the next one for the disk while it
  reads the file back to hash it. On a disk only just fast enough for the
  streams, the next episode's writer may fall behind for those seconds,
  and its rings absorb it as they do any slow write.
- The control answer to Stop still comes when the episode is finalised,
  so clients see no change but faster answers to everything else.
- The process and thread model lists `cap-finalise` beside capture's
  other threads.

## Alternatives considered

- **Closing on the capture thread, as before**: the simplest, and nothing
  runs concurrently, but the live view stops for seconds after every
  episode, longer the more it recorded.
- **Hashing as the file is written**: the writer updates the file's
  SHA-256 with each chunk it writes, so closing reads nothing back. It
  saves the reading, but costs the writer thread about a tenth of a core
  at two 1080p cameras' rate, on the recording path, for every episode
  whether or not anything waits on it.
- **Answering Stop before the episode is finalised**: the page would hear
  back at once, but a client told an episode stopped couldn't rely on its
  folder being complete, and the worker would need a second signal.
- **Closing in the worker process**: the worker already does slow work at
  low priority, but the open recording, its clock and counts would have to
  cross a process boundary, and a worker restart in the middle would leave
  an episode only capture's recovery can finish.
