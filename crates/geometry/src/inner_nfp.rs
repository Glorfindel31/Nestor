//! Port of `background.js`'s `getInnerNfp` dispatch: the three-fast-path
//! priority chain for computing where a part `b` can be placed inside a
//! container `a` (sheet, or a hole another part is nesting inside).
//!
//! **The general fallback is "the one piece of the whole project with no
//! existing correct reference to copy"** (per the plan) - the Electron app's
//! own general-case implementation shells out to a confirmed-buggy native
//! addon (`addon.calculateNFP`), reached via an artificial "frame trick"
//! (wrapping `a` in a big rectangle with `a` as the frame's hole, purely to
//! fit the addon's API shape). This port does **not** replicate that frame
//! trick - it isn't needed here. `geometry::nfp::no_fit_polygon` already
//! supports orbiting `b` *inside* `a` directly (`inside: true`), faithfully
//! ported from `geometryutil.js` in Phase 1; that's the real algorithm the
//! frame-trick-plus-addon was standing in for. This module composes that
//! with the already-ported Minkowski-based `outer_nfp` (for subtracting each
//! of `a`'s holes as collision obstacles) - a from-scratch solution built
//! from already-correct pieces, not a port of the addon's approach.

use clipper2::FillRule;

use crate::circular_nfp::fast_fit_disk;
use crate::clipper::{difference_polygons, outer_nfp};
use crate::dxf_import::{tessellate_circle, LayeredPolygon};
use crate::nfp::{no_fit_polygon, no_fit_polygon_rectangle};
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, polygon_area};

