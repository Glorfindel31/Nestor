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
use crate::simplify::simplify;
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, is_rectangle, polygon_area};

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
///
/// **Keeps the largest resulting ring**, the same convention
/// `clipper::clean_polygon` uses. An inward offset can split a concave
/// outline into several disjoint pieces - a U-shaped offcut from
/// `remnant::sheet_remnants`, stored by the library as a `Role::Sheet`, does
/// exactly this once `margin` exceeds half the notch width. Clipper's output
/// order is not area order, so taking the first ring can hand back a sliver
/// (measured: a 6800mm2 U-shaped offcut at margin 15 splits into rings of
/// 1mm2 and 2mm2, and the first one is the 1) and every part then reports as
/// unplaced on a sheet that visibly has material on it.
///
/// ponytail: largest ring, not every ring. The engine takes one polygon per
/// sheet, so representing a split offcut as two independent sheets is a
/// change to the sheet model, not to this function. Largest-ring is strictly
/// better than arbitrary and is what a caller means by "the" offcut; upgrade
/// path if split offcuts ever need their full area is to have the library
/// store each ring as its own sheet at remnant time.
fn offset_clearance(polygon: &[Point], delta: f64) -> Option<Vec<Point>> {
    if delta == 0.0 {
        return Some(polygon.to_vec());
    }
    if let Some(exact) = offset_rectangle_exact(polygon, delta) {
        return Some(exact);
    }
    offset_round(polygon, delta).into_iter().max_by(|a, b| polygon_area(a).abs().total_cmp(&polygon_area(b).abs()))
}

/// Prepares a sheet boundary for nesting: insets (or, when `spacing / 2 >
/// margin`, slightly grows) it so a part padded by `prepare_part` ends up
/// exactly `margin` from the sheet's true edge, independent of `spacing`.
/// `None` only if the resulting inset collapses the sheet to nothing
/// (e.g. a margin larger than the sheet itself).
///
/// Uses `offset_round`, not the plain miter-join `offset` - see
/// `clipper::offset_round` for why a clearance buffer needs a join that is
/// spike-free *and* never under-grows (`offset_bevel` has the first property
/// but not the second, and cost 71 fatal audit issues when it was tried).
/// An exact rectangle skips Clipper2 entirely - see `offset_rectangle_exact`.
/// A concave sheet that the inset splits keeps its largest piece - see
/// `offset_clearance`.
pub fn prepare_sheet(sheet: &[Point], margin: f64, spacing: f64) -> Option<Vec<Point>> {
    let delta = spacing / 2.0 - margin;
    offset_clearance(sheet, delta)
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
    let exact = offset_clearance(part_outer, spacing / 2.0)?;
    // **Capped by the buffer itself, so zero spacing stays a true no-op** - at
    // `spacing == 0` a part must come back exactly as it went in.
    let slack = simplification_slack(part_outer).min(spacing / 2.0);
    if slack <= 0.0 || exact.len() < SIMPLIFY_ABOVE {
        return Some(exact);
    }
    // Douglas-Peucker moves a vertex by at most its tolerance, so simplifying
    // by exactly the slack we over-offset by lands somewhere between the true
    // buffer and the over-grown one - never inside the true buffer. Outward is
    // the safe direction: a part that thinks it is slightly bigger than it is
    // can only refuse a placement, never allow an overlap.
    let Some(padded) = offset_clearance(part_outer, spacing / 2.0 + slack) else { return Some(exact) };
    let simplified = simplify(&padded, Some(slack), true);
    // If it did not actually shed vertices, the exact buffer is strictly
    // better - same cost, no over-growth.
    if simplified.len() >= exact.len() {
        return Some(exact);
    }
    Some(simplified)
}

/// Padded outlines with fewer vertices than this are returned exactly, never
/// over-grown and simplified.
///
/// **This is what keeps `prepare_sheet` and `prepare_part` complementary for
/// the shapes where that is load-bearing.** A part exactly the size of its
/// sheet has to still fit at margin 0, which requires the part's padding and
/// the sheet's inset to cancel exactly - and any over-growth breaks that. In
/// practice that case is a rectangle, which `offset_rectangle_exact` pads to
/// four points, so anything at or below a simple polygon's vertex count opts
/// out and keeps the old behaviour bit for bit. It also opts out precisely
/// where simplification had nothing to offer: the win is superlinear in
/// vertex count, and there is none to be had at sixteen.
const SIMPLIFY_ABOVE: usize = 32;

