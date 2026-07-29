# RustyNesting

Help me paye for tokens
https://ko-fi.com/glorfindel31

A from-scratch Rust + Tauri rewrite of [Deepnest](https://deepnest.net/), the
open-source nesting tool for laser/CNC/waterjet cutting. Import DXF parts and
stock sheets, let a genetic-algorithm engine pack the parts onto the sheets
with as little wasted material as possible, export the result back to DXF.

The original Deepnest is an Electron app that coordinates its parallel
nesting workers across separate `BrowserWindow` processes over IPC, since
Electron has no shared memory between them. This rewrite replaces that with
real shared-memory threading (Rust + [rayon](https://github.com/rayon-rs/rayon)),
eliminating an entire class of process-coordination bugs by construction
rather than patching around them.

## Status

Actively being ported/rewritten. Geometry core, the NFP (no-fit-polygon)
engine, the placement/GA/concurrency model, sheet consolidation and
repacking, and a real Tauri UI are all in place and covered by unit tests.
See [`docs/PORT_STATUS.md`](docs/PORT_STATUS.md) for the living, detailed
breakdown of what's ported, what's deliberately not ported, and what's still
outstanding — check it before assuming something is or isn't done.

## Features

- **DXF import/export**, layers preserved end to end (cut/etch/drill stay
  distinguishable through the whole nest → export round trip)
- **Multiple placement strategies** — Tight Fit (contact-based, the
  recommended default for irregular/interlocking shapes), Gravity, Box,
  Convex Hull, and two Gravity/Tight-Fit hybrids — picked per job, not
  hardcoded
- **Genetic-algorithm search** with escalating runs: a cheap first pass, then
  progressively wider rotation grids and larger populations, so you don't
  need to understand rotations/population/generations for it to work well
- **Sheet repacking** — an already-nested sheet can be manually re-arranged
  in place (or automatically, below a configurable utilisation threshold)
  without touching any other sheet
- **Independent margin and spacing** — sheet-edge clearance and inter-part
  clearance are configured separately, each down to `0`
- **Bilingual UI** (English / Vietnamese), configurable accent color and
  text size, all live-switchable from the app itself
- Dark, brutalist, no-framework frontend — plain HTML/CSS/JS, no build step

## Getting started

Requires a recent stable Rust toolchain ([rustup.rs](https://rustup.rs)).

```sh
cargo build                    # whole workspace (geometry, nesting, src-tauri)
cargo run -p deepnest-tauri    # launch the app
```

There's no frontend bundler or dev server — `frontend/dist/` is plain
HTML/CSS/JS, embedded into the binary at compile time. **Editing anything
under `frontend/dist/` requires re-running `cargo build`/`cargo run` to pick
it up**, since Cargo only reruns `src-tauri/build.rs` (which does the
embedding) when it sees that instruction — this is already wired via
`cargo:rerun-if-changed`, but only takes effect on the next build.

```sh
cargo test -p geometry         # geometry unit tests
cargo test -p nesting          # nesting unit tests
cargo test --workspace         # everything
```

## Architecture

```
crates/
  geometry/     pure geometry math, zero I/O, zero threading
                (NFP, Clipper2 boolean ops, DXF import, polygon simplification)
  nesting/      NfpCache, GA, rayon-based per-generation dispatch,
                placement engine, consolidation, repacking
src-tauri/      Tauri v2 shell + IPC commands, DTO/serialization boundary
frontend/dist/  the UI actually served - index.html/app.js/app.css/i18n.js/
                prefs.js/render.js, no framework, no bundler
docs/           PORT_STATUS.md - the living tracking doc
```

`geometry` and `nesting` are plain library crates with no Tauri/UI
dependency, so the entire engine is unit-testable and reusable outside the
desktop shell. 

## Reference

- [`RUST-REWRITE-PLAN.md`](RUST-REWRITE-PLAN.md) — the original master plan
  for this rewrite: scope, phases, and the decisions already made (Rust +
  Tauri, no GPU, Clipper2 for boolean ops, rayon for concurrency)
- [`docs/PORT_STATUS.md`](docs/PORT_STATUS.md) — phase-by-phase status
  this repo; also doubles as detailed architecture documentation for humans

## License

[MIT](LICENSE)
