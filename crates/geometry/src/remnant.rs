//! What is left of a sheet after a nest: the offcut, as a shape that can be
//! nested onto again.
//!
//! **New scope, not a port.** The original app throws the leftover away - a
//! partly-used sheet is simply a sheet that scored badly. But an offcut is
//! real material sitting on a real shelf, and re-using it is where the
//! material saving in this whole problem actually lives: the commercial tools
//! lead with remnant nesting and claim ~4% average yield from it alone, which
//! is more than another few percent of packing density is ever going to give.
//!
//! **Why this is a small module rather than a hard one.** The engine already
//! accepts an arbitrary `LayeredPolygon` as a sheet, holes included - that is
//! what `inner_nfp`'s general fallback exists for. So a remnant needs no new
//! algorithm, no new engine path and no new placement code. It is a boolean
//! subtraction and a sanity filter, and everything downstream treats the
//! result as an ordinary sheet without knowing where it came from.
//!
//! Two decisions worth not re-litigating:
//!
//! - **`offset_bevel`, never the plain miter `offset`, to grow the placed
//!   parts before subtracting.** `clipper`'s own doc records miter joins
//!   spiking 15-44mm on real sliver-shaped parts at a 6.5mm setting. Here
//!   that spike would be *subtracted* from the remnant, silently eating a
//!   chunk of material that is actually there.
//! - **The remnant is reported as a true shape *and* as the largest
//!   axis-aligned rectangle inside it.** The true shape is what the engine
//!   should nest onto - it is strictly better material-wise, and costs
//!   nothing to use. The rectangle is for the human: a remnant is a physical
//!   object that gets labelled, stacked and found again months later, and
//!   "1200 x 380 offcut" is a thing a person can act on in a way that a
//!   fourteen-vertex polygon is not.

use crate::clipper::{difference_polygons, offset_bevel, union_polygons};
use clipper2::FillRule;
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, polygon_area, Bounds};

/// Ignore leftover islands smaller than this, in square millimetres.
///
/// Boolean subtraction on real geometry leaves slivers along every edge it
/// touched - zero-width artefacts of the fixed-point grid, not material.
/// Without a floor, a "remnant" list is mostly those. 400mm^2 is a 2cm
/// square: below that nobody is nesting anything on it anyway.
pub const MIN_REMNANT_AREA: f64 = 400.0;

/// One reusable offcut.
#[derive(Clone, Debug)]
pub struct Remnant {
    /// The true free-material outline. This is what to nest onto.
    pub outline: Vec<Point>,
    /// Islands of *not*-free material enclosed by `outline` - the parts that
    /// were cut out of the middle of it.
    ///
    /// A boolean subtraction returns a region with holes as several separate
    /// rings, and treating each as its own remnant counts the cut parts as
    /// reclaimable material and hands back an offcut you would nest straight
    /// on top of them. They belong to their ring, and whoever turns a remnant
    /// into a sheet must carry them across as that sheet's own holes - which
    /// the engine has always supported (see this module's doc comment).
    pub holes: Vec<Vec<Point>>,
    /// Its area, in square millimetres.
    pub area: f64,
    /// The largest axis-aligned rectangle that fits inside `outline` - the
    /// human-readable "usable size". See the module doc for why both exist.
    pub usable: Bounds,
}

/// A part as it sits on the sheet: its outline already rotated and moved into
/// place.
///
/// Deliberately plain points rather than a `LayeredPolygon`: a part's *holes*
/// are not free material. A drilled hole is a hole in a piece someone is going
/// to pick up and use, and treating it as reclaimable offcut would produce
/// remnants that physically fall out of the sheet.
pub type PlacedOutline = Vec<Point>;

