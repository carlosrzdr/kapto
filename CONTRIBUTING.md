# Contributing to Kapto

The rules for working in this repository (design guidelines, invariants,
coding style, testing, PR format) are in **[AGENTS.md](AGENTS.md)**. They
apply to humans and AI coding agents and are not repeated here.

## Getting started

1. Read `AGENTS.md`. While the project is being built, the planning
   documents in `docs/project/` describe what comes next.
2. The only prerequisite for building is Docker with BuildKit:
   `docker compose build`. For working on the Rust code directly, install
   the toolchain pinned in `rust-toolchain.toml` *(once the code exists)*.
3. Install the two cargo plugins the checks use, once:
   `cargo install --locked cargo-nextest cargo-deny`.
4. Run `cargo xtask ci` before opening a pull request. It runs the same
   checks as the CI workflow.
5. Builds take several GB in `target/`. `cargo xtask clean` shows what it
   holds, and `cargo xtask clean --workspace` removes Kapto's own outputs
   while keeping the dependencies built, so the next build takes a minute or
   two. Development builds keep line numbers for backtraces but no full
   debug info; for a debugger session, build with
   `CARGO_PROFILE_DEV_DEBUG=true`.

## Commands

Every command Kapto's development uses, all through `cargo xtask` or
the `kapto` binary. `cargo xtask ci` checks that each command and path
quoted here exists.