/// Port of `getInnerNfp`'s dispatch logic (minus the DB cache lookup/insert,
/// which is `nesting`'s job, not `geometry`'s - see Phase 5's `NfpCache`).
/// Returns the valid placement region(s) for `b`'s reference point such that
/// `b` fits entirely inside `a`, or `None` if it can't fit at all.
pub fn inner_nfp(a: &LayeredPolygon, b: &LayeredPolygon, curve_tolerance: f64) -> Option<Vec<Vec<Point>>> {
    // Fast path 1: both a circular hole and a circular part, with no
    // sub-features of a's own - exact closed-form disk math (Phase 1,
    // circular_nfp.rs). Deliberately not applied when b is non-circular
    // (see circular_nfp.rs's doc comment for why that's only ever a
    // conservative approximation, not exact, and out of scope for now).
    if a.children.is_empty() {
        // `b.points.first()` as part of the pattern, not a raw `b.points[0]`
        // index: `LayeredPolygon`'s fields are all `pub`, nothing stops a
        // caller from constructing one with `is_circle: Some(_)` but empty
        // `points` (a data-integrity inconsistency, not a type-level
        // impossibility) - falling through to the rectangle/general fallback
        // below (both already guard on point count) beats an unchecked
        // index panic on that degenerate input.
        if let (Some(a_circle), Some(b_circle), Some(&b0)) = (a.is_circle, b.is_circle, b.points.first()) {
            return fast_fit_disk(
                Point::new(a_circle.cx, a_circle.cy),
                a_circle.r,
                b0,
                Point::new(b_circle.cx, b_circle.cy),
                b_circle.r,
            )
            .map(|fit| vec![tessellate_circle(fit.cx, fit.cy, fit.r, curve_tolerance)]);
        }
    }

    // Fast path 2: a is (approximately) an axis-aligned rectangle with no
    // holes of its own - exact for ANY shape of b, since a rectangle is
    // convex (if b's bbox stays inside a's, all of b does too). Checked via
    // area rather than a per-point check (matches the original): the
    // spacing-offset step can leave extra collinear points along a
    // still-rectangular sheet's edges, which a strict per-vertex check
    // would wrongly reject.
    if a.children.is_empty() {
        if let Some(bounds) = get_polygon_bounds(&a.points) {
            let a_area = polygon_area(&a.points).abs();
            let bbox_area = bounds.width * bounds.height;
            if bbox_area > 0.0 && (a_area - bbox_area).abs() < 0.001 * bbox_area {
                return no_fit_polygon_rectangle(&a.points, &b.points);
            }
        }
    }

    // General fallback: orbit b inside a's own outer boundary, then
    // subtract each of a's holes as a collision obstacle.
    let mut a_outer = a.points.clone();
    let mut b_pts = b.points.clone();
    let fit_regions = no_fit_polygon(&mut a_outer, &mut b_pts, true, true)?;
    if fit_regions.is_empty() {
        return None;
    }
    if a.children.is_empty() {
        return Some(fit_regions);
    }

    let mut hole_nfps: Vec<Vec<Point>> = Vec::new();
    for hole in &a.children {
        if let Some(h) = outer_nfp(&hole.points, &b.points) {
            hole_nfps.push(h);
        }
    }
    if hole_nfps.is_empty() {
        return Some(fit_regions);
    }

    match difference_polygons(&fit_regions, &hole_nfps, FillRule::NonZero) {
        Ok(result) if !result.is_empty() => Some(result),
        _ => Some(fit_regions),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circular_nfp::Circle;

    fn square_layered(x: f64, y: f64, size: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![
                Point::new(x, y),
                Point::new(x + size, y),
                Point::new(x + size, y + size),
                Point::new(x, y + size),
            ],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    #[test]
    fn circular_fast_path_fires_for_two_circles_with_no_holes() {
        let a = LayeredPolygon {
            points: crate::dxf_import::tessellate_circle(0.0, 0.0, 10.0, 0.01),
            layer: "0".into(),
            is_circle: Some(Circle { cx: 0.0, cy: 0.0, r: 10.0 }),
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        let b = LayeredPolygon {
            points: crate::dxf_import::tessellate_circle(0.0, 0.0, 4.0, 0.01),
            layer: "0".into(),
            is_circle: Some(Circle { cx: 0.0, cy: 0.0, r: 4.0 }),
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };

        let result = inner_nfp(&a, &b, 0.01).expect("small circle should fit inside big circle");
        assert_eq!(result.len(), 1);
        // valid region should be a disk of radius 10-4=6
        let area = polygon_area(&result[0]).abs();
        let expected = std::f64::consts::PI * 6.0 * 6.0;
        assert!((area - expected).abs() / expected < 0.02, "area was {area}");
    }

    #[test]
    fn rectangular_fast_path_fires_for_a_plain_rectangle_container() {
        let a = square_layered(0.0, 0.0, 20.0);
        let b = square_layered(0.0, 0.0, 4.0);

        let result = inner_nfp(&a, &b, 0.1).expect("small square should fit inside big square");
        assert_eq!(result.len(), 1);
        let bounds = get_polygon_bounds(&result[0]).unwrap();
        // valid positions for b[0]: a 16x16 square (20-4 in each dimension)
        assert!((bounds.width - 16.0).abs() < 1e-6, "width was {}", bounds.width);
        assert!((bounds.height - 16.0).abs() < 1e-6, "height was {}", bounds.height);
    }

    #[test]
    fn general_fallback_fires_when_container_has_holes() {
        // A 20x20 square container with a 4x4 hole in the middle - forces the
        // general fallback (fast path 2 requires NO holes).
        let hole = LayeredPolygon {
            points: vec![
                Point::new(8.0, 8.0),
                Point::new(12.0, 8.0),
                Point::new(12.0, 12.0),
                Point::new(8.0, 12.0),
            ],
            layer: "DRILL".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        let mut a = square_layered(0.0, 0.0, 20.0);
        a.children.push(hole);
        let b = square_layered(0.0, 0.0, 2.0);

        let result = inner_nfp(&a, &b, 0.1).expect("small square should fit somewhere inside the container");
        assert!(!result.is_empty());
    }

    #[test]
    fn oversized_part_does_not_fit_anywhere() {
        let a = square_layered(0.0, 0.0, 10.0);
        let b = square_layered(0.0, 0.0, 20.0);
        assert!(inner_nfp(&a, &b, 0.1).is_none());
    }
}
