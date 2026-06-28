# Kapto: implementation plan

Companion to [`requirements.md`](requirements.md) **v0.8** (cited below as
"REQUIREMENTS §n"). The requirements say *what* and in
what order (§9 roadmap). This plan says **how**: the practices every change
follows, the testing strategy, and each work package (WP) broken into small
pull requests (PRs) that can be read one at a time.

**How to use it**
- Design and contribution rules for every PR are in
  [`AGENTS.md`](../../AGENTS.md); this plan
  explains them and applies them per work package.
- One work package = one milestone from REQUIREMENTS §9 (M0…M18, R0).
- Each WP lists its PRs in merge order. Each PR is one branch, one concern,
  reviewable in one sitting (target **≤ 400 changed lines**, excluding
  generated files, fixtures and lockfiles).
- Every PR row says what it delivers (with requirement IDs), how it is tested,
  and **what to check when reviewing it**.
- PR numbers are stable identifiers (`M1-05`). If a PR turns out too big,
  split it as `M1-05a`, `M1-05b`, never by merging two rows.

---

## Part 1. Working agreements

### 1.1 Branches and pull requests

| Topic | Rule |
|---|---|
| Branch name | `<wp>/<nn>-<slug>`, e.g. `m1/05-mcap-writer`, `m16/03-scorer-trait` |
| Base | Branch from `main`. When PRs in a WP depend on each other, **stack** them: open `m1/06` with base `m1/05`. When `m1/05` is squash-merged and its branch deleted, GitHub retargets `m1/06` to `main`; rebase it onto `main` to drop the already-merged commits |
| Size | ≤ 400 changed lines of hand-written code + tests. Bigger is allowed only for mechanical changes (renames, generated files), stated in the description |
| One concern | One requirement cluster or one refactor per PR. A refactor needed by a feature lands as its own PR first |
| Draft PRs | Open as draft early when a design choice is involved, so the choice is reviewed before the code is polished |
| Merge | **Squash-merge**; the PR title becomes the commit on `main` |
| Commit format | `type(scope): description`. Types: `feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `perf`, `ci`, `build`. Scope = crate without the prefix (`capture`, `decode`, `robot`, `select`) or area (`docker`, `ci`, `docs`). The description says what changed in words: no requirement IDs, plan sections or PR/WP numbers. No AI or co-author trailers |
| `main` | Always green and always buildable. Protected on GitHub: changes only through pull requests, no force pushes, no deletion. While there is a single contributor, CI runs locally (`cargo xtask ci`) and no status checks are required; when CI runs on GitHub, its jobs (`check`, `test`, `build`, `e2e`) become required checks. Squash merge only; head branches deleted after merge |
| Versions | One workspace version. `0.<milestone>.<patch>` until the v1 release, then semver. Changelog generated from commit titles (`git-cliff`) |

### 1.2 PR description template

Stored as `.github/pull_request_template.md` (M0-07):

```markdown
## What
One paragraph describing the change.

## Planning reference
Optional, while planning documents exist: the requirements and plan row this covers.

## Why this way
The design choice and the alternative not taken. Link its design document if one exists.

## Read in this order
1. crates/kapto-capture/src/finalise.rs  – the state machine
2. crates/kapto-capture/tests/finalise.rs – how each step is interrupted
3. docs/concepts/episodes.md             – the user-facing explanation

## How to try it
cargo xtask demo record --seconds 10
ls data/  # expect <episode>/ with _FINALISED last

## Tests added
Unit / property / integration / golden / benchmark / doc. What each proves.

## Checklist
- [ ] Tests for every new behaviour, including the failure paths
- [ ] Docs page or reference updated in this PR
- [ ] Generated references regenerated (`cargo xtask gen --check` passes)
- [ ] New error paths have a descriptive cause code, message and remedy
- [ ] No requirement IDs or plan references in code, tests, comments, config, docs or commit messages
- [ ] Hot-path change: benchmark before/after attached
- [ ] AGENTS.md updated if a command, invariant or rule changed
- [ ] New dependency justified below (maintenance, licence, size)
```

The **"Read in this order"** and **"How to try it"** sections are mandatory.
The template itself is a repository file, so it contains no requirement IDs
(§2.9); the IDs go in the *Planning reference* section of each PR.
They let a reviewer follow the change without reconstructing it from the diff.

### 1.3 Definition of done (every PR)

1. CI green on all required checks (§3.6).
2. Every new behaviour has a test, and every new **failure path** has a test
   that triggers it and checks the cause code.
3. User-visible behaviour is documented in `docs/` in the same PR; reference
   pages are regenerated, never hand-edited.
4. Hot-path changes (`kapto-capture`, `kapto-transport`, `kapto-decode`)
   attach the perf-smoke result and stay within the budget.
5. Requirement IDs and plan references appear in the PR description only,
   never in code, tests, comments, config, error messages, user docs or
   commit messages (§2.9).

### 1.4 Definition of done (every work package)

There is no separate document per work package. Instead:

- The WP's **exit criterion** (from REQUIREMENTS §9; noted under or in the last
  row of each WP below) is shown passing in the description of its last PR: the test
  names, benchmark output or demo steps.
- That last PR also updates **`docs/concepts/architecture.md`**: a diagram
  of the components that now exist and how data flows between them, and the
  commands to see them running. This page is user documentation, written
  without planning references (§2.9), and is always current, so reading it
  is the quickest way to see how the system fits together at any point.

### 1.5 Design documents

- A design that is expensive to change gets a **design document**
  (`docs/design/<topic>.md`: context, approach, consequences, alternatives
  considered) in its own PR **before** the code that depends on it (DOC-9).
- Exploratory code lives in `spikes/<name>/`, is never imported by product
  crates, and is deleted or frozen once its design document is written.
- Settled approaches are listed in Part 7; choices still open are marked
  **Open question** where they come up, or listed in Part 7.

---

## Part 2. Engineering practices

### 2.1 Repository layout

```
kapto/
├── Cargo.toml                 # workspace, shared lints, shared dependency versions
├── rust-toolchain.toml        # pinned stable toolchain + components
├── deny.toml                  # cargo-deny: licences, advisories, bans, sources
├── crates/
│   ├── kapto-core/            # shared types, errors with cause codes, clocks
│   ├── kapto-config/          # TOML config, schemas, precedence, snapshots (§4.15)
│   ├── kapto-transport/       # UDP, TCP, ROS 2 (feature `ros2`), plugin trait
│   ├── kapto-decode/          # declarative decoders
│   ├── kapto-capture/         # ring buffers, accounting, MCAP writer, finalisation
│   ├── kapto-robot/           # packages, URDF, meshes, mapping, linter, FK
│   ├── kapto-qa/              # checks, statistics
│   ├── kapto-select/          # relevance scorers, plans, retention
│   ├── kapto-viz/             # viewer bridge and replay views
│   ├── kapto-store/           # SQLite runtime state, episode.json, manifest
│   ├── kapto-upload/          # Sink trait + object_store implementation
│   ├── kapto-server/          # daemon, supervisor, REST/WS API, embedded UI
│   ├── kapto-cli/             # `kapto` binary
│   ├── kapto-wasm/            # Rust → WebAssembly module used by the web UI
│   └── kapto-py/              # PyO3 bindings → `kapto` wheel
├── tools/
│   ├── kapto-loadgen/         # synthetic robot: UDP/TCP/ROS 2 streams, camera
│   └── xtask/                 # dev automation: ci, gen, bench, golden, demo
├── ui/                        # web UI source (built in a Docker stage)
├── packages/                  # sample robot packages (DCK-2)
├── testdata/                  # small fixtures in git; golden recordings via LFS
├── fuzz/                      # cargo-fuzz targets
├── docker/                    # Dockerfile, compose files
├── docs/                      # Markdown docs (DOC-1..3), adr/, concepts/architecture.md
│   └── project/               # requirements.md, implementation-plan.md
├── spikes/                    # throwaway prototypes (R0)
├── README.md  AGENTS.md  CLAUDE.md  CONTRIBUTING.md  LICENSE  NOTICE
```

This matches the crate table in REQUIREMENTS §3 (v0.8). `kapto-config` and
`kapto-upload` are separate from `kapto-store` because `object_store` pulls
in tokio and HTTP, which `kapto-store` must not.

**`cargo xtask` instead of Makefiles and shell scripts.** All developer
automation (`ci`, `gen`, `bench`, `profile`, `golden`, `demo`) is Rust in
`tools/xtask`, so it runs the same on every platform and nothing in the repo
depends on shell glue (NFR-6). CI calls the same xtask commands you run
locally.

### 2.2 Rust rules

| Area | Rule | Enforced by |
|---|---|---|
| Toolchain | Pinned stable in `rust-toolchain.toml`; bumped by PR | CI |
| Format | `rustfmt` default config | CI `fmt --check` |
| Lints | Workspace `[lints]`: `clippy::all` deny; selected `pedantic` warn; `unwrap_used`, `expect_used`, `panic`, `todo`, `dbg_macro` deny outside tests (`clippy.toml` allows them in tests); `missing_docs` warn on library crates | CI `clippy -D warnings` |
| `unsafe` | `unsafe_code = "deny"` workspace-wide. Allowed only in named modules (`transport::udp::sys`: `recvmmsg`, and `poll` outside Linux; `cli::worker::priority`: `ioprio_set`; the counting-allocator tests) with `#[allow]` on the module, a `// SAFETY:` comment on every block, Miri tests where Miri can run them, and a second reviewer | clippy `undocumented_unsafe_blocks`, review |
| Errors | Libraries: `thiserror` enums. Every user-facing error converts to `KaptoError { code, message, remedy }` (NFR-8). `anyhow` only in `xtask` and tools | Cause-code sweep test (M0-03) |
| Panics | No panics on input data. Parsers return errors; fuzzing finds the rest | Fuzz targets (§3.3) |
| Logging | `tracing` with structured fields; spans per session, episode, stream. **Never log per message** on the hot path; rate-limited warnings only | Review, allocation test |
| Async | `tokio` only in `kapto-server`, `kapto-upload` and the ROS 2 adapter. Capture, decode and select are synchronous code on dedicated threads | Crate dependency check in `deny.toml` (`bans` with `wrappers`) |
| Hot path | No allocation, lock, syscall or log per message in steady state. Pre-allocated batch buffers, recycled | **Counting-allocator test** (M1-02) asserts zero allocations per batch after warm-up |
| Platform | Linux-only code behind `cfg(target_os = "linux")` with a working (slower) fallback so the demo runs on Docker Desktop (DCK-11). **Approach:** Kapto must run on Linux, macOS and Windows. Natively is the goal; until then, Docker (Docker Desktop on macOS and Windows) is how it runs on those two, and must keep working there: a change that breaks the container on Docker Desktop is a bug. Native macOS and Windows are deferred to Phase C (Part 6) | CI builds macOS target for `kapto-cli` (check only) |
| Features | `ros2` (ROS 2 transport), `python` (bindings). Default features build the slim image | CI builds both feature sets |
| Dependencies | Build-vs-buy rule in §2.7. New dependency = stated reason in the PR (maintenance, licence, transitive size). `cargo-deny` checks licences (permissive only, LIC-1), advisories, duplicate versions and sources | CI `cargo deny check`, dependency budget report (§2.7) |
| Public API | Every public item documented; examples as doctests for anything a user calls | `missing_docs`, doctests |
| Schemas | Config and output formats are Rust types with `serde` + `schemars`; JSON Schemas are generated, committed and diff-reviewed | `cargo xtask gen --check` |

