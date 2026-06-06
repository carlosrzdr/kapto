# Uploads: one engine in the worker, storage through `object_store`

## Context

A robot records its episodes on its own disk. They have to get to the
storage of whoever runs the robot: a bucket in the cloud, a NAS on site that
speaks S3, later an SFTP server or a shared drive. The uploads must hold to
what the rest of Kapto holds to:

- **Recording comes first.** An upload must never take the CPU, disk or
  network a recording needs, nor wait on capture, nor make capture wait on it
  ([process and thread model](process-and-thread-model.md)).
- **The network is unreliable.** A robot's link drops for minutes or hours,
  or the machine restarts mid-upload. An upload has to resume, not start
  over, and must never leave something in the bucket that looks finished
  when it isn't, as `_FINALISED` does locally.
- **Consumers of the bucket need one completeness rule** that doesn't
  depend on Kapto: a pipeline that reads the bucket should know an episode
  is complete without asking the robot.
- **Nothing is uploaded twice and nothing is overwritten.** The same episode
  uploaded again, after a restart or with `kapto.db` lost, must be a no-op.
- **Behaviour is configuration.** When an episode is uploaded, retries,
  backoff, timeouts, part sizes, parallelism, bandwidth, how an upload is
  verified and whether local copies are deleted all come from `kapto.toml`,
  with documented defaults.
- **No secrets in files.** Credentials are referenced, never written in
  configuration, recordings, the database or logs.
- **Destinations differ.** S3 has multipart uploads, server-side checksums
  and put-if-absent; SFTP has byte offsets and no checksums; Google Drive
  has resumable sessions and its own IDs. One engine has to work over all
  of them without assuming any feature exists.
- **Uploads should use the link fully.** A robot uploads hours of
  recording through whatever link it has, often only at night or between
  shifts. When nothing else needs the link, an upload should keep it full,
  not wait on one request at a time, and still not be throttled or refused
  by the provider: S3 answers too many requests with `503 SlowDown`, GCS
  and Azure with `429` and `503`, and each provider limits the size and
  number of a file's parts.
- **A robot's disk is small.** Episodes have to leave it once they are safe
  elsewhere, and only then.

Storage libraries considered:

| | `object_store` | OpenDAL | AWS SDK for Rust | rclone |
|---|---|---|---|---|
| What | Apache Arrow's object storage crate | Apache's data access layer | Amazon's own SDK | A separate program, in Go |
| Covers | S3 and compatible, GCS, Azure, local files, HTTP | Over 40 services, SFTP and Google Drive among them | S3 only (one crate per service) | Over 70 services |
| Multipart resumed after a restart | Yes: `MultipartStore` exposes the upload's ID and each part's | Its writer doesn't expose them | Yes, by hand | Its own state |
| Checksums | SHA-256 sent and checked by S3 | Per service | Yes | Per service |
| Licence | Apache-2.0 or MIT | Apache-2.0 | Apache-2.0 | MIT |
| Weight | `reqwest` and `rustls`, already near Kapto's tree | Large, feature per service | Large | A second binary to ship and drive |

## Approach

**One upload engine in `kapto-upload`, talking to storage through
`object_store`'s own interface.** Putting bytes into storage is what
`object_store` does, maintained by the Apache Arrow project, and its
interface already covers what the engine needs; a trait of Kapto's in
front of it would be code of ours repeating it. The engine uses
`object_store`'s `ObjectStore` and `MultipartStore` directly.

The engine owns what no storage library does, because it is about
episodes, not objects: which episodes are uploaded and when, the state of
each episode's upload, the bandwidth cap and how many requests are in
flight, retrying a file `object_store` gave up on, verification, the
completeness marker, deleting local copies, progress, and errors with
their cause codes. `object_store` owns the protocol: signing, each request
and the parts of a multipart upload. Its own retries are turned off, and
the engine tries each request again itself, with the backoff and attempts
`[upload]` sets. A request then counts once however it failed, the engine
alone decides what a throttled answer means for the requests in flight,
and a retry `object_store` would have hidden shows in the upload's
progress.

- **What the engine needs of a store.** Objects put whole and put only if
  absent (`PutMode::Create`), for the marker; listed under a prefix; read
  back by range; and large files put in parts through `MultipartStore`,
  whose upload and part IDs the engine saves in `kapto.db`, so a file
  interrupted by a restart carries on from its last part. A store without
  `MultipartStore` (a local folder) puts each file whole, sent again from
  the start if interrupted. S3 is sent each part's SHA-256 and checks it
  itself; where a store doesn't, the engine reads back or compares sizes,
  as configured.
