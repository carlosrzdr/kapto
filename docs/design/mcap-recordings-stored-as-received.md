# MCAP recordings, payloads stored as received

## Context

A recording mixes very different data: joint states at 2 kHz, force and
torque readings, compressed camera frames, audio, robot status. The file
format has to hold all of it, survive a crash or power cut part-way through,
describe its own contents, and be readable by tools people already use.

Raw robot data is the most valuable thing Kapto produces. Once anything is
lost or altered on the way to disk, it can't be recovered.

## Approach

- Each episode is recorded as an **MCAP** file (or a series of rotated
  segments), with one channel per stream and the schema of each channel
  embedded in the file. Data is written in chunks, compressed with a fast
  compressor (lz4, or zstd at a low level).
- **Payloads are stored exactly as received.** Nothing is transcoded or
  re-encoded while recording.
  - Payloads in self-describing encodings get the matching MCAP schema
    encoding (ROS 2 CDR, Protobuf, FlatBuffers, JSON), so other tools can
    decode them directly.
  - Payloads in a plain binary layout are stored as received, with a schema
    describing the layout. Kapto's readers, exporters and Python package
    decode them.
- **Two timestamps per message**, never overwriting each other: when Kapto
  received it (host clock) and the timestamp the robot put in it (robot
  clock), kept raw.

## Consequences

- Recordings open in the `mcap` command-line tool, Foxglove, and ROS 2 tools
  (MCAP is ROS 2's default bag format).
- Chunking makes crash recovery possible: a recording cut off part-way
  through is recoverable up to its last complete chunk.
- Recording costs little CPU, because nothing is converted on the way in.
- Streams with a plain binary layout appear as raw bytes in other tools until
  they are exported; Kapto's own tools show them decoded.

## Alternatives considered

- **ROS 2 bags in SQLite:** tied to ROS, and not readable without it.
- **HDF5:** good for arrays, but poor at appending many interleaved streams
  and fragile when a write is cut off.
- **Rerun's `.rrd` format:** made for its viewer, not as an archival format
  with stability guarantees.
- **A format of our own:** full control, but no existing tools could read it.
