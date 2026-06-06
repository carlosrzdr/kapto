# Episodes on disk

An episode is one continuous recording. Each lives in its own folder in the
data directory, named after its ID, e.g. `20260305T192345.123Z-1a2b3c4d`:
the UTC time it was created and a random suffix, so folders sort by time.

```
<data>/
├── .recording/                       episodes still being written
│   └── 20260305T192345.123Z-1a2b3c4d/
│       └── recording.mcap
└── 20260305T181002.004Z-77e0a91c/    a finished episode
    ├── recording.mcap
    ├── episode.json
    ├── manifest.json
    └── _FINALISED
```

## What the files hold

- `recording.mcap`: every message exactly as received, one channel per
  stream, readable with the `mcap` command-line tool and Foxglove.
- `episode.json`: what the episode is and what happened while it was
  recorded: its ID, start and end times, how the host clock behaved, how
  full the buffers got, how long messages waited before they were on disk,
  and every stream's message counts (received, written, dropped by Kapto,
  lost before reaching it). The [episode summary reference](../reference/episode.md)
  describes every field.
- `manifest.json` and `_FINALISED`: see below.

## When an episode is complete

An episode is complete only when its folder holds `_FINALISED`, and its
`manifest.json` lists exactly the files in the folder with their sizes and
SHA-256 checksums. The [manifest reference](../reference/manifest.md) gives
the exact rule and format. Anything that reads episodes (uploads, exports,
your own scripts) should check this and nothing else: a folder that fails
it is either still being written or was interrupted.

## How an episode is finished

```
 being written           moved                   listed                  complete
.recording/<id>/  ──▶  <id>/            ──▶  <id>/              ──▶  <id>/
 recording.mcap         recording.mcap        + manifest.json         + _FINALISED
 episode.json           episode.json
```

1. **Written in staging.** The recording is written under `.recording/`,
   where nothing mistakes it for a finished episode. When it is closed,
   `episode.json` is written next to it with the final counts.
2. **Moved.** Once the recording file is closed, the folder is renamed out of
   `.recording/`. On one disk a rename is atomic: the folder is either still
   staged or fully moved.
3. **Listed.** Every file is listed with its size and checksum in
   `manifest.json`. The manifest is written to a temporary file first, so it
   is either complete or absent.
4. **Marked complete.** `_FINALISED` is written last.

Each step is put on disk before the next starts. If Kapto or the machine
stops partway, the folder is left at a recognisable step: still under
`.recording/`, moved without a manifest, or with a manifest but no
`_FINALISED`. It never looks finished.

Kapto never overwrites an episode folder. If a folder with the same ID
already exists, finishing stops with `episode.already_exists` and the staged
recording is left where it is.

## When recording stops without finishing

A crash, a kill or a power cut can stop recording at any step above. The
next time `kapto capture` or `kapto record` starts, it finishes every
episode left that way, and says so. `kapto record` does it before
recording; `kapto capture` on a thread of its own, receiving and recording
meanwhile:

- **An episode still in `.recording/`** is salvaged. The writer puts the
  recording on disk every half second, so everything up to the last time
  it did is there. The
  recording is cut at the end of its last intact part, every part's
  checksum checked, and closed. Each message kept is exactly as received.
  `episode.json` is then written from what the recording itself holds: the
  package, profile and task it was recorded with, and each stream's
  messages. Its `recovered` field says when it was recovered, how many
  bytes were cut and the time of the last message kept. The episode is
  then finalised like any other.
- **An episode moved out of `.recording/`** but stopped before its
  manifest or `_FINALISED` was complete already, so it is only finalised,
  not marked recovered.
- **A folder in `.recording/` with nothing to recover**, such as one
  stopped before its recording was created, is left where it is, and
  Kapto says why. It is never deleted, and never taken for an episode.

What only finishing a recording would have counted isn't known for a
recovered episode, so it isn't made up: its streams' loss detection is
`undetectable`, and the clock and buffer figures are left empty. The
[episode summary reference](../reference/episode.md) describes
`recovered`.

The process recording an episode holds its folder locked until the
episode is finalised. The operating system releases the lock when the
process ends, however it ends, so recovery never touches an episode
another process is still recording or finalising.

`kapto serve` lists each recovered episode as soon as capture has
recovered it, and checks it like any other. The API gives its `recovered`
object, and the web UI marks it **recovered** in the episode list, with
its loss unknown, and says in its details when it was recovered, how much
was cut off and when its last message was.