/// Computes the reusable offcuts of one sheet.
///
/// `spacing` grows each placed part before subtraction, so a remnant never
/// includes material that has to stay attached to a part for clearance. Pass
/// the same value the nest ran with.
///
/// Returns largest-first, so a caller that only wants "the" remnant can take
/// the first without sorting.
#[must_use]
pub fn sheet_remnants(sheet: &[Point], placed: &[PlacedOutline], spacing: f64) -> Vec<Remnant> {
    if sheet.len() < 3 {
        return Vec::new();
    }
    // Nothing placed: the whole sheet is still stock, not an offcut. Saying
    // otherwise would file untouched sheets into the remnant shelf.
    if placed.is_empty() {
        return Vec::new();
    }

    let grown: Vec<Vec<Point>> = placed.iter().filter(|p| p.len() >= 3).flat_map(|p| offset_bevel(p, spacing / 2.0)).collect();
    if grown.is_empty() {
        return Vec::new();
    }

    // Union first, subtract once. Subtracting each part in turn would work
    // but re-runs Clipper per part on a shape that keeps growing in
    // complexity - and overlapping grown outlines (adjacent parts, once
    // buffered) would each re-cut ground the previous one already removed.
    let occupied = match union_polygons(&grown, &[], FillRule::NonZero) {
        Ok(u) if !u.is_empty() => u,
        // A failed union must not silently yield "the whole sheet is free" -
        // that is the one wrong answer that loses material by cutting into
        // parts. No answer is the safe failure here.
        _ => return Vec::new(),
    };

    let free = match difference_polygons(std::slice::from_ref(&sheet.to_vec()), &occupied, FillRule::NonZero) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    // **Rings, not regions.** Clipper hands back one ring per boundary: the
    // outside of each free area, plus one for every part enclosed by it. A
    // ring is a hole when an odd number of other rings contain it, which is
    // the only classification that does not depend on a winding convention
    // this wrapper does not promise.
    let rings: Vec<Vec<Point>> = free.into_iter().filter(|r| r.len() >= 3).collect();
    let contains = |outer: &[Point], inner: &[Point]| {
        inner.first().is_some_and(|p| crate::polygon::point_in_polygon(*p, outer, Point::new(0.0, 0.0), None) == Some(true))
    };
    let depth: Vec<usize> = rings
        .iter()
        .map(|ring| rings.iter().filter(|other| !std::ptr::eq(other.as_slice(), ring.as_slice()) && contains(other, ring)).count())
        .collect();

    let mut out: Vec<Remnant> = rings
        .iter()
        .enumerate()
        .filter(|(i, _)| depth[*i].is_multiple_of(2))
        .filter_map(|(i, ring)| {
            let holes: Vec<Vec<Point>> = rings
                .iter()
                .enumerate()
                .filter(|(j, other)| depth[*j] == depth[i] + 1 && contains(ring, other))
                .map(|(_, other)| other.clone())
                .collect();
            // Net of its holes - the number someone books material against.
            let area = polygon_area(ring).abs() - holes.iter().map(|h| polygon_area(h).abs()).sum::<f64>();
            if area < MIN_REMNANT_AREA {
                return None;
            }
            let usable = largest_inscribed_rect_with_holes(ring, &holes).unwrap_or(Bounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 });
            Some(Remnant { outline: ring.clone(), holes, area, usable })
        })
        .collect();

    out.sort_by(|a, b| b.area.total_cmp(&a.area));
    out
}

/// Largest axis-aligned rectangle fitting inside `polygon`.
#[must_use]
pub fn largest_inscribed_rect(polygon: &[Point]) -> Option<Bounds> {
    largest_inscribed_rect_with_holes(polygon, &[])
}

