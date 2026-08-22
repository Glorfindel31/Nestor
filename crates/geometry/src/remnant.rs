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
        .filter(|(i, _)| depth[*i] % 2 == 0)
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
///
/// ponytail: a coordinate-sweep approximation, not the exact algorithm. It
/// takes the distinct X and Y values of the polygon's own vertices as candidate
/// edges and tests each resulting box for containment. For the shapes this
/// actually sees - a rectangular sheet with rectangular-ish bites taken out of
/// it - the optimal rectangle's edges lie on vertex coordinates, so this is
/// exact in practice; for a curved outline it under-reports slightly, which is
/// the safe direction for a number someone is going to cut against.
///
/// O(n^4) in the vertex count, hence the cap: this runs once per remnant, not
/// in any loop, and a remnant with hundreds of vertices is a sliver nobody is
/// going to use anyway. Upgrade to the proper largest-rectangle-in-polygon
/// algorithm only if that ever stops being true.
#[must_use]
pub fn largest_inscribed_rect(polygon: &[Point]) -> Option<Bounds> {
    largest_inscribed_rect_with_holes(polygon, &[])
}

/// As `largest_inscribed_rect`, but the rectangle must also miss every hole.
///
/// **A hole is rejected by its bounding box, not its outline.** That
/// under-reports a little - a rectangle tucked into the corner beside a
/// triangular part is refused because it overlaps that part's box - and
/// under-reporting is the only safe direction for a number someone is going to
/// cut against. The exact test is a polygon intersection per candidate box,
/// and there are tens of thousands of candidates.
///
/// The old vertex cap returned the polygon's own bounding box for anything
/// complicated, which is the *unsafe* direction and is where "usable
/// 2440 x 1220" came from on a sheet that was two thirds full. Now the
/// candidate coordinates are thinned instead, so the search stays bounded
/// without ever claiming material that is not there.
#[must_use]
pub fn largest_inscribed_rect_with_holes(polygon: &[Point], holes: &[Vec<Point>]) -> Option<Bounds> {
    /// Most distinct candidate coordinates per axis. The sweep is O(n^4).
    const MAX_COORDS: usize = 26;
    let bounds = get_polygon_bounds(polygon)?;
    let hole_boxes: Vec<Bounds> = holes.iter().filter_map(|h| get_polygon_bounds(h)).collect();

    let mut xs: Vec<f64> = polygon.iter().map(|p| p.x).collect();
    let mut ys: Vec<f64> = polygon.iter().map(|p| p.y).collect();
    // A usable rectangle very often stops exactly at a hole's edge, so those
    // edges have to be candidates or the sweep cannot find it.
    for b in &hole_boxes {
        xs.push(b.x);
        xs.push(b.x + b.width);
        ys.push(b.y);
        ys.push(b.y + b.height);
    }
    dedup_sorted(&mut xs);
    dedup_sorted(&mut ys);
    thin(&mut xs, MAX_COORDS);
    thin(&mut ys, MAX_COORDS);

    let clear_of_holes = |r: &Bounds| {
        hole_boxes.iter().all(|h| {
            r.x + r.width <= h.x + 1e-9 || h.x + h.width <= r.x + 1e-9 || r.y + r.height <= h.y + 1e-9 || h.y + h.height <= r.y + 1e-9
        })
    };

    let mut best: Option<Bounds> = None;
    for i in 0..xs.len().saturating_sub(1) {
        for j in (i + 1)..xs.len() {
            for k in 0..ys.len().saturating_sub(1) {
                for l in (k + 1)..ys.len() {
                    let candidate = Bounds { x: xs[i], y: ys[k], width: xs[j] - xs[i], height: ys[l] - ys[k] };
                    // Cheap reject before the containment test, which is the
                    // expensive part of this loop.
                    if best.as_ref().is_some_and(|b| b.width * b.height >= candidate.width * candidate.height) {
                        continue;
                    }
                    if clear_of_holes(&candidate) && rect_inside(&candidate, polygon) {
                        best = Some(candidate);
                    }
                }
            }
        }
    }
    let _ = bounds;
    best
}

/// Keeps at most `max` values, spread evenly, ends always included.
fn thin(v: &mut Vec<f64>, max: usize) {
    if v.len() <= max || max < 2 {
        return;
    }
    let last = v.len() - 1;
    *v = (0..max).map(|i| v[i * last / (max - 1)]).collect();
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
}

fn dedup_sorted(v: &mut Vec<f64>) {
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
}

/// Whether a rectangle lies entirely within `polygon`.
///
/// Tests the rectangle's corners *and* the midpoints of its edges. Corners
/// alone are not enough: a C-shaped region can contain all four corners of a
/// box while its notch cuts straight through the middle of an edge, and
/// reporting that as usable material is exactly the error that gets someone
/// cutting into thin air.
fn rect_inside(rect: &Bounds, polygon: &[Point]) -> bool {
    let (x0, y0) = (rect.x, rect.y);
    let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
    let (mx, my) = (x0 + rect.width / 2.0, y0 + rect.height / 2.0);
    [
        (x0, y0),
        (x1, y0),
        (x1, y1),
        (x0, y1),
        (mx, y0),
        (mx, y1),
        (x0, my),
        (x1, my),
        (mx, my),
    ]
    .iter()
    .all(|&(x, y)| crate::polygon::point_in_polygon(Point::new(x, y), polygon, Point::new(0.0, 0.0), None) != Some(false))
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
}
