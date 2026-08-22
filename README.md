# RustyNesting

Help me pay for tokens
https://ko-fi.com/glorfindel31

A from-scratch Rust rewrite of [Deepnest](https://deepnest.net/), the
open-source nesting tool for laser/CNC/waterjet cutting. Import DXF or SVG
parts and stock sheets, let a genetic-algorithm engine pack the parts onto the
sheets with as little wasted material as possible, export the result back to
DXF or SVG.

The original Deepnest is an Electron app that coordinates its parallel
nesting workers across separate `BrowserWindow` processes over IPC, since
Electron has no shared memory between them. This rewrite replaces that with
real shared-memory threading (Rust + [rayon](https://github.com/rayon-rs/rayon)),
eliminating an entire class of process-coordination bugs by construction
rather than patching around them.

It is a single native binary. No webview, no IPC, no HTML/CSS/JS — the UI is
Rust ([egui](https://github.com/emilk/egui)/eframe) drawing directly from the
same data the engine works on.

## Status

Actively being ported/rewritten. Geometry core, the NFP (no-fit-polygon)
engine, the placement/GA/concurrency model, sheet consolidation and
repacking, and the full native UI are all in place and covered by unit tests.
See [`docs/PORT_STATUS.md`](docs/PORT_STATUS.md) for the living, detailed
breakdown of what's ported, what's deliberately not ported, and what's still
outstanding — check it before assuming something is or isn't done.

## Features

- **DXF import/export**, layers preserved end to end (cut/etch/drill stay
  distinguishable through the whole nest → export round trip)
- **SVG import/export** as a second path, producing the same internal shape
  tree DXF does — metric only (`mm`/`cm`/`m`/`px`; imperial units are a hard
  error, not a silent conversion)
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

## Getting started

Requires a recent stable Rust toolchain ([rustup.rs](https://rustup.rs)).

```sh
cargo build                    # whole workspace (geometry, nesting, app)
cargo run -p rustynesting      # launch the app
```

No bundler, no dev server, no `tauri-cli`, no asset embedding step — `cargo`
is the whole build. `build.rs` only stamps the Windows exe icon.

```sh
cargo test -p geometry         # geometry unit tests
cargo test -p nesting          # nesting unit tests
cargo test -p rustynesting     # engine entry points + UI
cargo test --workspace         # everything
```

## Architecture

```
crates/
  geometry/     pure geometry math, zero I/O, zero threading
                (NFP, Clipper2 boolean ops, DXF/SVG import+export,
                 polygon simplification, clearance)
  nesting/      NfpCache, GA, rayon-based per-generation dispatch,
                placement engine, consolidation
app/            the binary: engine entry points (commands.rs), the
                DTO/persistence boundary (dto.rs), the worker thread
                (worker.rs), and the whole egui UI (ui/)
docs/           PORT_STATUS.md - the living tracking doc
```

`geometry` and `nesting` are plain library crates with no UI dependency, so
the entire engine is unit-testable and reusable outside the desktop app.

The one architectural rule in `app/`: nothing under `ui/` calls
`commands::*` directly. Every backend call goes through `worker.rs` onto a
background thread, because the UI update loop runs on the thread pumping the
window's event loop — a synchronous import or nest run on it freezes the
window solid for its whole duration.

## Reference

- [`docs/PORT_STATUS.md`](docs/PORT_STATUS.md) — phase-by-phase status for
  this repo; also doubles as detailed architecture documentation for humans

## License

[MIT](LICENSE)
