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

### Nest quality

The engine is measured against a commercial nester (SuperNesting) on six
jobs, re-runnable with `sh bench.sh`. As of v2.4.0 it matches it on every
one — same sheet count on all six, including the four-part 800-piece mixed
job that had been one sheet behind:

| job | commercial | this |
|---|---|---|
| single part x250, four different shapes | 11 / 5 / 5 / 63 | 11 / 5 / 5 / 63 |
| four shapes mixed, 800 pieces | 31 | 31 |
| interlocking triangles x50 | 14 | 14 |

Sheet count is a coarse measure, so the harness also reports parts on the
best single sheet, per-sheet utilisation spread, and a pass/fail audit.

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
- **Two structurally different packers, run against each other** — a
  contact-driven greedy pass (good at irregular, interlocking shapes) and a
  shelf/band packer (good at rectangle-ish ones, and able to give each band
  its own orientation, which the greedy pass cannot represent). Whichever put
  more material on a sheet keeps it
- **Genetic-algorithm search** over part order and rotation, with optional
  escalating runs. The defaults are set where the search actually pays:
  raising rotations changes results, raising runs/population/generations
  mostly costs time, and the app shows an estimated cost beside RUN NEST so
  the price of a setting is visible before the wait rather than after it
- **Orientation-independent import** — a part is turned so its
  minimum-area bounding box is square to the axes, so a drawing saved on the
  diagonal nests exactly as well as the same part saved straight
- **Manufacturability audit** on every result — overlapping or off-sheet
  parts are fatal, clearance shortfalls advisory; recomputed at export time
  from the geometry actually being written, so it cannot certify a stale
  layout
- **Offcut/remnant tracking** — usable remnants come back as stock sheets
  and are marked consumed once nested onto, with a persistent parts library
- **PDF job report** — part list, sheet list (identical layouts collapsed
  into a duplicate count) and remnant info
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

There is also a headless CLI, which runs the same code the window does and
prints a diffable JSON summary — this is the nest-quality regression harness,
not a test:

```sh
cargo run --release --bin nest -- tests/fixtures/two.dxf --qty 50     --sheet 2440x1220 --spacing 6 --json
sh bench.sh                    # the whole board against the commercial targets
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
