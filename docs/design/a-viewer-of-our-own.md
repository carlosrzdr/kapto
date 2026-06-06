# A viewer of Kapto's own, live and replay, in the web UI

## Context

Kapto shows the robot moving on its 3D model, and every decoded value
plotted, live and when an episode is replayed. The page showing it is left
open all day beside a robot, on whatever computer the operator has, often
without a graphics card. What it needs is bounded by design: the robot's
pose now, and the last minute or so of each plot.

What has to hold:

- **Memory stays flat.** Leaving the page open for a shift must cost what
  leaving it open for a minute does, in the browser and in Kapto.
- **Little delay.** Between a message's arrival and the robot moving on
  screen, nothing should wait on a timer it doesn't need, and the delay
  should be visible.
- **One page.** Live, replay and the rest of the UI share one page and one
  Content-Security-Policy; nothing is loaded from elsewhere and nothing
  contacts the internet.
- **What Kapto already has is reused.** Forward kinematics, mesh loading
  and shapes are Kapto's own, shared with the checks; the viewer needs only
  to draw meshes, plot, and play a timeline.

## Approach

**Kapto draws the robot and its plots itself, in the web UI, live and in
replay.** Forward kinematics is Kapto's own and shared with the checks.

- **In the web UI's own page.** The 3D view is drawn with three.js and the
  plots with uPlot, both permissively licensed, pinned and built into the UI
  like the rest of it; nothing is loaded from elsewhere. The view is a
  Svelte component, not an iframe, so live, replay and the rest of the page
  share one page and one policy.
- **The browser never reads robot files.** The server sends the robot as a
  scene: each link, its visual shapes (boxes, cylinders, spheres, meshes),
  their colours and where they sit, from the robot model Kapto already
  loads. Meshes go as plain triangles (positions, normals, indices) in a
  small binary form, converted once by Kapto's mesh loader, so the page needs
  no loader for any mesh format. A replay's scene is the robot its recording
  carries ([recordings carry what they were recorded with](recordings-carry-what-they-were-recorded-with.md)).
- **The live view comes from the viewer bridge, over a WebSocket.** The
  bridge is a process of its own, reads the capture process's lossy tap and
  computes link poses, and sends each connected page, at most at the
  display's rate, one binary frame: every link's pose and each stream's
  minimum and maximum of each plotted value since the last frame. The frame
  format is a specification in `docs/reference/`. A page that can't keep up
  misses frames: the bridge keeps at most a few frames queued per page,
  counts what it drops, and never waits on a page.
- **As little delay as the recording allows.** The bridge forwards each
  sample as it arrives from the tap, waiting on no timer of its own, and the
  page draws the newest pose on its next frame: the only waits left are the
  tap's pacing and the browser's frame, about 10 to 20 ms. Each frame
  carries when Kapto received what it shows, so the page displays the
  delay. The tap's pacing stays in the capture process's tap thread, off
  the recording path, and stays lossy: nothing about the live view can
  slow recording, and lowering the delay further is a change of that pacing,
  measured like any other.
- **The last minutes can be looked at again, downsampled.** The bridge keeps
  what it sent for the last few minutes (a setting, ten by default): poses
  at display rate and each plotted value's minimum and maximum, a few tens
  of kilobytes a second for a robot arm. The live view has a timeline:
  pausing it, the operator can scrub back over those minutes to look at
  something odd, then return to now. The page asks the bridge for the
  stretch on screen at the resolution the screen can show, and keeps only
  that.
- **Memory is bounded on both ends.** The bridge keeps those minutes, cut
  to a size as well as a time; the page keeps the stretch on screen and the
  newest pose. Neither grows with how long the page stays open.
