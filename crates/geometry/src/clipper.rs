//! Thin wrapper around the `clipper2` crate (Clipper2 C++ FFI), covering the
//! plan's "offset, boolean ops, SimplifyPolygon+CleanPolygon" bullet. Ports
//! two real composed functions from `main/deepnest.js` (`polygonOffset`,
//! `cleanPolygon`) rather than just exposing raw Clipper2 primitives, since
//! those are what the app actually calls. `Area` isn't re-wrapped here -
//! `polygon::polygon_area` (already ported from geometryutil.js) works
//! identically on any point list, Clipper2-sourced or not.
//!
//! Clipper2 stores coordinates as scaled i64s (see `PointScaler`). The
//! `clipper2` crate's own default scaler (`Centi`, ×100 = 2 decimal places)
//! would be a real precision regression from the Electron app, which scales
//! by 10^7 before ever calling into Clipper1 (`ClipperLib.JS.ScaleUpPath(_,
//! 10000000)`, chosen there specifically "to ensure integer precision...
//! while avoiding overflow"). `DeepnestScale` matches that choice exactly.

use clipper2::{difference, inflate, intersect, union, xor, ClipperError, EndType, FillRule, JoinType, Paths};

use crate::point::Point;
use crate::polygon::polygon_area;

/// Matches the Electron app's `ClipperLib.JS.ScaleUpPath(_, 10000000)` - see
/// the module doc comment for why the crate's own default (`Centi`, ×100)
/// isn't precise enough for this application.
#[derive(Debug, Default, Clone, Copy, PartialEq, Hash)]
pub struct DeepnestScale;

impl clipper2::PointScaler for DeepnestScale {
    const MULTIPLIER: f64 = 1e7;
}

type ClipperPaths = Paths<DeepnestScale>;

fn to_raw_path(points: &[Point]) -> Vec<(f64, f64)> {
    points.iter().map(|p| (p.x, p.y)).collect()
}

fn to_raw_paths(polygons: &[Vec<Point>]) -> Vec<Vec<(f64, f64)>> {
    polygons.iter().map(|p| to_raw_path(p)).collect()
}

fn from_paths(paths: ClipperPaths) -> Vec<Vec<Point>> {
    let raw: Vec<Vec<(f64, f64)>> = paths.into();
    raw.into_iter()
        .map(|path| path.into_iter().map(|(x, y)| Point::new(x, y)).collect())
        .collect()
}

/// Port of `polygonOffset`: expands (positive `delta`) or contracts
/// (negative `delta`) a polygon. Matches the app's exact parameters (miter
/// join, miter limit 4, closed-polygon ends) rather than exposing every
/// Clipper2 offsetting knob - callers that need round/bevel joins can call
/// `clipper2::inflate` directly.
pub fn offset(polygon: &[Point], delta: f64) -> Vec<Vec<Point>> {
    // Exact comparison is deliberate, not a float_cmp footgun: `delta: 0.0`
    // is a real, common sentinel (a caller with margin/spacing both zero,
    // e.g. `clearance::prepare_sheet`/`prepare_part`), and a true zero-input
    // no-op short-circuit is the correct behavior - there's no "close to
    // zero" case here that should still go through the Clipper round-trip.
    if delta == 0.0 {
        return vec![polygon.to_vec()];
    }

    let paths: ClipperPaths = vec![to_raw_path(polygon)].into();
    let result = inflate(paths, delta, JoinType::Miter, EndType::Polygon, 4.0);
    from_paths(result)
}

