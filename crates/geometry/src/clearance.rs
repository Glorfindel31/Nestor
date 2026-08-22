//! Margin (sheet-edge clearance) and spacing (inter-part clearance) as two
//! independently configurable values - a real capability, not just
//! benchmark methodology (see the fix history in
//! `crates/nesting/examples/bench.rs` for how this was worked out).
//!
//! **Why two knobs, and why they need this exact math.** For CNC/laser
//! cutting, the tool can legitimately travel past the sheet's physical edge
//! (there's no material there to protect), but it must never get closer
//! than `spacing` to another part's own cut path. So `margin` and `spacing`
//! are genuinely different physical constraints and must be settable
//! independently - including both being `0.0` (a laser job with no required
//! clearance at all, which must be a true no-op, not a degenerate case of
//! some combined formula).
//!
//! **How it works, given the engine only takes one polygon per part.** A
//! part's true (unpadded) boundary is what should sit `margin` from the
//! sheet edge and `spacing` from another part's true boundary - but
//! `nesting::placement` has no way to use a different shape for "is this
//! part touching the sheet boundary" vs. "is this part touching another
//! part". The standard trick: pad every part outward by `spacing / 2`
//! (`prepare_part`) so two padded parts touching means their true
//! boundaries are the full `spacing` apart. That same padding, left
//! uncorrected, would also silently apply to the part-vs-sheet check - so
//! the sheet's own inset (`prepare_sheet`) has to net that back out:
//!
//! ```text
//! sheet_delta = spacing / 2 - margin
//! ```
//!
//! Working through what a placed (padded) part's *true* edge ends up at,
//! relative to the *true* sheet edge, with this delta:
//!
//! ```text
//! true part edge = true sheet edge - margin
//! ```
//!
//! `spacing` cancels out completely - the part-vs-sheet clearance is
//! `margin`, full stop, regardless of what `spacing` is. `sheet_delta` can
//! come out negative (the "sheet" actually grows slightly) whenever
//! `spacing / 2 > margin` - that's not a bug, it's this same cancellation:
//! the part's own padding already provides more edge clearance than the
//! requested margin asks for, so the sheet needs less inward shrink to
//! compensate (occasionally none at all, or a hair of outward growth).
//!
//! **Placements stay valid for the true geometry, unpadded.** A placement's
//! `(rotation, x, y)` is a rigid transform computed against the *padded*
//! shape, but since padding doesn't recenter or reposition a polygon (it
//! grows the boundary uniformly around the same location), the true and
//! padded shapes share the same local origin. Applying that exact same
//! `(rotation, x, y)` to the *true* shape's own points - not the padded
//! one - lands the true shape in the geometrically correct spot. Nothing
//! downstream (rendering, export) needs the padded geometry at all; it's
//! purely an internal detail of how placement decisions get made.

use crate::clipper::offset_round;
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, is_rectangle};

/// Grows (or shrinks) an already-exact axis-aligned rectangle by `delta` on
/// every side via plain arithmetic - no Clipper2 call, no join type, no
/// extra points, no possible deviation from an exact rectangle at all.
///
/// This matters downstream, not just as an optimization: `inner_nfp`'s fast
/// rectangular-container path only fires when a shape's area matches its own
/// bounding box within 0.1% (the same tolerance the original JS app uses -
/// not a Rust-specific looseness). `offset_bevel` chamfers every corner
/// regardless of angle, unlike a miter join (which only cuts a corner when
/// it's too acute for the miter limit, leaving an ordinary right angle
/// untouched) - so it never produces an exact rectangle, even from one. On
/// a small shape relative to the clearance delta (confirmed with a 100mm
/// square at a 6.5mm spacing, exactly the "part padded to the same size as
/// its sheet" case `docs/PORT_STATUS.md`'s two-parameter clearance design
/// was built around) that chamfer is a big enough fraction of the bounding
/// box to miss the 0.1% tolerance, forcing every sheet/rectangular-part fit
/// check through the general-fallback NFP algorithm - which fails outright
/// on this specific degenerate case (sheet and part identical, zero
/// placement freedom). Skipping the offset entirely for real rectangles
/// keeps them exact, so this fast path - and this exact-fit guarantee -
/// keeps firing reliably, the same as it would for a miter-joined rectangle.
fn offset_rectangle_exact(polygon: &[Point], delta: f64) -> Option<Vec<Point>> {
    if polygon.len() != 4 || !is_rectangle(polygon, None) {
        return None;
    }
    let bounds = get_polygon_bounds(polygon)?;
    let (min_x, max_x) = (bounds.x - delta, bounds.x + bounds.width + delta);
    let (min_y, max_y) = (bounds.y - delta, bounds.y + bounds.height + delta);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    Some(
        polygon
            .iter()
            .map(|p| {
                let x = if (p.x - bounds.x).abs() <= (p.x - (bounds.x + bounds.width)).abs() { min_x } else { max_x };
                let y = if (p.y - bounds.y).abs() <= (p.y - (bounds.y + bounds.height)).abs() { min_y } else { max_y };
                Point::new(x, y)
            })
            .collect(),
    )
}

