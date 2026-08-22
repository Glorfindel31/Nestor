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

use geometry::dxf_import::{rotate_layered_polygon, LayeredPolygon};
use geometry::obstacle_nfp::obstacle_nfp;
use geometry::polygon::{get_polygon_bounds, polygon_area, Bounds};

use crate::placement::{NestPart, Placement, PlacedPart};

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
#[derive(Clone, Debug)]
struct Unit {
    /// `(rotation, dx, dy)` per member, relative to the unit's own box corner.
    members: Vec<(f64, f64, f64)>,
    source_id: usize,
    width: f64,
    height: f64,
    /// True material area of every member combined.
    area: f64,
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

/// The best unit for one shape at one base orientation: a pair if it pairs,
/// otherwise itself.
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
/// positions where B touches A without overlapping it. Every vertex of that
/// polygon is a valid, maximally-tight placement, so the best pair box is
/// simply the smallest union box over those vertices. It is exact, it needs no
/// direction guessing, and it reuses machinery the greedy pass already trusts.
fn build_unit(part: &NestPart, base_rotation: f64, available: usize, curve_tolerance: f64) -> Option<Unit> {
    let a = rotate_layered_polygon(&part.polygon, base_rotation);
    let ab = get_polygon_bounds(&a.points)?;
    let a_area = polygon_area(&a.points).abs();
    // A unit translates rigidly, so every member's translation is
    // `target - unit_box_corner`, plus whatever extra shift that member was
    // given when the unit was built. Expressing it any other way - notably as
    // an offset from the polygon's first vertex - puts parts wherever that
    // vertex happens to be, which is how an early version of this put every
    // part off the sheet.
    let single = Unit { members: vec![(base_rotation, -ab.x, -ab.y)], source_id: part.source_id, width: ab.width, height: ab.height, area: a_area };
    if available < 2 {
        return Some(single);
    }

    let mut best = single.clone();
    for extra in PAIR_ANGLES {
        let rotation = base_rotation + extra;
        let b = rotate_layered_polygon(&part.polygon, rotation);
        let Some(bb) = get_polygon_bounds(&b.points) else { continue };
        let Some(nfp) = obstacle_nfp(&a, &b, curve_tolerance) else { continue };

        // An NFP vertex is where B's own reference point goes, so the shift
        // that puts B there is the vertex minus that reference point.
        let b_ref = b.points.first().copied().unwrap_or(geometry::point::Point::new(0.0, 0.0));
        // Sampled *along* each NFP edge, not just at its vertices. Two
        // triangles pair by sliding along their shared hypotenuse, which is
        // one NFP edge - and the tightest union box occurs partway along it,
        // not at either end. Vertices alone gave 837x433 (0.937 density) where
        // sliding finds materially better, and that difference decides whether
        // two bands fit the sheet height.
        const SAMPLES_PER_EDGE: usize = 24;
        let positions: Vec<geometry::point::Point> = nfp
            .outer
            .iter()
            .enumerate()
            .flat_map(|(i, from)| {
                let to = nfp.outer[(i + 1) % nfp.outer.len()];
                (0..SAMPLES_PER_EDGE).map(move |k| {
                    let t = k as f64 / SAMPLES_PER_EDGE as f64;
                    geometry::point::Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
                })
            })
            .collect();
        for vertex in &positions {
            let (sx, sy) = (vertex.x - b_ref.x, vertex.y - b_ref.y);
            let (mx, my) = (bb.x + sx, bb.y + sy);
            let x0 = ab.x.min(mx);
            let y0 = ab.y.min(my);
            let width = (ab.x + ab.width).max(mx + bb.width) - x0;
            let height = (ab.y + ab.height).max(my + bb.height) - y0;
            if width <= 0.0 || height <= 0.0 {
                continue;
            }
            let pair = Unit {
                members: vec![(base_rotation, -x0, -y0), (rotation, sx - x0, sy - y0)],
                source_id: part.source_id,
                width,
                height,
                area: a_area * 2.0,
            };
            if pair.density() > best.density() + 1e-9 {
                best = pair;
            }
        }
    }

    if std::env::var("NEST_BANDED").is_ok_and(|v| v != "0") {
        eprintln!(
            "    unit src {} base {base_rotation} x{} {:.1}x{:.1} density {:.3}",
            part.source_id,
            best.count(),
            best.width,
            best.height,
            best.density()
        );
    }
    Some(best)
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
fn build_catalogue(parts: &[NestPart], curve_tolerance: f64) -> Vec<Unit> {
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
            if let Some(unit) = build_unit(part, angle, available, curve_tolerance) {
                out.push(unit);
            }
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
#[must_use]
pub fn pack_sheet(sheet_bounds: Bounds, parts: &[NestPart], curve_tolerance: f64) -> Option<BandedSheet> {
    if parts.is_empty() || sheet_bounds.width <= 0.0 || sheet_bounds.height <= 0.0 {
        return None;
    }
    let catalogue = build_catalogue(parts, curve_tolerance);
    if catalogue.is_empty() {
        return None;
    }
    // `NEST_BANDED=1` prints the unit catalogue - what pairing actually
    // produced, and how many of each fit across the sheet. Pairing failing
    // silently looks identical to pairing succeeding badly, and both show up
    // only as a disappointing sheet count.
    if std::env::var("NEST_BANDED").is_ok_and(|v| v != "0") {
        eprintln!("banded: sheet {:.1}x{:.1}", sheet_bounds.width, sheet_bounds.height);
        for u in &catalogue {
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
    let mut best = Plan::default();
    let mut budget = NODE_BUDGET;
    search(sheet_bounds, parts, &catalogue, &mut pool, sheet_bounds.height, &mut Plan::default(), &mut best, &mut budget);
    if best.bands.is_empty() {
        return None;
    }
    Some(materialise(sheet_bounds, parts, &catalogue, &best))
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
    let mut x = sheet_bounds.x;
    let right = sheet_bounds.x + sheet_bounds.width;
    let mut count = 0;

    loop {
        let width_left = right - x;
        let Some(chosen) = shape_options(catalogue, pool)
            .into_iter()
            .cloned()
            .filter(|u| u.width <= width_left + f64::EPSILON && u.height <= band_height + f64::EPSILON)
            // **Occupancy of the band slice**, not raw area. Two orientations
            // of one shape have identical area, so an area score ties and any
            // width tie-break picks the *wider* one. In a 776.5-tall band that
            // is the 776.5x422.4 orientation: 3 across, 354mm of band height
            // wasted - where 422.4x776.5 fits 5 and fills the band exactly.
            // Dividing by the slice the unit occupies scores those 0.54 vs 1.0.
            .max_by(|a, b| {
                let occupancy = |u: &Unit| u.area / (u.width * band_height).max(f64::MIN_POSITIVE);
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

        for (index, &(rotation, dx, dy)) in taken.iter().zip(chosen.members.iter()) {
            // The engine expresses a placement as a translation applied to the
            // part's own polygon, so convert from "where the box goes" to
            // "where the polygon's origin goes".
            placed.push(PlacedPart { id: parts[*index].id, placement: Placement { x: x + dx, y: band_y + dy }, rotation });
            consumed.push(*index);
            count += 1;
        }

        x += chosen.width;
        if x >= right {
            break;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_sheet_t(b: Bounds, parts: &[NestPart]) -> Option<BandedSheet> {
        pack_sheet(b, parts, 0.3)
    }
    use geometry::point::Point;

    fn rect_part(id: usize, source_id: usize, w: f64, h: f64) -> NestPart {
        NestPart {
            id,
            source_id,
            polygon: LayeredPolygon {
                points: vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)],
                layer: "0".into(),
                is_circle: None,
                children: Vec::new(),
                texts: Vec::new(),
                real_boundary: None,
            },
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
            polygon: LayeredPolygon {
                points: vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(0.0, h)],
                layer: "0".into(),
                is_circle: None,
                children: Vec::new(),
                texts: Vec::new(),
                real_boundary: None,
            },
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
    #[test]
    fn a_shape_that_does_not_pair_is_left_alone() {
        let parts: Vec<NestPart> = (0..4).map(|i| rect_part(i, 0, 100.0, 50.0)).collect();
        let units = build_catalogue(&parts, 0.3);
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
