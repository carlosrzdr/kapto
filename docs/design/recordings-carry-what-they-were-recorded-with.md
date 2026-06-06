# A recording carries everything it was recorded with

## Context

An episode is an MCAP recording and, beside it, `episode.json`, the
manifest and `_FINALISED`. The messages, a schema per stream describing
its layout, and the clock map have to be in the recording to read it at
all. Everything else an episode was recorded with could otherwise be only
beside it, or nowhere:

- the robot package: which one, which version, and its files (the URDF,
  `streams.toml`, `mapping.toml`, `qa.toml`);
- the session profile and task;
- the capture settings that shaped the recording, e.g. buffer sizes and
  how often it is put on disk;
- the counts at the end of recording: messages received, written, dropped
  by Kapto and lost before they reached it.

Recordings travel. They are uploaded, copied, handed to people and tools
that have never seen the machine that made them, often as the `.mcap` file
alone. A recording that needs its package folder, or Kapto's database, to
be understood loses its meaning the moment it leaves.

Settings that live only in the package also drift. Quality checks run after
recording, sometimes long after, and the package's `qa.toml` may have been
edited, or the package removed, in between. Checking an episode against
settings it wasn't recorded with changes its verdict without anyone asking.

## Approach

**Every recording carries, inside the MCAP file, everything Kapto recorded
it with.** It is understandable, and its checks reproducible, from the
file alone.

- **At the start of an episode**, before the first message, the writer
  thread writes once:
  - a `kapto.episode` metadata record: the episode's ID, Kapto's version,
    when it started, the robot package's identity (ID, version, content
    hash), and the copies of the session profile and task, with their
    hashes;
  - a `kapto.capture` metadata record: every capture setting that shaped
    the recording. Secrets are never among them;
  - the robot package's files as attachments, exactly as they were, each
    named after its path in the package. Meshes and their textures are
    left out by default, since a real robot's can take tens of megabytes
    per episode; `capture.attach_meshes` puts them in. Each mesh left out
    is still named in `kapto.capture` with its SHA-256, so it can be matched
    to a package later.
- **When the recording ends**, before the file is closed, it writes a
  `kapto.summary` metadata record with what `episode.json` says at that
  point: when it ended, every stream's counts, the buffers and durability.
  The clock map record is already written then.
- Metadata records hold text values under dotted keys
  (`package.id`, `profile.operator`), with lists as JSON, so `mcap info`,
  Foxglove and similar tools show them as they are. The exact records are a
  format specification in `docs/reference/`, additive-only like every
  output format.
- **What is computed after the recording is finalised can't be inside it**,
  since a finalised recording is never changed: the manifest check, quality
  results and, later, labels and relevance. These are kept in files beside
  it, listed in the manifest.
- **Anything that works on a recorded episode reads its settings from the
  episode**, never from the package as it is now. The quality checks are
  made from the `qa.toml` the recording carries; an episode recorded
  without one is checked with the defaults, and its results say so.
- `episode.json` stays as a quick summary for tools that don't read MCAP,
  written from the same values as the records.
- Every feature that adds something an episode is recorded with adds it to
  the recording in the same pull request.

## Consequences

- A recording opened anywhere says which robot, package version, operator,
  task and settings it came from, and how complete it is.
- A verdict never changes because a package was edited after recording;
  checking old episodes with new settings is something asked for on
  purpose.
- An episode whose package was since removed can still be checked.
- The start records are written before any message, so a recording cut off
  by a crash and recovered still identifies itself; the summary record is
  missing from it, which says it wasn't finished.
- Each recording grows by its package's size without meshes, usually a few
  tens of kilobytes, or with them when `capture.attach_meshes` is on.
- Writing the records is once per episode, in the writer thread, never per
  message: the capture path's cost doesn't change.
- `episode.json` and the records say the same things, so a test checks that
  every value of one is in the other.

## Alternatives considered

- **Keep metadata only beside the recording** (`episode.json`, the
  package folder): smaller files, but the `.mcap` alone means little, and
  checks after the fact depend on files that may have changed or gone.
- **Copy the settings into the episode folder only**: reproducible inside
  Kapto, but lost as soon as the `.mcap` travels on its own.
- **Always attach the meshes**: the recording alone can draw the robot,
  at tens of megabytes per episode for a real robot. Left as an option
  rather than the default.
- **Write everything when the recording ends**: one place to write it, but
  a recording cut off by a crash would carry nothing about itself.