/// The offset both `prepare_part` and `prepare_sheet` use.
///
/// **It has to grow by *exactly* `delta`, not "at most".** `prepare_sheet`
/// nets the part padding back out by growing the sheet by `spacing / 2`, so
/// any part whose own padding falls short of that somewhere puts that part of
/// its true outline off the real material when placed flush - see
/// `clipper::offset_round`, which is why this is the round join and not the
/// cheaper bevel one.
fn offset_clearance(polygon: &[Point], delta: f64) -> Vec<Vec<Point>> {
    if delta == 0.0 {
        return vec![polygon.to_vec()];
    }
    if let Some(exact) = offset_rectangle_exact(polygon, delta) {
        return vec![exact];
    }
    offset_round(polygon, delta)
}

/// Prepares a sheet boundary for nesting: insets (or, when `spacing / 2 >
/// margin`, slightly grows) it so a part padded by `prepare_part` ends up
/// exactly `margin` from the sheet's true edge, independent of `spacing`.
/// `None` only if the resulting inset collapses the sheet to nothing
/// (e.g. a margin larger than the sheet itself).
///
/// Uses `offset_bevel`, not the plain miter-join `offset` - see its doc
/// comment for why a clearance buffer needs a spike-free join specifically.
/// An exact rectangle skips Clipper2 entirely - see `offset_rectangle_exact`.
pub fn prepare_sheet(sheet: &[Point], margin: f64, spacing: f64) -> Option<Vec<Point>> {
    let delta = spacing / 2.0 - margin;
    offset_clearance(sheet, delta).into_iter().next()
}

