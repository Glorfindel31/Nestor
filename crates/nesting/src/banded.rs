//! Shelf/band packing: a second, structurally different way to fill a sheet.
//!
//! **Why this exists.** `place_parts` is greedy and contact-driven - it puts
//! one part down, then repeatedly adds whichever part/position touches the
//! existing cluster most. That is genuinely good at irregular interlocking
//! shapes, and it is *structurally incapable* of one thing: deciding that the
//! sheet should be cut into horizontal bands with different orientations in
//! each.
//!
//! Measured on a real 200-part job of right triangles (2440x1220 stock,
//! spacing 6):
//!
//! - one orientation, uniform grid: 3 across x 2 down = 6 pair-rectangles =
//!   12 parts = 65.6%
//! - two bands: 3 horizontal (2329.5 wide, 422.4 tall) **+** 5 vertical
//!   (2112 wide, 776.5 tall), heights summing to 1198.9 <= 1220
//!   = 8 pair-rectangles = 16 parts = **88.1%**
//!
//! The greedy engine plateaued at 76.5% across rotations 4/8/16 and 3 vs 25
//! generations - eight times the search moved it by zero, because the answer
//! is not in the space it searches. A commercial nester reached 87.9% on the
//! same job.
//!
//! **This does not replace NFP placement.** Band packing works on bounding
//! boxes, so it is bad-to-useless on shapes that interlock (it would nest the
//! hat monotile at ~50%). It is excellent on rectangle-ish parts, which is
//! precisely where the greedy pass plateaus. The caller runs both and keeps
//! whichever sheet came out better - see `place_parts`.
//!
//! **Bounding boxes, deliberately.** No NFP, no Clipper, no contact scoring:
//! a band layout is decided by rectangle arithmetic, and doing it on true
//! outlines would cost orders of magnitude more for an answer that is only
//! ever as good as the boxes anyway. The result is then handed back as
//! ordinary placements, and the caller validates them exactly like any other.

use std::collections::HashMap;

use geometry::dxf_import::rotate_layered_polygon;
use geometry::obstacle_nfp::obstacle_nfp;
use geometry::polygon::{get_polygon_bounds, polygon_area, Bounds};

use crate::placement::{NestPart, PartRules, Placement, PlacedPart};

/// One placeable unit: either a single part, or two parts paired into a
/// tighter composite. A band is filled with units, not parts.
///
/// **Pairing is what makes this module work at all.** A right triangle fills
/// exactly half its bounding box, so a box-based packer nesting them
/// individually tops out near 50% - worse than the greedy NFP pass it is meant
/// to beat, so it would simply never win a sheet. Two such triangles, one
/// turned 180 degrees, fill *one* box completely. Pairing first turns a
/// 50%-dense unit into a 100%-dense one, and only then does band packing have
/// anything to offer.
///
/// The test is general, not triangle-specific: rotate a second copy, align its
/// bounding box onto the first's, and ask whether the two share material.
/// Anything that tiles its own box in two - triangles, L-shapes,
/// parallelograms - passes; anything else stays a single unit.
#[derive(Clone, Debug, PartialEq)]
struct Unit {
    /// `(rotation, dx, dy)` per member, relative to the unit's own box corner.
    members: Vec<(f64, f64, f64)>,
    source_id: usize,
    width: f64,
    height: f64,
    /// True material area of every member combined.
    area: f64,
    /// How far along the row the *next copy of this same unit* has to sit -
    /// which is not the same as the unit's width, and that difference is
    /// worth a sheet.
    ///
    /// Two triangles paired at 180 degrees form a parallelogram. Its bounding
    /// box is 62mm wider than the lattice it tiles, because the slanted end of
    /// one copy slots into the slanted end of the next. Advancing a row by the
    /// box width throws that overhang away every time - on the reference job
    /// it is the difference between 2 units across and 3, i.e. 14 parts on the
    /// sheet against 16, 77.1% against 88.1%. Measured on the shells, so it is
    /// conservative, and only ever used between two copies of the *same* unit
    /// (see `fill_band`); anything else advances by the full width.
    step: f64,
}

impl Unit {
    fn count(&self) -> usize {
        self.members.len()
    }

    /// How much of its own bounding box this unit fills.
    fn density(&self) -> f64 {
        if self.width <= 0.0 || self.height <= 0.0 {
            return 0.0;
        }
        self.area / (self.width * self.height)
    }
}

/// Rotations tried when looking for a pairing partner. 180 first because it is
/// the one that works for any shape that is half of its own box; the rest are
/// cheap and catch shapes whose complement is a quarter turn away.
const PAIR_ANGLES: [f64; 4] = [180.0, 90.0, 270.0, 0.0];

/// Base orientations per shape. 180 maps a bounding box onto itself, so only
/// two are distinct.
const ANGLES: [f64; 2] = [0.0, 90.0];

/// Every unit worth considering for one shape at one base orientation: the
/// Pareto-optimal pair boxes, plus the unpaired shape itself.
///
/// **The pair position comes from the NFP, not from a heuristic push.** The
/// obvious approach - lay the partner's bounding box onto the first's and
/// shove it until the material clears - is wrong twice over. It fails outright
/// once parts carry clearance padding, because two copies laid box-on-box
/// overlap along the very edge they are meant to share; and when it does
/// separate them it does so along whichever direction was guessed, leaving
/// tens of millimetres of slack. Measured on the reference job that heuristic
/// produced an 847x428 pair box at 0.94 density where the true answer is about
/// 787x433 at 0.99 - and that 60mm of slack is the difference between two
/// bands fitting in the sheet height and not.
///
/// `geometry::obstacle_nfp` already answers this exactly: it is the set of
/// positions where B touches A without overlapping it. Every point on that
/// polygon is a valid, maximally-tight placement, and each gives a different
/// pair box - so this returns the whole Pareto front of those boxes rather
/// than one winner. See `pareto_front` for why picking one is wrong.
fn build_units(part: &NestPart, base_rotation: f64, available: usize, curve_tolerance: f64, rules: &PartRules) -> Vec<Unit> {
    // **Memoised, because none of this depends on the sheet.** A unit
    // catalogue is a property of the shapes alone, but `pack_sheet` is called
    // once per sheet, per individual, per generation - so a 15-sheet job at
    // population 10 over 3 generations rebuilt the identical catalogue 450
    // times, each one a fistful of Minkowski sums and, since `row_step`, a
    // bisection of Clipper intersections per unit on top.
    let key = unit_cache_key(part, base_rotation, available, curve_tolerance, rules);
    if let Some(hit) = UNIT_CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    let out = build_units_uncached(part, base_rotation, available, curve_tolerance, rules);
    if let Ok(mut cache) = UNIT_CACHE.lock() {
        // Same cap policy as `NfpCache`: stop growing, keep what is there.
        if cache.len() < MAX_UNIT_CACHE_ENTRIES {
            cache.insert(key, out.clone());
        }
    }
    out
}

/// Identifies a shape by what `build_units` actually reads off it. `source_id`
/// alone would be wrong - ids restart with every job, and a cache that lives
/// as long as the process would then hand a new job the previous one's
/// geometry - so the outline's own fingerprint is part of the key.
type UnitCacheKey = (usize, u64, bool, u64, usize, u64, u64, u64);

