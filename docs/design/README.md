# Design

Each document describes one part of how Kapto is built that is expensive to
change: the situation, the approach taken, what follows from it, and the
other approaches considered. They are written to be read on their own,
without other documents, and always describe the design as it is.

| Design | Approach |
|---|---|
| [Engine language](rust-for-the-engine.md) | Rust for the engine, Python on top |
| [Recordings](mcap-recordings-stored-as-received.md) | MCAP recordings, payloads stored as received |
| [Licence](apache-2-licence.md) | Apache License 2.0 |
| [Configuration](toml-for-configuration.md) | TOML for all configuration |
| [Processes and threads](process-and-thread-model.md) | Four processes from one binary; threads for high-rate work |
| [Web UI](web-ui-stack.md) | Svelte and TypeScript for the web UI, built in Docker, embedded in the binary |
| [What a recording carries](recordings-carry-what-they-were-recorded-with.md) | A recording carries everything it was recorded with |
| [Viewer](a-viewer-of-our-own.md) | A viewer of Kapto's own, live and replay, in the web UI |
| [ROS 2](ros-2-through-rcl.md) | ROS 2 topics through ROS 2's own client library, taken as received |
| [Uploads](uploads.md) | One engine in the worker, storage through `object_store` |
| [Kept messages](kept-messages-start-each-episode.md) | A stream's kept messages start each episode |
| [Large messages](large-messages-in-rings-of-their-own.md) | Large messages go through rings of their own |
| [Cameras in the live view](cameras-in-the-live-view.md) | Each camera's newest frame over a stream socket of its own; the bridge turns raw frames into JPEG |

## Writing one

Copy [`template.md`](template.md) to `<topic>.md` and open it as its own
pull request before the code that depends on it. When the approach changes,
the document is rewritten to describe the new one, and the approach it
replaces moves to *Alternatives considered* with why it isn't taken.