/// Expands (positive `delta`) or contracts (negative `delta`) a polygon
/// using a **bevel** join instead of `offset`'s miter one - safe for a
/// clearance/keep-out buffer (`geometry::clearance`) the same way a round
/// join would be, but without a round join's cost.
///
/// `offset`'s miter join is correct for parity with the original app's
/// `polygonOffset` (used by the simplification pipeline, where matching the
/// JS output matters), but a miter join's spike length grows without bound
/// as a corner gets more acute (capped only by the miter limit, e.g. up to
/// 4x `delta` at the sharpest corners `offset` allows before falling back to
/// a bevel) - which is exactly wrong for a clearance buffer: a sliver-shaped
/// part with a sharp tip would get padded far more than the requested
/// spacing at that tip, potentially no longer fitting a sheet it obviously
/// should (confirmed against the real `tests/fixtures/*.dxf` parts - several
/// sliver profiles grew by 15-44mm instead of the expected ~6.5mm at a
/// spacing of 6.5).
///
/// A **round** join (this function's first version) also has no such
/// spike - every point on the result is within exactly `delta` of some point
/// on the original boundary - but it pays for that guarantee by tessellating
/// every corner into an arc, and this crate's coordinate scale
/// (`DeepnestScale`, x10^7) makes Clipper2's default arc tessellation far
/// denser than needed: a plain 4-point rectangle came back as ~56 points on
/// real fixture parts, and every downstream NFP computation (a Minkowski
/// diff per obstacle pair, per rotation, per generation) then did legitimate
/// but pointless O(n*m) work on those bloated shapes - confirmed to be the
/// dominant real-world cost of a full nest run. A **bevel** join gives the
/// same no-spike guarantee (confirmed against the same real fixtures: never
/// more than `delta` growth, often exactly `delta`) while flat-cutting each
/// corner with a single extra segment instead of an arc - about 2x the
/// original point count on real parts, not ~14x.
/// Outward offset with a **round** join: every point of the result is at
/// exactly `delta` from the original boundary, corners included.
///
/// This is the one `clearance.rs` needs, and the distinction from
/// `offset_bevel` is not cosmetic. A bevel caps growth at `delta` but
/// *under*-grows at a sharp corner - the chord across the corner passes
/// within `delta * cos(angle / 2)` of the original vertex, which goes to zero
/// as the corner sharpens. `prepare_sheet` compensates for part padding by
/// growing the sheet by exactly `spacing / 2`, so a part whose padding is
/// less than that at some corner has that corner hanging off the real
/// material once it is placed flush - measured at 1.5mm on `two.dxf`'s
/// triangles at spacing 6, i.e. scrap.
///
/// **The point-count objection turned out not to hold.** `offset_bevel`'s doc
/// records round being dropped for producing ~14x the points and dominating
/// NFP cost. Re-measured: `hat_bench` is unchanged in both utilisation and
/// time (4.6s), and the reference 200-part job goes from 15 sheets to 14 -
/// exact padding packs *better*, not just more honestly. The multi-sheet
/// benchmarks pay 1.2-3.4x in time. The 14x figure came from a 4-point
/// rectangle, and rectangles never reach here at all: `offset_rectangle_exact`
/// short-circuits them before any Clipper call.
#[must_use]
pub fn offset_round(polygon: &[Point], delta: f64) -> Vec<Vec<Point>> {
    if delta == 0.0 {
        return vec![polygon.to_vec()];
    }
    let paths: Paths<DeepnestScale> = Paths::from(vec![polygon.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()]);
    let result = inflate(paths, delta, JoinType::Round, EndType::Polygon, 4.0);
    result.iter().map(|path| path.iter().map(|p| Point::new(p.x(), p.y())).collect()).collect()
}

pub fn offset_bevel(polygon: &[Point], delta: f64) -> Vec<Vec<Point>> {
    // Same deliberate exact-zero short-circuit as `offset` above, same reason.
    if delta == 0.0 {
        return vec![polygon.to_vec()];
    }

    let paths: ClipperPaths = vec![to_raw_path(polygon)].into();
    let result = inflate(paths, delta, JoinType::Bevel, EndType::Polygon, 4.0);
    from_paths(result)
}

/// Port of `cleanPolygon`: resolves self-intersections (Clipper2's modern
/// equivalent of Clipper1's `SimplifyPolygon` is a self-union), keeps only
/// the largest-area resulting loop, then removes near-duplicate/collinear
/// points (Clipper2's `simplify` is the equivalent of Clipper1's
/// `CleanPolygon`) within `0.01 * curve_tolerance`. Returns `None` if
/// nothing is left after simplification, same as the original.
pub fn clean_polygon(polygon: &[Point], curve_tolerance: f64) -> Option<Vec<Point>> {
    let paths: ClipperPaths = vec![to_raw_path(polygon)].into();
    let empty: ClipperPaths = Vec::<Vec<(f64, f64)>>::new().into();

    let simple = union(paths, empty, FillRule::NonZero).ok()?;
    let simple = from_paths(simple);
    if simple.is_empty() {
        return None;
    }

    let biggest = simple
        .into_iter()
        .max_by(|a, b| polygon_area(a).abs().total_cmp(&polygon_area(b).abs()))?;

    let cleaned_paths: ClipperPaths = vec![to_raw_path(&biggest)].into();
    let cleaned = clipper2::simplify(cleaned_paths, 0.01 * curve_tolerance, false);
    let mut cleaned = from_paths(cleaned).into_iter().next()?;
    if cleaned.is_empty() {
        return None;
    }

    // remove a duplicated closing endpoint, same as the original
    if let (Some(&first), Some(&last)) = (cleaned.first(), cleaned.last()) {
        if cleaned.len() > 1 && first.x == last.x && first.y == last.y {
            cleaned.pop();
        }
    }

    Some(cleaned)
}