/// Largest axis-aligned rectangle inside `polygon` and clear of every hole.
///
/// **Never larger than the truth.** The region is first rasterised on a grid
/// of `GRID` cells along its long side, and a cell only counts as free when no
/// boundary edge touches it and its centre is inside - so the best block of
/// free cells is real material. Each side of that block is then pushed out by
/// binary search against an exact test (no edge crosses the open rectangle),
/// which recovers all but a sliver of the last cell.
///
/// The coordinate sweep this replaced sampled nine points per candidate and
/// accepted points *on* the boundary, so it reported rectangles straight
/// through a slot or a curved bite - "1000 x 981" on a sheet with a 300mm
/// part in the corner - and thinned its candidates by index, which on a
/// curved outline dropped exactly the coordinate that bounds the real strip.
/// Both are numbers someone cuts against, and both moved the last-sheet
/// compaction (`nesting::repack::compact_for_offcut`) the wrong way.
///
/// ponytail: O(rows x edges + cells touched by edges). Raise `GRID`, or move
/// to an exact maximal-empty-rectangle algorithm, if a label ever needs to be
/// sub-millimetre on a huge sheet.
#[must_use]
pub fn largest_inscribed_rect_with_holes(polygon: &[Point], holes: &[Vec<Point>]) -> Option<Bounds> {
    const GRID: usize = 200;
    let bounds = get_polygon_bounds(polygon)?;
    let cell = bounds.width.max(bounds.height) / GRID as f64;
    if !(cell > 0.0) {
        return None;
    }
    let nx = ((bounds.width / cell).ceil() as usize).max(1);
    let ny = ((bounds.height / cell).ceil() as usize).max(1);
    let rings: Vec<&[Point]> = std::iter::once(polygon).chain(holes.iter().map(Vec::as_slice)).filter(|r| r.len() >= 3).collect();
    let edges: Vec<(Point, Point)> = rings.iter().flat_map(|r| r.iter().zip(r.iter().cycle().skip(1)).map(|(a, b)| (*a, *b))).collect();
    let cell_box = |i: usize, j: usize| Bounds { x: bounds.x + i as f64 * cell, y: bounds.y + j as f64 * cell, width: cell, height: cell };

    // A cell any edge touches is not free, whatever side its centre is on.
    let mut free = vec![true; nx * ny];
    let index = |v: f64, origin: f64, n: usize| (((v - origin) / cell).floor().max(0.0) as usize).min(n - 1);
    for (a, b) in &edges {
        let (i0, i1) = (index(a.x.min(b.x) - 1e-9, bounds.x, nx), index(a.x.max(b.x) + 1e-9, bounds.x, nx));
        let (j0, j1) = (index(a.y.min(b.y) - 1e-9, bounds.y, ny), index(a.y.max(b.y) + 1e-9, bounds.y, ny));
        for j in j0..=j1 {
            for i in i0..=i1 {
                if free[j * nx + i] && segment_hits_box(*a, *b, &cell_box(i, j), 0.0) {
                    free[j * nx + i] = false;
                }
            }
        }
    }
    // The rest are wholly in or wholly out: one even-odd scanline per row.
    for j in 0..ny {
        let y = bounds.y + (j as f64 + 0.5) * cell;
        let mut crossings: Vec<f64> =
            edges.iter().filter(|(a, b)| (a.y > y) != (b.y > y)).map(|(a, b)| a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x)).collect();
        crossings.sort_by(f64::total_cmp);
        let mut passed = 0;
        for i in 0..nx {
            let x = bounds.x + (i as f64 + 0.5) * cell;
            while passed < crossings.len() && crossings[passed] < x {
                passed += 1;
            }
            if passed % 2 == 0 {
                free[j * nx + i] = false;
            }
        }
    }

    // Largest all-free block of cells: a histogram per row, scanned with a stack.
    let mut heights = vec![0usize; nx];
    let mut best: Option<(usize, usize, usize, usize)> = None; // (i0, i1, j0, j1), exclusive ends
    let mut best_area = 0;
    for j in 0..ny {
        for i in 0..nx {
            heights[i] = if free[j * nx + i] { heights[i] + 1 } else { 0 };
        }
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..=nx {
            let h = if i < nx { heights[i] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] < h {
                    break;
                }
                stack.pop();
                let left = stack.last().map_or(0, |&s| s + 1);
                let area = heights[top] * (i - left);
                if area > best_area {
                    best_area = area;
                    best = Some((left, i, j + 1 - heights[top], j + 1));
                }
            }
            stack.push(i);
        }
    }
    let (i0, i1, j0, j1) = best?;
    let mut rect = Bounds {
        x: bounds.x + i0 as f64 * cell,
        y: bounds.y + j0 as f64 * cell,
        width: (i1 - i0) as f64 * cell,
        height: (j1 - j0) as f64 * cell,
    };

    // Push each side out as far as the exact test allows, at most a cell.
    let grown = |r: &Bounds, side: usize, d: f64| match side {
        0 => Bounds { x: r.x - d, width: r.width + d, ..*r },
        1 => Bounds { width: r.width + d, ..*r },
        2 => Bounds { y: r.y - d, height: r.height + d, ..*r },
        _ => Bounds { height: r.height + d, ..*r },
    };
    if !rect_is_free(&rect, &edges, &rings) {
        return Some(rect);
    }
    // Which side goes first matters (a side pushed into a curve can pin its
    // neighbour), so try across-then-up and up-then-across and keep the larger.
    let refine = |order: [usize; 4]| {
        let mut r = rect;
        for _ in 0..2 {
            for side in order {
                let (mut lo, mut hi) = (0.0, cell);
                for _ in 0..40 {
                    let mid = (lo + hi) / 2.0;
                    if rect_is_free(&grown(&r, side, mid), &edges, &rings) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                r = grown(&r, side, lo);
            }
        }
        // The exact test lets a side sit within 1e-9 of an edge; give it back
        // so the answer never ends past the boundary it stopped at.
        Bounds { x: r.x + 1e-8, y: r.y + 1e-8, width: r.width - 2e-8, height: r.height - 2e-8 }
    };
    let (a, b) = (refine([0, 1, 2, 3]), refine([2, 3, 0, 1]));
    rect = if a.width * a.height >= b.width * b.height { a } else { b };
    Some(rect)
}

