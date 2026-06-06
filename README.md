# Kapto

[![CI](https://github.com/carlosrzdr/kapto/actions/workflows/ci.yml/badge.svg)](https://github.com/carlosrzdr/kapto/actions/workflows/ci.yml)
[![Licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)

**Kapto** is an open-source edge collector for robot data, written in Rust.
It runs next to the robot and:

- **receives** data over UDP, TCP or ROS 2, at up to 2 kHz per stream, with
  no silent loss;
- **shows** what is happening, live and on replay, on the robot's own URDF
  model, in its own web UI;
- **checks** each episode as soon as it is recorded: completeness, motion,
  statistics and anomalies against the robot's other episodes;
- **uploads** it to storage you own (S3 or S3-compatible, Google Cloud
  Storage, Azure, or a folder), if you configure one.

It works fully offline: nothing leaves the machine except uploads to
destinations you configure. It runs with Docker on a modest PC without a GPU.

## Status

**Early development.** Recording from UDP, TCP and ROS 2, the live view and
replays in the web UI, quality checks on each episode, and uploads work.
Planned: deciding what is worth keeping (scoring episodes and stretches of
them for relevance, and keeping, downsampling or discarding them with an
audit of what was dropped), labelling, picking up recordings from a watched
folder, and a Python package.

## Try it

With Docker, from a clone of the repository:

```sh
docker compose build
docker run --rm -p 127.0.0.1:7700:7700 -p 127.0.0.1:9877:9877 kapto:dev demo
```

Then open <http://localhost:7700>: a synthetic seven-joint arm moves in the
viewer, and the Record button records it. [Getting started](docs/getting-started/README.md)
has the same without Docker, and a first recording of your own streams.

| Document | What it covers |
|---|---|
| [Requirements](docs/project/requirements.md) | Planning: what Kapto must do, priorities, performance targets, roadmap |
| [Implementation plan](docs/project/implementation-plan.md) | Planning: how it is built, as small pull requests |
| [AGENTS.md](AGENTS.md) | Design and contribution rules for everyone working in the repo, human or AI |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to propose a change |

## Licence

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE).