/// How much a padded outline is allowed to be over-grown and then simplified
/// back down.
///
/// **This is the single biggest lever on NFP cost in the engine.** The NFP is
/// a Minkowski sum whose cost is superlinear in the vertex count, and
/// `offset_round`'s round join is generous with vertices on anything curved:
/// `curvy.dxf`'s 303-point outline pads to 559 points, and its self-NFP takes
/// 114 seconds. Over-offsetting by a tenth of a millimetre and simplifying
/// back leaves 177 points and takes 1.5 seconds - the same shape to within a
/// rounding error nobody cutting metal can hold, 75 times faster.
///
/// A tenth of a millimetre is chosen against real spacings, which are
/// millimetres, but it would be a fifth of a half-millimetre part - so it is
/// also capped at half a percent of the part's own shorter side. Parts that
/// small are dominated by their own kerf anyway.
fn simplification_slack(part_outer: &[Point]) -> f64 {
    /// The most a padded outline is allowed to drift from its true offset.
    const MAX_SLACK: f64 = 0.1;
    /// ...and the least, so a degenerate bounding box cannot drive it to zero
    /// and reinstate the full vertex count.
    const MIN_SLACK: f64 = 0.01;
    let Some(bounds) = get_polygon_bounds(part_outer) else { return MIN_SLACK };
    (bounds.width.min(bounds.height) * 0.005).clamp(MIN_SLACK, MAX_SLACK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polygon::get_polygon_bounds;

    fn square(x: f64, y: f64, size: f64) -> Vec<Point> {
        vec![Point::new(x, y), Point::new(x + size, y), Point::new(x + size, y + size), Point::new(x, y + size)]
    }

    /// **The invariant the whole simplification rests on, and the one thing
    /// the audit cannot check.** `prepare_part` over-offsets by `slack` and
    /// then simplifies by `slack`, which is only safe if the result still
    /// contains the true `spacing / 2` buffer everywhere. If it ever came in
    /// under that, two parts could be placed closer than the spacing asks -
    /// and `audit` would not notice, because it pads through this very same
    /// function and would agree with the mistake.
    ///
    /// So this compares against a buffer built independently, straight from
    /// `offset_round`, and asserts nothing of it pokes outside what
    /// `prepare_part` returned.
    #[test]
    fn a_prepared_part_always_contains_the_exact_spacing_buffer() {
        let shapes: Vec<(&str, Vec<Point>)> = vec![
            ("square", square(0.0, 0.0, 50.0)),
            // A sliver with a sharp tip - the case `offset_round` exists for.
            ("sliver", vec![Point::new(0.0, 0.0), Point::new(200.0, 2.0), Point::new(0.0, 4.0)]),
            // A concave U, so an inward-moving simplification would show.
            (
                "concave U",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(100.0, 0.0),
                    Point::new(100.0, 100.0),
                    Point::new(70.0, 100.0),
                    Point::new(70.0, 20.0),
                    Point::new(30.0, 20.0),
                    Point::new(30.0, 100.0),
                    Point::new(0.0, 100.0),
                ],
            ),
            // Something curved and vertex-dense, which is what the
            // simplification is actually aimed at.
            ("circle-ish", (0..240).map(|i| {
                let t = f64::from(i) / 240.0 * std::f64::consts::TAU;
                Point::new(150.0 + 120.0 * t.cos(), 150.0 + 95.0 * t.sin())
            }).collect()),
        ];

        for (name, shape) in &shapes {
            for spacing in [0.0, 1.0, 5.0, 12.0] {
                let prepared = prepare_part(shape, spacing).unwrap_or_else(|| panic!("{name} at spacing {spacing} should offset"));
                // The exact buffer, built without going through prepare_part.
                let exact = crate::clipper::offset_round(shape, spacing / 2.0);
                let outside = crate::clipper::difference_polygons(&exact, std::slice::from_ref(&prepared), clipper2::FillRule::NonZero)
                    .expect("difference should compute");
                let leaked: f64 = outside.iter().map(|r| polygon_area(r).abs()).sum();
                assert!(
                    leaked < 1e-6,
                    "{name} at spacing {spacing}: {leaked:.6} sq mm of the exact buffer falls outside the prepared outline - parts could be placed closer than the spacing allows"
                );
            }
        }
    }

    /// A U-shaped offcut - what `remnant::sheet_remnants` hands back and the
    /// library stores as a `Role::Sheet`. Once `margin` exceeds half the
    /// notch width the inset splits it in two, and Clipper's output order is
    /// not area order, so taking the first ring silently returned the smaller
    /// piece. Every part then reports unplaced on a sheet with material on it.
    #[test]
    fn a_split_concave_sheet_keeps_its_largest_piece() {
        let u = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(70.0, 100.0),
            Point::new(70.0, 20.0),
            Point::new(30.0, 20.0),
            Point::new(30.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let rings = crate::clipper::offset_round(&u, -15.0);
        assert!(rings.len() > 1, "this margin must actually split the offcut, or the test proves nothing");
        let biggest = rings.iter().map(|r| crate::polygon::polygon_area(r).abs()).fold(0.0, f64::max);

        let prepared = prepare_sheet(&u, 15.0, 0.0).expect("a split sheet still has usable area");
        assert!(
            (crate::polygon::polygon_area(&prepared).abs() - biggest).abs() < 1e-6,
            "kept {} of a possible {biggest}",
            crate::polygon::polygon_area(&prepared).abs()
        );
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