- **The stores.** `object_store` covers S3 and S3-compatible storage, GCS,
  Azure, a local folder and memory (for tests). For SFTP, Google Drive and
  the other services Apache OpenDAL covers, `object_store_opendal` (part
  of OpenDAL) presents an OpenDAL backend as an `ObjectStore`; a storage
  neither covers is one `ObjectStore` implementation.
- **Tested the same way for every store.** A conformance suite checks that
  a store does what the engine needs, run against `object_store`'s
  in-memory store wrapped to fail as the tests say, and against each real
  kind of store the engine is configured with.
- **RustFS is the S3 target for the demo and the tests**: Apache-2.0,
  written in Rust, run from a pinned image. Every destination kind is tested
  against a real server of its kind; AWS S3 itself is a configuration and
  one more conformance run, done when it is needed.

**The engine runs in the worker, on tokio.** Uploads are mostly idle
connections, the one other place besides serve where async fits. The
worker runs them on a small runtime on threads of their own (`wrk-upload`),
at the worker's low priority, beside its CPU jobs: a long upload doesn't
hold up the checks of the next episode, and a check doesn't hold up an
upload. Uploads pause while an episode is recorded, by default.

**Uploads keep the link full, and back off before the provider refuses
them.** Throughput comes from concurrency, not from a single request:

- **Parts in parallel, files in parallel.** A file goes up as parts sent
  side by side, several files at once, each part read from disk as it is
  sent, so memory stays bounded at part size × parts in flight. The part
  size grows for large files so a file never needs more parts than its
  destination allows, and never falls below the smallest part it accepts.
- **Concurrency adapts.** The engine starts with the configured number of
  requests in flight and raises it while throughput keeps rising, up to the
  configured most. When the provider signals throttling (`503 SlowDown`,
  `429`, its `Retry-After`), it halves what is in flight and waits as told,
  then grows again slowly: the way TCP shares a link, applied to requests.
  A throttled request is retried without counting as a failed attempt; a
  provider that keeps throttling shows as such in the upload's status, not
  as an error. With its retries off, `object_store` 0.14 gives a throttled answer only as its generic error: the status is
  held in a type it keeps private, and the headers, `Retry-After` among
  them, aren't kept at all. The engine tells throttling from the error's
  text, a `429` or `503` status or S3's `SlowDown`, and waits Kapto's own
  backoff rather than as told. Tests point `object_store`'s own S3, GCS
  and Azure clients at a stand-in server answering as each provider
  throttles, so a change in `object_store`'s wording fails a test rather
  than throttling going unnoticed. Waiting as
  `Retry-After` says comes back once `object_store` exposes it.
- **A bandwidth cap**, optional and off by default, with a token bucket of
  Kapto's own, for links shared with the robot's own traffic, optionally
  only at certain hours.
- **The defaults are a starting point.** Part size, parallel parts and
  files, the most requests in flight and how throttling is answered are all
  in `[upload]`, overridable per destination, so a site with a fast link
  and its own S3 can go further, and a robot on a mobile link can stay
  gentle. A benchmark against RustFS reports throughput across part sizes
  and parallelism, and capture's loss while uploading, which must stay
  zero.