fn unit_cache_key(part: &NestPart, base_rotation: f64, available: usize, curve_tolerance: f64, rules: &PartRules) -> UnitCacheKey {
    let first = part.polygon.points.first().copied().unwrap_or(geometry::point::Point::new(0.0, 0.0));
    (
        part.source_id,
        (base_rotation + part.rotation).to_bits(),
        available >= 2,
        curve_tolerance.to_bits(),
        part.polygon.points.len(),
        polygon_area(&part.polygon.points).to_bits(),
        (first.x * 1e6 + first.y).to_bits(),
        // The allowed-angle set is part of what the catalogue depends on: the
        // same shape constrained to 0/180 has a different unit list from the
        // same shape free to turn, and this cache outlives a job.
        allowed_angles(rules, part.id).map_or(0, |angles| {
            angles.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, a| (h ^ a.to_bits()).wrapping_mul(0x100_0000_01b3))
        }),
    )
}

/// This part's allowed angles, or `None` when it is free to turn however it
/// likes. Mirrors `placement::rotation_steps`' own lookup, including masking
/// off `MIRROR_ID_BIT` - a mirrored copy's rule is authored against the
/// un-flipped id.
fn allowed_angles(rules: &PartRules, part_id: usize) -> Option<&[f64]> {
    rules.get(&(part_id & !crate::dispatch::MIRROR_ID_BIT)).map(|r| r.angles.as_slice()).filter(|a| !a.is_empty())
}

/// Whether this part may come to rest at `angle`, absolute.
///
/// **The band packer used to ignore rotation rules entirely.** Its base
/// orientations are 0 and 90 and it pairs at 0/90/180/270, and none of that
/// consulted `PartRules` - so a grain-locked part could be handed back at an
/// angle its material does not allow, and `place_parts` accepts a banded
/// sheet wholesale whenever it holds more material. On a coated or grained
/// sheet that is a scrapped part, not a cosmetic problem. It stayed hidden
/// until `uniform_plan` made banded win the sheet in
/// `placement::tests::place_parts_never_places_a_constrained_part_off_its_allowed_angles`
/// - and then only on macOS and Linux, where the tie broke the other way.
///
/// Compared with a tolerance rather than exactly: the rule's angles arrive
/// normalised into `[0, 360)`, but the angle tested here is the sum of a
/// part's carried rotation and a catalogue angle, so it can land a float step
/// either side of the value in the rule.
fn angle_is_allowed(rules: &PartRules, part_id: usize, angle: f64) -> bool {
    let Some(angles) = allowed_angles(rules, part_id) else { return true };
    let angle = angle.rem_euclid(360.0);
    angles.iter().any(|a| {
        let d = (a.rem_euclid(360.0) - angle).abs();
        d < 1e-6 || (360.0 - d) < 1e-6
    })
}

/// A unit list is a handful of small polygons; this cap is memory insurance
/// against a pathological job, not a working limit.
const MAX_UNIT_CACHE_ENTRIES: usize = 4096;

static UNIT_CACHE: std::sync::LazyLock<std::sync::Mutex<HashMap<UnitCacheKey, Vec<Unit>>>> = std::sync::LazyLock::new(Default::default);

/// Band *plans* already searched, keyed by everything the search reads.
///
/// `search` is a depth-first walk of up to `NODE_BUDGET` nodes and it was the
/// single most expensive thing in the engine - 38s of the ~50s `pack_sheet`
/// spent per 800-part run, at ~96ms a call. It is also called once per sheet
/// per individual per generation, and the GA only permutes part *order*: the
/// pool a sheet starts from is very often one an earlier individual already
/// searched from.
///
/// Only `Plan::bands` is cached, never the placements. `materialise` re-runs
/// against the caller's own `parts`/`Pool`, so the `consumed` indices always
/// belong to this call and no index remapping is involved.
///
/// The key is what `search` can actually see: the sheet box, the anchor
/// filter, and per shape - **in first-seen order, because `build_catalogue`
/// walks `parts` and takes the first part of each `source_id`** - that
/// shape's own `unit_cache_key` plus how many copies the pool holds.
type PlanCacheKey = (u64, u64, Option<usize>, Vec<(UnitCacheKey, usize)>);

static PLAN_CACHE: std::sync::LazyLock<std::sync::Mutex<HashMap<PlanCacheKey, Vec<f64>>>> = std::sync::LazyLock::new(Default::default);

/// The shape fingerprints `build_catalogue` reads, in the order it reads
/// them - the key both caches below are built on.
fn shape_fingerprints(parts: &[NestPart], curve_tolerance: f64, rules: &PartRules) -> Vec<(UnitCacheKey, usize)> {
    let counts = Pool::new(parts);
    let mut shapes: Vec<(UnitCacheKey, usize)> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    for part in parts {
        if seen.contains(&part.source_id) {
            continue;
        }
        seen.push(part.source_id);
        let available = counts.available(part.source_id);
        // `base_rotation` 0.0: the catalogue is built at every `ANGLES` entry
        // off this same part, so the base is a constant across them and the
        // part's own carried rotation is what distinguishes one key from
        // another - which `unit_cache_key` already folds in.
        shapes.push((unit_cache_key(part, 0.0, available, curve_tolerance, rules), available));
    }
    shapes
}

fn plan_cache_key(sheet_bounds: Bounds, shapes: &[(UnitCacheKey, usize)], anchor: Option<usize>) -> PlanCacheKey {
    (sheet_bounds.width.to_bits(), sheet_bounds.height.to_bits(), anchor, shapes.to_vec())
}

/// `build_catalogue`'s own output, cached whole.
///
/// `UNIT_CACHE` already stops the Minkowski work from repeating, but the
/// catalogue still paid for a full `Vec<Unit>` clone out of that cache per
/// shape per base angle, once per `pack_sheet` - 13s of an 800-part run.
/// An `Arc` hands the same list back instead.
static CATALOGUE_CACHE: std::sync::LazyLock<std::sync::Mutex<HashMap<Vec<(UnitCacheKey, usize)>, std::sync::Arc<Vec<Unit>>>>> = std::sync::LazyLock::new(Default::default);

fn cached_catalogue(parts: &[NestPart], curve_tolerance: f64, rules: &PartRules, shapes: &[(UnitCacheKey, usize)]) -> std::sync::Arc<Vec<Unit>> {
    if let Some(hit) = CATALOGUE_CACHE.lock().ok().and_then(|c| c.get(shapes).cloned()) {
        return hit;
    }
    let built = std::sync::Arc::new(build_catalogue(parts, curve_tolerance, rules));
    if let Ok(mut cache) = CATALOGUE_CACHE.lock() {
        if cache.len() < MAX_UNIT_CACHE_ENTRIES {
            cache.insert(shapes.to_vec(), std::sync::Arc::clone(&built));
        }
    }
    built
}