| Task | Command |
|---|---|
| Every check required before a pull request (format, lints with and without the development features `profiling` and `failpoints`, release builds without them, dependency policy, generated pages, links, this file, tests) | `cargo xtask ci` |
| Only the static checks, or only the tests | `cargo xtask ci check` · `cargo xtask ci test` |
| Format / lint | `cargo fmt --all` · `cargo clippy --workspace --all-targets -- --deny warnings` |
| Tests | `cargo nextest run --workspace` · `cargo test --workspace --doc` |
| Lint and test the `ros2` feature (ROS 2 topics, and the synthetic robot publishing them), which needs ROS 2 to build: in the ROS 2 image, the tests over each middleware it has (Fast DDS, Cyclone DDS, Zenoh with its router) and reading message types from its installation, with a build folder of its own in `target/ros2/` | `cargo xtask ros2` |
| Test uploads to S3-compatible storage (the stores' conformance, a file or part with the wrong SHA-256 refused, the worker uploading an episode in parts, the network cut in the middle of an upload and restored, the guide to trying uploads) against the RustFS server of `compose.yaml`'s `storage` profile, pinned by digest, started as a Compose project of its own on a free port | `cargo xtask storage` |
| Dependency and licence checks | `cargo deny check` |
| Regenerate the reference pages in `docs/reference/` (commands, settings, error codes) | `cargo xtask gen` (`cargo xtask ci` checks they are current) |
| Check the API for changes that could break a client, against the OpenAPI document committed on `main` (oasdiff in Docker) | `cargo xtask api-diff` · `cargo xtask api-diff --base <revision>` |
| Show what the build folder `target/` holds; remove Kapto's own build outputs (dependencies stay built) or everything | `cargo xtask clean` · `cargo xtask clean --workspace` · `cargo xtask clean --all` |
| Build both container images (`kapto:dev`, and `kapto-ros2:dev`, whose Kapto is built with ROS 2) | `docker compose build` |
| List the ROS 2 topics the machine's ROS 2 network has, with their types (only the `kapto-ros2` image's Kapto is built with ROS 2) | `docker run --rm --network host kapto-ros2:dev topics` |
| Send synthetic robot streams (e.g. 16 streams at 2 kHz to ports 9000–9015), over UDP or TCP (`--transport tcp-connect` or `tcp-listen`, each message after its 4-byte length), or as ROS 2 joint states on `/joint_states` (`--transport ros2`, built with its `ros2` feature in the ROS 2 image; `--wait-for-subscribers 10` to start only once every topic has a subscriber), or a camera's JPEG frames instead (`--camera 640x480`, over TCP or ROS 2; `--raw` for its pixels in rgb8, uncompressed, a `sensor_msgs/msg/Image` over ROS 2) | `cargo run --release -p kapto-loadgen -- --streams 16 --rate 2000 --target 127.0.0.1:9000` · `cargo run --release -p kapto-loadgen -- --streams 4 --transport tcp-connect` · `cargo run --release -p kapto-loadgen -- --camera 640x480 --rate 30 --transport tcp-connect` |
| Record one episode from the streams in a `streams.toml` | `cargo run --release --bin kapto -- record --streams streams.toml --duration 10` |
| Try Kapto without a robot: the sample arm and a synthetic robot, UI on http://localhost:7700 | `cargo run --release --bin kapto -- demo` |
| The demo as a new user starts it, with its episodes uploaded to RustFS, in one command | `docker compose --profile demo up --build` |
| That command checked end to end: the arm moving in a browser, an episode recorded and found uploaded in RustFS, then everything taken down | `cargo xtask demo-compose` |
| Show Kapto to people: the demo, narrated step by step (Enter goes on), recording ordinary episodes, then one where the robot misbehaves, and what Kapto found | `cargo run --release --bin kapto -- showcase` · `cargo run --release --bin kapto -- showcase --unattended --episodes 6` |
| Run Kapto: the capture process, the viewer bridge and the worker, restarted if they exit, and the API on http://127.0.0.1:7700/api; Ctrl-C stops everything | `cargo run --release --bin kapto -- serve --robot packages/arm7/urdf/arm7.urdf --streams packages/arm7/streams.toml --mapping packages/arm7/mapping.toml` |
| Run Kapto with a ROS 2 robot: the `kapto-ros2` image on the host's network, here with the sample arm's ROS 2 package, whose joint states the synthetic robot publishes with `--transport ros2` | `docker run --rm --network host -v kapto-data:/data -v "$PWD/packages:/packages:ro" kapto-ros2:dev serve --robot /packages/arm7_ros2/urdf/arm7.urdf --streams /packages/arm7_ros2/streams.toml --mapping /packages/arm7_ros2/mapping.toml` |
| Run the capture process: receive the streams all the time, feeding the live view, and record episodes on command | `cargo run --release --bin kapto -- capture --streams packages/arm7/streams.toml` |
| Start, stop, abort or check episodes of a running capture process, or run its pre-flight checks again | `cargo run --bin kapto -- episode start` · `cargo run --bin kapto -- episode start --profile lab_ana --task pick_and_place` · `cargo run --bin kapto -- episode stop` · `cargo run --bin kapto -- episode abort` · `cargo run --bin kapto -- episode status` · `cargo run --bin kapto -- episode preflight` |
| Send an event of the vocabulary to a running capture process: a moment, the beginning or end of a stretch, or the last seconds as a whole one | `cargo run --bin kapto -- episode event --kind note` · `cargo run --bin kapto -- episode event --kind intervention --begin i1` · `cargo run --bin kapto -- episode event --kind intervention --end i1` · `cargo run --bin kapto -- episode event --kind problem --last 10` |
| Stream the robot moving live to the web UI's live view, from the live tap of a running capture process or recording | `cargo run --release --bin kapto -- viz-bridge --robot packages/arm7/urdf/arm7.urdf --streams packages/arm7/streams.toml --mapping packages/arm7/mapping.toml` |
| Do the jobs serve queues (serve starts it; on its own it waits for serve's job socket in the data directory) | `cargo run --bin kapto -- worker` |
| List the robot packages in `paths.packages`, or show one with its files and content hash | `cargo run --bin kapto -- package list` · `cargo run --bin kapto -- package show packages/arm7` |
| Check a robot package and report every problem with where it is and what to do (fails on errors, not warnings) | `cargo run --bin kapto -- package lint packages/arm7` · `cargo run --bin kapto -- package lint packages/arm7 --format json` |
| List the session profiles and tasks saved in the configuration folder (`profiles/`, `tasks/` beside `kapto.toml`), or show one | `cargo run --bin kapto -- profile list` · `cargo run --bin kapto -- profile show lab_ana` · `cargo run --bin kapto -- task list` · `cargo run --bin kapto -- task show pick_and_place` |
| Show a recording's messages as named values (table, or CSV of one stream), or decode a packet capture (pcap, pcapng) with a package's streams | `cargo run --bin kapto -- decode episode-folder` · `cargo run --bin kapto -- decode episode-folder --stream arm_joints --format csv` · `cargo run --bin kapto -- decode --pcap robot.pcap --package packages/arm7` |
| Test each upload destination in `[[upload.destination]]`, or one by name, as an upload would use it: a probe object written under its prefix, read back and removed | `cargo run --bin kapto -- upload check` · `cargo run --bin kapto -- upload check site_nas` |
| Write an episode's ROS 2 topics as a bag `ros2 bag play` replays on the robot's topics | `cargo run --bin kapto -- export rosbag2 episode-folder --output episode.mcap` |
| The full-load recording test (16 streams at 2 kHz for 30 s) | `cargo nextest run --cargo-profile release -p kapto-cli --run-ignored only sixteen` |
| Benchmark recording against the CPU, memory, buffer, time-to-disk and drop targets (16 streams at 2 kHz for 60 s; reports in `target/bench/`, the recording removed once measured unless `--keep-recordings`), optionally with some of the streams over TCP, or with cameras beside them (1920x1080 JPEG frames at 60 fps over TCP, by default; raw rgb8 frames with `--raw-cameras`) | `cargo xtask bench system` · `cargo xtask bench system --tcp 8` · `cargo xtask bench system --cameras 2` |
| Benchmark twice that load (32 streams at 2 kHz), which must still be recorded completely | `cargo xtask bench headroom` |
| Find how many 2 kHz streams this machine records completely | `cargo xtask bench capacity` |
| Benchmark recording the reference load while the live view's bridge streams to a page, is stopped, has its page stop reading and is killed, checking nothing is dropped and capture's CPU doesn't change, with the delay to each frame sent | `cargo xtask bench viewer` |
| Watch the live view in a browser drawing with the machine's graphics card (`/dev/dri`; Playwright in Docker) at the reference load: the delay from each message's arrival to it drawn, and the page's memory staying flat (reports in `target/bench/`) | `cargo xtask bench browser` · `cargo xtask bench browser --minutes 30` |
| Benchmark each recording stage on its own (UDP receive; hand-off, loss accounting, MCAP write, clock) across batch and message sizes; results in `target/criterion/` | `cargo bench -p kapto-transport --bench receive` · `cargo bench -p kapto-capture --bench stages` |
| Benchmark reading a recorded episode's events back, as the worker does to index them, across episode lengths and message sizes; results in `target/criterion/` | `cargo bench -p kapto-capture --bench events_job` |
| Benchmark checking a recorded episode (reading it by role, the checks, its statistics) across episode lengths and message sizes, and learning and scoring the anomaly autoencoder; results in `target/criterion/` | `cargo bench -p kapto-qa --bench qa_job` · `cargo bench -p kapto-qa --bench autoencoder` |
| Benchmark making a recorded episode's replay (the replay view the worker writes) across episode lengths and message sizes; results in `target/criterion/` | `cargo bench -p kapto-viz --bench replay_job` |
| Run the stage benchmarks and the episode events', check's and replay's, fit each one's cost per batch (an episode, for those three), per message and per KiB, and compare with `perf-budget.toml` (add `--quick` for a fast, noisier look) | `cargo xtask bench model` |
| Benchmark the reference load as ROS 2 `sensor_msgs/msg/JointState` topics (16 at 2 kHz for 60 s) in the ROS 2 image, built first, with the live view's bridge moving the arm and streaming to a page: the system benchmark's targets, recording and the bridge within two cores, and the delay to each frame sent (reports in `target/bench/`), over any middleware the image has, optionally with cameras publishing `sensor_msgs/msg/CompressedImage` (1920x1080 at 60 fps by default), or `sensor_msgs/msg/Image` in rgb8 with `--raw-cameras`, and with environment variables for both sides, such as a middleware's settings, which the report names | `cargo xtask bench ros2` · `cargo xtask bench ros2 --middleware rmw_zenoh_cpp` · `cargo xtask bench ros2 --cameras 2` · `cargo xtask bench ros2 --middleware rmw_zenoh_cpp --env ZENOH_CONFIG_OVERRIDE='transport/shared_memory/enabled=true'` |
| Benchmark uploading an episode (256 MiB by default) to RustFS in Docker for each part size and number of parts at once, three times each, as the worker uploads: the median speed, CPU and peak memory of each, and whether the default settings come within 10 % of the fastest (reports in `target/bench/`) | `cargo xtask bench upload` · `cargo xtask bench upload --mib 1024 --repeats 5` |
| Benchmark recording the reference load for 30 s with nothing else running, then 30 s while episodes go up to RustFS one after another as the worker uploads them (its default settings and low priority), checking recording doesn't notice: nothing dropped, buffers at most half full, capture's CPU as without | `cargo xtask bench upload --while-recording` |
| Run any benchmark with the recorder in the `kapto:dev` container (rebuilt first, host networking), or limited to the reference PC's 4 CPUs and 16 GiB | `cargo xtask bench system --in-docker` · `cargo xtask bench system --limit reference` |
| Capture's behaviour at its failpoints (closing an episode held at a step, or ended there as a crash would), built with the development-only `failpoints` feature and set by `KAPTO_FAILPOINTS`, as `cargo xtask ci` runs them | `cargo nextest run -p kapto-cli --features failpoints -E 'test(closing::)'` |
| Profile recording under a load, by default the reference one with two 1080p cameras at 60 fps beside it; without them, or with five to stress the cameras' path, their frames JPEG or raw (`--raw-cameras`, each pixel in rgb8, which the viewer bridge encodes itself): built with frame pointers (in `target/profiling/`), every thread sampled by `perf` from a container given `CAP_PERFMON` (the Dockerfile's `profiler` stage): each thread's time on a core, waiting for one and blocked, the functions its samples land in, and a flamegraph (`profile.md`, `flamegraph.svg`, and the stacks for other tools in `stacks.folded`, in `target/bench/`) | `cargo xtask profile record` · `cargo xtask profile record --scenario reference --seconds 60` · `cargo xtask profile record --scenario five-cameras` · `cargo xtask profile record --raw-cameras` |
| Trace a load, by default the reference one with two 1080p cameras at 60 fps beside it, with Kapto built with the `profiling` feature (in `target/tracing/`), then the worker verifying, finding events, checking and making the replay of the episode: each process writes its spans to the folder `KAPTO_PROFILE_TRACE` names, merged into one trace for Perfetto, with each stage's cost per batch, how long batches wait for the writer, each job's cost per minute recorded and checksums' per GB (`trace.md` and `trace.json` in `target/bench/`); with `--upload`, the episode also goes up to the RustFS server of `compose.yaml`, started for it, with each file and part in the trace and the upload's CPU-seconds per GB | `cargo xtask profile trace` · `cargo xtask profile trace --upload` · `cargo xtask profile trace --scenario five-cameras` |
| Profile the live view in Chromium drawing with the machine's graphics card (`/dev/dri`; Playwright in Docker) under a load: by default 50 streams and two 1080p cameras at 60 fps, the load its budget is stated for; or the reference one alone, with two cameras or with five, JPEG or raw (`--raw-cameras`); pictures drawn a second, each frame's delay to the screen, the browser's memory and the main thread's busy time, checked against `[page]` in `perf-budget.toml`, and the main thread's profile (`page.md` and `page.cpuprofile`, for DevTools, in `target/bench/`) | `cargo xtask profile page` · `cargo xtask profile page --scenario five-cameras --minutes 10` · `cargo xtask profile page --scenario cameras --raw-cameras` |
| Hold the newest run of `bench model`, `profile trace` and `profile page` in `target/bench/` against `perf-budget.toml` in one report: each stage, job (per minute recorded), checksums and uploads (per GB) and the page's measures within budget, over it, or unexpectedly cheap (under a tenth of it); fails if anything is over | `cargo xtask profile report` |
| Build, type-check or test the web UI in `ui/` (Node.js runs in Docker; `build` writes `ui/dist`, which the next `cargo build` embeds) | `cargo xtask ui build` · `cargo xtask ui check` · `cargo xtask ui test` · `cargo xtask ui install` (after changing `ui/package.json`) |
| Run the web UI's flows in a browser (Playwright in Docker) against `kapto demo` on free ports: record and list an episode, then the page with capture stopped; no request may leave the machine | `cargo xtask ui e2e` |
| Take a screenshot of each tab and state of the web UI (live, recording, an episode open, robot, system, settings, capture stopped) the same way, into `target/screenshots/`, for a pull request's description | `cargo xtask ui screenshots` |
| Regenerate the forward-kinematics reference poses of the robots in `testdata/robots/` with ROS 2's robot_state_publisher (Docker, needs network) | `cargo xtask golden fk` |
| Regenerate the ROS 2 messages in `testdata/cdr/`, serialised by ROS 2 itself, that check the CDR reader (Docker, needs network) | `cargo xtask golden cdr` |
| Regenerate the Protobuf descriptor set and messages in `testdata/protobuf/` with buf, from the `.proto` and JSON files there (Docker) | `cargo xtask golden protobuf` |

Not built yet:

| Task | Command |
|---|---|
| Profiling, development only (AGENTS.md section 6.7) *(planned)* | `cargo xtask profile record --scenario <name>` · `cargo xtask profile trace --scenario <name>` · `cargo xtask profile report` |
| Golden recordings *(planned)* | `cargo xtask golden generate` |
| UI development *(planned)* | `docker compose --profile ui-dev up` |

## Proposing a change

- **A fix or a planned feature:** open a PR on a branch named after the plan
  row (`<wp>/<nn>-<slug>`), using the PR template.
- **Something not in the plan:** open an issue first, naming the requirement
  it serves or proposing a new one.
- **A design expensive to change** (a format, a public trait, a dependency
  at the core, an invariant): write or rewrite its document in
  `docs/design/<topic>.md`, with context, approach, consequences and the
  alternatives considered, and open it as its own PR before the code that
  depends on it.

## Licence

Kapto is licensed under Apache-2.0. By contributing you agree that your
contribution is licensed under the same terms (inbound = outbound, as
section 5 of the licence provides). No
CLA is required.