**State lives in `kapto.db`, written by serve.** Serve stays the only
writer: the worker takes upload jobs from serve's job socket, as it takes
the others, and reports each step back. Per episode and destination,
`kapto.db` holds the upload's state (`queued → uploading → verifying →
verified`, or `failed` with its cause), the destination's name, provider
and bucket, and the episode's key as it was written; per object, its key,
size, SHA-256, the provider's version or ETag, the resume handle while in
flight, and when it was verified. A key is never recomputed from today's
settings.

**The bucket holds the record that survives.** An episode's key is made
from a path template and a part of its manifest's hash, so the same episode
always lands on the same key. Its files go up first; each is verified; then
a `_SUCCESS` object is written **last**, put only if absent, listing every
file with its size and SHA-256. It is the bucket's completeness rule, on by
default; its name and content are configurable, and turning it off is
allowed but warned about. Its format is a spec in `docs/reference/` with a
JSON Schema, additive only. An episode whose `_SUCCESS` already matches is
skipped, never overwritten. Listing the markers is enough to rebuild the
records if `kapto.db` is lost.

**Every behaviour comes from `[upload]` in `kapto.toml`**: defaults, then
each destination's overrides, resolved into one `UploadPolicy` captured
when an upload starts, so a change applies to uploads that start after it.

- `when`: `"never"` (only by hand), `"finalised"` (every finished episode,
  the default, so nothing is left behind unnoticed), `"finalised_clean"`
  (finished with nothing lost, dropped or recovered and pre-flight passed)
  or `"accepted"` (no quality gate failed). Any episode can still be
  uploaded by hand.
- `delete_local`: `"when_disk_low"` (the default: when free space falls
  below `capture.disk_warn_below`, the level the UI warns at and the
  replay cache is emptied at, the oldest episodes already verified go
  first, and deleting clears the warning),
  `"never"`, or
  `"after_upload"`. An episode is deleted locally only once verified on the
  configured number of destinations (`min_verified_destinations`, one by
  default), and only as a whole; its entry in the index stays, saying where
  it is.
- Retries, backoff, timeouts, part size, parallel parts and files, the
  most requests in flight and how throttling is answered, the bandwidth
  cap, verification (`server-checksum`, `read-back`, `size-only`)
  and the marker: each with its default in one `Default` implementation,
  which is also the source of the configuration reference.

**Credentials by reference only.** A destination names where its
credentials come from: environment variables, a mounted credentials or
secrets file, or the machine's role where the provider has one. Validation
refuses a value that looks like a secret written inline; `config show`, the
API and the logs redact what is referenced. A "test connection" writes a
small probe object and deletes it.

## Consequences

- A robot's episodes reach the user's storage with nothing to run beside
  Kapto, resume across network cuts and restarts, and arrive with a marker
  any consumer can trust. Adding a kind of storage is configuring one
  `object_store` or OpenDAL backend, and its conformance run.
- **`object_store`'s types appear in the engine's interface**, an
  exception to the rule that a library's types stay out of Kapto's traits:
  here the library's interface is the one wanted. Moving off `object_store`
  would mean rewriting the engine's storage calls, a cost accepted for not
  maintaining a layer of our own.
- **The worker has tokio and `object_store`'s tree** (`reqwest`,
  `rustls`, the cloud providers' signing). The capture-side crates still
  never depend on tokio. The slim image grows by that much.
- **Local deletion is the retention step's first case.** Raw data stays
  immutable until an episode is verified elsewhere; then the worker may
  delete its folder whole, as configured. Deleting parts of an episode
  waits for relevance selection's retention plans.
- An episode deleted locally can't be replayed or checked again on the
  robot until it is fetched back; the UI says where it is.
- `kapto.db` has upload tables, which serve writes from the worker's
  reports; the job protocol has the messages for them.
- The `_SUCCESS` format is a published, additive-only format, like
  `episode.json` and the manifest.
- **Full speed has a cost on the robot**: parallel parts read the disk
  and use memory while they are in flight, which is why uploads pause
  while recording by default and run at the worker's low priority. The
  defaults are chosen from the benchmark, provisional until the reference
  machine exists, and tuned per site later.
- **Throttling is recognised by text**, as `object_store` words its
  errors: upgrading `object_store` means checking the tests that pin those
  words still pass, and a provider whose throttling reads differently is
  answered as an unreachable store, with tries counted, until its words are
  added.
- Verifying by reading back doubles the traffic, so it is used only where
  the destination can't check a checksum itself, or when configured.
- Testing needs a RustFS container: the conformance run, a network cut
  in the middle of an upload, and a benchmark of capture while uploading.

## Alternatives considered

- **A `Destination` trait of Kapto's own, `object_store` behind it.** It
  would let SFTP's byte offsets and Drive's sessions fit as they are, and keep library types out of the engine, at the cost of a
  layer of our own repeating `object_store`'s, with its own in-memory
  store and tests. With `object_store_opendal` covering SFTP and Drive
  through `object_store`'s interface, the layer buys nothing.
- **OpenDAL as the interface.** More services at once, SFTP and Drive
  among them, with retries, concurrency limits and throttling as layers,
  but it doesn't expose S3's multipart IDs to resume across a restart,
  which matters most on a robot's unreliable link. It comes in behind
  `object_store`'s interface instead, for the services `object_store`
  doesn't cover.
- **The AWS SDK.** Complete for S3, but S3 only, and the largest tree.
- **rclone, run as a program.** Covers everything, but it is a second
  binary to ship and drive, its retries, state and verification are its
  own, and Kapto couldn't account for an upload's progress or gate local
  deletion on it with the same certainty.
- **Uploads in serve**, which already runs tokio. Serve must stay light and
  always answer; a stuck or crashing upload would take the API and the UI
  with it. The worker is where slow work goes, and serve restarts it.
- **A fifth process for uploads.** Isolation the worker already gives, at
  the cost of another process to supervise and another socket.
- **State in files beside each episode instead of `kapto.db`.** Survives
  the database, but the queue, the progress and the record of where each
  episode went are runtime state, which belongs in `kapto.db`; the bucket's
  markers are what survives losing it.