/// Whether segment `a`-`b` touches `r` grown by `pad` on every side (Liang-Barsky).
fn segment_hits_box(a: Point, b: Point, r: &Bounds, pad: f64) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, q) in [(-dx, a.x - (r.x - pad)), (dx, r.x + r.width + pad - a.x), (-dy, a.y - (r.y - pad)), (dy, r.y + r.height + pad - a.y)] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Exact: no boundary edge enters the open rectangle, and its centre is inside
/// the region (even-odd over every ring, so holes count as outside).
fn rect_is_free(r: &Bounds, edges: &[(Point, Point)], rings: &[&[Point]]) -> bool {
    if edges.iter().any(|(a, b)| segment_hits_box(*a, *b, r, -1e-9)) {
        return false;
    }
    let (cx, cy) = (r.x + r.width / 2.0, r.y + r.height / 2.0);
    let crossings = rings
        .iter()
        .flat_map(|ring| ring.iter().zip(ring.iter().cycle().skip(1)))
        .filter(|(a, b)| (a.y > cy) != (b.y > cy) && cx < a.x + (cy - a.y) / (b.y - a.y) * (b.x - a.x))
        .count();
    crossings % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<Point> {
        vec![Point::new(x, y), Point::new(x + w, y), Point::new(x + w, y + h), Point::new(x, y + h)]
    }

    /// **A part in the middle of the sheet is a hole in the offcut, not part
    /// of it.** Clipper returns such a region as two rings - the sheet's edge
    /// and the part's - and counting each as its own remnant reports the whole
    /// sheet as reclaimable *and* files the cut part itself as reusable stock.
    /// Measured before the fix: 10000mm2 of "offcut" on a sheet with a part
    /// still in it, and a real 14-part sheet reporting 179% of itself free.
    #[test]
    fn a_part_enclosed_by_the_offcut_is_a_hole_in_it_not_material() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        let placed = vec![rect(45.0, 45.0, 10.0, 10.0)];
        let remnants = sheet_remnants(&sheet, &placed, 0.0);

        assert_eq!(remnants.len(), 1, "one offcut, not one per ring: {remnants:?}");
        let offcut = &remnants[0];
        assert!((offcut.area - 9900.0).abs() < 1.0, "area must be net of the part, got {}", offcut.area);
        assert_eq!(offcut.holes.len(), 1, "the part must come back as a hole so nothing nests on top of it");

        // And the usable rectangle must miss the part rather than swallowing it.
        assert!(offcut.usable.width * offcut.usable.height <= 9900.0, "usable {:?} cannot exceed the free area", offcut.usable);
        let u = &offcut.usable;
        let hits_part = u.x < 55.0 && u.x + u.width > 45.0 && u.y < 55.0 && u.y + u.height > 45.0;
        assert!(!hits_part, "the usable rectangle runs through the part: {u:?}");
    }

    /// The basic claim: cut one strip off a sheet, and what is left is
    /// reported with the right area.
    #[test]
    fn one_part_leaves_the_rest_of_the_sheet() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        let remnants = sheet_remnants(&sheet, &[rect(0.0, 0.0, 100.0, 40.0)], 0.0);
        assert_eq!(remnants.len(), 1, "{remnants:?}");
        assert!((remnants[0].area - 6000.0).abs() < 1.0, "expected ~6000mm2, got {}", remnants[0].area);
        assert!((remnants[0].usable.width - 100.0).abs() < 0.1, "{:?}", remnants[0].usable);
        assert!((remnants[0].usable.height - 60.0).abs() < 0.1, "{:?}", remnants[0].usable);
    }

    /// An untouched sheet is stock, not an offcut. Filing it as a remnant
    /// would fill the shelf with sheets nobody has cut yet.
    #[test]
    fn an_empty_sheet_produces_no_remnant() {
        assert!(sheet_remnants(&rect(0.0, 0.0, 100.0, 100.0), &[], 0.0).is_empty());
    }

    /// A fully used sheet has nothing left. The area floor is what stops the
    /// boolean-subtraction slivers along each cut edge being reported.
    #[test]
    fn a_fully_covered_sheet_produces_no_usable_remnant() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        let remnants = sheet_remnants(&sheet, &[rect(0.0, 0.0, 100.0, 100.0)], 0.0);
        assert!(remnants.is_empty(), "{remnants:?}");
    }

    #[test]
    fn slivers_below_the_area_floor_are_discarded() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        // Leaves a 100 x 0.5 strip = 50mm2, well under MIN_REMNANT_AREA.
        let remnants = sheet_remnants(&sheet, &[rect(0.0, 0.0, 100.0, 99.5)], 0.0);
        assert!(remnants.is_empty(), "{remnants:?}");
    }

    /// Spacing has to be honoured: material within the clearance zone of a
    /// part is not reclaimable, and a remnant that includes it would overlap
    /// the part when nested onto.
    #[test]
    fn spacing_shrinks_the_remnant_away_from_the_parts() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        let tight = sheet_remnants(&sheet, &[rect(0.0, 0.0, 100.0, 40.0)], 0.0);
        let spaced = sheet_remnants(&sheet, &[rect(0.0, 0.0, 100.0, 40.0)], 10.0);
        assert_eq!(spaced.len(), 1, "{spaced:?}");
        assert!(spaced[0].area < tight[0].area, "spacing must reduce the remnant: {} vs {}", spaced[0].area, tight[0].area);
    }

    /// Two separated parts leave two separate offcuts, each reported in its
    /// own right and biggest first.
    #[test]
    fn disjoint_free_regions_are_reported_separately_largest_first() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        // A full-width band across the middle splits the sheet in two, with
        // the lower piece deliberately the larger of the two.
        let remnants = sheet_remnants(&sheet, &[rect(0.0, 40.0, 100.0, 20.0)], 0.0);
        assert_eq!(remnants.len(), 2, "{remnants:?}");
        assert!(remnants[0].area >= remnants[1].area, "must be sorted largest-first");
        assert!((remnants[0].area - 4000.0).abs() < 1.0, "{:?}", remnants[0]);
    }

    /// The usable rectangle must fit *inside* the remnant, not merely share
    /// its bounding box - an L-shaped offcut is the case that separates the
    /// two, and reporting its bounding box would claim material that isn't
    /// there.
    #[test]
    fn the_usable_rect_of_an_l_shape_is_not_its_bounding_box() {
        let sheet = rect(0.0, 0.0, 100.0, 100.0);
        // Bite the top-right corner out, leaving an L.
        let remnants = sheet_remnants(&sheet, &[rect(50.0, 50.0, 50.0, 50.0)], 0.0);
        assert_eq!(remnants.len(), 1, "{remnants:?}");
        let usable = remnants[0].usable;
        let bounding = 100.0 * 100.0;
        assert!(usable.width * usable.height < bounding, "must not claim the whole bounding box: {usable:?}");
        // The two honest answers for this L are 100x50 and 50x100.
        assert!((usable.width * usable.height - 5000.0).abs() < 200.0, "expected ~5000mm2 usable, got {usable:?}");
    }

    #[test]
    fn a_degenerate_sheet_is_handled_rather_than_panicking() {
        assert!(sheet_remnants(&[], &[rect(0.0, 0.0, 10.0, 10.0)], 0.0).is_empty());
        assert!(sheet_remnants(&rect(0.0, 0.0, 100.0, 100.0), &[vec![Point::new(0.0, 0.0)]], 0.0).is_empty());
    }

    #[test]
    fn the_inscribed_rect_of_a_plain_rectangle_is_itself() {
        let r = largest_inscribed_rect(&rect(10.0, 20.0, 30.0, 40.0)).expect("has bounds");
        assert!((r.width - 30.0).abs() < 1e-6 && (r.height - 40.0).abs() < 1e-6, "{r:?}");
    }

    /// The slot is a notch cut from the top edge. The rectangle may sit beside
    /// it or above it, never across it, and the 61 x 100 right-hand part is the
    /// answer.
    #[test]
    fn the_inscribed_rect_does_not_cross_a_slot() {
        let slot = vec![
            Point::new(0.0, 0.0),
            Point::new(37.0, 0.0),
            Point::new(37.0, 80.0),
            Point::new(39.0, 80.0),
            Point::new(39.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let r = largest_inscribed_rect(&slot).expect("has bounds");
        let crosses = r.x < 39.0 && r.x + r.width > 37.0 && r.y < 80.0;
        assert!(!crosses, "rect runs through the slot: {r:?}");
        assert!(r.width * r.height >= 61.0 * 100.0 - 100.0, "must find the 61x100 part, got {r:?}");
    }

    /// A quarter-disc bite out of the corner. Every corner of the answer must
    /// lie outside the disc, and the answer must be close to the 700 x 1000
    /// rectangle beside it.
    #[test]
    fn the_inscribed_rect_does_not_cross_a_curve() {
        let mut poly = vec![Point::new(1000.0, 0.0), Point::new(1000.0, 1000.0), Point::new(0.0, 1000.0)];
        for k in 0..=200 {
            let t = std::f64::consts::FRAC_PI_2 * k as f64 / 200.0;
            poly.push(Point::new(300.0 * t.sin(), 300.0 * t.cos()));
        }
        let r = largest_inscribed_rect(&poly).expect("has bounds");
        assert!(r.width * r.height >= 700_000.0 - 2_000.0, "expected ~700000mm2, got {r:?}");
        for (x, y) in [(r.x, r.y), (r.x + r.width, r.y), (r.x, r.y + r.height), (r.x + r.width, r.y + r.height)] {
            // 0.01: the bite is 200 chords, which sag ~0.002mm inside the true arc.
            assert!(x.hypot(y) >= 300.0 - 0.01, "corner ({x}, {y}) is inside the bite: {r:?}");
        }
    }

    /// A hole in the middle of a square. The answer must miss the hole box and
    /// still be a 40 x 100 strip beside it, not the 0 it would be if the hole
    /// were ignored and the sweep kept colliding with it.
    #[test]
    fn the_inscribed_rect_misses_a_hole() {
        let r = largest_inscribed_rect_with_holes(&rect(0.0, 0.0, 100.0, 100.0), &[rect(40.0, 40.0, 20.0, 20.0)]).expect("has bounds");
        let overlaps = r.x < 60.0 && r.x + r.width > 40.0 && r.y < 60.0 && r.y + r.height > 40.0;
        assert!(!overlaps, "rect overlaps the hole: {r:?}");
        assert!(r.width * r.height >= 40.0 * 100.0 - 100.0, "expected a 40x100 strip beside the hole, got {r:?}");
    }

    #[test]
    fn a_plain_rectangle_is_found_to_full_size() {
        let r = largest_inscribed_rect(&rect(0.0, 0.0, 100.0, 50.0)).expect("has bounds");
        assert!(r.width >= 99.99 && r.height >= 49.99, "{r:?}");
    }
}
