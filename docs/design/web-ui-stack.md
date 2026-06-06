# Svelte and TypeScript for the web UI, built in Docker, embedded in the binary

## Context

Operators use Kapto from a browser: they watch the robot and its streams,
start and stop recordings, review episodes, see where uploads are, and
replay them. The UI talks to the local API
([API reference](../reference/api.md)) and draws the robot with its own
viewer ([viewer](a-viewer-of-our-own.md)).

Kapto ships as one binary and promises that Docker is the only thing needed
to build it. Contributors to the engine shouldn't need Node.js, and a UI
that disagrees with the API it was built against must say so rather than
fail in odd ways.

## Approach

- **Svelte 5 and TypeScript, built with Vite.** Svelte compiles components
  to small plain JavaScript without a runtime framework, which suits a
  dashboard that mostly shows live numbers. TypeScript is strict.
- **API types are generated, never written by hand.** `cargo xtask gen`
  writes `ui/src/lib/api-types.ts` from the API's OpenAPI document, with a
  small generator in the repository, so the engine's checks need no Node.js
  and CI fails when the types are stale.
- **Node.js runs in Docker only.** `cargo xtask ui` builds, checks and tests
  the UI in a pinned Node image, and the image build has a UI stage. Exact
  versions of every UI dependency are pinned, with a lockfile.
- **The built UI is embedded in the `kapto` binary** and served by
  `kapto serve` on the API's origin, under a Content-Security-Policy that
  allows nothing but that origin and the viewer. A binary built without the
  UI serves a page saying how to build it.
- **The UI checks `/api/version` when it loads** and refuses to run against
  another API version.
- **The viewer is embedded as a frame of the viewer bridge's own page,**
  which keeps its policy separate from the UI's.
- **Logic that must match the server exactly** (validation, rule checks)
  will come from Rust compiled to WebAssembly (`kapto-wasm`) when the first
  such screen arrives; not before.

## Consequences

- A contributor working on the engine never installs Node.js, and `cargo
  build` works without the UI.
- The UI is always the one built with the binary, so they can't drift apart.
- A UI change needs a Docker run to build and test (`cargo xtask ui build`,
  `cargo xtask ui test`), slower than a local Node.js setup.
- The generator handles the shapes the API uses (objects, arrays, numbers,
  strings, optional and nullable fields, references); a richer API may need
  it extended, or replaced by a Node.js tool run in Docker.

## Alternatives considered

- **React or Vue:** larger runtimes and more code for the same dashboard;
  nothing Kapto needs is missing from Svelte.
- **A Rust UI framework (Leptos, Dioxus):** one language, but a smaller
  ecosystem of components and a harder time embedding the Rerun viewer.
- **`openapi-typescript` for the types:** the usual tool, but it would bring
  Node.js into `cargo xtask gen` and the engine's CI.
- **Serving the UI from the filesystem:** easier to swap, but the UI and the
  binary could then come from different versions.
