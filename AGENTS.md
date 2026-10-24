# AGENTS.md

How to work in the Kapto repository, for every contributor, human or AI
agent. The rules here stand on their own; the parts of the design that are
expensive to change are explained in `docs/design/`.

> **Project state:** early development. Recording (UDP, TCP, ROS 2), the
> live view and replays in the web UI, the serve command with its API and
> `kapto.db`, the worker (verification, events, quality checks, replays,
> uploads to S3, GCS, Azure or a folder) and the demo work; selection
> doesn't yet. Anything marked *(planned)* doesn't exist yet. A change that
> adds a crate, command, invariant or design document updates this file in the
> same PR; `cargo xtask ci` checks the commands and paths quoted here.

---

## 1. What Kapto is

An open-source edge collector for robot data: it receives (UDP, TCP, ROS 2,
watched folders), records at up to 2 kHz per stream without silent loss,
shows it live and in replay on the robot's URDF in its own web UI, decides
what is worth keeping, and uploads to the user's own storage. Rust engine,
a Python package over the same crates, a Svelte UI with a Rust → WebAssembly
module. Built and run with Docker, works offline, nothing leaves the machine
but uploads to configured destinations. Apache-2.0; contributions under the
same terms.

## 2. Where to find things

| Need | Read |
|---|---|
| How the system fits together now | [`docs/concepts/architecture.md`](docs/concepts/architecture.md) |
| Why Kapto is built the way it is | [`docs/design/`](docs/design/README.md) |
| Every development command | [`CONTRIBUTING.md`](CONTRIBUTING.md#commands) |
| How to use Kapto | [`docs/`](docs/README.md) |
| Commands, settings, error codes, formats | [`docs/reference/`](docs/reference/README.md), generated from the code |
| What is built next (planning only) | `docs/project/` |

---

## 3. Commands

All automation is `cargo xtask`, never shell scripts or Makefiles. The checks
need `cargo install --locked cargo-nextest cargo-deny`. The ones every change
uses are below; [CONTRIBUTING.md](CONTRIBUTING.md#commands) lists every
command (benchmarks, golden files, the demo, serve, the load generator).

| Task | Command |
|---|---|
| Every check required before a PR | `cargo xtask ci` · only static checks or tests: `cargo xtask ci check` · `cargo xtask ci test` |
| The `ros2` feature, linted and tested in the ROS 2 image over each middleware | `cargo xtask ros2` |
| Uploads against a RustFS server in Docker | `cargo xtask storage` |
| Regenerate `docs/reference/` and the UI's API types | `cargo xtask gen` |
| Check the API for breaking changes | `cargo xtask api-diff` |
| Build, type-check or test the web UI (Node.js in Docker) | `cargo xtask ui build` · `cargo xtask ui check` · `cargo xtask ui test` |
| The UI's flows, and screenshots for a PR | `cargo xtask ui e2e` · `cargo xtask ui screenshots` |
| Try Kapto without a robot | `cargo run --release --bin kapto -- demo` · with uploads, in Docker: `docker compose --profile demo up --build` |

---

## 4. Repository map

| Path | Purpose |
|---|---|
| `crates/kapto-core` | Shared types: IDs, timestamps, roles, cause-coded errors, clocks |
| `crates/kapto-config` | TOML config: schemas, precedence, source tracking |
| `crates/kapto-transport` | `Transport` trait; UDP, TCP, ROS 2 (feature `ros2`) |
| `crates/kapto-decode` | `Decoder` trait; struct layout, CDR, Protobuf, JSON, MsgPack; camera frames; FlatBuffers *(planned)* |
| `crates/kapto-capture` | Batches, rings, loss accounting, MCAP writer, finalisation, kept messages |
| `crates/kapto-robot` | Robot packages, URDF, meshes, mapping, linter, forward kinematics |
| `crates/kapto-qa` | `Check` trait, checks, statistics, labellers |
| `crates/kapto-select` | `Scorer` trait, relevance plans, retention, discard audit |
| `crates/kapto-viz` | The viewer bridge and replay views |
| `crates/kapto-store` | SQLite runtime state, `episode.json`, manifest, schemas, exporters |
| `crates/kapto-upload` | The upload engine, storing through `object_store` |
| `crates/kapto-server` | Supervisor, REST/WebSocket API, embedded UI |
| `crates/kapto-cli` | The `kapto` binary and its subcommands |
| `crates/kapto-py` | Python bindings (`kapto` wheel) |
| `crates/kapto-wasm` | Rust → WebAssembly logic shared with the UI |
| `crates/kapto-testkit` | What the tests share; only ever a dev-dependency |
| `tools/kapto-loadgen` | Synthetic robot: UDP, TCP or ROS 2 streams at a fixed rate |
| `tools/xtask` | Developer automation |
| `ui/` | Svelte + TypeScript web UI |
| `packages/` | Sample robot packages (`arm7`, `arm7_ros2`) |
| `testdata/` | Fixtures; third-party ones with a `PROVENANCE.md` |
| `docs/project/` | Planning documents; nothing else refers to them |
| `docker/` | The Dockerfile for both images, built by `compose.yaml` |
| `perf-budget.toml` | What each recording stage may cost, with the reason |
| `fuzz/` · `spikes/` | Fuzz targets · throwaway prototypes *(planned)* |

---

## 5. Invariants: never break these

Each is enforced by a test once its code exists. Breaking one needs its
design document changed first.

1. **The capture path never waits on anything else.** UI, viewer, QA,
   selection, upload and exporters may crash or stall; recording continues.
2. **No allocation, lock, syscall or log per message on the hot path**
   (`kapto-capture`, `kapto-transport`, the live parts of `kapto-decode`).
3. **Taps to the viewer and streaming checks are lossy**: they drop and
   count, never apply backpressure.
4. **Loss is never silent**: every dropped or missing message is counted and
   visible live and in `episode.json`.
5. **Payloads are stored exactly as received.**
6. **Raw data is immutable**, except through a final retention plan.
7. **A partial recording never looks finished**: stage, rename, manifest,
   `_FINALISED` last.
8. **Output schemas are additive-only** (`episode.json`, manifest,
   `_SUCCESS`, plans).
9. **Nothing advisory can reject data**: only deterministic gates block.
10. **No outbound connections** but to the robot and to configured upload
    destinations. No telemetry.
11. **Every failure has a cause code, a message and a remedy.**
12. **Decoding is deterministic.**
13. **User configuration lives in files**; SQLite holds runtime state only.
    Secrets are referenced, never stored in config, recordings or logs.
14. **A recording carries everything it was recorded with** inside the MCAP
    (package, profile, task, capture settings, summary); only what is
    computed after finalisation lives beside it.

---

## 6. Design guidelines

### 6.1 Interfaces first

Anything with more than one variant now or plausibly later is a **trait plus
a generic engine** (credential sources, transports, decoders, importers,
exporters, checks, scorers, labellers, triggers). Where a maintained
library's interface already is that trait, use it: upload storage is
`object_store` ([uploads](docs/design/uploads.md)). The engine owns everything common (queueing,
retries, state, accounting, verification, config, errors, API);
implementations know only their protocol, declare capabilities, and pass the
trait's conformance suite. Design each trait against two implementations.
Implementation config is tagged by `kind`. Third-party types stay out of our
traits unless the engine's design document says otherwise.

### 6.2 Behaviour is configuration

Retries, timeouts, sizes, rates, thresholds and limits come from typed
config with documented defaults (one `Default` per policy type, the source
of the generated reference). Precedence: deployment keys (ports, paths,
auth, destinations): default < `kapto.toml` < env < flag. Recording keys
(decoders, mapping, QA, relevance): default < package < profile < task <
per-range override, never env or flags.

### 6.3 Dependencies

Use a crate for large or evolving formats and protocols, security, and
specialist algorithms. Write it in the repo when it is under ~300 lines,
has a stable spec, and is on the hot path or central to Kapto (struct and
CDR decoding, framing, FK, decimation, statistics, backoff, bandwidth
limiting, clocks). Justify each new dependency in the PR; licences must be
permissive (`cargo deny`).

### 6.4 Processes and threads

Four processes from one binary: `kapto-server` (supervisor, API, UI, the
only SQLite writer), `capture`, `viz-bridge`, `worker` (all slow work at
low priority); Python plugins run as time-limited subprocesses. Control over Unix stream sockets; the live tap over
non-blocking datagrams that drop. Threads for high-rate work, `tokio` only
for idle connections; `kapto-capture`, `kapto-decode`, `kapto-select`,
`kapto-qa` and `kapto-robot` never depend on tokio. Every thread is named
(`cap-writer`, `wrk-upload`). Per-message work in `capture` costs
nanoseconds; anything slower goes to the worker.

### 6.5 Time

Two timestamps per message, never mixed or overwritten: receive time
(`CLOCK_MONOTONIC`, mapped to UTC per episode) and source time as received.
Never read wall-clock time per message.

### 6.6 Formats

Config is TOML (ROS YAML only as an import source); recordings MCAP;
sidecars JSON with a schema per version; formats specified in
`docs/reference/`.

### 6.7 Profiling

Development only, from `cargo xtask` against load-generator scenarios,
behind the `profiling` feature. Hot-path stages are batch functions with
their own benchmark; compare against `perf-budget.toml` and update it in
the same PR when a cost legitimately changes.

### 6.8 Web UI

Svelte + TypeScript own the screens; `kapto-wasm` only what must match the
server. API types are generated (`cargo xtask gen`). The UI is embedded and
checks `/api/version`. Node.js only in Docker. No inline scripts or styles.
Colours, type, spacing and radii come from the tokens in `ui/src/app.css`;
green means actions, selection and good state. Fonts are bundled.

---

## 7. Coding guidelines

- `rustfmt` defaults; `clippy -D warnings` with the workspace lints.
- No `unwrap`, `expect`, `panic!`, `todo!`, `dbg!` outside tests.
- `thiserror` in libraries, `KaptoError` with a cause code at the boundary,
  `anyhow` only in tools. Never a generic message.
- `unsafe` only where a module allows it, each block with `// SAFETY:`,
  Miri where possible, reviewed by a second person.
- `tracing` with structured fields *(planned: every process prints for
  now)*; no per-message logs.
- Public items documented; Linux-only code behind `cfg` with a fallback.
- Match the surrounding code. Comments give the reason in place; never
  requirement IDs, plan references or milestone numbers outside
  `docs/project/` (CI rejects them). Error codes are strings like
  `capture.disk_full`.
- Python bindings release the GIL and avoid copies; TypeScript is strict,
  with Vitest for components and Playwright for flows.

---

## 8. Testing expectations

| Change | Must include |
|---|---|
| Any behaviour | A test, including the failure path with its cause code |
| Parser or decoder | Property round-trip, fuzz target, error snapshots |
| Hot-path code | Property tests, the zero-allocation test, a before/after `bench model` report |
| New worker job kind | A `profiling` span and a `perf-budget.toml` entry |
| Check or scorer | Positive and negative synthetic cases |
| Output field | Regenerated schema, schema-lock test, `episode.json` snapshot |
| Config key | Schema, precedence test, regenerated reference |
| API endpoint | Regenerated OpenAPI, handler and error-code tests |
| UI flow | Playwright happy path and one failure path |
| Bug fix | A test that fails without the fix |

Test names describe the behaviour in words. Reference values come from
independent tools, never only from Kapto. Golden recordings are generated,
never edited. Timing assertions only on the benchmark machine; no automatic
retries for capture, durability or isolation tests. Benchmark numbers are
provisional until the reference PC exists; each report names the machine.

---

## 9. Documentation

Markdown in `docs/`, written in the same PR as the code.
`docs/concepts/architecture.md` always shows the current system. Reference
pages are generated (`cargo xtask gen`), never edited. Code blocks run and
links are checked in CI. A design expensive to change gets its design
document before the code.

---

## 10. Pull requests and commits

- One concern per PR, about 400 changed lines of hand-written code and
  tests at most (generated files and lockfiles excluded); split rather than
  grow.
- Branch `<wp>/<nn>-<slug>` from the plan; title and commit
  `type(scope): description` (`feat`, `fix`, `refactor`, `chore`, `docs`,
  `test`, `perf`, `ci`, `build`; scope a crate without `kapto-`, or an
  area), in words, without plan numbers or AI and co-author trailers.
- Squash-merge; `main` always green. Descriptions follow
  `.github/pull_request_template.md`, with **Read in this order** and **How
  to try it**.
- A PR changing what the UI shows has screenshots of every state it adds or
  changes, against `kapto demo`, before and after.
- Done means: CI green, tests for new behaviour and failure paths, docs and
  references updated, cause codes for new errors, perf report for hot-path
  changes, this file updated if a rule changed.
- Risky changes: an output field is added optional, then regenerated and
  snapshotted; a cause code goes in `crates/kapto-core/src/error/codes.rs`,
  never renamed or removed once released; a new transport, decoder,
  destination, check or scorer passes its trait's conformance suite; the hot
  path is benchmarked before and after on the same machine.

---

## 11. For AI coding agents

- Stay inside the task; if it needs more, stop and say so.
- Don't change an invariant, a format spec, a public trait or a documented
  design on your own: propose it and wait.
- Don't weaken, skip or delete a test to pass CI; fix the code or report
  why the test is wrong.
- Don't edit generated files; regenerate them.
- Don't add dependencies section 6.3 says to write, and justify any you add.
- Run `cargo xtask ci` (or the narrowest subset covering the change) and
  report the results faithfully, failures included.
- Don't commit, push or merge unless asked in the current request.
- When something isn't covered here or in the plan, ask.

---

## 12. Design

Each has a document in `docs/design/`: Rust for the engine with a
Python package on top; MCAP recordings stored as received, carrying
everything they were recorded with; a viewer of Kapto's own (three.js,
uPlot) with bounded memory and its own forward kinematics; Apache-2.0; TOML
configuration; four processes, threads for high-rate work; a Svelte UI
embedded in the binary; ROS 2 through rcl (`r2r`), types loaded by name,
only in the ROS 2 image; kept messages (`transient_local`) starting every
episode; each stream's pre-roll held in a ring set aside at startup and
written first, its post-roll after Stop, the episode spanning its data; messages over a datagram's size (cameras) through rings of their
own, to the live view over a stream socket of their own, kept thinned
to look back over, and in replay
read from the recording by its indexes, frame by frame; layouts as
columns of panels in TOML, the operator's own over the config folder's
default per package over the package's own over one made from its roles; uploads through one engine in the worker on `object_store`, state
in `kapto.db`, `_SUCCESS` last, credentials by reference, RustFS for tests;
an episode closed on a thread of its own while capture receives again
*(planned)*; episodes deleted by hand put in a bin, emptied by the
retention executor *(planned)*;
Docker as the only build prerequisite, automation in `cargo xtask`.