### 2.3 Architectural rules (the invariants)

These go into AGENTS.md (M0-07) and each one has a test that fails if it is
broken:

| Invariant | Test |
|---|---|
| Capture never waits on anything else | Isolation suite: SIGSTOP/SIGKILL the viewer bridge, server, QA worker under load; capture counters unchanged (M4-09, M5-09) |
| No per-message allocation on the hot path | Counting global allocator in `kapto-capture/tests/alloc.rs` (M1-02) |
| Lossy taps never apply backpressure | Stalled-consumer test (M4-01) |
| No outbound connections except declared ones | Egress test in a network namespace (M7-09, SEC-3) |
| Raw data immutable except via a final retention plan | Checksums in the manifest re-verified after every pipeline stage in integration tests (M1-06, M16-07) |
| Output schemas additive-only | Schema-lock test against all previous versions (M9-08) |
| Nothing advisory rejects; QA only raises relevance | Property test over random QA results (M16-04) |
| Deterministic decoding | Replay same pcap twice → byte-identical output (M2-10) |
| Every failure has a cause code | Sweep test: no `KaptoError` without code/remedy; no generic messages (M0-03) |

### 2.4 Process and thread model

The model is specified in REQUIREMENTS §3.1 (processes, threads, async vs
threads, communication, lifecycle) and recorded as the processes and threads design in M0-06. This
section covers how it is implemented.

**Async or threads.** Dedicated threads for high-rate, latency-sensitive work
(capture, the tap, CPU jobs); tokio only for many mostly-idle connections
(API, WebSockets, uploads). At 32k messages/s an async runtime adds
scheduling jitter, work-stealing between threads and allocation inside
futures, none of which capture can afford. Crate rule: `kapto-capture`,
`kapto-decode`, `kapto-select`, `kapto-qa`, `kapto-robot` never depend on
tokio (enforced in `deny.toml`).

**Implementation notes per process**

| Process | Implementation |
|---|---|
| `kapto-server` | `tokio` multi-thread runtime with 2 worker threads (configurable). **SQLite writer actor**: one dedicated thread owns the `rusqlite` connection and receives typed commands over a channel; every other part of the server sends it messages. This is how NFR-4's single writer is enforced in code: no other code path can open the database for writing. Readers use separate read-only connections (WAL) |
| `capture` | Threads spawned at session start from `streams.toml`: `cap-rx-udp-<n>` per socket group, `cap-rx-tcp-<n>` per connection, `cap-ros2`, `cap-cam-<n>`, `cap-audio`, plus `cap-writer` and `cap-stats`. Each source owns an `rtrb` SPSC queue to the writer; the writer polls all queues in a fixed order with a short park when all are empty. Stream-field triggers are evaluated in the source thread right after live decoding (one comparison per message on an already-decoded field) and emitted as events on a small bounded queue |
| `viz-bridge` | `viz-tap` thread reads datagrams; FK and decimation run on it; frames go to each page over a WebSocket |
| `worker` | `wrk-sched` thread pulls jobs from the server over the control socket; `wrk-cpu-<n>` pool (std threads, default 1) runs CPU-bound jobs; `wrk-stream` reads the tap for streaming checks and keep-window triggers; `wrk-upload` hosts a current-thread tokio runtime for the upload engine. Python plugins are subprocesses spawned per run with a wall-clock limit |

**Before the server exists** (M1–M6): `kapto record` hosts the `capture` process's
threads directly, and the SQLite writer actor (M5-04) and job scheduler
(M5-08) run inside that same process. M7-01 moves them to `kapto-server`
and the `worker` process without changing their interfaces.

**Control protocol.** One `postcard`-encoded enum per direction, versioned
in `kapto-core`, length-prefixed on a Unix stream socket. Each child sends
a `Hello { version }` first; a version mismatch is a cause-coded startup
error, not a silent incompatibility.

**Thread names and priorities.** Every thread is named (the names above), so
`top -H`, `perf` and the profiling tools (§2.8) attribute time correctly.
Priorities are set at spawn: worker `nice 10` with idle-class I/O priority,
viz-bridge `nice 5`, capture normal or real-time when configured (CAP-12).

### 2.5 Data path design

- **Receive time**: `CLOCK_MONOTONIC` in ns, read once per `recvmmsg` batch
  (all messages in a batch share the batch time; precision = batch latency,
  reported by the benchmark, PERF-3). A `ClockMap` of
  (`CLOCK_MONOTONIC`, `CLOCK_REALTIME`) pairs sampled every second maps to UTC.
- **MCAP fields**: `log_time` = receive time mapped to UTC ns (consistent
  within an episode because it comes from the monotonic clock);
  `publish_time` = source time when the decoder knows it, else `log_time`.
  The raw monotonic time and the `ClockMap` are stored in an MCAP metadata
  record and `episode.json`. The source timestamp field also stays in the raw
  payload (§8.1: never overwrite either).
- **Payloads are stored as received** (STO-1, NFR-5). Self-describing
  encodings get the matching MCAP schema encoding (`ros2msg` for CDR,
  `protobuf`, `flatbuffer`, `jsonschema`), so Foxglove and rosbag2 read them
  directly. Binary struct layouts (STO-1), which have no
  standard MCAP encoding, are stored raw with a `kapto.struct/v1` schema (the
  layout from `streams.toml`) and decoded by Kapto readers, exporters and the
  Python package. The `capture` process decodes only the few fields needed live
  (sequence number, source timestamp, mapped joints for the tap). This keeps
  capture cheap and raw data exact; the cost is that third-party tools see
  struct streams as bytes until they are exported.

### 2.6 Extension points: interface first, implementations second

Anything that has, or will plausibly have, more than one variant is built as
**a trait plus a generic engine**, with the specific cases as small
implementations. The engine holds everything common (queueing, retries,
state, accounting, verification, config, errors, UI/API exposure); an
implementation only knows its own protocol. Adding FTP upload or a new
transport later is then one new module and its tests, not a rewrite.