pub fn union_polygons(subject: &[Vec<Point>], clip: &[Vec<Point>], fill_rule: FillRule) -> Result<Vec<Vec<Point>>, ClipperError> {
    let s: ClipperPaths = to_raw_paths(subject).into();
    let c: ClipperPaths = to_raw_paths(clip).into();
    Ok(from_paths(union(s, c, fill_rule)?))
}

pub fn intersection_polygons(
    subject: &[Vec<Point>],
    clip: &[Vec<Point>],
    fill_rule: FillRule,
) -> Result<Vec<Vec<Point>>, ClipperError> {
    let s: ClipperPaths = to_raw_paths(subject).into();
    let c: ClipperPaths = to_raw_paths(clip).into();
    Ok(from_paths(intersect(s, c, fill_rule)?))
}

pub fn difference_polygons(
    subject: &[Vec<Point>],
    clip: &[Vec<Point>],
    fill_rule: FillRule,
) -> Result<Vec<Vec<Point>>, ClipperError> {
    let s: ClipperPaths = to_raw_paths(subject).into();
    let c: ClipperPaths = to_raw_paths(clip).into();
    Ok(from_paths(difference(s, c, fill_rule)?))
}

pub fn xor_polygons(subject: &[Vec<Point>], clip: &[Vec<Point>], fill_rule: FillRule) -> Result<Vec<Vec<Point>>, ClipperError> {
    let s: ClipperPaths = to_raw_paths(subject).into();
    let c: ClipperPaths = to_raw_paths(clip).into();
    Ok(from_paths(xor(s, c, fill_rule)?))
}