- **Events, recorded for curation and shown on the timeline.** An event is
  something that happened at a moment, or over a stretch: an episode
  started or ended, a stream lost messages, fell silent or came back, an
  action began or ended (a grasp, a subtask, an intervention), an operator's
  note. Every event has a kind, from Kapto's own (`kapto.…`) or a vocabulary
  the configuration and the task set (a task's subtasks among them); the
  moment, on the same clock as the messages, so it lines up with what the
  robot sent; whether it is a moment, the beginning or the end of a stretch,
  a beginning and its end paired by an ID; where it came from; and an
  optional text and attributes.
- **Kapto takes the events, not the page.** The capture process receives
  every event, whoever sends it: itself, the web UI's buttons or keys, the
  API, `kapto episode event`, or the robot through a stream declared for
  it, so a teleoperation tool or a pedal can mark an action without the
  page open. During an episode it writes each one into the recording, on a
  channel of Kapto's own handed to the writer between batches, so the
  recording carries its events and anything working on it later reads
  them from there; between episodes they are shown live only. Every event
  is announced, so the live view's timeline shows it as it happens, and the
  bridge keeps it with the last minutes. The episode's events are also
  indexed with the episode, so selection, labelling and exports can find
  episodes and stretches by them; the recording stays the source the index
  is rebuilt from.
- **The timeline shows them**, live and in replay, with the moments the
  checks found in a replay.
- **Things around the robot can be drawn too.** An operator can load an
  environment into the viewer, meshes or simple shapes such as a table or
  a wall, and place it around the robot so its motion reads against its
  surroundings. It is for display only: nothing in it changes what is
  recorded or how it is checked. It belongs to the viewer: it isn't part of
  the configuration or carried in recordings.
- **A replay is a view file the worker makes, read in windows.** The
  `replay` job writes, in the replay cache, the episode's link poses at
  display rate and, for each plotted value, its minimum and maximum at
  successively coarser steps, so any stretch of the episode at any zoom is
  served from a few kilobytes. The page asks the API for the window on
  screen and plays it on a timeline of its own: play, pause, speed,
  scrubbing, the moments the checks found marked on it, and a moment opened
  a few seconds before it. The view file is a cache, not a format: it carries
  a version, and one from another version is made again.
- **The view file is a cache**: made when first asked for, by the worker,
  outside the episode's folder, emptied by size, age, free space and with
  its episode, never uploaded or listed in the manifest.

## Consequences

- Leaving the page open costs the same after an hour as after a minute.
- The live view moves soon after a message arrives, says by how much, and
  can be paused and rewound over the last minutes.
- One viewer, in one page: no second port, no iframe, nothing to block.
- Kapto maintains a 3D view, plots and a replay player. Each is kept to
  what an operator needs; anything more is added deliberately, like any
  other feature.
- Recordings are MCAP, which other tools read, and `kapto decode` exports
  any stream's values as CSV; there is no file of a viewer's own. A Rerun
  file can be added as an exporter if someone needs it.
- What the viewer shows is specified in `docs/reference/`: the scene, the
  live frame and the replay view.
- The replay job's cost is measured, with its `perf-budget.toml` entry.
- A recording has a channel of Kapto's own for events, which the recording
  format's specification documents. The event's shape is an output format:
  fields are only ever added.
- Curation has something to run over: what keeps a stretch of an episode,
  what labels it, and what an export cuts can start from its events, and
  labellers that find events themselves add them in the same shape.
- The page reads the mesh formats an environment comes in, through
  three.js's loaders; the robot itself comes as triangles from the server.

## Alternatives considered

- **Rerun's web viewer, embedded in the page.** It draws meshes, plots and
  scrubs a timeline, and has a file format of its own. Measured on the
  demo's page with a headless Chrome, with nothing recorded: the bridge sent
  the browser about 3.5 MiB a second (each plotted value a message of its
  own, about 60 a second, each describing what it holds); the page grew
  past 3.5 GB in a minute and 5 GB in three, reached the viewer's own memory
  limit (2.3 GiB), began freeing data and stopped with a WebAssembly error.
  That limit can't be set from the page that embeds it. The live path
  waited on three timers (the tap's pacing, the bridge's own 16 ms timer and
  the SDK's 8 ms batching) before the browser's frame: 25 to 35 ms with a
  graphics card, and 24 seconds behind in the containers the browser checks
  run in. It also brought about 370 crates, a 42 MB WebAssembly viewer in
  the binary, a second page on a port of its own, messages across an
  iframe to drive a replay, and a viewer that tries to contact the internet
  unless a Content-Security-Policy stops it. It suits a debugging session
  in its desktop viewer, not a page left open all day.
- **Rerun, sent less:** batching cut the traffic eightfold and kept the tab
  near 430 MiB for four minutes, but it still grew by a few MiB a minute,
  without end, and the viewer can't be told to keep less.
- **Our own live view, Rerun for replays:** a replay is opened for minutes,
  where Rerun's growth matters less, and its files would be available. But
  the page would carry two viewers that look and behave differently, and
  all of Rerun's weight for half the job.
- **Foxglove or PlotJuggler, fed exported data:** no viewer to maintain, but
  nothing in Kapto's page, and the operator leaves Kapto to look at the
  robot.
- **Our own WebGL instead of three.js, or our own plotting:** smaller, but
  scene graphs, cameras, lighting and fast line drawing are large and
  well-solved problems: the rule is to use a library for those.
