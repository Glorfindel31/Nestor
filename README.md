# Nestor

### Free nesting for less waste.

Nestor is a free, open-source nesting tool for **laser, CNC, and waterjet cutting**.

Import your DXF or SVG parts, choose your stock sheets, and let Nestor figure out how to fit as much as possible onto them — using as little material as possible.

**No subscription. No paywall. No limit on how much material you can save.**

---

## The idea

### Nesting should remain free.

We live in a world where almost everything can become a service, a subscription, or another small payment.

Even tools whose entire purpose is to **help us waste less** can end up behind a paywall.

We think this one shouldn't be.

Nesting is useful to a manufacturing company producing thousands of parts.

It is also useful to someone with a CNC machine in their garage, a maker cutting plywood, a student learning fabrication, or someone simply trying to make one project without throwing half a sheet in the bin.

The principle is simple:

> **If software can help you use less material, making that software freely available creates less waste.**

One person saving 10% of a sheet might not seem like much.

Thousands of people doing it on thousands of projects is different.

And if the tool costs nothing to use, there is no reason not to try.

This is not just about saving money.

It is about making **material efficiency accessible to everyone**.

So Nestor is free because we believe that tools that help people waste less should be as easy to access as possible.

---

## What is nesting?

When you cut parts from a sheet of material, you usually have a lot of empty space left over.

Nesting is the process of arranging those parts as efficiently as possible:

```text
┌───────────────────────────────┐
│  ◇◇    ┌─────┐     △△△       │
│ ◇◇◇    │     │    △△△△      │
│  ◇     └─────┘      △△       │
│                               │
│  ┌──────┐   ○ ○ ○    ▱▱▱     │
│  │      │   ○ ○ ○   ▱▱▱▱    │
│  └──────┘                    │
└───────────────────────────────┘
```

Better nesting means:

* less material purchased
* less material thrown away
* fewer sheets to cut
* lower production costs
* less waste

Nestor tries to automate that process.

---

## Why Nestor?

Nestor started as a from-scratch Rust rewrite of [Deepnest](https://deepnest.net/).

The goal was not simply to make another nesting application.

It was to build a **native, open-source nesting engine** that could be improved, inspected, reused and freely distributed.

The result is a single native application written in Rust.

No webview.
No browser.
No JavaScript runtime.
No server.

Just the geometry, the nesting engine and the application.

---

## What it can do

### Import & export

* DXF import/export
* SVG import/export
* DXF layers are preserved
* Metric units supported
* Export the finished nest back to DXF or SVG

### Nesting

* Genetic-algorithm search
* Multiple placement strategies
* Tight-fit placement for irregular shapes
* Gravity and box packing
* Convex-hull placement
* Shelf/band packing
* Multiple strategies can compete against each other
* Part rotation and ordering optimisation
* Interlocking shapes

### Material management

* Multiple stock sheets
* Offcut/remnant tracking
* Persistent parts library
* Sheet consolidation
* Automatic repacking
* Independent edge clearance and part spacing

### Manufacturing safety

Every result is audited before export.

Nestor checks for things such as:

* overlapping parts
* parts outside the sheet
* insufficient clearances

The final audit is performed again from the actual geometry being exported.

---

## How good is it?

We don't want "free" to mean "good enough."

Nestor is benchmarked against a commercial nesting engine.

As of **v2.4.0**, Nestor matches the commercial target on the six benchmark jobs currently in the test suite — including an 800-piece mixed-shape job.

The benchmark can be reproduced locally:

```bash
sh bench.sh
```

The goal is simple:

> **Free software should not have to apologize for being free.**

---

## Built with Rust

Nestor is a native Rust application.

The project is split into three main parts:

```text
crates/
├── geometry/    geometry, NFP, DXF/SVG, boolean operations
├── nesting/     placement, genetic algorithm, packing
└── app/         native UI and application logic
```

The geometry and nesting engines are independent library crates, so they can be tested and reused without the UI.

The application itself uses `egui` / `eframe`, with `rayon` handling parallel computation.

---

## Getting started

You need a recent stable Rust toolchain.

Install Rust with [rustup](https://rustup.rs/).

Then:

```bash
git clone https://github.com/Glorfindel31/RustyNesting.git
cd RustyNesting

cargo build
cargo run -p rustynesting
```

Run the tests:

```bash
cargo test --workspace
```

You can also run the headless nesting engine:

```bash
cargo run --release --bin nest -- \
    tests/fixtures/two.dxf \
    --qty 50 \
    --sheet 2440x1220 \
    --spacing 6 \
    --json
```

---

## For businesses, makers, and everyone in between

Nesting is not only an industrial problem.

A factory might use it to save thousands of euros in material.

A small workshop might use it to save a few sheets of plywood.

A maker might use it to squeeze one more project out of the material they already have.

The scale changes.

The principle doesn't.

**Use what you have. Waste less.**

---

## Open source means more than free

Nestor is released under the **MIT License**.

That means you can use it, modify it, study it, build on it and share it.

If you find a bug, improve the algorithm, add a feature, or simply have an idea for making nesting better, contributions are welcome.

This project belongs to everyone who wants to make better use of material.

---

## Support the project

Nestor is free to use and will remain free.

That doesn't mean developing it costs nothing.

If Nestor saves you material, saves you time, or simply becomes a useful part of your workshop, you can help keep development going:

**[Support Nestor on Ko-fi](https://ko-fi.com/glorfindel31)**

No feature is locked behind a donation.

No material-saving capability is reserved for paying users.

---

## The manifesto

**Material is finite.**

**Waste is not inevitable.**

**Efficiency should not be a luxury.**

If a small piece of software can help someone use 10% less material, that software has value far beyond its price.

And if we can give that tool to everyone for free, the potential impact becomes much larger than any single business, workshop, or project.

So let's keep the useful things useful.

Let's build tools that help people make more with less.

Let's share them.

Let's improve them.

And let's keep nesting free.

---

### Nestor

**Free nesting. Less waste.**

[GitHub](https://github.com/Glorfindel31/RustyNesting) · [License](LICENSE) · [Support](https://ko-fi.com/glorfindel31)