/// Port of `background.js`'s outer/collision-NFP worker computation
/// (the `process` function under the "No-Fit Polygon" comment block).
/// Computes the outer NFP of stationary `a` against moving `b` as the
/// Minkowski difference `a ⊖ b = {p - q | p ∈ a, q ∈ b}` directly via
/// Clipper2's `minkowski_diff` - the crate does the "negate B, then
/// Minkowski sum" trick the old app needed to work around Clipper1 lacking
/// a difference primitive.
///
/// `a` and `b` should already be rotated to their target angles (rotation
/// is `rotate_polygon`'s job, one layer up) - this only does the Minkowski
/// step, largest-loop selection, and the translate-back-to-`b[0]` that the
/// original does inline. Returns `None` for degenerate input or if Clipper2
/// produces no solution.
pub fn outer_nfp(a: &[Point], b: &[Point]) -> Option<Vec<Point>> {
    if a.len() < 3 || b.len() < 3 {
        return None;
    }

    let a_paths: ClipperPaths = vec![to_raw_path(a)].into();
    let b_pattern: clipper2::Path<DeepnestScale> = to_raw_path(b).into();

    let solution = from_paths(clipper2::minkowski_diff(b_pattern, a_paths, true));
    if solution.is_empty() {
        return None;
    }

    // keep the most-negative-signed-area loop (the solid outer ring), same
    // convention `simplify_polygon`'s offset-selection and offset-shell
    // re-merge steps use
    let mut best: Option<(Vec<Point>, f64)> = None;
    for candidate in solution {
        let area = polygon_area(&candidate);
        if best.as_ref().is_none_or(|(_, best_area)| area < *best_area) {
            best = Some((candidate, area));
        }
    }
    let (mut nfp, _) = best?;

    // the Minkowski difference is computed with B effectively at its own
    // local origin; translate back to B's actual reference point (B[0])
    let b0 = b[0];
    for p in &mut nfp {
        p.x += b0.x;
        p.y += b0.y;
    }

    Some(nfp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, size: f64) -> Vec<Point> {
        vec![
            Point::new(x, y),
            Point::new(x + size, y),
            Point::new(x + size, y + size),
            Point::new(x, y + size),
        ]
    }

    #[test]
    fn offset_expands_a_square() {
        let sq = square(0.0, 0.0, 10.0);
        let result = offset(&sq, 1.0);
        assert_eq!(result.len(), 1);
        let area = polygon_area(&result[0]).abs();
        // an offset-by-1 square (miter join) should be ~12x12 = 144
        assert!((area - 144.0).abs() < 1.0, "area was {area}");
    }

    #[test]
    fn offset_by_zero_returns_the_polygon_unchanged() {
        let sq = square(0.0, 0.0, 10.0);
        let result = offset(&sq, 0.0);
        assert_eq!(result, vec![sq]);
    }

    #[test]
    fn offset_contracts_with_negative_delta() {
        let sq = square(0.0, 0.0, 10.0);
        let result = offset(&sq, -1.0);
        assert_eq!(result.len(), 1);
        let area = polygon_area(&result[0]).abs();
        assert!((area - 64.0).abs() < 1.0, "area was {area}");
    }

    #[test]
    fn clean_polygon_resolves_a_self_intersecting_bowtie_to_its_larger_lobe() {
        // A bowtie ("hourglass"): (0,0)->(10,10)->(10,0)->(0,10) self-intersects at
        // its center, forming two triangles. The larger one (base 10, here both
        // triangles happen to be equal area since the shape is symmetric) should
        // come back as a simple, non-self-intersecting polygon.
        let bowtie = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(10.0, 0.0),
            Point::new(0.0, 10.0),
        ];
        let cleaned = clean_polygon(&bowtie, 0.01).expect("bowtie should resolve to a lobe");
        assert!(cleaned.len() >= 3);
        // each resolved triangle lobe has area 25 (half of a 10x10 square, halved again)
        let area = polygon_area(&cleaned).abs();
        assert!((area - 25.0).abs() < 1.0, "area was {area}");
    }

    #[test]
    fn union_merges_two_overlapping_squares() {
        let a = square(0.0, 0.0, 10.0);
        let b = square(5.0, 5.0, 10.0);
        let result = union_polygons(&[a], &[b], FillRule::NonZero).expect("union should succeed");
        assert_eq!(result.len(), 1);
        let area = polygon_area(&result[0]).abs();
        // 10x10 + 10x10 - 5x5 overlap = 175
        assert!((area - 175.0).abs() < 1e-6, "area was {area}");
    }

    #[test]
    fn difference_subtracts_the_overlap() {
        let a = square(0.0, 0.0, 10.0);
        let b = square(5.0, 5.0, 10.0);
        let result = difference_polygons(&[a], &[b], FillRule::NonZero).expect("difference should succeed");
        assert_eq!(result.len(), 1);
        let area = polygon_area(&result[0]).abs();
        // 10x10 minus the 5x5 overlap corner = 75
        assert!((area - 75.0).abs() < 1e-6, "area was {area}");
    }

    #[test]
    fn outer_nfp_of_two_axis_aligned_squares_is_the_sum_of_their_extents() {
        // A: 10x10 square at the origin. B: 2x2 square, reference point B[0] at
        // its own origin corner. The outer NFP describes valid positions for
        // B[0] such that B just touches A from outside - for two axis-aligned
        // rectangles that's a rectangle expanded by B's full footprint: a
        // (10+2) x (10+2) square running from (-2,-2) to (10,10).
        let a = square(0.0, 0.0, 10.0);
        let b = square(0.0, 0.0, 2.0);

        let nfp = outer_nfp(&a, &b).expect("outer NFP should exist for two squares");
        let bounds = crate::polygon::get_polygon_bounds(&nfp).expect("nfp has bounds");

        assert!((bounds.width - 12.0).abs() < 1e-6, "width was {}", bounds.width);
        assert!((bounds.height - 12.0).abs() < 1e-6, "height was {}", bounds.height);
        assert!((bounds.x - -2.0).abs() < 1e-6, "x was {}", bounds.x);
        assert!((bounds.y - -2.0).abs() < 1e-6, "y was {}", bounds.y);
    }

    #[test]
    fn outer_nfp_translates_by_bs_reference_point() {
        // Same as above, but B's reference point (B[0]) is offset from its own
        // shape's origin corner - the whole NFP should shift by that same offset.
        let a = square(0.0, 0.0, 10.0);
        let mut b = square(0.0, 0.0, 2.0);
        // rotate B's vertex order so index 0 is the (2,0) corner instead of (0,0)
        b.rotate_left(1);

        let nfp = outer_nfp(&a, &b).expect("outer NFP should exist");
        let bounds = crate::polygon::get_polygon_bounds(&nfp).expect("nfp has bounds");

        // shifted by +2 in x relative to the b[0]=(0,0) case above
        assert!((bounds.x - 0.0).abs() < 1e-6, "x was {}", bounds.x);
        assert!((bounds.width - 12.0).abs() < 1e-6, "width was {}", bounds.width);
    }
}