/// Prepares a part's outer boundary for nesting: grows it outward by half
/// the spacing, so two parts placed this way end up with the full
/// `spacing` between their true outlines. Holes aren't touched - spacing is
/// a keep-out zone around the *outside* of a part for inter-part
/// clearance, unrelated to interior features. `None` only if the offset
/// degenerates (not expected for a positive/zero outward offset on a
/// simple closed profile).
///
/// Uses `offset_round`, not the plain miter-join `offset` - a sliver-shaped
/// part with a sharp tip would otherwise grow far more than `spacing` at
/// that tip (confirmed against real fixture parts: up to +44mm at a
/// spacing of 6.5mm), potentially making an obviously-fitting part get
/// reported as too big to place. Not `offset_bevel` either, which has the
/// same no-spike property but *under*-grows at a sharp corner, and
/// `prepare_sheet` compensates for this padding assuming it is exact - see
/// `clipper::offset_round` for the 1.5mm of real overhang that caused. An
/// exact rectangular part skips Clipper2 entirely - see
/// `offset_rectangle_exact`.
pub fn prepare_part(part_outer: &[Point], spacing: f64) -> Option<Vec<Point>> {
    offset_clearance(part_outer, spacing / 2.0).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polygon::get_polygon_bounds;

    fn square(x: f64, y: f64, size: f64) -> Vec<Point> {
        vec![Point::new(x, y), Point::new(x + size, y), Point::new(x + size, y + size), Point::new(x, y + size)]
    }

    #[test]
    fn zero_margin_zero_spacing_is_a_true_no_op() {
        let sheet = square(0.0, 0.0, 100.0);
        let part = square(0.0, 0.0, 10.0);

        let prepared_sheet = prepare_sheet(&sheet, 0.0, 0.0).unwrap();
        let prepared_part = prepare_part(&part, 0.0).unwrap();

        assert_eq!(prepared_sheet, sheet, "zero margin/spacing must not touch the sheet at all");
        assert_eq!(prepared_part, part, "zero spacing must not touch the part at all");
    }

    #[test]
    fn full_sheet_size_part_fits_exactly_on_a_same_size_sheet_at_zero_margin() {
        // The concrete case that motivated the two-parameter design: a part
        // exactly the sheet's size should be placeable with zero waste, as
        // long as margin is 0 - regardless of what spacing is set to.
        let sheet_size = 2440.0;
        let part_size = 2440.0;
        for spacing in [0.0, 6.5, 20.0] {
            let sheet = square(0.0, 0.0, sheet_size);
            let part = square(0.0, 0.0, part_size);

            let prepared_sheet = prepare_sheet(&sheet, 0.0, spacing).expect("sheet prep should succeed");
            let prepared_part = prepare_part(&part, spacing).expect("part prep should succeed");

            let sheet_bounds = get_polygon_bounds(&prepared_sheet).unwrap();
            let part_bounds = get_polygon_bounds(&prepared_part).unwrap();

            // the padded part's bounding box must be exactly the padded
            // sheet's bounding box (touching exactly, not overflowing) -
            // i.e. the true part fits with exactly zero margin
            assert!((sheet_bounds.width - part_bounds.width).abs() < 1e-6, "spacing={spacing}: sheet width {} vs part width {}", sheet_bounds.width, part_bounds.width);
            assert!((sheet_bounds.height - part_bounds.height).abs() < 1e-6, "spacing={spacing}: sheet height {} vs part height {}", sheet_bounds.height, part_bounds.height);
        }
    }

    #[test]
    fn margin_alone_governs_edge_clearance_independent_of_spacing() {
        // A part touching the padded sheet's boundary should end up with
        // its TRUE edge exactly `margin` inside the TRUE sheet edge, no
        // matter what spacing is - spacing must cancel out of the
        // part-vs-sheet relationship entirely.
        let margin = 3.0;
        let sheet_size = 200.0;
        let true_sheet_edge = 0.0; // the original, unpadded sheet's corner

        let mut true_edge_clearances = Vec::new();
        for spacing in [0.0, 6.5, 20.0] {
            let sheet = square(true_sheet_edge, true_sheet_edge, sheet_size);
            let prepared_sheet = prepare_sheet(&sheet, margin, spacing).expect("sheet prep should succeed");
            let sheet_bounds = get_polygon_bounds(&prepared_sheet).unwrap();

            // a padded part placed flush against the padded sheet's min-x
            // corner (the tightest valid position) has its PADDED edge
            // exactly at sheet_bounds.x; its TRUE edge is inset from that
            // by half the spacing (how far prepare_part grows a part)
            let padded_part_edge = sheet_bounds.x;
            let true_part_edge = padded_part_edge + spacing / 2.0;
            true_edge_clearances.push(true_part_edge - true_sheet_edge);
        }

        for clearance in &true_edge_clearances {
            assert!((clearance - margin).abs() < 1e-6, "expected true edge clearance to always be margin ({margin}), got {clearance}");
        }
    }

    #[test]
    fn spacing_alone_governs_part_to_part_clearance() {
        let spacing = 6.5;
        let a = square(0.0, 0.0, 20.0);
        let b = square(0.0, 0.0, 15.0);

        let padded_a = prepare_part(&a, spacing).expect("a prep should succeed");
        prepare_part(&b, spacing).expect("b prep should succeed");

        let bounds_a = get_polygon_bounds(&padded_a).unwrap();

        // place padded_b immediately to the right of padded_a, touching
        let b_x = bounds_a.x + bounds_a.width;
        // true edges: a's true right edge is spacing/2 inside its padded
        // right edge; b's true left edge is spacing/2 inside its padded
        // left edge (which sits at b_x)
        let true_a_right_edge = (bounds_a.x + bounds_a.width) - spacing / 2.0;
        let true_b_left_edge = b_x + spacing / 2.0;

        assert!((true_b_left_edge - true_a_right_edge - spacing).abs() < 1e-6, "true parts should end up exactly `spacing` apart, got {}", true_b_left_edge - true_a_right_edge);
    }

    #[test]
    fn a_sharp_sliver_does_not_grow_far_beyond_spacing_at_its_tip() {
        // Regression test: found against real DXF parts (several sliver
        // profiles in tests/fixtures/*.dxf grew by 15-44mm instead of the
        // expected ~6.5mm at a spacing of 6.5, before prepare_part switched
        // from offset (miter join) to a spike-free join). A long, thin
        // triangle with a very acute tip is the minimal case that reproduces
        // it: a miter join's spike length is unbounded as the corner angle
        // shrinks (capped only by the miter limit, e.g. 4x the offset), so
        // the bounding box could grow by many times `spacing` right at the
        // tip. A round join grows by exactly `spacing / 2` everywhere,
        // corner or not - see `clipper::offset_round`'s doc comment.
        let spacing = 6.5;
        let sliver = vec![Point::new(0.0, 0.0), Point::new(200.0, 1.0), Point::new(0.0, 2.0)];

        let true_bounds = get_polygon_bounds(&sliver).unwrap();
        let padded = prepare_part(&sliver, spacing).expect("sliver prep should succeed");
        let padded_bounds = get_polygon_bounds(&padded).unwrap();

        let w_growth = padded_bounds.width - true_bounds.width;
        let h_growth = padded_bounds.height - true_bounds.height;
        // Expected growth per axis is ~spacing (offset outward by
        // spacing/2 on each side); allow a little slack for the bevel
        // join's own corner cut, but nowhere near the old miter blowup.
        assert!(w_growth < spacing * 1.5, "width grew by {w_growth}, expected roughly {spacing}");
        assert!(h_growth < spacing * 1.5, "height grew by {h_growth}, expected roughly {spacing}");
    }
}
