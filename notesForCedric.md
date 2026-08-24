# Notes for Cedric — SuperNesting tests to run

Everything here is a job you run on SuperNesting, whose answer is readable off
the **free report** alone. Each one is written as: what to run, what to write
down, and what our own engine gets so there is something to compare against.

**Common settings for every test below**, so the numbers stay comparable:

| setting | value |
|---|---|
| sheet | 1500 x 1500 |
| margin | 0 |
| spacing | 5 |
| nesting time | 60 s (what you always use) |

> **Why margin 0.** Every remnant in the five reports you already sent is
> exactly 1500 mm tall, which is impossible with a 5 mm edge inset — their
> margin does not mean what ours does. At margin 0 the two engines are asking
> the same question. Sheet counts did not change either way, so the earlier
> comparison still stands.

**The one thing to record everywhere:** not just the sheet count, but the
**parts on the best single sheet**. Sheet count is coarse — we tie you on four
jobs partly by rounding — and parts-per-sheet is exact.

---

## Test 1 — Concavity (most important)

Two files are in the repo at `probes/`:

- `probes/probe_solid.dxf` — a plain 280 x 150 rectangle, 42,000 mm²
- `probes/probe_bitten.dxf` — the same rectangle with `nestTest03`'s exact
  quarter-disc bite (R100 at one corner), 34,146 mm² — **19 % less material**

Run **each separately**, qty **250**.

| file | their sheets | their best-sheet parts | ours |
|---|---|---|---|
| `probe_solid.dxf` | | | 6 sheets, **48**/sheet |
| `probe_bitten.dxf` | | | 6 sheets, **49**/sheet |

**What it tells us.** The bitten part has a fifth less material, so if it does
not clearly beat the solid one on parts-per-sheet, that engine is packing
bounding boxes and ignoring concavity altogether. Note that *we* barely
exploit it either — 48 vs 49. If their bitten number jumps well past 49, there
is a real geometry gap to chase. If theirs also sits near 48, neither engine
interlocks it and their whole advantage is in how they mix parts.

---

## Test 2 — Mixing ratio (second most important)

Run **one job**: `nestTest04.dxf` x **50** + `nestTest02.dxf` x **250**, and
nothing else.

Write down, for two or three typical sheets, **how many of each part is on
it**.

- We produce roughly **3 x nestTest04 + 3 x nestTest02** per sheet → 78.7 %
- We *can* build **3 x nestTest04 + 18 x nestTest02** → **80.3 %**, but the
  engine never picks it

**What it tells us.** Their ratio is their filler rule, stated directly. This
is the single measurement most likely to explain the one sheet they beat us by
on the four-part mixed job.

---

## Test 3 — Per-sheet maxima for each part alone

Run each of the four `nestTest` parts on its own at qty **250**. Record the
best single sheet's part count.

| part | their best-sheet parts | ours |
|---|---|---|
| nestTest01 | | **23** |
| nestTest02 | | **56** |
| nestTest03 | | **52** |
| nestTest04 | | **4** |

**What it tells us.** If all four match, their advantage is purely in mixing
and we should stop looking at geometry. If any is higher, that one part is a
concrete target.

Two we already believe we know:

- **nestTest04 = 4.** Your report gives 63 sheets for 250 parts = 3.97/sheet,
  so you get 4 as well. Five genuinely do not fit — we checked at 20
  generations x population 20 x 16 rotations. We are tied at the real ceiling.
- **nestTest03** is the one to watch. We get 52. The pair geometry says 54
  should be possible (9 across x 3 bands). If they get 54, that is worth
  most of the sheet we are missing.

---

## Test 4 — The Duplicate column (no new job needed)

On the **five reports you already have**, look at the SHEET LIST and count how
many sheets are flagged as a duplicate of another.

**What it tells us.** If most sheets are duplicates, they nest *one* sheet and
stamp it N times (pattern replication). We built that, measured it, and
deleted it because it bought nothing — but if that is genuinely how they work,
their advantage is structural and worth re-examining. If every sheet is
unique, they nest each sheet independently and the difference is in the tail.

---

## Test 5 — Mirroring

Run `nestTest04.dxf` at qty **250** and look at the sheet preview: **are any
parts flipped** (mirror image), as opposed to merely rotated?

**What it tells us.** We have mirroring in the engine but it is not switched
on. `nestTest04` is chiral with a lobe on one side, so a flipped copy may
interlock where no rotation can.

**This one needs your judgement, not mine:** mirroring is only legal if your
stock has no grain, no coating and no one-sided finish. Tell me which, and
whether it should be a per-part option.

---

## Test 6 — Pre-rotated input

Open `nestTest01.dxf` in CAD, rotate the geometry by **37 degrees**, save as a
new file, and nest it at qty **250**.

**What it tells us.** Same result as the un-rotated file means they normalise
orientation or search rotation continuously. A worse result means they use a
fixed angle grid relative to the file, like we do — in which case our grid is
not the thing holding us back.

---

## Test 7 — Spacing sweep

Run `nestTest02.dxf` (a plain 120 x 300 rectangle) at qty **250**, three
times: spacing **0**, **5**, **20**.

Record parts-per-sheet each time.

**What it tells us.** A pure grid/shelf packer's counts follow
`floor(1500/(120+s)) x floor(1500/(300+s))` exactly. If theirs do, their
engine is grid-based and every bit of their advantage is scheduling rather
than geometry.

---

## Where we stand right now

| job | SuperNesting | ours |
|---|---|---|
| test01 (nestTest01 x250) | 11 | 11 ✅ |
| test02 (nestTest02 x250) | 5 | 5 ✅ |
| test03 (nestTest03 x250) | 5 | 5 ✅ |
| test04 (nestTest04 x250) | 63 | 63 ✅ |
| test05 (01/02/03 x250 + 04 x50) | 31 | **32** ❌ |

Re-run any time with `sh bench.sh`.

The one job we lose needs an **80.32 % average fill** for 31 sheets, and the
best `nestTest04`-bearing sheet we can build measures 80.3 % exactly — there is
no slack anywhere. Extra search does not help: 25 generations at population 20
(620 s, about 50x the normal run) returns the identical answer, as do all six
placement types and three different part orderings.

Tests **1**, **2** and **3** are the ones that would actually move this. If
you only run two, run **1 and 2**.
