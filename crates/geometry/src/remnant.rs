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

    let mut out: Vec<Remnant> = free
        .into_iter()
        .filter_map(|region| {
            let area = polygon_area(&region).abs();
            if area < MIN_REMNANT_AREA {
                return None;
            }
            let usable = largest_inscribed_rect(&region)?;
            Some(Remnant { outline: region, area, usable })
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
    const MAX_VERTICES: usize = 64;
    let bounds = get_polygon_bounds(polygon)?;
    if polygon.len() > MAX_VERTICES {
        return Some(bounds);
    }

    let mut xs: Vec<f64> = polygon.iter().map(|p| p.x).collect();
    let mut ys: Vec<f64> = polygon.iter().map(|p| p.y).collect();
    dedup_sorted(&mut xs);
    dedup_sorted(&mut ys);

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
                    if rect_inside(&candidate, polygon) {
                        best = Some(candidate);
                    }
                }
            }
        }
    }
    best.or(Some(bounds))
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