| Extension point | Trait (crate) | Generic engine owns | Implementations now | Plausible later |
|---|---|---|---|---|
| Upload storage | `object_store`'s `ObjectStore` + `MultipartStore`, no trait of ours ([uploads design](../design/uploads.md)) | Persistent queue, scheduling, pause-while-recording, bandwidth cap, in-flight requests, file-level retry, idempotent keys, verification, `_SUCCESS`, retention gating, status/ETA | S3 and S3-compatible, GCS, Azure, local FS, in-memory (tests), all `object_store`'s | SFTP, Google Drive, WebDAV through `object_store_opendal` |
| Credential sources | `CredentialProvider` (`kapto-upload`) | Resolution order, redaction, "test connection" | env, file, instance role (delegated to the backend's SDK) | OS keyring, Vault |
| Transports | `Transport` (`kapto-transport`) | Batch buffers, receive timestamps, accounting, reconnect-as-gap, allow-lists | UDP, TCP, ROS 2 | Serial, CAN, shm, WebSocket, gRPC, OPC UA (TRN-9) |
| Decoders | `Decoder` (`kapto-decode`) | Field model (names, units, scale), raw passthrough, error counting | Struct layout, CDR, Protobuf, FlatBuffers, JSON, MsgPack | Custom vendor formats |
| Importers | `Importer` (`kapto-capture`) | Folder watching, completeness detection, episode creation | MCAP, ROS 2 bag | ROS 1 bag, PX4 ULog, `.rrd`, LeRobot (TRN-12) |
| Exporters | `Exporter` (`kapto-store`) | Episode selection, resampling, plan application, progress | (P2) LeRobot, HDF5, Parquet, `.rrd` | Vendor formats |
| Checks | `Check` (`kapto-qa`) | Gate/advisory, params, results schema, worker isolation | Built-ins + Python plugins | — |
| Relevance scorers | `Scorer` (`kapto-select`) | Score model, plan building, tiers, audit | Built-ins + Python plugins | Learned scorers (REL-12) |
| Triggers | `TriggerSource` (`kapto-capture`) | Debounce, keep windows, episode cutting | ROS 2, UDP/TCP command, stream bit, HID | MQTT, GPIO |

Rules for every extension point:

1. **The trait is designed with two implementations in mind**, even if only
   one ships. For upload that means checking the trait against both S3
   (multipart, server-side checksums) and something unlike it (SFTP: append
   and offset resume, no checksum API; Google Drive: resumable sessions,
   file IDs instead of paths).
2. **Capabilities are declared, not assumed.** `Destination::capabilities()`
   returns e.g. `{ resumable, server_checksum, conditional_put, atomic_rename }`.
   The engine adapts: no server checksum → read-back verify; no conditional
   put → existence check before write. A new backend never needs engine
   changes to be correct, only to be faster.
3. **One conformance test suite per trait**, generic over the
   implementation (`fn destination_conformance<D: Destination>(make: impl Fn() -> D)`),
   run against every implementation: the in-memory one, RustFS (S3), local FS. A
   new backend passes the suite before it merges.
4. **Config is tagged by kind** (`kind = "s3"`, `kind = "sftp"`) with
   per-kind schemas, so the config format, the UI form and the docs grow by
   adding a kind, not by changing existing keys.
5. Traits live in our crates and use our types. A backend library's types
   (e.g. `object_store::Path`) never appear in the trait, so swapping or
   adding a library touches one implementation.

### 2.7 Dependencies: don't reinvent, don't over-import

Use a crate when the problem is **hard, security-sensitive or a moving
standard**; write it in the repo when it is **small, stable and central to
how Kapto works**, so it is readable, testable and tuned for our hot path.

**Use a crate** when any of these hold:
- the format or protocol is large or still evolving (MCAP, Protobuf, URDF,
  glTF/COLLADA, cloud storage APIs, HTTP, SQLite, ROS 2);
- it is security-sensitive (cryptographic hashes, TLS, auth, archive
  formats);
- correct implementation needs deep expertise we don't want to own
  (collision geometry, mesh simplification, linear algebra, compression).

**Write it in the repo** when all of these hold:
- roughly **under 300 lines** including tests' helpers, with a stable spec;
- we would use a small part of a bigger crate, or the crate pulls a large
  dependency tree for it;
- it sits on the hot path or central to Kapto's behaviour, where we need to
  understand and control every line.

**Which is which** (a PR that deviates says why):

| Write in the repo | Use a crate |
|---|---|
| Struct-layout decoder; CDR reader; `.msg` definition parser | `mcap` (MCAP read/write), `prost-reflect` (Protobuf), `flatbuffers` reflection, `serde_json`, `rmpv` (MsgPack) |
| TCP framing (length-prefix, delimiter, fixed size) | `socket2`, `nix` (socket options, `recvmmsg`) |
| Clock map, receive timestamping, episode IDs (time + random) | `three.js` and `uPlot` in the web UI (3D view and plots) |
| Batch buffer pool; per-stream accounting | `rtrb` (SPSC ring: tiny, no dependencies, heavily tested; a lock-free queue is subtle enough to buy) |
| Forward kinematics; joint-state assembly | `urdf-rs` (URDF parsing), `nalgebra` (math), `mesh-loader` (STL/OBJ/COLLADA), `meshopt` (simplification), `parry3d` (collision distance) |
| Min/max decimation, median/MAD, resampling, moments, fixed histograms; the cost-model least-squares fit | A quantile-sketch crate for t-digest/KLL (merge correctness is subtle) |
| Retry with backoff; token-bucket bandwidth limit; path templates | `object_store` (S3, GCS, Azure, local) |
| Content hashing of packages (walk + sort + hash) | `sha2` (the hash itself), `zip`, `tar` |
| Linter rules and report rendering | `inferno` (flamegraphs, dev tooling only), `rusqlite` (+ migrations), `axum`, `utoipa` (OpenAPI), `tokio` |
| Folder-completeness detection | `notify` (file watching), `pcap-parser` (pcap and pcapng; pcapng is Wireshark's default and not small) |
| Config precedence/merge and source tracking | `serde`, `toml`, `toml_edit`, `schemars`, `clap`, `thiserror`, `tracing` |
| | `xacro-rs` (xacro expansion, ROB-11: Python expressions and `load_yaml` as ROS's xacro has them), `r2r` (ROS 2), `pyo3` + `numpy` + `pyo3-arrow` (Python), `wasm-bindgen` (WebAssembly) |

**Dependency budget.** `cargo xtask deps` reports the number of crates in the
`kapto` binary's tree per feature set. CI posts it on every PR; growth above
5 crates in one PR needs a sentence of justification in the description.
`cargo deny` bans duplicate major versions of large crates (tokio, hyper,
arrow).

### 2.8 Profiling: where the time goes, and whether it should

Requirements PROF-1..7 (REQUIREMENTS §6.1). Profiling is **development
tooling**, run from `cargo xtask` against load-generator scenarios. Release
binaries carry no profiling instrumentation: anything that exists only for
profiling sits behind the `profiling` cargo feature (PROF-1).

| Question | Tool | Command |
|---|---|---|
| What does each **hot-path stage** cost per message, per batch and per byte, and is that cheap enough for the rate? | Stage microbenchmarks + fitted cost model | `cargo xtask bench model` |
| Which **functions** take the time in a realistic run, and is any thread waiting when it shouldn't? | Sampling profiler on the whole system | `cargo xtask profile record --scenario <name>` |
| How does work **flow between threads and processes**, and what does each background job cost per episode-minute or per GB? | Timeline trace from a `profiling` build | `cargo xtask profile trace --scenario <name>` |
| Does all of that match what we **expect**? | Budget comparison | `cargo xtask profile report` |

**Scenarios** are named load-generator setups in `tools/xtask/scenarios/`
(e.g. `reference-16x2khz`, `reference-plus-cameras`, `unattended-1h-events`,
`upload-10gb`), so every tool runs against the same, repeatable load.

**Stage benchmarks and cost model (PROF-2).** Each hot-path stage is a
function that takes a batch, so it can be benchmarked in isolation with
`criterion` and synthetic batches (`crates/kapto-capture/benches/`,
`kapto-transport/benches/`, `kapto-decode/benches/`). `xtask bench model`
runs them over a grid (messages per batch, payload sizes) and fits, per stage,
`t = a·batches + b·messages + c·bytes` by least squares (in-repo, a
3-parameter fit). The report puts each per-message cost next to the time
available per message at the target rate:

```
stage            per batch   per msg   per KiB   share at 32k msg/s   budget    verdict
recv (recvmmsg)    3.1 µs     55 ns      —         1.9 %               ≤ 3 %     within
queue hand-off     0.4 µs     12 ns      —         0.4 %               ≤ 1 %     within
mcap serialise     1.2 µs     90 ns     25 ns      3.4 %               ≤ 4 %     within
compress (lz4)       —          —      210 ns      1.1 %               ≤ 3 %     within
```
(illustrative numbers)

This answers "handing over MCAP frames should be fast, just very frequent":
the per-message column must stay in the tens of nanoseconds, and the fit
shows whether cost grows per message, per batch or per byte. Benchmarks in
isolation miss interference between threads, which is what the system
profile and the trace are for.

**System profile (PROF-3).** `xtask profile record` starts Kapto and the
load generator for a scenario, runs `perf record -g` on all Kapto threads,
renders a flamegraph SVG with the `inferno` crate and a table of top
functions per thread (thread names from §2.4). Off-CPU time comes from
`perf record -e sched:sched_switch` and `perf sched timehist`. Release builds
keep frame pointers (`-C force-frame-pointers=yes`) so this works on the
same binary that ships, without the `profiling` feature. Needs `CAP_PERFMON`
in the container.

**Timeline trace (PROF-4).** With `--features profiling`, the `capture`
process opens a `tracing` span per batch per stage and the worker one per job;
a `tracing` layer writes a Chrome-format trace that Perfetto opens. The trace
shows where batches wait between threads and how jobs overlap. `xtask`
derives job costs from it plus per-thread CPU from `/proc` (thread names):
CPU per episode-minute per job kind, CPU-seconds per GB for upload and
checksums.

**Budget (PROF-5).** `perf-budget.toml` at the repo root, one entry per
hot-path stage and job kind, reviewed like code. `xtask profile report`
combines the latest cost model, profile and trace results into the verdict
table: within, over, or **unexpectedly cheap** (a check costing nothing per
episode-minute is probably not running). Over-budget is a warning now and a
perf-smoke failure once the reference PC exists.

**Async (PROF-6).** The `profiling` feature also enables `tokio-console`
support in the server and the upload runtime.

### 2.9 The product explains itself

The planning documents (`docs/project/`) are scaffolding: they may be
rewritten or removed once the product is built. Nothing outside them may
depend on them.

- **Code, tests, comments, config, error messages, logs, user docs and commit
  messages** never contain requirement IDs (`CAP-9`), plan sections, or
  work-package and PR numbers. They say what they mean in words.
- **Comments explain the reason in place.** "Write `_FINALISED` last so a
  reader never mistakes a partial recording for a complete one", not
  "per CAP-9".
- **Identifiers are descriptive:** error codes (`capture.disk_full`), config
  keys, metric and thread names, test names.
- **Design documents and user docs are self-contained**: a design document
  states its context in full rather than pointing at a requirement ID.
- Requirement IDs belong in planning documents and PR descriptions only.
- CI enforces the first rule with a check that rejects requirement-ID
  patterns (e.g. `\b(CAP|TRN|ROB|…)-[0-9]+\b`) outside `docs/project/`.

---

## Part 3. Testing strategy

### 3.1 Test layers

| Layer | What it proves | Tooling | Where | Runs |
|---|---|---|---|---|
| Unit | One function or type behaves, including error paths | `cargo nextest` | `src/` `#[cfg(test)]` | Every push |
| Property | Invariants hold for all inputs, not just examples (ring buffer order, framing reassembly, FK orthonormality, sketch merge associativity) | `proptest` | `tests/` or `src/` | Every push |
| Snapshot | Human-readable outputs don't change by accident (lint reports, error messages, `episode.json`, CLI help, generated docs) | `insta` | `tests/snapshots/` | Every push; changes reviewed as diffs |
| Doc | Every example in rustdoc and `docs/` runs | doctests, xtask doc runner | crates, `docs/` | Every push |
| Integration | Components work together in-process or as processes (loadgen → capture → MCAP → read back) | nextest, real sockets on loopback | `crates/*/tests/` | Every push |
| Fuzz | Parsers never panic or hang on hostile input (decoders, framing, URDF, xacro, archives, TOML, MCAP recovery) | `cargo-fuzz` (libFuzzer) | `fuzz/` | Nightly, 10 min per target; corpus committed |
| Golden | Known recordings give known results across versions and across Rust/Python | golden MCAPs + expected JSON | `testdata/golden/` (LFS) | Every PR |
| Contract | Formats and APIs others build on don't break (JSON Schemas additive, OpenAPI diff, MCAP readable by `mcap` CLI / rosbag2 / Foxglove-compatible schema encodings) | schema diff tool, `oasdiff`, interop containers | `tests/contract/` | Every PR |
| Container / E2E | The product works as a user runs it (compose up → browser → record → episode on disk) | Docker, Playwright | `tests/e2e/` | Every PR (short), nightly (full) |
| Fault injection | Isolation and durability claims (kill -9, SIGSTOP, disk full, network cut, truncated files, power cut) | `fail` crate failpoints, tmpfs/loop devices, toxiproxy, VM power-cut job | `tests/fault/` | Every PR (fast ones), nightly (VM) |
| Performance | PERF-1..7 on the reference PC, inside the container | `xtask bench`, criterion micro-benchmarks | `benches/`, `tools/xtask` | Perf-smoke on hot-path PRs; full nightly |
| Mutation | Tests actually detect wrong code in core crates | `cargo-mutants` on `kapto-capture`, `kapto-decode`, `kapto-robot` (FK), `kapto-select` | — | Weekly report; surviving mutants become issues |
| Soak | Long unattended runs don't drift, leak or fill disks | 8 h unattended run (M16-10) | `tests/soak/` | Weekly |

### 3.2 Test data

- **Small fixtures** (< 1 MB: URDFs, pcaps, one-chunk MCAPs, malicious
  archives) live in `testdata/` in git.
- **Golden recordings** are **generated, not hand-collected**:
  `cargo xtask golden generate` drives the load generator with a fixed seed
  and scripted events (faults, interventions, idle periods) through the real
  pipeline. The generator script and seeds are the source of truth; the
  MCAPs go to Git LFS with a checksum manifest. Regeneration is an explicit
  PR whose diff shows which expected results changed.
- **Third-party URDFs** (UR, Franka, …) are committed only after a licence
  check, with a `PROVENANCE.md` per fixture (source URL, commit, licence).
- **Reference values from independent tools** (FK poses from
  `robot_state_publisher`/pinocchio, CDR bytes from real ROS 2) are generated
  once by scripts in `tools/fixtures/` that run in the ROS 2 image, and
  committed with the command that produced them. Kapto never tests against
  its own output only.
- Live-session artifacts are never committed or published (SEC-4); `testdata/`
  has a CI check that rejects pcaps not listed in its manifest.

### 3.3 Required tests by kind of change

| Change | Must include |
|---|---|
| New parser or decoder | Unit + property round-trip + fuzz target + snapshot of error messages |
| Hot-path code | Property tests + allocation test still passing + perf-smoke before/after |
| New check or scorer | Unit on synthetic signals (positive and negative case) + golden expectation update |
| New output field | Schema regenerated + schema-lock test + `episode.json` snapshot |
| New config key | Schema + precedence test (which layer can set it) + config reference regenerated |
| New API endpoint | OpenAPI regenerated + handler test + error-code test |
| New UI flow | Playwright test of the happy path and one failure path |
| New failure mode | Test that triggers it and asserts the cause code and remedy |
| Bug fix | A test that fails before the fix (stated in the PR) |

### 3.4 Flaky tests

- Timing-sensitive assertions (rates, latencies, CPU) only run on the
  reference runner (for now, the maintainer's machine), never on shared CI
  runners. Shared runners check counts
  and correctness, not speed.
- No automatic retries for capture, durability or isolation tests: a flake
  there may be a real race. A flaky test is quarantined (`#[ignore = "flaky:
  #issue"]`) with an issue, and fixing it outranks new features in that WP.

### 3.5 Test names and requirement coverage

Test names describe the behaviour they check, in full words, so a failing
test explains itself: `finalised_marker_is_written_after_manifest`,
`sequence_gap_across_wraparound_counts_as_lost_upstream`,
`stalled_viewer_does_not_increase_capture_drops`. Not `test_cap9` or
`finalise_works`.

Which tests cover which requirement is **planning information** and stays in
the planning documents: each PR description names the requirements it
covers and the tests it adds, and the MVP and v1 release checklists (at the
ends of Parts 4 and 5) are ticked off in the release PR with the tests, benchmarks
or demo steps that show each item working. Nothing in the code, tests or tooling depends on
requirement IDs, so the planning documents can be rewritten or removed
without touching the code.

### 3.6 CI pipeline

| Stage | Contents | When | Target time |
|---|---|---|---|
| `check` | fmt, clippy (both feature sets), `cargo deny`, `xtask gen --check`, AGENTS.md checker, planning-reference check (§2.9), link check (`lychee`) | Every push | ≤ 5 min |
| `test` | nextest (unit, property, snapshot, integration, doc), Miri on `unsafe` modules | Every push | ≤ 10 min |
| `build` | Both Docker images via BuildKit + `cargo-chef` cache; UI build, also kept as the `kapto-ui-<version>.tar.gz` workflow artifact (UI-7); Python wheel (from M13) | Every PR | ≤ 15 min warm |
| `e2e` | Container smoke, contract/interop tests, short Playwright run, fast fault tests | Every PR | ≤ 15 min |
| `perf-smoke` | 5 min at PERF-1 load in the container on the **reference runner** (until it exists: run locally, report attached, see below) | PRs labelled `perf` or touching hot-path crates (path rule) | ≤ 10 min |
| `nightly` | PERF-1 1 h, PERF-2 stress, capacity search, fuzz (10 min/target), VM power-cut test, full Playwright | Nightly on `main` | — |
| `weekly` | `cargo-mutants`, 8 h soak (from M16), dependency update PR | Weekly | — |

**On GitHub Actions** each row is a job; they are grouped into workflows in
`.github/workflows/`. With a single contributor the workflows are manual
only: `cargo xtask ci` runs the same checks locally before each pull request.

| Workflow | Trigger | Jobs |
|---|---|---|
| `ci.yml` | Manual (`workflow_dispatch`) while there is a single contributor; `pull_request` and `push` to `main` once others contribute | `check`, `test`, `build`, `e2e`; `perf-smoke` only when the PR has the `perf` label or changes hot-path crates (`paths` filter) |
| `nightly.yml` | `schedule` (nightly) and `workflow_dispatch` | `nightly` |
| `weekly.yml` | `schedule` (weekly) and `workflow_dispatch` | `weekly` |

- Jobs call `cargo xtask …`, so the workflow files stay thin and everything
  runs the same locally.
- Rust and Docker layer caches use the Actions cache (`Swatinem/rust-cache`,
  BuildKit `type=gha`).
- Timing-sensitive jobs (`perf-smoke`, the perf part of `nightly`) run on a
  **self-hosted runner** labelled `kapto-reference` once the reference PC
  exists; until then they are skipped in CI and run locally (see below).
- Build outputs (benchmark reports, the UI bundle, wheels from M13) are
  uploaded as workflow artifacts.
- Third-party actions are pinned to a commit SHA, and Dependabot keeps them
  and the Cargo dependencies up to date (the weekly dependency PR).

**Benchmark machine (open question 1, interim):** until the reference PC
exists, benchmarks run on the maintainer's own machine and every number is
**provisional**. In practice:
- `cargo xtask bench` runs locally (natively and in the container) and writes
  a report that starts with a machine fingerprint: CPU model, cores/threads,
  RAM, disk model and type, kernel, Docker version, CPU governor.
- `perf-smoke` and nightly perf are not CI gates yet. A hot-path PR attaches
  the local before/after report instead; the reviewer compares the two runs
  on the same machine, not absolute numbers.
- To approximate the reference PC, `xtask bench --limit reference` runs the
  container with `--cpus=4 --memory=16g`; the report says it was limited.
- When the reference PC arrives it becomes a self-hosted runner, the perf
  stages become gates, and Gate 1 is re-run there for the official figure.

---

## Part 4. Phase A work packages: MVP (P0)

Order follows REQUIREMENTS §9.1. Critical path: M0 → M1 → Gate 1 → M2 → M3b →
M4 → M7 → M8. M3 starts in parallel with M1; M5, M6 and R0 start after Gate 1.

Each table row is one PR. Columns: **Delivers** (requirement IDs),
**Tests**, **You check** (what to look at to understand it).

#### Gate 1: architecture go/no-go
One PR `docs(bench): gate 1 report` adding `docs/project/gate-1.md`:
PERF-1 (1 h, 0 drops, ring ≤ 50 %), PERF-2, PERF-5 in the container, the
**headline capacity figure**, and the cost-model table and flamegraph for the
hot path (PROF-2, PROF-7). If any fails, the next PRs fix the core; nothing
else in Phase A starts. Run on the maintainer's machine for now, so the report
is marked **provisional** with the machine fingerprint (§3.6), and Gate 1 is
re-run on the reference PC when it exists.

### WP R0: Relevance spike *(parallel after Gate 1)*

**Goal:** answer "does one relevance config carry over to a second task?"
before M16 builds the product version (REQUIREMENTS R0, open question 8).

**How:** in `spikes/relevance/`, not product code. Python notebooks reading
MCAP (via the `mcap` Python package; the Kapto wheel doesn't exist yet).
1. Record or obtain two tasks (ideally two embodiments): loadgen scripted
   scenarios plus at least one real dataset (e.g. a LeRobot/DROID episode
   set converted to MCAP).
2. Hand-label "keep" ranges with a written labelling guide.
3. Implement 4–5 simple scorers (QA events, novelty on windowed statistics,
   activity, interventions, gripper/contact events) and a combiner.
4. Tune on task A, measure precision/recall and data reduction on A and B,
   then re-tune B with a thin per-task layer and measure again.

| PR | Delivers | You check |
|---|---|---|
| R0-01 `docs(spike): relevance labelling guide and data` | Data sources, licences, labelling guide | Labels are reproducible by someone else |
| R0-02 `chore(spike): scorers and results` | Notebooks + results table | The numbers, not the code |
| R0-03 `docs(design): relevance scorer interface` | Its design document: `Scorer` trait shape, `relevance.toml` shape, which scorers ship first, how much per-task tuning is expected; answer to open question 8 | M16 is built from it |

### WP M4: Visualisation

**Goal:** the live view and replays arranged as each robot and operator
need, without ever costing capture anything. The views are Kapto's own
([viewer design](../design/a-viewer-of-our-own.md)).

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M4-08 `feat(robot): display mesh decimation` | `meshopt` simplification above a triangle budget; original kept for collision (ADD-17) | Triangle budget respected; bounding box preserved within tolerance | — |

### WP M5: Durability + pre-flight *(parallel after Gate 1)*

**How:**
- Runtime state store arrives here because the job queue needs it: SQLite in
  `kapto-store` with versioned migrations, WAL, single writer (CFG-2).
- Recovery reads the staged MCAP up to the last complete chunk and writes a
  **new** file marked `recovered`; the original partial file is kept until the
  recovered one is finalised (raw is never rewritten).
- Power-cut testing: per-PR tests truncate files at every chunk boundary and
  random offsets; nightly, a VM job (QEMU) writes under load and is hard-reset,
  then recovery is checked.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M5-01 `feat(capture): pre-roll and post-roll` | As [pre-roll and post-roll](../design/pre-roll-and-post-roll.md) says, in PRs: `pre_roll_s`, `post_roll_s` and `capture.pre_roll_max` with the rings set aside at startup; the writer holding and writing the pre-roll; loss before Start counted; post-roll after Stop; `commanded_*` times, `pre_roll`/`post_roll` counts and the replay's marks | Pre-roll content equals the last N s sent; memory bound; loss in the pre-roll counted; zero-allocation test and before/after `bench model` | Memory budget declared at startup (PERF-5b) |
| M5-03 `test(fault): power-cut VM job` | Nightly QEMU job: record, hard reset, verify loss ≤ fsync window | Nightly result | How the durability claim is proven |
| M5-07b `feat(server): resource history` | Each process's CPU and memory samples kept in SQLite runtime state (after M5-08), and shown in the web UI over time and for the span of any episode; never written into `episode.json` or the manifest, which hold only what was recorded (EFF-8) | Samples survive a restart of serve; an episode's span shows the samples taken during it; old samples are removed past the retention limit | **Open question:** how long samples are kept and at what resolution (e.g. one a second for a day, then thinned), as settings with defaults |
| M5-08i `perf(worker): job costs in profile traces` (with the profiling tooling) | Per-job spans in `profiling` builds so `profile trace` reports a cost per episode-minute for each job kind (PROF-4 worker part) | `profile trace` on a scenario reports a cost per episode-minute for each job kind | — |
| M5-09 `test(fault): isolation suite` | NFR-1 fault injection for worker, server, bridge; a small tmpfs filled during an episode, which capture finalises before it is full and never leaves corrupt (the disk limits of M5-06, there tested with configured limits and a pretend disk) | Fault suite | Exit: power-cut and kill -9 lose ≤ fsync window; disk-full ends clean |

### WP M6: ROS 2 transport *(parallel after M2)*

**How:** `r2r` behind feature `ros2` (the [ROS 2 design](../design/ros-2-through-rcl.md)
compares `r2r`, `rclrs` and `ros2-client`). Subscriptions are **untyped**, each message taken as received through rcl (the ROS 2 design): bytes stay
CDR, stored as received with a `ros2msg` schema assembled from the installed
`.msg` definitions (as rosbag2 does). Only mapped fields are decoded live,
with the in-repo CDR reader from M2-03. The ROS executor runs on its own
thread and hands batches to the same capture queues as UDP.


| PR | Delivers | Tests | You check |
|---|---|---|---|
| M6-05d `perf(transport): ros 2 over zenoh at the cost of dds` | Recording the reference load as ROS 2 topics with two 1080p cameras costs more over `rmw_zenoh_cpp` than over DDS, every target met over each: in 20 runs each (`bench ros2 --cameras 2`), 0.73–0.74 cores over Zenoh against 0.55–0.56 over Cyclone DDS and 0.59–0.60 over Fast DDS. Where it goes: Zenoh's receive threads (`rx-*`) take about 0.30 cores against DDS's 0.22–0.24; Kapto's receiver hands over 23 % more batches (615 a second against 500, 52 messages each against a full 64), as Zenoh delivers in bursts that end the 2 ms batch wait early, which the writer and the viewer tap pay for per batch; and the machine runs about 25 % slower (1.53 GHz against 2.1–2.6), busier with Zenoh's own threads, so the same work costs more. In cycles (cores × clock) Zenoh costs 7–14 % more than Cyclone DDS. Shared memory (`ZENOH_CONFIG_OVERRIDE='transport/shared_memory/enabled=true'`) changed nothing in ten runs (0.74). A profile of `kapto record` over Zenoh (`perf`, 15 s) puts 45 % of its samples in Zenoh's receive threads: Zenoh itself (`libzenohc`) 13 %, and ROS 2's Zenoh layer (`rmw_zenoh_cpp` 0.2.11 with the C++ library it calls) about 14 %, roughly 0.10 cores, which is the receive cost's whole difference from DDS. Of that, about 5.5 % is `hash_gid`, which `add_new_message` calls for every message: it writes the publisher's 16-byte ID into a `std::stringstream` as hex and hashes the text. It is unchanged in every release up to 0.2.12 for Jazzy and 0.14.0 for Rolling; two draft pull requests replace it with FNV-1a over the bytes, [ros2/rmw_zenoh#1028](https://github.com/ros2/rmw_zenoh/pull/1028) and #422, held on whether it is faster, which this profile answers. The rest is `rmw_zenoh_cpp` allocating, copying and waking for each message. Kapto stays on rcl over every middleware: a transport of its own on the `zenoh` crate would skip that layer, but is one more thing to maintain, and Zenoh meets every target as it is. Left: batches that stay full over Zenoh (taking what Zenoh has delivered before ending a batch, or a batch wait of Zenoh's burst length), and the session's tuning (batching, the receive threads' count) measured the same way; dropping DDS isn't called for, as it costs less. **Open question:** once #1028 (or #422) is merged and released for Jazzy, measure Zenoh again with the image's `rmw_zenoh_cpp` updated, and whether that brings it within reach of the target | `bench ros2 --cameras 2` over Zenoh within 10 % of Cyclone DDS's cycles, every message written, in ten runs each | Recording a Zenoh robot with cameras |

### WP M6b: Cameras

Capture takes camera frames as it takes every message: large ones through
rings of their own ([large messages design](../design/large-messages-in-rings-of-their-own.md)),
each recorded as received (EFF-1, no transcoding), and the synthetic robot
can be a camera (`kapto-loadgen --camera`). The viewer shows each camera up
to `viewer.camera_max_fps`. **Open question:** whether direct cameras over
V4L2 (M15-01) move here rather than waiting for Phase B.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M6b-04b `perf(capture): ros 2 cameras within a core` | Recording the reference load as ROS 2 topics with two 1080p cameras at 60 fps beside it takes 1.02–1.13 cores on the maintainer's laptop (`cargo xtask bench ros2 --cameras 2`), over the one-core target, with every message written. The two threads receiving the joint states (`cap-rx-ros2` and its middleware listener) take 2.6 times what they do without cameras, doing the same work; the cameras' own receivers take 0.01 each and the writer 0.24–0.28, lz4 on frames included. Find whether it is the machine (clock speed with more cores busy, under `powersave`) or Kapto, on the reference PC first, and what `capture.compression = "none"` saves Measured again five times with the camera path to the live view in place: 0.56, 0.58, 0.76, 0.91 and 1.03 cores over the three middlewares, so the cost swings from run to run on the same build rather than sitting above the target; look for what differs between a run at half a core and one at a whole one before what Kapto spends. The benchmark's report now says what the machine was doing while recording (CPU clock on average and at its lowest, the CPU's hottest reading, cores busy besides recording); left: comparing those between a run at half a core and one at a whole one, and runs on the reference PC. | `bench ros2 --cameras 2` within a core over each middleware, on the reference PC | Capture's CPU with ROS 2 cameras |

### WP M8: `docker run` robot + one-command demo

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M8-01 `feat(server): robot from mounts and env` | `KAPTO_URDF`, `KAPTO_MESH_PATH`, `/packages` discovery, `KAPTO_ROBOT`, invalid robot kept running with report (ADD-9, ADD-10, ADD-12) | Container tests per mount style incl. a broken URDF (no crash loop) | — |
| M8-02 `feat(cli): package preview` | The preview's CLI equivalent; lint gate usage (ADD-15). `kapto package import` and `kapto package lint` exist | CLI snapshots; exit codes | Using `kapto package lint` as a CI gate in a robot-packages repo |
| M8-05 `test(e2e): third-party urdf over udp` | `docker run -v ./robot:/robot -e KAPTO_URDF=…` with a third-party URDF and loadgen mapped to it | E2E | Exit: MVP demo both ways |

The demo is `kapto demo`: `kapto serve` with the synthetic robot inside it, one command with Cargo or `docker run`. Compose holds what runs beside Kapto: storage for uploads, UI development, ROS 2.

#### MVP release checklist
Worked through in the description of the PR that tags the MVP release; each
item is ticked with the test, benchmark or demo step that shows it.

- [ ] Every P0 requirement covered by a merged PR whose tests pass
- [ ] PERF-1..6 met in the container (on the reference PC once it exists;
      provisional numbers until then)
- [ ] Profiling results attached: cost model, flamegraph, budget verdicts
- [ ] Two robot packages recorded end to end: one in-house robot, one
      third-party URDF over UDP
- [ ] `docker compose up` demo works on a fresh machine
- [ ] `docs/concepts/architecture.md` matches what is shipped

---

## Part 5. Phase B work packages: v1 (P1)

Critical path: MVP → M9 → M10 → M16 → M12 → M13. M14, M15, M17, M18 run in
parallel after M9; M11 after M16.

### WP M9: Sessions, storage contract, configuration, security

**Goal:** freeze the formats that everything in v1 builds on, and make
configuration complete (§4.15).

**How:**
- Config layering lives in `kapto-config`: each key declares its kind
  (deployment or recording-behaviour) in the schema; the merge produces
  `Effective<T>` values carrying their source, which `config show` and the
  per-episode snapshot both print.
- UI writes go through one `ConfigWriter` using `toml_edit` (preserves
  comments), atomic temp + rename, refusing with a clear error when `/config`
  is read-only.
- Schema lock: every released schema version is kept in
  `schemas/<name>/v<N>.json`; a test checks that v(N+1) only **adds** optional
  properties relative to every earlier version.
- Fields needed later (relevance scores and plans, discard totals, clock
  state, provenance) are added now as optional, so later WPs only fill them.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M9-02 `feat(config): writes, secrets, reload` | `ConfigWriter` (CFG-4), secret references + inline-secret rejection + redaction (CFG-5), live reload applying to next episode, restart-needed keys reported (CFG-8) | Comment preservation snapshot; read-only mount → UI form disabled (Playwright); reload during recording doesn't change the session | Where a UI click ends up on disk |
| M9-02b `feat(wasm): shared validation for config forms` | `kapto-wasm` crate; `kapto-core`/`kapto-config` build for `wasm32` with I/O behind a feature; profile, task and destination forms validate in the browser with the server's cause codes; Docker UI stage builds it | **Parity test**: one fixture table of valid/invalid inputs run natively and via `wasm-bindgen-test` in headless Chrome, identical results; size budget check | Where the browser/server boundary sits, and why validation can't drift |
| M9-02c `feat(ui): create and edit profiles and tasks` | Forms in the web UI to add, edit and remove session profiles and tasks beside those already in the configuration folder, writing `profiles/<id>.toml` and `tasks/<id>.toml` through the `ConfigWriter` (CFG-4: comments kept, read-only folder means read-only forms with the reason), validated in the browser with the server's cause codes (M9-02b); `POST/PUT/DELETE /api/profiles/{id}` and `/api/tasks/{id}`; the session picker lists the new ones at once. Episodes keep the copies they were recorded with, whatever is edited later | Handler tests per write, including a read-only folder and an invalid form; Playwright: add a task, record with it, edit it, the episode keeps the old copy; screenshots | — |
| M9-02d `feat(ui): quality settings per task` | A task (and a profile) can override the robot package's `qa.toml` in a `[qa]` table of its file, with the same keys (thresholds, checks and detectors turned on or off, statistics chosen), applied in the recording-behaviour order package < profile < task (M9-01d); the effective settings are what the recording carries (the design of what a recording carries), so an episode is still checked with what it was recorded with. The task form in the web UI (M9-02c) edits them, showing each value's source and default, validated as `qa.toml` is. **Approach:** motion checks and their thresholds differ from task to task, so they are set per task, from the UI too | Precedence matrix test per key; a task's override reaches the recording and the checks; Playwright: edit a threshold in a task, record, find it applied; a bad value refused with its cause code | Where a task's quality settings live and how they show |
| M9-03 `feat(config): snapshots and history` | `<episode>/config/effective.toml` + copies, in manifest, and attached to the recording (the design of what a recording carries); history in SQLite; `config diff`; API/CLI/Python hooks (CFG-6, 7, 9) | Snapshot replay test: re-run QA with snapshot = original results; diff output snapshot | Reproducing an old episode's config |
| M9-04b `feat(capture): your own metadata per episode` | Keys the user defines, in a `[metadata]` table of the robot package and the session profile, and at start: `kapto episode start --meta key=value`, `metadata` in `POST /api/episodes/start`, a form in the web UI; optionally declared in the package with type and whether required; written as an MCAP `Metadata` record `kapto.user` and as an optional `metadata` object in `episode.json`; shown in the episode list and details (CTL-8) | A required key missing refuses the start with its cause code; a value of the wrong type is refused; the record read back from the MCAP with the `mcap` crate; schema-lock and `episode.json` snapshot; Playwright: record with metadata and find it in the episode's details | **Open question:** (1) types: MCAP metadata holds only text, so typed values (numbers, booleans) are typed in `episode.json` and text in the MCAP, or everything is text; (2) precedence when the package, the profile and the start give the same key (proposed: package < profile < start, as for recording behaviour); (3) whether metadata can be corrected after the episode is finalised: raw data is immutable, so proposed: corrections go to `episode.json` with who and when (as LAB-2), never into the MCAP |
| M9-05 `feat(capture): trigger engine` | `TriggerSource` trait + engine (debounce, start/stop/mark semantics) (§2.6) | Engine unit tests with a fake source | The trait, before any real source |
| M9-06 `feat(capture): trigger sources` | ROS 2 service/topic, UDP/TCP command message, stream bit edge, HID pedal via `evdev` (CTL-2) | Conformance suite per source; HID with a virtual uinput device on ref runner | Each source is small because the engine owns the logic |
| M9-07 `feat(capture): subtask marks and hitl` | Marks from trigger/UI with vocabulary: done as events (M4c-08 to M4c-09d, the viewer design), subtasks among their kinds; what remains is the HITL role → episode tags (CTL-3, CTL-5) | Integration: marks align to recv time within one batch | — |
| M9-08 `feat(store): schema versions and lock` | Versioned schemas for `episode.json`, manifest, `_SUCCESS`, plans; additive check; reserved optional fields for REL/CAP-4 (STO-5) | **Schema-lock test** against all previous versions; a deliberate removal in a fixture fails | How formats evolve without breaking readers |
| M9-09 `feat(store): second copy and retention` | Copy/verify to a second location; retention never deletes before verified copy (STO-4 base) | Retention property: no sequence of events deletes an unverified file | — |
| M9-09b `feat(store): deleting an episode by hand` | Deleting an episode from the UI, `kapto episode delete` and `DELETE /api/episodes/{id}`; its design document first, as it touches invariant 6 (raw data goes away only through the retention executor). **Open question:** how it works. Options considered: a bin, purged by the retention executor after a set time and emptied first when the disk runs low, with an audit entry and restore; deleting at once as a one-episode discard plan, audited; allowed only once a verified copy is uploaded | — | — |
| M9-11 `feat(server): auth and listener scoping` | Token auth required for non-loopback (token from secret ref), listener scoping to declared interfaces, egress test hardened (SEC-1..3) | Bind non-loopback without auth → refused with cause code; auth tests | — |
| M9-12 `feat(robot): package versioning` | Version history, episode embeds package copy or hash + local copy (ROB-8) | Replay with old version after package edit | — |
| M9-13 `docs(design): open-source boundary` | Its design document (LIC-3) | — | — |
| M9-14 `feat(core): structured logging` | `tracing` and `tracing-subscriber` in every process (NFR-11): `log.level` as the default filter and `RUST_LOG` per module on top; each line with level, timestamp, process (`serve`, `capture`, `viz-bridge`, `worker`) and fields; spans per session, episode and stream; `log.format = text \| json` as a new setting; today's ad-hoc `warning:` and `[serve]` lines become events, and failures keep their cause code and remedy as fields; rate-limited warnings; user-facing command output (tables, reports, live status) stays on stdout and stderr as it is | `log.level = "warn"` hides info lines; JSON lines parse and carry level, process and cause code; a secret reference is never printed; the allocation test still passes with logging enabled (nothing logged per message) | How a log line looks in a terminal and in `docker logs`, and which events are info vs debug |

**Exit:** schemas frozen and locked; deleting `kapto.db` loses no setting; a robot-triggered session is recorded with subtask marks.

### WP M10: QA

**How:**
- `Check` trait: `fn kind() -> Gate | Advisory`, `fn run(&EpisodeView,
  &Params) -> CheckResult`. `EpisodeView` gives role-based access to decoded
  columns (from MCAP via the decoders), so checks never see topic names.
- Streaming checks run in the worker on the lossy tap; post-episode checks
  run as low-priority jobs (EFF-5).
- Signal-processing helpers are in the repo (§2.7): finite differences,
  windowed statistics, Laplacian variance for sharpness, histogram exposure.
  JPEG decoding of **sampled** frames uses the `image`/`zune-jpeg` crate.
- Self-collision proximity uses `parry3d` distance queries on collision
  geometry at sampled timesteps.
- Statistics (STO-7): moments and fixed histograms in the repo; quantile
  sketches from a crate. All are mergeable, so session and dataset summaries
  never reopen recordings.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M10-02 `feat(qa): streaming checks` | Rate drop, gaps, frozen stream, sequence loss, clock skew (QA-2) | Synthetic signals: positive and negative case each | — |
| M10-03a `feat(qa): episode duration and stream presence` | Gates `episode.short` (under `min_seconds`, 2 s) and `streams.present` (a critical stream, or with none named every stream but the kept, without messages); advisories `episode.long` (over `max_seconds`, 600 s) and `streams.empty` (the other empty streams), set in `qa.toml` | Positive and negative synthetic episodes; contract suite | — |
| M10-03b `feat(qa): camera frames` | Gate `camera.format` (a camera's first frame isn't what its declared `format` says, e.g. no JPEG start-of-image marker) and advisory `camera.small_frames` (frames under a share of the median size: black, blank or broken), from the recording's index and first frame | Fixture frames, good and broken | — |
| M10-03c `feat(qa): stream timing` | Advisories for a stream starting `N` s after the episode or ending `N` s before it, measured rate off its declared `rate_hz` by more than a share, rate at the end below a share of the start, and the longest silence on a stream (a gate for critical streams), each with where it is; from per-message times in the recording's index | Synthetic recordings with each fault | — |
| M10-03d `feat(qa): segments` | From subtask events: none, a gap between subtasks over `N` s, coverage under a share of the episode, a subtask under `N` s, one with an empty prompt (a gate) | Synthetic event sequences | — |
| M10-03e `feat(qa): monotonic time and size bounds` | Log times out of order within a channel; episode size outside bounds | Same | — |
| M10-05 `feat(qa): sync checks` | Inter-camera sync, teleop lag, action/observation alignment (cross-correlation) | Injected known lags recovered within tolerance | — |
| M10-06 `feat(qa): video quality` | Sharpness, exposure, motion on sampled frames | Fixture frames | CPU cost recorded |
| M10-07 `feat(qa): urdf-aware checks` | Joint limits, saturation (a joint at its limit or its effort at the URDF's maximum, from M10-04), self-collision proximity, workspace bounds, FK vs reported EE (QA-3) | Injected violations; wrong mapping caught by FK-vs-EE | Why one kinematic model matters (§5) |
| M10-10 `test(qa): golden recordings and fault injection` | `xtask golden` scenarios per embodiment with injected faults; expected verdicts (QA-4) | Golden suite in CI | How a regression shows up |
| M10-11 `feat(viz): qa overlay` | Markers on timeline, link highlighting (VIS-7) | `.rrd` assertions | — |
| M10-12 `feat(qa): results with the episode` | QA results and statistics written with the episode, so they travel with it on upload and survive a lost `kapto.db`; SQLite becomes an index rebuilt from them | Schema-lock test; results rebuilt into SQLite from the episodes match what was computed | **Open question:** a `qa.json` sidecar with its own schema, or a section of `episode.json` (both additive-only), and whether the manifest lists it; its design document first, being expensive to change |
| M10-13f `feat(qa): an autoencoder of joint motion over time` | A second kind trained on short windows of the joint angles themselves (e.g. 1 s resampled to 50 Hz, each joint's positions and velocities), from a package and task's accepted episodes; reconstruction error per window, so a flag says *when* in the episode the motion was odd, as time ranges for the QA overlay (M10-11) and the misbehaving demo (M10-13b). **Approach:** after the statistics gain where each joint went and how smoothly (M10-08d); raw full-rate whole episodes aren't used: too much data for what they add | A planted freeze or vibration found with its time range; a normal episode isn't; the same episodes give the same model; benchmark of learning and scoring per episode-minute | Window length and rate; whether it runs on every episode or only on those the statistics flag |

### WP M16: Relevance selection + unattended mode

**Goal:** decide on the edge what is kept, and account for everything that
isn't (§4.6b).

**How:**
- Start from the relevance scorer's design document (R0-03). `Scorer` trait: `fn score(&SegmentView,
  &Params) -> Vec<ScoredRange>`; each range carries value, scorer id and
  version, params hash, reason.
- **Plan building is pure**: `fn build_plan(scores, triggers, overrides,
  tiers, budget) -> RetentionPlan`. No I/O, so it is exhaustively
  property-tested. The executor applies a final plan and is the only code
  that removes or replaces raw files.
- Continuous mode: the `capture` process rotates segments (time or size); segments sit
  in a bounded buffer (disk budget) until their plan is final.
- Downsampling (in repo): scalars min/max-preserving; MJPEG drops frames
  (each frame is independent); H.264 keeps keyframes only or stays `keep`
  (no re-encoding, EFF-1), documented.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M16-01 `feat(capture): continuous mode` | Segment rotation, bounded segment buffer, episodes cut from segments (CAP-13) | Rotation under load: no message lost across boundaries (counts); buffer bound respected | Segment boundaries in MCAP |
| M16-02 `feat(server): unattended mode` | Profile at boot, no operator, no human step required (CTL-6) | Container restart resumes unattended recording | — |
| M16-03 `feat(select): score model and scorer trait` | `ScoredRange`, `Scorer` trait, worker execution with time limit, `relevance.toml` + task overrides + `relevance_params_version` (REL-1, REL-2 trait, REL-3) | Trait conformance suite; params precedence | Consistency with the scorer's design document |
| M16-04 `feat(select): built-in scorers` | QA-based, novelty on STO-7 statistics, activity, events, mode (REL-2) | Synthetic segments per scorer; **property: QA results only ever raise scores** (invariant §2.3) | — |
| M16-05 `feat(select): keep windows` | Trigger/QA/HITL/manual windows with pre/post, an episode's events (M4c-08) among the triggers, applied retroactively on the segment buffer (REL-4) | Event at t keeps [t−pre, t+post] at full rate even across segment boundaries | Retroactive windows across segment boundaries |
| M16-06 `feat(select): plans, tiers, dry-run` | Pure plan builder; tiers keep/downsample/summary/discard; dry-run report with sizes (REL-5, REL-6) | **Property tests on the plan builder**: ranges cover the timeline exactly once; overrides win; highest scores never demoted before lower ones (REL-11) | The plan builder |
| M16-07 `feat(select): retention executor and audit` | Extends the executor M12-06 brings in (whole episodes) to ranges: applies final plans; downsampling implementations; discard audit with statistics; totals in `episode.json` (REL-7, STO-4) | **Accounting invariant**: recorded seconds = kept + downsampled + summary + discarded, per stream; manifest re-verified; crash mid-apply resumes correctly (failpoints) | The accounting invariant test |
| M16-08 `feat(ui): overrides, viewer track, relevance view` | Pin keep/discard (REL-8), scores timeline (REL-9), UI-5 | Playwright; override provenance stored | — |
| M16-09 `feat(select): evaluation harness and budgets` | Labelled-set replay → precision/recall + reduction (REL-10); daily/session budgets (REL-11) | Harness on golden scenarios with known answers | How a team tunes a task |
| M16-10 `test(soak): unattended 8 h` | Weekly soak with injected events | Exit criterion from REQUIREMENTS M16 | — |

### WP M11: Structuring and labelling

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M11-01 `feat(qa): labeller trait and rules` | `Labeller` trait (extension point) and rule engine from package/task config; `source: algo` (LAB-6); labellers read an episode's events (M4c-08) and add what they find as events of the same shape | Rule tests on synthetic events | — |
| M11-02 `feat(qa): built-in labellers` | Subtask segmentation (triggers, gripper, contact, idle), mode/intervention ranges, outcome hints, QA/relevance tags | Golden scenarios with expected segments | Labels on a run nobody touched |
| M11-03 `feat(server): review and corrections` | Optional accept/reject, notes, tags, vocabulary; corrections with provenance (LAB-1, LAB-2) | API tests; original values preserved | — |
| M11-04 `feat(ui): label from timeline` | Time range → label, subtask or keep/discard override (VIS-8, REL-8) | Playwright | Exit criterion |

### WP M13: Python package

**How:** `kapto-py` with PyO3 (abi3) + maturin, wheels built in CI
(manylinux, macOS for analysis). Reading returns Arrow tables through the
Arrow PyCapsule interface (`pyo3-arrow`) and NumPy arrays (`numpy` crate)
without copying where possible. The GIL is released around every Rust call
that does real work. Python plugins (checks, scorers, labellers) run in a
separate worker subprocess with a time limit, using the same traits on the
Rust side through an adapter. Stubs generated and verified with
`mypy.stubtest`.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M13-01 `build(py): wheel skeleton` | Crate, maturin config, CI wheel job, version shared with engine (PY-1, PY-11) | `pip install` in clean container; `import kapto` | — |
| M13-02 `feat(py): read recordings` | Episodes, streams, NumPy/Arrow, pandas/polars helpers, both timestamps (PY-2) | pytest against goldens; zero-copy asserted where promised | — |
| M13-03 `feat(py): resample and align` | Same code as exporters (PY-3) | Parity with Rust on goldens | — |
| M13-04 `feat(py): packages and fk` | Load, lint, roles, vectorised FK (PY-4) | Parity with Rust FK goldens | — |
| M13-05 `feat(py): checks, plugins, scorers` | Run checks; decorator API for checks, scorers, labellers; worker isolation and timeout (PY-5, PY-6, PY-13) | Plugin that hangs is killed with a cause code; results marked `python-plugin` | — |
| M13-06 `feat(py): control client` | A thin client over the REST API (M8b-03): status, health, start/stop/abort, marks, labels, events, upload status, config (PY-7, CFG-9) | Against a running server in e2e | — |
| M13-07 `test(py): parity suite and stubs` | Goldens shared with Rust (PY-12); `stubtest` | CI | Exit: notebook matches the Rust engine |

### WP M14: Robot onboarding+ *(parallel)*

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M14-02 `feat(robot): xacro cross-checked with ros's` | Optional: in the ROS 2 image, ROS's own `xacro` expands a description at import when asked, to cross-check the built-in expander (ROB-11) | The UR description of `testdata/xacro`, outputs compared | — |
| M14-03 `feat(robot): ros yaml import` | `ros2_control`, joint lists, camera calibration YAML → TOML (ROB-12 import) | Fixture per source format | — |
| M14-04 `feat(ui): mapping wizard` | Live fields, dropdown mapping, model moves as joints map (ADD-5) | Playwright with loadgen | — |
| M14-05 `feat(ui): camera frames` | Link pick, intrinsics, frustum preview (ADD-6) | Playwright | — |
| M14-06 `feat(robot): edits, versions, hot reload, read-only mounts` | ADD-8, ADD-13, ADD-14 | Edit during recording doesn't affect session | — |
| M14-07 `feat(robot): templates, scenes, embodiments` | ADD-19, ROB-7 (scene file with mount transforms), ROB-9 | Two-vendor scene FK + viewer | Exit criterion |

### WP M15: Direct sensors + tuning *(parallel)*

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M15-01 `feat(transport): v4l2 cameras` | MJPEG/H.264 passthrough via V4L2 (`Transport` impl), frame timestamps (TRN-7) | `v4l2loopback` device on ref runner | Still no transcoding |
| M15-02 `feat(transport): realsense` | Plugin, colour + depth as received (TRN-7) | Hardware test on ref runner (marked) | — |
| M15-03 `feat(transport): audio` | ALSA/PulseAudio input (TRN-8) | Loopback audio device | — |
| M15-04 `feat(capture): rt priority and pinning` | Optional, with `CAP_SYS_NICE` detection (CAP-12, DCK-8) | Works and degrades gracefully without the capability | — |
| M15-05 `feat(config): low-power profile` | Viewer rates, deferred QA (EFF-7) | Bench on lower-bound machine | — |
| M15-06 `build(docker): production profile and python` | DCK-9, DCK-10, DCK-11 docs | Container tests | — |
| M15-06b `feat(xtask): profile report and budgets` | Full `perf-budget.toml` (hot path + every job kind), `xtask profile report` with within/over/unexpectedly-cheap verdicts; `tokio-console` under the `profiling` feature (PROF-5, PROF-6) | Report snapshot on the `unattended-1h-events` scenario; a deliberately slowed check is flagged over budget, a disabled one as unexpectedly cheap | Which background jobs dominate CPU per episode-minute, and whether that matches the budget |
| M15-07 `test(docs): quick-start in ci` | DCK-13 steps executed as written | E2E | Exit criterion |

### WP M17: Time correlation *(parallel)*

**How:** clock-sync state from `adjtimex` (synchronised flag, estimated
error) plus chrony/ptp4l status when available. Offset and drift per stream:
network delay is always positive, so the estimate uses the **lower envelope**
of (receive − source) over time (convex-hull / minimum filter, then linear
fit), which is robust to delay spikes. Written in the repo (§2.7).

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M17-01 `feat(core): clock-sync state` | Host sync state per episode (CAP-4) | Unit with recorded `adjtimex` samples | — |
| M17-02 `feat(capture): offset and drift estimation` | Lower-envelope estimator per stream, stored beside raw data | Loadgen with injected skew, drift and delay spikes: estimate within tolerance | The estimator on a plot in the docs |
| M17-03 `feat(viz): aligned replay` | Corrected timeline in replay and readers | Exit criterion: skewed streams replay aligned | — |

### WP M18: Folder import + ROS 2 zero-config *(parallel)*

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M18-01 `feat(capture): importer trait and folder watch` | `Importer` trait (§2.6), `notify` watcher, completeness detection (stable size + valid MCAP footer / closed bag), files never modified (TRN-11) | Partial file not picked up; source checksum unchanged after import | — |
| M18-02 `feat(capture): mcap and ros 2 bag importers` | MCAP and ROS 2 bags (`.mcap`; `.db3` via `rusqlite`) | Fixtures from real `ros2 bag record` | — |
| M18-03 `feat(transport): ros 2 record-all` | Discovery-driven subscriptions with include/exclude (TRN-13) | ROS image test with several talkers | Exit criterion |

### WP M19: Operator and security extras *(low priority, not needed for v1)*

**Goal:** conveniences and hardening asked for. Built after
the rest of Phase B, in this order, when nothing more important is waiting.

**How:**
- Pausing never stops receiving or the live view, only writing; a paused
  span is recorded in the episode, so its counts still explain every
  message.
- Storage warnings build on the pre-flight check (M5-05): the same free
  space measurement, watched during the session.
- TLS uses `rustls` with certificates and keys referenced like other
  secrets (never stored in config); loopback-only setups keep working
  without any of it.

| PR | Delivers | Tests | You check |
|---|---|---|---|
| M19-01 `feat(capture): pause and resume episodes` | `pause` and `resume` in the control protocol, API, CLI and UI; while paused, messages are received and shown live but not written; each paused span in `episode.json` (new optional field) with the messages received during it counted separately, never as loss | Counts during and around a pause explain every message; schema lock and snapshot; pausing twice or resuming while not paused refused with a cause code | How a paused span looks in `episode.json` and in the UI |
| M19-02 `feat(server): storage warnings` | Free space of the data directory watched while running: a warning in `/api/status` and the UI below a threshold, with the minutes left at the current data rate; starting an episode refused below a minimum, with a cause code; thresholds as settings | Fake filesystem statistics: thresholds crossed in both directions; start refused below the minimum | Default thresholds and what the operator is told to do |
| M19-03 `feat(server): https for the ui and api` | TLS for the API and UI with a configured certificate and key, or a self-signed one generated on first start for a local network; the viewer page and its data port follow; HTTPS required before binding beyond loopback when enabled | TLS handshake tests; plain HTTP refused when TLS is required; certificate reload on change | How a certificate is provided in a container |
| M19-04 `feat(server): client certificates` | Mutual TLS for the API: clients (browser, Python client, other programs) present a certificate signed by a configured authority; works alongside token authentication (M9-11) | Unknown, expired and revoked certificates refused with cause codes; a valid one accepted | Which clients need certificates, and how they get them |
| M19-05 `feat(transport): authenticated tcp inputs` | TLS for TCP inputs, optionally mutual so only robots with a certificate from the configured authority can send; UDP stays with interface binding and the source allow-list (SEC-2), documented as its limit | Handshake and refusal tests against the TCP conformance suite; a robot without a valid certificate is refused and counted | The trade-off for UDP, where per-message authentication isn't standard |

#### v1 release checklist
Worked through in the description of the PR that tags v1, as for the MVP.

- [ ] Every P1 requirement covered by a merged PR whose tests pass
- [ ] Quick-start works as written for a new user with their own URDF
- [ ] One unattended run from capture to verified upload of only the kept
      ranges
- [ ] Profiling results with background-job costs against the budget
- [ ] Output schemas frozen; upgrade from the MVP release keeps all data and
      settings
- [ ] `docs/concepts/architecture.md` matches what is shipped
- [ ] The dependency updates Dependabot opened are each taken, with
      `cargo xtask ci` and `cargo xtask api-diff` green, or declined with
      the reason; a major version (such as `utoipa`'s) has the OpenAPI
      document and the generated TypeScript types regenerated and compared

---

## Part 6. Phase C and beyond

P2 items (REQUIREMENTS §9.4) are ordered by user demand. Most are new
implementations of existing extension points (§2.6):

| Item | Extension point |
|---|---|
| Serial, CAN, shm, WebSocket, gRPC, OPC UA (TRN-9) | `Transport` |
| ROS 1, PX4 ULog, `.rrd`, LeRobot import (TRN-12) | `Importer` |
| LeRobot, HDF5, Parquet, `.rrd` export (STO-6, PY-8) | `Exporter` |
| Several destinations, routing, object tags (UPL-10, UPL-11); SFTP, Google Drive | `Destination` + engine routing |
| Learned scorers (REL-12) | `Scorer` |
| Drift across sessions (QA-6) | `Check` on STO-7 history |
| **Native macOS and Windows** (required by §2.2, deferred while Docker covers them). What it takes: the local connections (capture's control socket, serve's job socket, the live tap's datagrams) behind one abstraction, e.g. loopback TCP with a shared secret or named pipes (`interprocess`), since Windows has no Unix datagram sockets; the UDP receiver's `poll` fallback (or IOCP/kqueue); priorities (Windows priority classes, macOS `setpriority`), the bridge and worker ending with serve (Windows job objects, macOS polling as now); `/proc` readings (process stats, pre-flight); CI building and testing on macOS and Windows runners. Each local connection kind must keep its guarantees: capture never waits, the tap stays lossy | Platform layer, not an extension point |
| Larger learned anomaly models, beyond the autoencoder of M10-13 (e.g. sequence models, GPU), possibly on `burn` (pure Rust, CPU and GPU backends), deferred until the in-repo models are measured too small | `AnomalyDetector` |

---

## Part 7. What comes next

The work packages above hold only what is left; what is built is described
in [`docs/concepts/architecture.md`](../concepts/architecture.md), the
design documents in [`docs/design/`](../design/README.md) and the
reference.

**Critical path**, taken in turn, one PR at a time:

1. The cameras: M6b-04b (capture's CPU with ROS 2 cameras, not reproduced
   in later runs; measured on the reference PC).
2. Zenoh made solid with cameras: its cost brought to DDS's (M6-05d).

The other work packages wait until then.

**Settled approaches** the work above builds on:

1. **Payloads stored exactly as received** (STO-1, §2.5); struct layouts
   raw with a `kapto.struct/v1` schema.
2. **UI: Svelte + TypeScript, with `kapto-wasm` for logic shared with the
   server** (`kapto-wasm` arrives in M9-02b).
3. **Benchmarks on the maintainer's machine**, provisional and
   fingerprinted, until the reference PC exists (§3.6).
4. **RustFS** is the S3 target for the demo, integration tests and upload
   benchmarks.
5. **Upload behaviour is configuration** (UPL-12).

**Open questions** not yet tied to a row:

1. **Upload defaults.** Growing to `upload.max_in_flight = 32` overshoots
   what a store takes well (on RustFS, 197 MiB/s against 297 at a fixed 4
   and 322 at 8 parts at once, 16 MiB parts). Proposed: `max_in_flight` 8
   by default, or the window growing only while throughput rises, or both;
   measured again against a remote store first.
2. **Files in parallel.** `upload.parallel_files` is documented as not yet
   used. Taken up if episodes of many files upload slower than the link;
   it needs request slots shared across files, progress shared between
   them, and the pause made a check that doesn't stall files in flight.
3. **`Retry-After`.** A throttled request waits Kapto's own backoff; waiting
   as the store says needs `object_store` to expose the header or status.
4. **Pacing inside a request.** The bandwidth cap waits between parts;
   smoothing within a part needs an HTTP client of Kapto's own, taken up
   only if one-second bursts are a problem somewhere.
5. **The writer's chunk with camera frames.** A 1 MiB frame fills a 1 MiB
   chunk, so the writer's per-chunk allocations come with each frame;
   proposed, chunks stay 1 MiB, as the writer takes 0.16 cores with two
   1080p cameras at 60 fps.
6. **A robot's own ROS 2 message types.** Kapto loads a type's support
   library from the image's installation; proposed, the robot's built
   interfaces mounted into the image and named in `AMENT_PREFIX_PATH` and
   `LD_LIBRARY_PATH`, with a test over a message package built for it.
7. **Fixed transforms in the ROS 2 demo.** Whether the synthetic robot also
   publishes the sample arm's fixed transforms (a camera's mount on
   `/tf_static`), so the ROS 2 demo shows a kept stream.
8. **`kapto topics` through Kapto's own node**, leaving `r2r_rcl` as the
   only ROS 2 dependency.
9. **Environments around the robot**, once they have been used in the
   viewer: kept with the configuration, chosen per session, checked like a
   package and carried in the recording.
10. **Jitter in the stream health.** Measuring the spread of arrival times
    needs every message's receive time, which only capture's threads see:
    a running spread per stream in the writer, or none until there is a
    measured need.
11. **Controls for a finger.** Controls are 1.75rem (26 px) tall, above
    WCAG's 24 px but under the 44 px a finger needs on a tablet beside the
    robot. Proposed: `--control-height` at 44 px under `@media (pointer:
    coarse)`, desktop unchanged; tried on a real tablet first, as the
    headless browser doesn't report a coarse pointer reliably.
