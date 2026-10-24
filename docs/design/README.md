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
| [Pre-roll and post-roll](pre-roll-and-post-roll.md) | A stream's last seconds before Start, held in a ring set aside at startup, start the episode; chosen streams go on after Stop; the episode spans its data |
| [Large messages](large-messages-in-rings-of-their-own.md) | Large messages go through rings of their own |
| [Cameras in the live view](cameras-in-the-live-view.md) | Each camera's newest frame over a stream socket of its own; the bridge turns raw frames into JPEG, and keeps a thinned history of each camera to look back over |
| [Layouts](layouts.md) | Columns of panels in TOML: the operator's own, else the config folder's default per package, else the package's, else made from its roles |
| [Cameras in replay](cameras-in-replay.md) | Each frame read from the recording by its MCAP indexes, on request; every frame can be stepped through |
| [Deleting episodes](deleting-episodes-through-a-bin.md) | Episodes deleted by hand go to a bin, emptied by the retention executor after a set time, when the disk runs low or on request *(planned)* |
| [Closing an episode](closing-an-episode-beside-receiving.md) | An episode is closed on a thread of its own while capture receives again; Stop is answered once it is finalised *(planned)* |

## Writing one

Copy [`template.md`](template.md) to `<topic>.md` and open it as its own
pull request before the code that depends on it. When the approach changes,
the document is rewritten to describe the new one, and the approach it
replaces moves to *Alternatives considered* with why it isn't taken.