/// Rewrites every member's angle from "relative to the polygon we were
/// handed" to absolute - the angle from the part's *original* outline, which
/// is what a `PlacedPart::rotation` means to everything downstream.
///
/// `part.polygon` arrives already rotated by `part.rotation` (`place_parts`
/// does that on entry), and all the box arithmetic above works in angles
/// relative to it, so the base has to be added back on the way out. Leaving
/// it relative is silently correct for any part the GA left at 0 degrees and
/// wrong for every other one, and the boxes still describe the absolute
/// geometry - so the caller places a part it has mis-rotated, off the sheet.
///
/// **A function rather than a loop at the end of `build_units_uncached`
/// because that function has two exits.** The `available < 2` one skipped it,
/// so the last remaining copy of a shape - and only that copy, and only once
/// the GA had turned it - came back relative. That is what put an 880x720 part
/// off a 1500x1500 sheet under `--placement box`.
fn into_absolute(mut units: Vec<Unit>, base: f64) -> Vec<Unit> {
    for unit in &mut units {
        for member in &mut unit.members {
            member.0 += base;
        }
    }
    units
}

fn build_units_uncached(part: &NestPart, base_rotation: f64, available: usize, curve_tolerance: f64, rules: &PartRules) -> Vec<Unit> {
    // `part.polygon` arrives already turned by `part.rotation` and
    // `into_absolute` adds that back on the way out, so the angle a rotation
    // rule is about is `part.rotation + <catalogue angle>`, not the catalogue
    // angle alone.
    if !angle_is_allowed(rules, part.id, part.rotation + base_rotation) {
        return Vec::new();
    }
    let a = rotate_layered_polygon(&part.polygon, base_rotation);
    let Some(ab) = get_polygon_bounds(&a.points) else { return Vec::new() };
    let a_area = polygon_area(&a.points).abs();
    // A unit translates rigidly, so every member's translation is
    // `target - unit_box_corner`, plus whatever extra shift that member was
    // given when the unit was built. Expressing it any other way - notably as
    // an offset from the polygon's first vertex - puts parts wherever that
    // vertex happens to be, which is how an early version of this put every
    // part off the sheet.
    let a_hull = shell_of(&a);
    let mut single =
        Unit { members: vec![(base_rotation, -ab.x, -ab.y)], source_id: part.source_id, width: ab.width, height: ab.height, area: a_area, step: ab.width };
    single.step = row_step(part, &single);
    if available < 2 {
        return into_absolute(vec![single], part.rotation);
    }

    // **Pairing searches shells, which are the true outlines up to
    // `shell_of`'s point-count bound and convex hulls above it.** Each
    // candidate angle costs one `obstacle_nfp`, and that is a Minkowski sum:
    // on `three.dxf`'s 258-point profile the eight of them take tens of
    // seconds, per sheet, per individual, per generation - it took a 100s
    // benchmark to 2728s. Hulling a big outline answers the same question for
    // a packer that only ever works in bounding boxes, and is conservative
    // rather than optimistic: two hulls that clear each other contain two
    // outlines that clear each other, so a pair it finds is always legal (and
    // `pair_is_legal` rechecks against the real outlines regardless).
    //
    // Conservative is not free, though, and that is why the bound exists: for
    // a *concave* part the material the hull fills in is the entire
    // interlocking opportunity, so hulling it away caps the packer at the
    // bounding-box answer. See `shell_of` for the job where that was worth a
    // whole sheet. Box sizes below always come from the true bounds, which
    // neither branch changes.
    let mut pairs: Vec<Unit> = Vec::new();
    for extra in PAIR_ANGLES {
        let rotation = base_rotation + extra;
        if !angle_is_allowed(rules, part.id, part.rotation + rotation) {
            continue;
        }
        let b = rotate_layered_polygon(&part.polygon, rotation);
        let Some(bb) = get_polygon_bounds(&b.points) else { continue };
        let b_hull = shell_of(&b);
        let Some(nfp) = obstacle_nfp(&a_hull, &b_hull, curve_tolerance) else { continue };

        // An NFP vertex is where B's own reference point goes, so the shift
        // that puts B there is the vertex minus that reference point - and
        // "B" here is the hull the NFP was built from, whose first vertex is
        // not the outline's. Reading the reference off the wrong polygon
        // translates every pair by the gap between them.
        let b_ref = b_hull.points.first().copied().unwrap_or(geometry::point::Point::new(0.0, 0.0));
        // Sampled *along* each NFP edge, not just at its vertices. Two
        // triangles pair by sliding along their shared hypotenuse, which is
        // one NFP edge - and the interesting boxes occur partway along it,
        // not at either end.
        //
        // **By length, not by a fixed count per edge.** A flat 32 samples put
        // 27mm between steps on the reference job's 884mm long edge, which is
        // enough to step straight over the boxes that matter - the front came
        // out with a 45mm hole in it right where the useful trade-off lives.
        // Stepping at `SAMPLE_STEP` is a few thousand iterations of pure
        // arithmetic per shape and took the fixture sheet from 12 parts to 15.
        // ...but capped in total. `SAMPLE_STEP` on a simple triangle's NFP is
        // a few thousand samples; on a complex outline whose NFP runs to
        // thousands of edges it is hundreds of thousands, per shape, per
        // orientation, on every sheet of every individual of every generation.
        // That took a 100s benchmark past 400s. The budget is spread across
        // edges by length, so a big NFP just gets sampled more coarsely.
        const SAMPLE_STEP: f64 = 0.5;
        const SAMPLE_BUDGET: f64 = 4000.0;
        let perimeter: f64 = nfp.outer.iter().enumerate().map(|(i, p)| p.distance_to(nfp.outer[(i + 1) % nfp.outer.len()])).sum();
        let step = SAMPLE_STEP.max(perimeter / SAMPLE_BUDGET);
        // The NFP boundary is where the two parts *touch*, and
        // `placement::has_material_overlap` counts any Clipper sliver above a
        // bare `0.0` as a real overlap - so a sample sitting exactly on the
        // boundary is a coin toss. Nudging outward from the NFP's own interior
        // (which is the overlapping region) by `NUDGE` separates them for
        // certain, at a cost far below any tolerance the job cares about.
        const NUDGE: f64 = 0.01;
        let n = nfp.outer.len() as f64;
        let cx = nfp.outer.iter().map(|p| p.x).sum::<f64>() / n;
        let cy = nfp.outer.iter().map(|p| p.y).sum::<f64>() / n;
        for (i, from) in nfp.outer.iter().enumerate() {
            let to = nfp.outer[(i + 1) % nfp.outer.len()];
            let samples = ((from.distance_to(to) / step).ceil() as usize).max(1);
            for k in 0..samples {
                let t = k as f64 / samples as f64;
                let (mut vx, mut vy) = (from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                let (ox, oy) = (vx - cx, vy - cy);
                let len = ox.hypot(oy);
                if len > 0.0 {
                    vx += ox / len * NUDGE;
                    vy += oy / len * NUDGE;
                }
                let (sx, sy) = (vx - b_ref.x, vy - b_ref.y);
                let (mx, my) = (bb.x + sx, bb.y + sy);
                let x0 = ab.x.min(mx);
                let y0 = ab.y.min(my);
                let width = (ab.x + ab.width).max(mx + bb.width) - x0;
                let height = (ab.y + ab.height).max(my + bb.height) - y0;
                if width <= 0.0 || height <= 0.0 {
                    continue;
                }
                pairs.push(Unit {
                    members: vec![(base_rotation, -x0, -y0), (rotation, sx - x0, sy - y0)],
                    source_id: part.source_id,
                    width,
                    height,
                    area: a_area * 2.0,
                    step: width,
                });
            }
        }
    }

    // A pair only earns its place by being denser than the shape alone. A
    // solid rectangle pairs into a box it fills exactly as well as one copy
    // does, so keeping it would only make the unit coarser - a band would have
    // to fit 200mm where 100mm would do - for no gain.
    let floor = single.density();
    pairs.retain(|u| u.density() > floor + 1e-9);
    let mut out = thin(pareto_front(pairs));
    // The nudge above makes a boundary sample safe for convex NFPs; nothing
    // guarantees it for a concave one, so every surviving pair is checked
    // against the same overlap test the caller will apply. Cheap here - the
    // front is single digits by this point - and much cheaper than shipping a
    // sheet the audit rejects.
    out.retain(|u| pair_is_legal(part, u));
    // After thinning and the legality filter, not before: `row_step` is a
    // bisection of real intersections and the front arrives here thousands of
    // samples long.
    for u in &mut out {
        u.step = row_step(part, u);
    }
    out.push(single);
    let out = into_absolute(out, part.rotation);
    if std::env::var("NEST_BANDED").is_ok_and(|v| v != "0") {
        for u in &out {
            eprintln!("    unit src {} base {base_rotation} x{} {:.1}x{:.1} density {:.3} step {:.1}", part.source_id, u.count(), u.width, u.height, u.density(), u.step);
        }
    }
    out
}

/// Keeps only the boxes no other box beats in *both* dimensions.
///
/// **Density is the wrong objective and that is what stalled this module.**
/// A pair box is scored by `fill_band` against the band it goes in, and the
/// band sequence is scored against the sheet height - so a slightly less dense
/// box that is 20mm shorter can be the one that lets a second band fit at all,
/// while the densest box wastes the leftover height entirely. Measured on the
/// reference job the densest pairing is 837.4x433.4 (0.937); sliding along the
/// same NFP edge trades width for height continuously, and only the search
/// over band sequences knows which point on that trade-off it needs.
///
/// Boxes are snapped to `GRID` first so a 1000-sample sweep along one edge
/// collapses to a handful of genuinely different options instead of feeding
/// the band search a thousand near-identical branches.
fn pareto_front(mut units: Vec<Unit>) -> Vec<Unit> {
    const GRID: f64 = 5.0;
    for u in &mut units {
        u.width = (u.width / GRID).ceil() * GRID;
        u.height = (u.height / GRID).ceil() * GRID;
    }
    units.sort_by(|a, b| a.width.total_cmp(&b.width).then(a.height.total_cmp(&b.height)));
    let mut out: Vec<Unit> = Vec::new();
    let mut best_height = f64::INFINITY;
    for u in units {
        if u.height < best_height {
            best_height = u.height;
            out.push(u);
        }
    }
    out
}

/// The polygon `obstacle_nfp` should pair on, as a hole-free `LayeredPolygon`:
/// the outline itself when it is cheap enough, otherwise a coarse shell of it -
/// simplified, then grown back by the simplification tolerance so it still
/// contains the original.
///
/// Which branch is taken is purely a cost decision; see `EXACT_AT_OR_BELOW` for
/// the measurement, and why handing a concave part a shape without its
/// concavities throws away the only thing that makes it nest.
///
/// **Simplified, not hulled, and that is the point.** The expensive branch used
/// to take the convex hull first. That is fine for a part which is nearly
/// convex anyway and catastrophic for one which is not: `nestTest04.dxf` pads
/// to 175 points, so it took this branch, and its hull measures 502,241mm²
/// against the part's own 395,372 - a fifth of the shape invented, including
/// the whole notch two copies would interlock through. Douglas-Peucker instead
/// keeps a subset of the *real* vertices, so the point count drops the same way
/// while every concavity big enough to nest into survives. It is the same
/// failure `EXACT_AT_OR_BELOW` was introduced for, one bound higher up.
///
/// Simplifying is what makes the branch affordable at all: `obstacle_nfp` is a
/// Minkowski sum, and on two ~200-point polygons it takes ~1.7s, eight times
/// per sheet - a couple of dozen points takes the whole catalogue from 13s to
/// well under a second.
///
/// **The re-offset is what keeps it honest.** Douglas-Peucker is *inscribed*
/// wherever it cuts a convex corner, which would let two parts sit up to
/// `SHELL_TOLERANCE` too close; growing it back by that much makes the shell a
/// superset again, so a pair the NFP calls legal really is. (Where it shortcuts
/// a concavity it covers material instead, which is conservative already.)
fn shell_of(poly: &geometry::dxf_import::LayeredPolygon) -> geometry::dxf_import::LayeredPolygon {
    /// Millimetres. Parts this packer helps are hundreds of mm across, so a
    /// couple of mm of slack in a pair box is under a percent of it.
    const SHELL_TOLERANCE: f64 = 2.0;
    /// At or below this many points, pair on the true outline instead of the
    /// hull - a concave part's whole interlocking opportunity is the material
    /// the hull fills in, and hulling it away costs real sheets.
    ///
    /// Measured, 1500x1500 stock, spacing 5, `nestTest03.dxf` (a rectangle
    /// with a concave bite; 87 points once padded): the hull pairs it at
    /// 160x525 / density 0.862 and packs **48** parts on a sheet - exactly the
    /// bounding-box ceiling, i.e. the bite bought nothing. On the true outline
    /// the pair is 160x485 / 0.933, three bands fit where two did, and the
    /// sheet takes **52**. Whole job: 6 sheets -> 5, matching the commercial
    /// nester, at identical run time.
    ///
    /// The bound is cost, not correctness. `obstacle_nfp` is a Minkowski sum,
    /// so it grows with the point count: at 274 points (`three.dxf`) the exact
    /// outline takes that job 51s -> 178s for a byte-identical answer, while
    /// at 110 (`one.dxf`) and 87 it is free. 128 sits in the measured gap.
    const EXACT_AT_OR_BELOW: usize = 128;
    if poly.points.len() <= EXACT_AT_OR_BELOW {
        return geometry::dxf_import::LayeredPolygon { points: poly.points.clone(), children: Vec::new(), texts: Vec::new(), real_boundary: None, ..poly.clone() };
    }
    /// Below this a shape is already cheap to pair on, and simplifying it only
    /// spends the tolerance for nothing - it cost `two.dxf`'s six-point profile
    /// a whole part per sheet. Unreachable from here today (the branch above
    /// keeps anything at or below 128 exact) and kept because the two bounds
    /// answer different questions.
    const SIMPLIFY_ABOVE: usize = 32;
    let points = if poly.points.len() > SIMPLIFY_ABOVE {
        let simplified = geometry::simplify::simplify(&poly.points, Some(SHELL_TOLERANCE), false);
        geometry::clipper::offset(&simplified, SHELL_TOLERANCE).into_iter().next().unwrap_or_else(|| poly.points.clone())
    } else {
        poly.points.clone()
    };
    geometry::dxf_import::LayeredPolygon { points, children: Vec::new(), texts: Vec::new(), real_boundary: None, ..poly.clone() }
}

/// The tightest horizontal distance at which a unit can repeat along a row
/// without any of its parts touching the copy's.
///
/// Bisected on the shells rather than solved. For a convex shell the
/// overlapping offsets form a single interval, so "all clear" is monotonic in
/// `dx` and bisection finds the true step. A shell that is a concave outline
/// (see `shell_of`) can break that monotonicity - but not the *safety* of the
/// result: `hi` starts at a width already checked clear and only ever moves to
/// another value `clear` returned true for, so whatever comes back is a
/// verified-clear step. Non-monotonicity can only make it miss a smaller one,
/// which costs density and never legality.
///
/// Falls back to the full width whenever the shells still clash at it, so a
/// shape with no useful overhang costs nothing but the bisection.
fn row_step(part: &NestPart, unit: &Unit) -> f64 {
    /// Millimetres. Ten times finer than the 5mm grid `pareto_front` snaps
    /// boxes to, so the step is never the coarse number in the layout.
    const RESOLUTION: f64 = 0.5;
    // Rebuilt here rather than carried on the `Unit`: `fill_band` clones every
    // catalogue entry on every placement, and hanging two polygons off each
    // one cost more than this whole measurement saves.
    let shells: Vec<_> = unit
        .members
        .iter()
        .map(|&(rotation, dx, dy)| {
            let shell = shell_of(&rotate_layered_polygon(&part.polygon, rotation));
            geometry::dxf_import::shift_layered_polygon(&shell, dx, dy)
        })
        .collect();
    let clear = |dx: f64| {
        shells.iter().all(|a| {
            shells.iter().all(|b| {
                let shifted = geometry::dxf_import::shift_layered_polygon(b, dx, 0.0);
                !crate::placement::has_material_overlap(a, &shifted)
            })
        })
    };
    if !clear(unit.width) {
        return unit.width;
    }
    let (mut lo, mut hi) = (0.0, unit.width);
    while hi - lo > RESOLUTION {
        let mid = (lo + hi) / 2.0;
        if clear(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Places a unit's members at the origin and asks whether they overlap.
fn pair_is_legal(part: &NestPart, unit: &Unit) -> bool {
    let placed: Vec<_> = unit
        .members
        .iter()
        .map(|&(rotation, dx, dy)| {
            let rotated = rotate_layered_polygon(&part.polygon, rotation);
            geometry::dxf_import::shift_layered_polygon(&rotated, dx, dy)
        })
        .collect();
    (0..placed.len()).all(|i| ((i + 1)..placed.len()).all(|j| !crate::placement::has_material_overlap(&placed[i], &placed[j])))
}

/// Caps a Pareto front at `MAX_UNITS`, keeping its ends and spreading the
/// rest evenly along it.
///
/// The front is a smooth trade-off curve, so its exact resolution buys very
/// little - but everything downstream is priced per unit: `pair_is_legal` runs
/// a Clipper intersection each, and `search` branches on every distinct band
/// height. A dozen options is plenty to find the layout and keeps a
/// pathological shape from making the band pass cost more than the nest.
fn thin(front: Vec<Unit>) -> Vec<Unit> {
    const MAX_UNITS: usize = 12;
    if front.len() <= MAX_UNITS {
        return front;
    }
    let last = front.len() - 1;
    (0..MAX_UNITS).map(|i| front[i * last / (MAX_UNITS - 1)].clone()).collect()
}

/// A part type plus every copy of it still unplaced.
struct Pool {
    /// Keyed by `source_id` so all quantity-copies of one shape share an entry
    /// - the band packer cares about shapes, not instances.
    by_source: HashMap<usize, Vec<usize>>,
}

impl Pool {
    fn new(parts: &[NestPart]) -> Self {
        let mut by_source: HashMap<usize, Vec<usize>> = HashMap::new();
        for (index, part) in parts.iter().enumerate() {
            by_source.entry(part.source_id).or_default().push(index);
        }
        Self { by_source }
    }

    fn take(&mut self, source_id: usize) -> Option<usize> {
        self.by_source.get_mut(&source_id)?.pop()
    }

    fn give_back(&mut self, source_id: usize, index: usize) {
        self.by_source.entry(source_id).or_default().push(index);
    }

    fn available(&self, source_id: usize) -> usize {
        self.by_source.get(&source_id).map_or(0, Vec::len)
    }
}

/// Every unit any shape can form, built once.
///
/// Deliberately computed once per sheet rather than per band iteration:
/// pairing runs a bisection of real Clipper intersections per shape and
/// orientation, and `fill_band` asks for the option list on every single
/// placement. Rebuilding it there turned a handful of calls into tens of
/// thousands.
fn build_catalogue(parts: &[NestPart], curve_tolerance: f64, rules: &PartRules) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let counts = Pool::new(parts);
    for part in parts {
        if seen.contains(&part.source_id) {
            continue;
        }
        seen.push(part.source_id);
        let available = counts.available(part.source_id);
        for angle in ANGLES {
            out.extend(build_units(part, angle, available, curve_tolerance, rules));
        }
    }
    out
}

/// The catalogue entries the pool can still supply.
fn shape_options<'a>(catalogue: &'a [Unit], pool: &Pool) -> Vec<&'a Unit> {
    catalogue.iter().filter(|u| pool.available(u.source_id) >= u.count()).collect()
}

/// The result of packing one sheet into bands.
pub struct BandedSheet {
    pub placed: Vec<PlacedPart>,
    /// Indices into the caller's `parts` that were consumed, so the caller can
    /// remove exactly those.
    pub consumed: Vec<usize>,
    /// True part area placed - what the caller compares against its own pass.
    pub area: f64,
}

/// How many recursive band-sequence nodes to explore before giving up and
/// taking the best found so far.
///
/// The search is exponential in principle (options^depth), but depth is
/// sheet height divided by the shortest band, and the option set is distinct
/// (shape, orientation) pairs - single digits in every real job. The budget
/// exists so a job of many tiny parts degrades to "good enough quickly"
/// rather than hanging.
const NODE_BUDGET: usize = 20_000;

/// Fills `sheet_bounds` with horizontal bands, searching over band sequences
/// rather than choosing each band greedily.
///
/// **The greedy version of this does not work, and that is the entire point.**
/// Scoring each band on its own density picks the densest first band every
/// time; on the reference job that is 3 horizontal pair-rectangles (422.4
/// tall, 3 across), and repeating it fills 1220 with two such bands for 6
/// rectangles. The better answer starts with the *same* band and then switches
/// orientation - 3 horizontal then 5 vertical, 8 rectangles - which no
/// per-band score can see, because the first band is identical in both and
/// the payoff is entirely in what the leftover height can then do.
///
/// `sheet_bounds` must be the *usable* area (margin/spacing already applied by
/// the caller) and `parts` the padded polygons, identical to what
/// `place_parts` works with, so the two passes compare like with like.
///
/// `anchor` restricts the catalogue to one shape's units. Left `None` the
/// packer is free to fill the sheet with whatever packs densest, which is what
/// it wants for its own sake and *not* always what the nest wants: given parts
/// of very different sizes it spends the small ones on early sheets and strands
/// the big one at the end, alone, with nothing left to fill around it. The
/// caller passes the shape at the front of its queue to say "this sheet is
/// about that part" - see `placement::place_parts`, which asks for a free
/// layout first and only re-asks anchored when the free one skips its queue.
#[must_use]
pub fn pack_sheet(sheet_bounds: Bounds, parts: &[NestPart], curve_tolerance: f64, anchor: Option<usize>, rules: &PartRules) -> Option<BandedSheet> {
    if parts.is_empty() || sheet_bounds.width <= 0.0 || sheet_bounds.height <= 0.0 {
        return None;
    }
    let shapes = crate::profile::BANDED_CATALOGUE.time(|| shape_fingerprints(parts, curve_tolerance, rules));
    let full_catalogue = crate::profile::BANDED_CATALOGUE.time(|| cached_catalogue(parts, curve_tolerance, rules, &shapes));
    // Only the anchored pass needs its own list; the common case borrows.
    let anchored: Vec<Unit>;
    let catalogue: &[Unit] = match anchor {
        Some(anchor) => {
            anchored = full_catalogue.iter().filter(|u| u.source_id == anchor).cloned().collect();
            &anchored
        }
        None => &full_catalogue,
    };
    if catalogue.is_empty() {
        return None;
    }
    // `NEST_BANDED=1` prints the unit catalogue - what pairing actually
    // produced, and how many of each fit across the sheet. Pairing failing
    // silently looks identical to pairing succeeding badly, and both show up
    // only as a disappointing sheet count.
    if std::env::var("NEST_BANDED").is_ok_and(|v| v != "0") {
        eprintln!("banded: sheet {:.1}x{:.1}", sheet_bounds.width, sheet_bounds.height);
        for u in catalogue {
            eprintln!(
                "  src {:>2} x{} {:>8.1}x{:<8.1} density {:.3}  across {}  bands {}",
                u.source_id,
                u.count(),
                u.width,
                u.height,
                u.density(),
                (sheet_bounds.width / u.width).floor(),
                (sheet_bounds.height / u.height).floor()
            );
        }
    }
    let mut pool = Pool::new(parts);
    // Seeded with the best single-height plan before the search, because the
    // search cannot be trusted to find it - see `uniform_plan`.
    let mut best = Plan::default();
    let mut heights: Vec<f64> = catalogue.iter().filter(|u| u.width <= sheet_bounds.width && u.height <= sheet_bounds.height).map(|u| u.height).collect();
    heights.sort_by(f64::total_cmp);
    heights.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    crate::profile::BANDED_UNIFORM.time(|| {
        for height in heights {
            let plan = uniform_plan(sheet_bounds, parts, catalogue, height);
            if plan.area > best.area {
                best = plan;
            }
        }
    });
    let plan_key = plan_cache_key(sheet_bounds, &shapes, anchor);
    match PLAN_CACHE.lock().ok().and_then(|c| c.get(&plan_key).cloned()) {
        Some(bands) => {
            crate::profile::PLAN_CACHE_HIT.add(1);
            // `area` is only ever compared against other candidate plans
            // during the search; a cached plan skips that comparison
            // entirely, and `materialise` recomputes the real area below.
            best = Plan { bands, area: 0.0 };
        }
        None => {
            let mut budget = NODE_BUDGET;
            crate::profile::BANDED_SEARCH.time(|| search(sheet_bounds, parts, catalogue, &mut pool, sheet_bounds.height, &mut Plan::default(), &mut best, &mut budget));
            crate::profile::PLAN_CACHE_MISS.add(1);
            if let Ok(mut cache) = PLAN_CACHE.lock() {
                // Same cap policy as `UNIT_CACHE`: stop growing, keep what is there.
                if cache.len() < MAX_UNIT_CACHE_ENTRIES {
                    cache.insert(plan_key, best.bands.clone());
                }
            }
        }
    }
    if best.bands.is_empty() {
        return None;
    }
    if std::env::var("NEST_BANDED").is_ok_and(|v| v != "0") {
        eprintln!("  chosen bands: {:?}", best.bands);
    }
    Some(materialise(sheet_bounds, parts, catalogue, &best))
}

/// A band sequence under consideration: the heights chosen so far and the
/// total part area they hold.
#[derive(Clone, Default)]
struct Plan {
    bands: Vec<f64>,
    area: f64,
}

/// Depth-first search over band heights, deepest-value-first.
///
/// Bands are identified by *height* alone: two options of the same height
/// produce the same band region, and which parts go into it is decided by
/// `fill_band`, so branching on both would double the work for identical
/// geometry.
fn search(sheet: Bounds, parts: &[NestPart], catalogue: &[Unit], pool: &mut Pool, height_left: f64, current: &mut Plan, best: &mut Plan, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;

    if current.area > best.area {
        *best = current.clone();
    }

    let mut heights: Vec<f64> = shape_options(catalogue, pool)
        .into_iter()
        .filter(|u| u.height <= height_left + f64::EPSILON && u.width <= sheet.width)
        .map(|u| u.height)
        .collect();
    heights.sort_by(f64::total_cmp);
    heights.dedup_by(|a, b| (*a - *b).abs() < 1e-6);

    for height in heights {
        // Simulate the band: fill it, recurse, then undo. Undoing rather than
        // cloning the pool keeps the search allocation-free at depth.
        let mut placed = Vec::new();
        let mut consumed = Vec::new();
        fill_band(sheet, 0.0, height, parts, catalogue, pool, &mut placed, &mut consumed);
        if consumed.is_empty() {
            continue;
        }
        let band_area: f64 = consumed.iter().filter_map(|&i| parts.get(i)).map(|p| polygon_area(&p.polygon.points).abs()).sum();

        current.bands.push(height);
        current.area += band_area;
        search(sheet, parts, catalogue, pool, height_left - height, current, best, budget);
        current.bands.pop();
        current.area -= band_area;

        for &index in &consumed {
            pool.give_back(parts[index].source_id, index);
        }
    }
}

/// The best plan that repeats one band height for the whole sheet.
///
/// `search` is depth-first over band heights in ascending order and capped by
/// `NODE_BUDGET`, so on a sheet that takes nine bands it spends the entire
/// budget inside the shortest band's subtree and never backtracks far enough
/// to try "the tallest band, nine times". That is the plain grid answer, it is
/// often the best one, and losing it is expensive: `probe_bitten.dxf`
/// (1500x1500, spacing 5) came out at 52 parts a sheet where a uniform
/// 160-tall plan holds **54**, which is exactly the commercial nester's
/// number. Seeding costs one `fill_band` per band per distinct height - a
/// couple of dozen calls of pure arithmetic per sheet - and the search can
/// then only improve on it.
fn uniform_plan(sheet: Bounds, parts: &[NestPart], catalogue: &[Unit], height: f64) -> Plan {
    let mut pool = Pool::new(parts);
    let mut plan = Plan::default();
    let mut height_left = sheet.height;
    while height <= height_left + f64::EPSILON {
        let mut placed = Vec::new();
        let mut consumed = Vec::new();
        fill_band(sheet, 0.0, height, parts, catalogue, &mut pool, &mut placed, &mut consumed);
        if consumed.is_empty() {
            break;
        }
        plan.area += consumed.iter().filter_map(|&i| parts.get(i)).map(|p| polygon_area(&p.polygon.points).abs()).sum::<f64>();
        plan.bands.push(height);
        height_left -= height;
    }
    plan
}

/// Replays a chosen band sequence to produce the real placements.
///
/// Deliberately a separate pass rather than recording placements during the
/// search: the search backtracks constantly, and carrying placement vectors
/// through it would allocate on every node for results that are almost all
/// discarded.
fn materialise(sheet: Bounds, parts: &[NestPart], catalogue: &[Unit], plan: &Plan) -> BandedSheet {
    let mut pool = Pool::new(parts);
    let mut placed = Vec::new();
    let mut consumed = Vec::new();
    let mut y = sheet.y;
    for &height in &plan.bands {
        fill_band(sheet, y, height, parts, catalogue, &mut pool, &mut placed, &mut consumed);
        y += height;
    }
    let area = consumed.iter().filter_map(|&i| parts.get(i)).map(|p| polygon_area(&p.polygon.points).abs()).sum();
    BandedSheet { placed, consumed, area }
}

/// Fills one band left-to-right with whatever units fit the remaining width.
///
/// Mixes shapes within the band rather than insisting on one: once the widest
/// unit no longer fits the leftover width, a narrower one often does, and
/// refusing it wastes the band's tail for nothing.
fn fill_band(
    sheet_bounds: Bounds,
    band_y: f64,
    band_height: f64,
    parts: &[NestPart],
    catalogue: &[Unit],
    pool: &mut Pool,
    placed: &mut Vec<PlacedPart>,
    consumed: &mut Vec<usize>,
) -> usize {
    // Two cursors: where the next unit goes if it is a fresh one, and where
    // it goes if it is another copy of the one just placed. `step` is only
    // legal between identical units - a different shape slotted into the
    // overhang would overlap it - so a mixed band pays the full width at every
    // change of unit, exactly as before.
    let mut cursor = sheet_bounds.x;
    let mut cursor_repeat = sheet_bounds.x;
    let right = sheet_bounds.x + sheet_bounds.width;
    let mut previous: Option<Unit> = None;
    let mut count = 0;

    loop {
        let origin = |u: &Unit| if previous.as_ref() == Some(u) { cursor_repeat } else { cursor };
        let Some(chosen) = shape_options(catalogue, pool)
            .into_iter()
            .filter(|&u| origin(u) + u.width <= right + f64::EPSILON && u.height <= band_height + f64::EPSILON)
            .cloned()
            // **Occupancy of the band slice**, not raw area. Two orientations
            // of one shape have identical area, so an area score ties and any
            // width tie-break picks the *wider* one. In a 776.5-tall band that
            // is the 776.5x422.4 orientation: 3 across, 354mm of band height
            // wasted - where 422.4x776.5 fits 5 and fills the band exactly.
            // Dividing by the slice the unit occupies scores those 0.54 vs 1.0.
            //
            // The slice is `step`, not `width`: a unit that interlocks with
            // its own next copy really does occupy only `step` of the row, and
            // scoring it on its box is what would keep picking a fatter unit
            // that packs worse.
            .max_by(|a, b| {
                let occupancy = |u: &Unit| u.area / (u.step * band_height).max(f64::MIN_POSITIVE);
                occupancy(a).total_cmp(&occupancy(b)).then(a.area.total_cmp(&b.area))
            })
        else {
            break;
        };

        // A pair needs both copies; taking one and failing on the second would
        // leave half a unit placed, so check supply before consuming anything.
        if pool.available(chosen.source_id) < chosen.count() {
            break;
        }
        let taken: Vec<usize> = (0..chosen.count()).filter_map(|_| pool.take(chosen.source_id)).collect();
        if taken.len() != chosen.count() {
            for index in taken {
                pool.give_back(chosen.source_id, index);
            }
            break;
        }

        let x = origin(&chosen);
        for (index, &(rotation, dx, dy)) in taken.iter().zip(chosen.members.iter()) {
            // The engine expresses a placement as a translation applied to the
            // part's own polygon, so convert from "where the box goes" to
            // "where the polygon's origin goes".
            placed.push(PlacedPart { id: parts[*index].id, placement: Placement { x: x + dx, y: band_y + dy }, rotation });
            consumed.push(*index);
            count += 1;
        }

        cursor = x + chosen.width;
        cursor_repeat = x + chosen.step;
        previous = Some(chosen);
        if cursor_repeat >= right {
            break;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use geometry::dxf_import::LayeredPolygon;

    fn pack_sheet_t(b: Bounds, parts: &[NestPart]) -> Option<BandedSheet> {
        pack_sheet(b, parts, 0.3, None, &PartRules::default())
    }
    use geometry::point::Point;

    fn rect_part(id: usize, source_id: usize, w: f64, h: f64) -> NestPart {
        NestPart {
            id,
            source_id,
            polygon: LayeredPolygon::new(vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)], "0".into(), None),
            rotation: 0.0,
        }
    }

    fn sheet(w: f64, h: f64) -> Bounds {
        Bounds { x: 0.0, y: 0.0, width: w, height: h }
    }

    #[test]
    fn a_single_row_of_identical_rectangles_fills_the_width() {
        let parts: Vec<NestPart> = (0..10).map(|i| rect_part(i, 0, 100.0, 50.0)).collect();
        let result = pack_sheet_t(sheet(350.0, 50.0), &parts).expect("should pack");
        assert_eq!(result.placed.len(), 3, "350 / 100 = 3 across");
    }

    #[test]
    fn bands_stack_downward_without_overlapping() {
        let parts: Vec<NestPart> = (0..20).map(|i| rect_part(i, 0, 100.0, 50.0)).collect();
        let result = pack_sheet_t(sheet(300.0, 150.0), &parts).expect("should pack");
        assert_eq!(result.placed.len(), 9, "3 across x 3 bands");
        let ys: std::collections::HashSet<i64> = result.placed.iter().map(|p| p.placement.y as i64).collect();
        assert_eq!(ys.len(), 3, "expected exactly three distinct band positions, got {ys:?}");
    }

    /// The whole reason this module exists: a layout that needs one band in
    /// one orientation and the next band in the other. A uniform grid gets 6
    /// here; the two-band answer gets 8.
    #[test]
    fn mixed_orientation_bands_beat_a_uniform_grid() {
        // Real numbers from the reference job: pair-rectangles of 776.5x422.4
        // on a 2440x1220 sheet. 3 horizontal + 5 vertical = 8; a uniform grid
        // of either orientation reaches only 6 or 5.
        let parts: Vec<NestPart> = (0..20).map(|i| rect_part(i, 0, 776.5, 422.4)).collect();
        let result = pack_sheet_t(sheet(2440.0, 1220.0), &parts).expect("should pack");
        assert!(
            result.placed.len() >= 8,
            "expected at least 8 (3 horizontal + 5 vertical), got {} - the band packer is not mixing orientations",
            result.placed.len()
        );
    }

    fn triangle_part(id: usize, source_id: usize, w: f64, h: f64) -> NestPart {
        NestPart {
            id,
            source_id,
            polygon: LayeredPolygon::new(vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(0.0, h)], "0".into(), None),
            rotation: 0.0,
        }
    }

    /// **The case the whole module exists for.** Right triangles fill half
    /// their bounding box, so packing them as individual boxes tops out near
    /// 50% and would never beat the greedy NFP pass. Paired, they fill a box
    /// completely and the band layout wins.
    ///
    /// Reference job numbers: 776.5 x 422.4 triangles on 2440 x 1220. Eight
    /// pair-boxes is 16 parts; anything less than that means pairing failed.
    #[test]
    fn right_triangles_are_paired_into_full_boxes_before_banding() {
        let parts: Vec<NestPart> = (0..40).map(|i| triangle_part(i, 0, 776.5, 422.4)).collect();
        let result = pack_sheet_t(sheet(2440.0, 1220.0), &parts).expect("should pack");
        assert!(
            result.placed.len() >= 16,
            "expected at least 16 parts (8 paired boxes), got {} - the pairing pass is not firing",
            result.placed.len()
        );
        // And the pairing must be real, not double-counted: every placed id
        // distinct, and the area consistent with the count.
        let ids: std::collections::HashSet<usize> = result.placed.iter().map(|p| p.id).collect();
        assert_eq!(ids.len(), result.placed.len(), "a part was placed twice");
    }

    /// A shape that cannot tile its own box must stay a single unit rather
    /// than being force-paired into an overlap.
    /// **A grain-locked part must not come out of the band packer turned.**
    ///
    /// The packer's own orientations are 0 and 90, and it pairs at 0/90/180/
    /// 270, none of which used to consult `PartRules` - so a part restricted
    /// to 0/180 by its material could be handed back at 90, and `place_parts`
    /// takes a banded sheet wholesale when it holds more material. That is a
    /// scrapped part on grained or coated stock.
    ///
    /// The rectangle here is deliberately 60x20: at 90 degrees it is 20x60, a
    /// different box, so a packer ignoring the rule has an obvious reason to
    /// prefer the turned orientation and the assertion has something to catch.
    #[test]
    fn a_grain_locked_part_is_never_offered_a_forbidden_orientation() {
        let parts: Vec<NestPart> = (0..8)
            .map(|id| NestPart { id, source_id: 0, polygon: rect_part(id, 0, 60.0, 20.0).polygon, rotation: 0.0 })
            .collect();

        let mut map = std::collections::HashMap::new();
        map.insert(0usize, crate::placement::PartRule { angles: vec![0.0, 180.0], mirror: false });
        // Every copy carries its own entry, exactly as `dto::expand_parts`
        // writes them - the rule is per part id, not per shape.
        for id in 1..8 {
            map.insert(id, crate::placement::PartRule { angles: vec![0.0, 180.0], mirror: false });
        }
        let rules: PartRules = std::sync::Arc::new(map);

        // 50 wide: the part is 60 long, so it only fits stood on end. A
        // packer honouring the 0/180 rule must place nothing here rather than
        // turn it, which is the sharper half of this test.
        let narrow = Bounds { x: 0.0, y: 0.0, width: 50.0, height: 300.0 };
        let bounds = Bounds { x: 0.0, y: 0.0, width: 300.0, height: 300.0 };
        let packed = pack_sheet(bounds, &parts, 0.3, None, &rules).expect("should pack something");
        assert!(!packed.placed.is_empty(), "the rule must not make the part unplaceable");
        for part in &packed.placed {
            let angle = part.rotation.rem_euclid(360.0);
            assert!(
                (angle - 0.0).abs() < 1e-6 || (angle - 180.0).abs() < 1e-6,
                "part {} came out at {angle} degrees, which its rule forbids",
                part.id
            );
        }

        // On a sheet too narrow to take it lying down, the same shape *does*
        // turn when nothing forbids it - so the rule is what kept it straight
        // above, not the geometry.
        let free = pack_sheet(narrow, &parts, 0.3, None, &PartRules::default()).expect("unconstrained, it should stand on end");
        assert!(
            free.placed.iter().all(|p| {
                let a = p.rotation.rem_euclid(360.0);
                (a - 90.0).abs() < 1e-6 || (a - 270.0).abs() < 1e-6
            }),
            "unconstrained on a narrow sheet the part should turn"
        );
        // ...and with the rule, it must decline the sheet instead.
        assert!(
            pack_sheet(narrow, &parts, 0.3, None, &rules).is_none_or(|p| p.placed.is_empty()),
            "a grain-locked part must be left unplaced rather than turned to fit"
        );
    }

    #[test]
    fn a_shape_that_does_not_pair_is_left_alone() {
        let parts: Vec<NestPart> = (0..4).map(|i| rect_part(i, 0, 100.0, 50.0)).collect();
        let units = build_catalogue(&parts, 0.3, &PartRules::default());
        assert!(units.iter().all(|u| u.count() == 1), "a solid rectangle has no room for a partner in its own box");
    }

    #[test]
    fn nothing_that_does_not_fit_is_placed() {
        let parts = vec![rect_part(0, 0, 5000.0, 5000.0)];
        assert!(pack_sheet_t(sheet(100.0, 100.0), &parts).is_none());
    }

    #[test]
    fn an_empty_part_list_packs_nothing_rather_than_panicking() {
        assert!(pack_sheet_t(sheet(100.0, 100.0), &[]).is_none());
    }

    /// Every placed part must be a real, distinct index into the caller's
    /// list - a duplicate would mean one physical part placed twice.
    #[test]
    fn consumed_indices_are_unique_and_match_the_placements() {
        let parts: Vec<NestPart> = (0..20).map(|i| rect_part(i, 0, 100.0, 50.0)).collect();
        let result = pack_sheet_t(sheet(300.0, 150.0), &parts).expect("should pack");
        let unique: std::collections::HashSet<usize> = result.consumed.iter().copied().collect();
        assert_eq!(unique.len(), result.consumed.len(), "a part was consumed twice");
        assert_eq!(result.consumed.len(), result.placed.len());
    }

    #[test]
    fn two_shapes_can_share_a_band_when_the_tail_only_fits_the_smaller() {
        let mut parts: Vec<NestPart> = (0..4).map(|i| rect_part(i, 0, 100.0, 50.0)).collect();
        parts.extend((4..8).map(|i| rect_part(i, 1, 40.0, 50.0)));
        let result = pack_sheet_t(sheet(240.0, 50.0), &parts).expect("should pack");
        // 2 x 100 = 200, leaving 40 - exactly one of the small ones.
        assert_eq!(result.placed.len(), 3, "the band's tail should take the smaller shape");
    }
}
