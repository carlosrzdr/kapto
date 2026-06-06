# Reference

Exact descriptions of Kapto's commands, settings and error codes. These pages
are generated from the code with `cargo xtask gen`, so they always match the
version they were built with.

- [Command line](cli.md): every command and flag.
- [API](api.md): the local HTTP API of `kapto serve`, with its OpenAPI
  document.
- [Configuration](configuration.md): every setting, its accepted values,
  default, environment variable and flag.
- [Error codes](error-codes.md): every problem Kapto can report, and what to
  do about it.
- [Episode summary](episode.md): `episode.json`, what each episode holds and
  every stream's message counts.
- [Manifest](manifest.md): the list of an episode's files and checksums, and
  the rule for when an episode is complete.
- [Upload marker](upload-marker.md): `_SUCCESS`, written last in an uploaded
  episode's folder at its destination, and the rule for when an uploaded
  episode is complete.
- [Recording format](recording.md): what each episode's MCAP file holds:
  its channels and the records that say what it was recorded with.
- [Capture control protocol](capture-control.md): how a running capture
  process is told to start and stop episodes.
- [Viewer scene](viewer-scene.md): how the web UI is told what the robot
  looks like, and the binary form its meshes are sent in.
- [Viewer stream](viewer-stream.md): how the robot's motion and plotted
  values are streamed to a page as they arrive.
- [Replay view](replay-view.md): what the web UI's player shows a recorded
  episode from, at every zoom, and how it is laid out.
- [Struct layout](struct-layout.md): how a message with a fixed binary layout
  is described, and how recordings store that description.
- [Robot package format](package.md): what `package.toml` says, and what
  identifies a version of a package.
- [Profiles and tasks](profiles.md): session setups saved in the
  configuration folder, and every key they can have.
- [JSON and MessagePack layout](path-layout.md): how fields are found in JSON
  and MessagePack messages by path, and how their values are converted.
