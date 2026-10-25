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

Closing still reads the whole recording back to hash it, which takes
longer the longer the episode: seconds for every minute recorded with two
cameras. On SIGTERM, capture has `capture.stop_grace` (30 s by default) to
finalise the episode being recorded: an episode long enough can't be
hashed in time, the process is killed, and the episode is left for
recovery. Docker's own grace (10 s unless `stop_grace_period` is set) cuts
it shorter still.

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
- **Hashing behind the writer.** While an episode is recorded, a thread of
  its own, `cap-hash`, at the worker's lower priority, reads the recording
  file back up to where the writer has written and flushed, and keeps the
  file's SHA-256 running. The writer only publishes how far the file is
  written and will no longer change, as one atomic store each time it
  puts the file on disk: a chunk's header is rewritten once the chunk is
  complete, so only what came before the chunk being written is final. It
  never waits on `cap-hash`, which may fall behind and catch up. The bytes
  it reads were just written and are usually still in the page cache, so
  it costs CPU rather than disk. At closing, `cap-finalise` hands
  `cap-hash` the rest of the file (the summary and index written at
  close, and whatever it hadn't reached) and takes the digest for the
  manifest. Every other file in the folder is small and is hashed at
  closing as today. If `cap-hash` stopped or failed, or the file isn't
  the one it read or has another size, closing hashes the whole file as
  before, so the manifest is never built from a hash that didn't see
  every byte.
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
- Closing takes about as long as hashing what `cap-hash` hadn't reached,
  usually the last moments of the episode, whatever its length: a long
  episode is finalised within `capture.stop_grace`.
- One more thread in capture during episodes, reading from the page cache
  at low priority. On a machine short of CPU it falls behind, and closing
  does the rest, as slow as before at worst.
- The process and thread model lists `cap-finalise` and `cap-hash` beside
  capture's other threads.

## Alternatives considered

- **Closing on the capture thread, as before**: the simplest, and nothing
  runs concurrently, but the live view stops for seconds after every
  episode, longer the more it recorded.
- **Hashing on the writer thread**: the writer updates the file's
  SHA-256 with each chunk it writes, so closing reads nothing back. It
  saves the reading, but costs the writer thread about a tenth of a core
  at two 1080p cameras' rate, on the recording path, for every episode
  whether or not anything waits on it.
- **A longer `capture.stop_grace`**: no code, but the grace would have to
  grow with the longest episode, and a service manager's own timeout
  still cuts it.
- **Answering Stop before the episode is finalised**: the page would hear
  back at once, but a client told an episode stopped couldn't rely on its
  folder being complete, and the worker would need a second signal.
- **Closing in the worker process**: the worker already does slow work at
  low priority, but the open recording, its clock and counts would have to
  cross a process boundary, and a worker restart in the middle would leave
  an episode only capture's recovery can finish.
