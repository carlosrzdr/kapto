# Rust for the engine, Python on top

## Context

Kapto records many high-rate streams at once (for example sixteen streams at
2 kHz each, plus cameras) on modest hardware: a mainstream 4-core PC without a
GPU. Recording must never drop data because the program paused, so garbage
collection or interpreter locks in the recording path are not acceptable.

The same program also decodes robot payloads, computes robot kinematics,
checks recordings, feeds a viewer, serves an API and a web UI, and uploads
data. Keeping all of that in one language keeps one build, one set of types
and one place for every rule.

Many of Kapto's users are researchers who work in Python and want to read
recordings, run kinematics and write their own checks there.

## Approach

The whole application is written in Rust. A Python package, built from the
same Rust crates with PyO3, gives users access to the most useful parts:
reading recordings, robot packages and kinematics, checks, and controlling a
running collector. Python never runs in the recording path.

## Consequences

- Predictable latency and memory use without a garbage collector, and memory
  safety without manual memory management.
- Mature libraries exist for what Kapto needs: MCAP, Rerun, ROS 2, cloud
  storage, SQLite, HTTP.
- The Python package gives identical results to the Rust engine because it
  is the Rust engine.
- Rust takes longer to learn and to compile. Builds use dependency caching to
  keep rebuilds short.
- The Python bindings are an extra surface to maintain and test.

## Alternatives considered

- **C++:** equally fast, but without Rust's memory safety, and with harder
  dependency management and builds.
- **Go:** simpler, but its garbage collector adds pauses, and its libraries
  for MCAP, Rerun and robotics are thinner.
- **Python:** what many users know, but too slow for the recording path, and
  its interpreter lock limits parallelism.
