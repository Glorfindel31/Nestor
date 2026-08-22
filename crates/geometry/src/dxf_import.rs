//! DXF entity -> polygon-tree conversion (replaces SVG import per the scope
//! change recorded in docs/PORT_STATUS.md). Does the same shape of work
//! `svgparser.js` did for SVG - closed-profile detection, parent/hole
//! containment nesting, `.isCircle` metadata for the circular-hole NFP fast
//! path (see circular_nfp.rs) - just against DXF entities instead of an SVG
//! DOM, via the `dxf` crate.
//!
//! Supported, one entity to one profile (`entity_to_polygon`): `LWPOLYLINE`
//! and the older `POLYLINE`, both closed and both including bulge/arc
//! segments, `CIRCLE`, and full-sweep `ARC` (treated as a circle, same as
//! SVG's isCircle handling for a `<circle>`-equivalent full arc).
//!
//! Supported across entities (`entities_to_polygons_chained`): a profile that
//! only becomes closed once loose `LINE`s, partial `ARC`s and open polylines
//! are walked end to end. That is a graph walk over what the per-entity pass
//! rejected, so it is a second pass rather than another match arm - see
//! `chain_edges`.
//!
//! Supported before either (`expand_inserts`): `INSERT` block references,
//! including nested blocks and the `MINSERT` array form. Block bodies live in
//! `Drawing::blocks()`, not `Drawing::entities()`, so this needs the whole
//! drawing rather than an entity iterator.
//!
//! Still not supported: `SPLINE` and `ELLIPSE`; 3D polylines and polyface
//! meshes are rejected deliberately rather than flattened onto XY. A ring
//! produced by chaining carries no `real_boundary`, so its arcs export as
//! their tessellation rather than as true arcs - reconstructing bulges from
//! chained segments is a follow-up, not an oversight.
//!
//! **`TEXT`/`MTEXT` are carried through, not converted to profiles**: these
//! entities have no closed boundary (nothing to nest against), so they don't
//! become `LayeredPolygon` nodes themselves - they're attached to whichever
//! profile's boundary contains their insertion point (`attach_texts`, reusing
//! the exact same containment logic `build_polygon_tree` already uses for
//! hole nesting) and carried in that node's own `texts` field. This is what
//! makes a part's engraved label/part-number move and rotate correctly with
//! it through nesting - `rotate_layered_polygon`/`shift_layered_polygon`
//! transform `texts` right alongside `points`/`children`, and
//! `dxf_export::add_node` writes them back out on export. A text entity
//! whose insertion point falls outside every profile (no containing shape at
//! all) is dropped - there's no "ownerless floating annotation" concept
//! anywhere else in this pipeline for it to attach to.

use dxf::entities::{Entity, EntityType};
use dxf::{Drawing, Point as DxfPoint};

use crate::circular_nfp::Circle;
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, point_in_polygon, polygon_area, Bounds};

/// One `TEXT`/`MTEXT` entity, reduced to the fields that matter for
/// nesting: where it sits and how it's oriented (both of which must move
/// with whatever part it's attached to), its content, and its height (font
/// size scales with the drawing, not with the part's rotation, so it's
/// carried through unchanged). `is_multiline` records which DXF entity type
/// it came from, so export can round-trip it as the same kind rather than
/// silently converting every text to a single-line `TEXT`; DXF-specific
/// formatting beyond that (columns, background fill, text style) isn't
/// preserved - a real simplification, not a bug, matching this module's
/// existing "reduce to what nesting needs" approach for circles.
#[derive(Clone, Debug, PartialEq)]
pub struct TextAnnotation {
    pub position: Point,
    /// Degrees. Note this is already normalized to DXF `TEXT`'s convention
    /// even for text parsed from an `MTEXT` entity - see `entity_to_text`'s
    /// doc comment for the radians-vs-degrees quirk between the two.
    pub rotation_deg: f64,
    pub height: f64,
    pub value: String,
    pub is_multiline: bool,
}

/// One original LWPOLYLINE vertex, kept verbatim (not tessellated) so a
/// rounded-corner part can be written back out on export as a real arc
/// instead of a many-sided polygon approximation. `bulge` is DXF's own
/// `tan(included_angle / 4)` encoding of the arc from this vertex to the
/// next (`0.0` for a plain straight segment) - see `tessellate_bulge`'s
/// doc comment for the convention. A chord-relative ratio, so it's
/// invariant under rotation/translation: only `point` needs transforming
/// in `rotate_layered_polygon`/`shift_layered_polygon`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RealVertex {
    pub point: Point,
    pub bulge: f64,
}

/// A closed profile extracted from one or more DXF entities, tagged with its
/// source layer and (for holes) nested children. Mirrors the `.children` /
/// `.isCircle` shape `svgparser.js` produced for SVG polygons.
#[derive(Clone, Debug, PartialEq)]
pub struct LayeredPolygon {
    pub points: Vec<Point>,
    pub layer: String,
    pub is_circle: Option<Circle>,
    pub children: Vec<LayeredPolygon>,
    /// Text/label entities whose insertion point falls inside this specific
    /// node's boundary - see `attach_texts`.
    pub texts: Vec<TextAnnotation>,
    /// The original (untessellated) LWPOLYLINE vertex/bulge list this node
    /// came from, if it came from one - lets `dxf_export` write a real arc
    /// back out on export instead of `points`' tessellated approximation.
    /// `None` for anything without a real boundary to retain: a bare
    /// circle/full-sweep arc (those have `is_circle` instead, which export
    /// checks first), an SVG-imported shape (no DXF-bulge equivalent),
    /// unsupported entity types, or a hand-built shape from the UI.
    /// Nothing in `nesting` ever reads this - it only exists to survive
    /// the round trip from import to export unpadded and untouched.
    pub real_boundary: Option<Vec<RealVertex>>,
}

impl LayeredPolygon {
    pub(crate) fn new(points: Vec<Point>, layer: String, is_circle: Option<Circle>) -> Self {
        LayeredPolygon {
            points,
            layer,
            is_circle,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }
}

/// Port of `background.js`'s `rotatePolygon`, extended to a `LayeredPolygon`
/// (points + recursive children, same as the original recursing into
/// `polygon.children`). Carries `isCircle` metadata through rotation (center
/// rotates, radius is invariant) - without this, a rotated circular hole/part
/// loses its fast-path eligibility in `inner_nfp`'s circular-disk dispatch.
pub fn rotate_layered_polygon(poly: &LayeredPolygon, degrees: f64) -> LayeredPolygon {
    let angle = degrees * std::f64::consts::PI / 180.0;
    let (sin, cos) = angle.sin_cos();
    let points = poly
        .points
        .iter()
        .map(|p| Point::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos))
        .collect();
    let children = poly.children.iter().map(|c| rotate_layered_polygon(c, degrees)).collect();
    let is_circle = poly.is_circle.map(|c| Circle {
        cx: c.cx * cos - c.cy * sin,
        cy: c.cx * sin + c.cy * cos,
        r: c.r,
    });
    // Text rotates the same way a circle's center does (position) plus its
    // own rotation angle accumulates - a label glued to a part must end up
    // reading in the same direction relative to the part, no matter how
    // many times the part itself gets rotated during placement search.
    let texts = poly
        .texts
        .iter()
        .map(|t| TextAnnotation {
            position: Point::new(t.position.x * cos - t.position.y * sin, t.position.x * sin + t.position.y * cos),
            rotation_deg: (t.rotation_deg + degrees).rem_euclid(360.0),
            height: t.height,
            value: t.value.clone(),
            is_multiline: t.is_multiline,
        })
        .collect();
    let real_boundary = poly.real_boundary.as_ref().map(|verts| {
        verts
            .iter()
            .map(|v| RealVertex { point: Point::new(v.point.x * cos - v.point.y * sin, v.point.x * sin + v.point.y * cos), bulge: v.bulge })
            .collect()
    });

    LayeredPolygon {
        points,
        layer: poly.layer.clone(),
        is_circle,
        children,
        texts,
        real_boundary,
    }
}

/// Port of `background.js`'s `shiftPolygon`: translates a polygon (and its
/// holes) by `(dx, dy)`. Non-destructive, same as the original. **Disclosed
/// non-bit-for-bit divergence**: the original has no `.isCircle` handling at
/// all and its shifted output silently loses circle metadata; this version
/// translates `is_circle`'s center along with the points instead (a real
/// circle's center really should move with it). Harmless today - nothing
/// downstream of this function reads `is_circle` on its result - but if a
/// future caller ever re-feeds a shifted polygon back into the circular-hole
/// NFP fast path, it'll see live metadata where the original would not have.
pub fn shift_layered_polygon(poly: &LayeredPolygon, dx: f64, dy: f64) -> LayeredPolygon {
    let points = poly.points.iter().map(|p| Point::new(p.x + dx, p.y + dy)).collect();
    let children = poly.children.iter().map(|c| shift_layered_polygon(c, dx, dy)).collect();
    let is_circle = poly.is_circle.map(|c| Circle { cx: c.cx + dx, cy: c.cy + dy, r: c.r });
    let texts = poly
        .texts
        .iter()
        .map(|t| TextAnnotation { position: Point::new(t.position.x + dx, t.position.y + dy), ..t.clone() })
        .collect();
    let real_boundary = poly.real_boundary.as_ref().map(|verts| verts.iter().map(|v| RealVertex { point: Point::new(v.point.x + dx, v.point.y + dy), bulge: v.bulge }).collect());

    LayeredPolygon {
        points,
        layer: poly.layer.clone(),
        is_circle,
        children,
        texts,
        real_boundary,
    }
}

/// Reflects a part across the Y axis (`x -> -x`) - the "flip it over"
/// variant the nest may place instead of the original when the material has
/// no good side (see `nesting::dispatch`'s rotation-gene decoding for how a
/// run opts into it). Not a rotation: no rotation angle produces a
/// reflection, which is exactly why a mirrored variant can fit where none of
/// the rotations do.
///
/// Point order is reversed, not just negated: a reflection alone flips
/// winding direction, and the Clipper2 `NonZero` fill rule downstream reads
/// an outline's winding relative to its holes' - flipping every ring's
/// winding but keeping the point order would silently change what counts as
/// material. Reversing restores the original winding.
pub fn mirror_layered_polygon(poly: &LayeredPolygon) -> LayeredPolygon {
    let points = poly.points.iter().rev().map(|p| Point::new(-p.x, p.y)).collect();
    let children = poly.children.iter().map(mirror_layered_polygon).collect();
    let is_circle = poly.is_circle.map(|c| Circle { cx: -c.cx, cy: c.cy, r: c.r });
    // ponytail: label geometry is mirrored along with the part (position
    // and baseline direction), which is what physically happens to an
    // engraved label on a flipped part - the glyphs are not re-laid-out to
    // read left-to-right again.
    let texts = poly
        .texts
        .iter()
        .map(|t| TextAnnotation { position: Point::new(-t.position.x, t.position.y), rotation_deg: (180.0 - t.rotation_deg).rem_euclid(360.0), ..t.clone() })
        .collect();
    // `bulge` is a *signed* arc encoding (`tan(angle/4)`, positive = CCW)
    // attached to the segment leaving each vertex, so the reversal above
    // has to move it as well as flip it: reflecting negates the sign,
    // traversing the segment backwards negates it again (net: unchanged),
    // and the segment that used to leave vertex `src` now leaves the vertex
    // that follows it in the reversed list - i.e. each reversed vertex
    // inherits its *predecessor's* bulge.
    let real_boundary = poly.real_boundary.as_ref().map(|verts| {
        let n = verts.len();
        (0..n)
            .map(|j| {
                let src = n - 1 - j;
                RealVertex { point: Point::new(-verts[src].point.x, verts[src].point.y), bulge: verts[(src + n - 1) % n].bulge }
            })
            .collect()
    });

    LayeredPolygon { points, layer: poly.layer.clone(), is_circle, children, texts, real_boundary }
}

/// Port of `background.js`'s `polygonMaterialArea`: polygon area minus the
/// area of its holes, clamped to non-negative (matches `Math.max(0, ...)`).
pub fn polygon_material_area(poly: &LayeredPolygon) -> f64 {
    let mut area = polygon_area(&poly.points).abs();
    for child in &poly.children {
        area -= polygon_area(&child.points).abs();
    }
    area.max(0.0)
}

/// Minimum angular step per tessellated arc segment, regardless of how loose
/// `curve_tolerance` is - keeps degenerate/huge-tolerance inputs from
/// collapsing an arc to a single chord.
const MIN_ARC_SEGMENTS: u32 = 2;
/// Upper bound on tessellation segments for one arc/circle, so a tiny
/// `curve_tolerance` on a huge-radius circle can't runaway-allocate.
const MAX_ARC_SEGMENTS: u32 = 720;

/// The max angular step (radians) that keeps the chord-to-arc sagitta error
/// within `tolerance` for the given `radius` (basic circular chord-error
/// bound: error ~= r*(1 - cos(dtheta/2))).
fn arc_step_angle(radius: f64, tolerance: f64) -> f64 {
    let r = radius.abs().max(1e-9);
    let ratio = (1.0 - (tolerance / r)).clamp(-1.0, 1.0);
    (2.0 * ratio.acos()).max(0.001)
}

pub(crate) fn segment_count(total_angle: f64, radius: f64, tolerance: f64) -> u32 {
    let step = arc_step_angle(radius, tolerance);
    let n = (total_angle.abs() / step).ceil() as u32;
    n.clamp(MIN_ARC_SEGMENTS, MAX_ARC_SEGMENTS)
}

/// Tessellates a full circle into a closed polygon, starting at angle 0
/// (matching the plan's "circle tessellation always starts on the boundary"
/// invariant that `circular_nfp`'s fast path depends on).
pub fn tessellate_circle(cx: f64, cy: f64, r: f64, tolerance: f64) -> Vec<Point> {
    let n = segment_count(2.0 * std::f64::consts::PI, r, tolerance);
    (0..n)
        .map(|i| {
            let theta = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
            Point::new(cx + r * theta.cos(), cy + r * theta.sin())
        })
        .collect()
}

/// Converts a DXF LWPOLYLINE bulge segment (from `p0` to `p1`) into the
/// intermediate points of its arc, excluding both endpoints (the caller
/// already has them). `bulge` is `tan(included_angle / 4)`; positive = CCW,
/// negative = CW, by DXF convention.
fn tessellate_bulge(p0: Point, p1: Point, bulge: f64, tolerance: f64) -> Vec<Point> {
    if bulge == 0.0 {
        return Vec::new();
    }

    let dx = p1.x - p0.x;
    let dy = p1.y - p0.y;
    let chord = dx.hypot(dy);
    if chord < 1e-12 {
        return Vec::new();
    }

    let theta = 4.0 * bulge.atan(); // signed included angle
    let sagitta = bulge * chord / 2.0; // exact identity: sagitta = tan(theta/4) * chord/2
    let radius = (sagitta * sagitta + (chord / 2.0) * (chord / 2.0)) / (2.0 * sagitta);

    let ux = dx / chord;
    let uy = dy / chord;
    let nx = -uy; // perpendicular, 90 deg CCW from chord direction
    let ny = ux;

    let mx = (p0.x + p1.x) / 2.0;
    let my = (p0.y + p1.y) / 2.0;
    let cx = mx + nx * (radius - sagitta);
    let cy = my + ny * (radius - sagitta);

    let start_angle = (p0.y - cy).atan2(p0.x - cx);
    let n = segment_count(theta, radius, tolerance);

    (1..n)
        .map(|i| {
            let a = start_angle + theta * (i as f64) / (n as f64);
            Point::new(cx + radius.abs() * a.cos(), cy + radius.abs() * a.sin())
        })
        .collect()
}

/// Builds a `LayeredPolygon::real_boundary` list straight off the same
/// vertex slice `lwpolyline_to_points` tessellates, so it's automatically
/// consistent with whatever closed-loop handling (e.g.
/// `closes_itself_by_duplicate_point`) the caller already applied to that
/// slice - no separate dedup logic needed here.
fn real_boundary_from_vertices(verts: &[dxf::LwPolylineVertex]) -> Vec<RealVertex> {
    verts.iter().map(|v| RealVertex { point: Point::new(v.x, v.y), bulge: v.bulge }).collect()
}

/// The older `POLYLINE`/`VERTEX` entity pair, reduced to the same
/// `(point, bulge)` list an `LWPOLYLINE` gives us. The `dxf` crate has
/// already re-assembled the trailing `VERTEX` entities and their `SEQEND`
/// into `Polyline`'s own vertex list by the time we see it, so there is no
/// entity-stream state machine to write here - the two entity types differ
/// only in how their vertices are spelled, and every consumer below works on
/// `RealVertex` so neither needs its own tessellation path.
fn polyline_real_vertices(poly: &dxf::entities::Polyline) -> Vec<RealVertex> {
    poly.vertices().map(|v| RealVertex { point: Point::new(v.location.x, v.location.y), bulge: v.bulge }).collect()
}

fn lwpolyline_to_points(verts: &[RealVertex], is_closed: bool, tolerance: f64) -> Vec<Point> {
    let mut points = Vec::with_capacity(verts.len());
    let n = verts.len();

    for i in 0..n {
        let p0 = verts[i].point;
        points.push(p0);

        // only emit the arc between this vertex and the next if there IS a
        // next vertex to connect to (the last vertex only connects onward
        // when the polyline is closed, wrapping back to vertex 0)
        let has_next = i + 1 < n || is_closed;
        if has_next && verts[i].bulge != 0.0 {
            let p1 = verts[if i + 1 < n { i + 1 } else { 0 }].point;
            points.extend(tessellate_bulge(p0, p1, verts[i].bulge, tolerance));
        }
    }

    points
}

/// True if `poly`'s DXF closed flag is unset but its own vertex list already
/// closes itself by repeating the first vertex's coordinates as the last -
/// a real-world export quirk (confirmed against
/// github.com/christianp/aperiodic-monotile's `hat-monotile.dxf`: a tool
/// that emits an "open" polyline but duplicates the first point at the end
/// instead of setting the closed bit and omitting the duplicate). The
/// duplicate point itself must still be dropped before treating the loop as
/// closed - see the `entity_to_polygon` call site.
fn closes_itself_by_duplicate_point(verts: &[RealVertex]) -> bool {
    match (verts.first(), verts.last()) {
        (Some(first), Some(last)) if verts.len() >= 4 => {
            (first.point.x - last.point.x).abs() < 1e-9 && (first.point.y - last.point.y).abs() < 1e-9
        }
        _ => false,
    }
}

/// Shared tail of every polyline-shaped entity arm: turn a `(point, bulge)`
/// list plus its closed flag into a closed profile, or `None` if it isn't
/// one. Keeps `LWPOLYLINE` and the older `POLYLINE` on literally the same
/// code path rather than two near-copies that can drift.
fn polyline_profile(verts: &[RealVertex], is_closed: bool, layer: String, tolerance: f64) -> Option<LayeredPolygon> {
    // Drop the redundant closing vertex (identical to the first) before
    // treating the loop as closed - otherwise it'd produce a zero-length
    // final edge back to vertex 0.
    let verts = if !is_closed && closes_itself_by_duplicate_point(verts) { &verts[..verts.len() - 1] } else { verts };
    if !is_closed && !closes_itself_by_duplicate_point(verts) && verts.len() == verts.len() {
        // fall through - the caller decides; see `entity_to_polygon`.
    }
    let points = lwpolyline_to_points(verts, true, tolerance);
    if points.len() < 3 {
        return None;
    }
    Some(LayeredPolygon { real_boundary: Some(verts.to_vec()), ..LayeredPolygon::new(points, layer, None) })
}

/// Samples a DXF `SPLINE` into points.
///
/// **Why this is here rather than a crate.** A DXF spline is a clamped,
/// possibly rational B-spline: control points, a knot vector, and optional
/// weights. Evaluating one is de Boor's algorithm, which is the twenty lines
/// below - and nothing else in a NURBS library would be used. The `dxf` crate
/// parses the fields and stops there.
///
/// Real CAD files lean on this constantly. Anything drawn with a freehand or
/// fitted curve exports as `SPLINE`, and without this such a file imports as
/// no geometry at all rather than as a bad approximation of some.
///
/// Rational splines (a `weight_values` list of the right length) are handled
/// in homogeneous coordinates; the far more common non-rational case skips
/// that entirely.
#[must_use]
pub fn tessellate_spline(spline: &dxf::entities::Spline, tolerance: f64) -> Vec<Point> {
    let degree = spline.degree_of_curve.max(1) as usize;
    let control: Vec<Point> = spline.control_points.iter().map(|p| Point::new(p.x, p.y)).collect();
    let knots = &spline.knot_values;
    // A valid clamped B-spline has exactly `points + degree + 1` knots. Junk
    // that does not satisfy that cannot be evaluated, and guessing at a repair
    // would invent geometry the drawing does not contain.
    if control.len() <= degree || knots.len() != control.len() + degree + 1 {
        return Vec::new();
    }
    let weights: Option<&Vec<f64>> = (spline.weight_values.len() == control.len()).then_some(&spline.weight_values);

    // The curve only exists over `knots[degree] ..= knots[n]`; the repeated
    // knots either side are the clamping.
    let (lo, hi) = (knots[degree], knots[control.len()]);
    if !(hi > lo) {
        return Vec::new();
    }

    // One sample budget per knot span, from how far that span's control
    // points wander - a nearly straight span needs two points, a tight curl
    // needs many. `segment_count` on the span's own polygon length against
    // `tolerance` is the same sagitta rule the arc and bulge tessellators use.
    let mut points: Vec<Point> = Vec::new();
    for span in degree..control.len() {
        let (a, b) = (knots[span], knots[span + 1]);
        if b <= a {
            continue; // repeated knot: no parameter range, no samples
        }
        let reach: f64 = control[span - degree..=span].windows(2).map(|w| w[0].distance_to(w[1])).sum();
        let steps = segment_count(1.0, reach, tolerance).max(2);
        for i in 0..steps {
            let t = a + (b - a) * (i as f64) / (steps as f64);
            points.push(de_boor(t, degree, &control, knots, weights));
        }
    }
    points.push(de_boor(hi, degree, &control, knots, weights));
    points.dedup_by(|a, b| a.within_distance(*b, 1e-9));
    points
}

/// One point on a B-spline, by de Boor's algorithm.
fn de_boor(t: f64, degree: usize, control: &[Point], knots: &[f64], weights: Option<&Vec<f64>>) -> Point {
    // The span `t` falls in: the last index whose knot is still <= t, clamped
    // so the very end of the curve evaluates on the final span rather than
    // running off the end of the array.
    let last = control.len() - 1;
    let mut span = degree;
    for k in degree..control.len() {
        if t >= knots[k] {
            span = k;
        }
    }
    span = span.min(last);

    // Work in homogeneous coordinates so a rational spline needs no separate
    // path: a non-rational one is simply every weight = 1.
    let w = |i: usize| weights.map_or(1.0, |v| v[i]);
    let mut d: Vec<(f64, f64, f64)> = (0..=degree)
        .map(|j| {
            let i = span + j - degree;
            let wi = w(i);
            (control[i].x * wi, control[i].y * wi, wi)
        })
        .collect();

    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = span + j - degree;
            let denom = knots[i + degree + 1 - r] - knots[i];
            let alpha = if denom.abs() < 1e-12 { 0.0 } else { (t - knots[i]) / denom };
            let (p, q) = (d[j - 1], d[j]);
            d[j] = (p.0 + (q.0 - p.0) * alpha, p.1 + (q.1 - p.1) * alpha, p.2 + (q.2 - p.2) * alpha);
        }
    }

    let (x, y, weight) = d[degree];
    if weight.abs() < 1e-12 {
        Point::new(x, y)
    } else {
        Point::new(x / weight, y / weight)
    }
}

/// Whether a sampled spline comes back to where it started.
///
/// The `is_closed` flag is the documented way to say so and real files do not
/// always set it - LibreCAD writes `flags 4096` on a closed loop, which is not
/// any documented spline flag - so the geometry is the authority and the flag
/// is a hint.
fn spline_is_closed(spline: &dxf::entities::Spline, points: &[Point]) -> bool {
    if spline.is_closed() || spline.is_periodic() {
        return true;
    }
    matches!((points.first(), points.last()), (Some(a), Some(b)) if a.within_distance(*b, 1e-6) && points.len() > 2)
}

/// True if an ARC's angular sweep is a full circle (some DXF exporters
/// represent circles as a 0-360 degree ARC rather than a CIRCLE entity).
fn arc_is_full_circle(arc: &dxf::entities::Arc) -> bool {
    let sweep = (arc.end_angle - arc.start_angle).rem_euclid(360.0);
    sweep < 1e-6 || (360.0 - sweep) < 1e-6
}

/// Converts one DXF entity into a closed profile, if it represents one.
/// Returns `None` for entity types that aren't (yet) supported, or that
/// aren't closed (e.g. an open LWPOLYLINE or a partial ARC) - see the module
/// doc comment for what's deliberately not handled yet.
pub fn entity_to_polygon(entity: &Entity, curve_tolerance: f64) -> Option<LayeredPolygon> {
    let layer = entity.common.layer.clone();

    match &entity.specific {
        EntityType::LwPolyline(poly) => polyline_profile(&real_boundary_from_vertices(&poly.vertices), poly.is_closed(), layer, curve_tolerance),
        // The older POLYLINE entity, same treatment - see
        // `polyline_real_vertices`. 3D polylines and polyface/polygon meshes
        // share the entity type but are not planar profiles, so they're
        // rejected outright rather than silently flattened onto XY.
        EntityType::Polyline(poly) if !poly.is_3d_polyline() && !poly.is_3d_polygon_mesh() && !poly.is_polyface_mesh() => {
            polyline_profile(&polyline_real_vertices(poly), poly.is_closed(), layer, curve_tolerance)
        }
        // A spline that comes back to its own start is a profile in its own
        // right; one that does not is an edge for `entity_to_edge` to chain.
        EntityType::Spline(spline) => {
            let mut points = tessellate_spline(spline, curve_tolerance);
            if !spline_is_closed(spline, &points) {
                return None;
            }
            // Drop the duplicated closing point: every consumer here treats a
            // point list as implicitly closed, and keeping it would leave a
            // zero-length final edge.
            if matches!((points.first(), points.last()), (Some(a), Some(b)) if a.within_distance(*b, 1e-6)) {
                points.pop();
            }
            if points.len() < 3 {
                return None;
            }
            Some(LayeredPolygon::new(points, layer, None))
        }
        EntityType::Circle(circle) => {
            let points = tessellate_circle(circle.center.x, circle.center.y, circle.radius, curve_tolerance);
            let meta = Circle {
                cx: circle.center.x,
                cy: circle.center.y,
                r: circle.radius,
            };
            Some(LayeredPolygon::new(points, layer, Some(meta)))
        }
        EntityType::Arc(arc) if arc_is_full_circle(arc) => {
            let points = tessellate_circle(arc.center.x, arc.center.y, arc.radius, curve_tolerance);
            let meta = Circle {
                cx: arc.center.x,
                cy: arc.center.y,
                r: arc.radius,
            };
            Some(LayeredPolygon::new(points, layer, Some(meta)))
        }
        _ => None,
    }
}

/// One open, already-tessellated edge waiting to be chained into a closed
/// ring - a `LINE`'s two endpoints, a partial `ARC`'s tessellated sweep, or
/// an open polyline that never closed itself. Carries `layer` because
/// chaining is per-layer (see `chain_edges`).
#[derive(Clone, Debug)]
struct Edge {
    points: Vec<Point>,
    layer: String,
}

impl Edge {
    fn start(&self) -> Point {
        self.points[0]
    }

    fn end(&self) -> Point {
        self.points[self.points.len() - 1]
    }

    fn reversed(&self) -> Edge {
        Edge { points: self.points.iter().rev().copied().collect(), layer: self.layer.clone() }
    }
}

/// Tessellates a partial (non-full-circle) ARC into an open point list,
/// endpoints included. DXF arcs always sweep counter-clockwise from
/// `start_angle` to `end_angle`, both in degrees.
fn arc_to_points(arc: &dxf::entities::Arc, tolerance: f64) -> Vec<Point> {
    let start = arc.start_angle.to_radians();
    let sweep = (arc.end_angle - arc.start_angle).rem_euclid(360.0).to_radians();
    let n = segment_count(sweep, arc.radius, tolerance);
    (0..=n)
        .map(|i| {
            let theta = start + sweep * (i as f64) / (n as f64);
            Point::new(arc.center.x + arc.radius * theta.cos(), arc.center.y + arc.radius * theta.sin())
        })
        .collect()
}

/// An open polyline is just a many-point edge - a real profile is often
/// drawn as two or three open polylines meeting end to end, not only as
/// bare LINEs.
fn open_polyline_edge(verts: &[RealVertex], is_closed: bool, layer: String, tolerance: f64) -> Option<Edge> {
    if is_closed || closes_itself_by_duplicate_point(verts) || verts.len() < 2 {
        return None; // already a closed profile (or too short to be an edge)
    }
    Some(Edge { points: lwpolyline_to_points(verts, false, tolerance), layer })
}

/// The open edge an entity contributes to the chaining pass, if any.
/// Deliberately only the cases `entity_to_polygon` itself rejects - anything
/// that already converts to a closed profile on its own never reaches here
/// (see `entities_to_polygons_chained`).
fn entity_to_edge(entity: &Entity, curve_tolerance: f64) -> Option<Edge> {
    let layer = entity.common.layer.clone();
    match &entity.specific {
        EntityType::Line(line) => {
            let a = Point::new(line.p1.x, line.p1.y);
            let b = Point::new(line.p2.x, line.p2.y);
            if a.within_distance(b, 1e-9) {
                return None; // zero-length line: no direction, nothing to chain
            }
            Some(Edge { points: vec![a, b], layer })
        }
        EntityType::Arc(arc) if !arc_is_full_circle(arc) => Some(Edge { points: arc_to_points(arc, curve_tolerance), layer }),
        EntityType::Spline(spline) => {
            let points = tessellate_spline(spline, curve_tolerance);
            // A closed spline is already a profile - `entity_to_polygon` took
            // it - and offering it here as well would let the chainer emit the
            // same ring twice.
            (points.len() >= 2 && !spline_is_closed(spline, &points)).then_some(Edge { points, layer })
        }
        EntityType::LwPolyline(poly) => open_polyline_edge(&real_boundary_from_vertices(&poly.vertices), poly.is_closed(), layer, curve_tolerance),
        EntityType::Polyline(poly) if !poly.is_3d_polyline() && !poly.is_3d_polygon_mesh() && !poly.is_polyface_mesh() => {
            open_polyline_edge(&polyline_real_vertices(poly), poly.is_closed(), layer, curve_tolerance)
        }
        _ => None,
    }
}

/// Joins open edges into closed rings: the case where a profile only exists
/// once its `LINE`/`ARC`/open-polyline pieces are walked end to end. Real CAD
/// files export profiles this way constantly, and every one of those entities
/// is individually meaningless to `entity_to_polygon`.
///
/// Greedy walk, not a global optimisation: take any unused edge, keep
/// extending from its tail by whichever unused neighbour touches it within
/// `epsilon`, and emit a ring the moment the walk arrives back at its own
/// start. A walk that runs out of neighbours without closing is
/// **discarded** - an unclosed chain is not a profile, and inventing a
/// closing segment for it would silently fabricate material that isn't in
/// the drawing.
///
/// **Chaining never crosses layers.** Layer identity has to survive import
/// (cut/etch/drill are different operations), so a `CUT` line and a `DRILL`
/// arc that happen to share an endpoint are not two halves of one ring.
///
/// `epsilon` is the endpoint-join radius. It is derived from
/// `curve_tolerance` (see `entities_to_polygons_chained`), **not** from
/// `polygon::TOL`: 1e-9 is a float-noise tolerance, while real CAD endpoints
/// that are meant to coincide routinely differ by far more than that after a
/// round trip through another tool. A join radius that tight would reject
/// exactly the files this pass exists to rescue.
///
/// ponytail: O(n^2) endpoint matching, and first-match-wins where several
/// edges meet at one point. Real files bring hundreds of loose edges, not
/// millions, and this runs once per import - reach for a spatial hash (and a
/// smarter branch choice) only if a real fixture makes it hurt.
fn chain_edges(edges: Vec<Edge>, epsilon: f64) -> Vec<LayeredPolygon> {
    let mut used = vec![false; edges.len()];
    let mut rings = Vec::new();

    for seed in 0..edges.len() {
        if used[seed] {
            continue;
        }
        used[seed] = true;
        let mut walk = edges[seed].clone();

        loop {
            if walk.points.len() >= 4 && walk.end().within_distance(walk.start(), epsilon) {
                // Closed. Drop the duplicated closing point - a
                // `LayeredPolygon` never repeats its first vertex.
                walk.points.pop();
                rings.push(LayeredPolygon::new(std::mem::take(&mut walk.points), walk.layer.clone(), None));
                break;
            }

            let tail = walk.end();
            let next = edges
                .iter()
                .enumerate()
                .find(|(i, e)| !used[*i] && e.layer == walk.layer && (e.start().within_distance(tail, epsilon) || e.end().within_distance(tail, epsilon)));

            match next {
                Some((i, edge)) => {
                    used[i] = true;
                    let oriented = if edge.start().within_distance(tail, epsilon) { edge.clone() } else { edge.reversed() };
                    // Skip the shared endpoint itself, keep the rest.
                    walk.points.extend_from_slice(&oriented.points[1..]);
                }
                // Ran out of neighbours without closing: not a profile.
                None => break,
            }
        }
    }

    rings
}

/// `entities_to_polygons`, plus a second pass that chains whatever it
/// rejected (loose `LINE`s, partial `ARC`s, open polylines) into closed
/// rings. This is the function a whole-drawing caller wants;
/// `entities_to_polygons` stays as-is for callers that only want the
/// one-entity-one-profile conversion.
///
/// Two passes rather than one because they are genuinely different problems:
/// the first is a pure per-entity map with no cross-entity state, the second
/// is a graph walk over everything the first one couldn't use.
pub fn entities_to_polygons_chained<'a>(entities: impl Iterator<Item = &'a Entity>, curve_tolerance: f64) -> Vec<LayeredPolygon> {
    // Collected rather than taking `impl Iterator + Clone`: `Drawing::entities()`
    // isn't `Clone`, and both passes need to see every entity.
    let entities: Vec<&Entity> = entities.collect();
    let mut polygons = Vec::new();
    let mut leftovers = Vec::new();
    for entity in entities {
        match entity_to_polygon(entity, curve_tolerance) {
            Some(polygon) => polygons.push(polygon),
            None => leftovers.extend(entity_to_edge(entity, curve_tolerance)),
        }
    }
    polygons.extend(chain_edges(leftovers, curve_tolerance));
    polygons
}

/// A 2x3 affine transform, just enough for `INSERT` expansion.
/// Deliberately local and minimal rather than a general matrix type in
/// `geometry` - the plan dropped the Electron app's `matrix.ts` outright, and
/// two call sites don't justify bringing it back. (`svg_import` has its own
/// private equivalent for the same reason.)
#[derive(Clone, Copy, Debug)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Affine {
    const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    /// `self` applied after `inner` - i.e. `self * inner`.
    fn then(self, inner: Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    fn apply(self, p: DxfPoint) -> DxfPoint {
        DxfPoint::new(self.a * p.x + self.c * p.y + self.e, self.b * p.x + self.d * p.y + self.f, p.z)
    }

    /// The scale this transform applies to a radius. A non-uniform INSERT
    /// scale would turn a circle into an ellipse, which no `Circle`/`Arc`
    /// entity can represent - see `transform_entity` for how that case is
    /// handled instead of silently picking one axis.
    fn uniform_scale(self) -> Option<f64> {
        let sx = self.a.hypot(self.b);
        let sy = self.c.hypot(self.d);
        if (sx - sy).abs() < 1e-9 {
            Some(sx)
        } else {
            None
        }
    }

    fn rotation_deg(self) -> f64 {
        self.b.atan2(self.a).to_degrees()
    }
}

/// How deep nested `INSERT`s are followed before giving up. Blocks
/// referencing each other (directly or in a cycle) is malformed but does
/// occur in the wild, and without a cap it is an infinite loop, not an error.
const MAX_INSERT_DEPTH: usize = 8;

/// Applies `xf` to one entity's geometry, returning the transformed copy.
/// Only the entity kinds this module actually consumes are transformed;
/// anything else is passed through untouched so it gets dropped downstream
/// exactly as it would have been anyway.
fn transform_entity(entity: &Entity, xf: Affine, insert_layer: &str, curve_tolerance: f64) -> Entity {
    let mut out = entity.clone();
    // DXF's layer-0 inheritance rule: geometry drawn on layer "0" inside a
    // block takes the layer of the INSERT that placed it, rather than staying
    // on "0". Anything drawn on a named layer keeps that layer. Getting this
    // wrong silently moves a block's cut lines onto the wrong operation.
    if out.common.layer == "0" {
        out.common.layer = insert_layer.to_string();
    }

    out.specific = match &entity.specific {
        EntityType::Line(line) => {
            let mut l = line.clone();
            l.p1 = xf.apply(line.p1.clone());
            l.p2 = xf.apply(line.p2.clone());
            EntityType::Line(l)
        }
        EntityType::LwPolyline(poly) => {
            let mut p = poly.clone();
            for v in &mut p.vertices {
                let t = xf.apply(DxfPoint::new(v.x, v.y, 0.0));
                v.x = t.x;
                v.y = t.y;
            }
            EntityType::LwPolyline(p)
        }
        EntityType::Polyline(poly) => {
            let mut p = poly.clone();
            for v in p.vertices_mut() {
                v.location = xf.apply(v.location.clone());
            }
            EntityType::Polyline(p)
        }
        EntityType::Circle(circle) => match xf.uniform_scale() {
            Some(scale) => {
                let mut c = circle.clone();
                c.center = xf.apply(circle.center.clone());
                c.radius = circle.radius * scale;
                EntityType::Circle(c)
            }
            // Non-uniform scale turns the circle into an ellipse. Rather than
            // emit a wrong circle, tessellate it into an LWPOLYLINE and
            // transform the points - the shape stays right, it just loses its
            // `is_circle` fast-path metadata.
            None => tessellated_as_polyline(&tessellate_circle(circle.center.x, circle.center.y, circle.radius, curve_tolerance), xf, true),
        },
        EntityType::Arc(arc) => match xf.uniform_scale() {
            Some(scale) => {
                let mut a = arc.clone();
                a.center = xf.apply(arc.center.clone());
                a.radius = arc.radius * scale;
                a.start_angle = arc.start_angle + xf.rotation_deg();
                a.end_angle = arc.end_angle + xf.rotation_deg();
                EntityType::Arc(a)
            }
            None => {
                let pts = if arc_is_full_circle(arc) {
                    tessellate_circle(arc.center.x, arc.center.y, arc.radius, curve_tolerance)
                } else {
                    arc_to_points(arc, curve_tolerance)
                };
                tessellated_as_polyline(&pts, xf, arc_is_full_circle(arc))
            }
        },
        EntityType::Text(text) => {
            let mut t = text.clone();
            t.location = xf.apply(text.location.clone());
            t.rotation += xf.rotation_deg();
            t.text_height *= xf.uniform_scale().unwrap_or(1.0);
            EntityType::Text(t)
        }
        EntityType::MText(text) => {
            let mut t = text.clone();
            t.insertion_point = xf.apply(text.insertion_point.clone());
            t.rotation_angle += xf.rotation_deg().to_radians();
            t.initial_text_height *= xf.uniform_scale().unwrap_or(1.0);
            EntityType::MText(t)
        }
        other => other.clone(),
    };
    out
}

/// Wraps already-tessellated points as an LWPOLYLINE with `xf` applied - the
/// escape hatch for curves a non-uniform INSERT scale can't keep as curves.
fn tessellated_as_polyline(points: &[Point], xf: Affine, closed: bool) -> EntityType {
    let mut poly = dxf::entities::LwPolyline {
        vertices: points
            .iter()
            .map(|p| {
                let t = xf.apply(DxfPoint::new(p.x, p.y, 0.0));
                dxf::LwPolylineVertex { x: t.x, y: t.y, bulge: 0.0, ..Default::default() }
            })
            .collect(),
        ..Default::default()
    };
    poly.set_is_closed(closed);
    EntityType::LwPolyline(poly)
}

/// Expands every `INSERT` in a drawing's model space into the block's own
/// entities, transformed into place. Model-space entities that aren't
/// `INSERT`s pass through unchanged.
///
/// This needs the whole `Drawing`, not an entity iterator, because block
/// bodies live in `drawing.blocks()` and are **not** part of
/// `drawing.entities()` - an INSERT on its own carries only a block *name*.
///
/// Handles MINSERT (the `column_count` x `row_count` array form) by emitting
/// one transformed copy per cell, and nested INSERTs by recursing, capped at
/// `MAX_INSERT_DEPTH`.
pub fn expand_inserts(drawing: &Drawing, curve_tolerance: f64) -> Vec<Entity> {
    let mut out = Vec::new();
    for entity in drawing.entities() {
        expand_entity(drawing, entity, Affine::IDENTITY, None, 0, curve_tolerance, &mut out);
    }
    out
}

fn expand_entity(drawing: &Drawing, entity: &Entity, xf: Affine, insert_layer: Option<&str>, depth: usize, curve_tolerance: f64, out: &mut Vec<Entity>) {
    let EntityType::Insert(insert) = &entity.specific else {
        // Plain geometry. At depth 0 with the identity transform this is a
        // clone-only no-op, which is what keeps a drawing with no blocks in it
        // behaving exactly as it did before this pass existed.
        out.push(transform_entity(entity, xf, insert_layer.unwrap_or(&entity.common.layer), curve_tolerance));
        return;
    };

    if depth >= MAX_INSERT_DEPTH {
        return; // cyclic or absurdly nested blocks - drop rather than hang
    }
    let Some(block) = drawing.blocks().find(|b| b.name == insert.name) else {
        return; // dangling block reference: nothing to draw
    };

    let (sx, sy) = (insert.x_scale_factor, insert.y_scale_factor);
    let (sin, cos) = insert.rotation.to_radians().sin_cos();
    // Column/row spacing is measured in the INSERT's own rotated frame, same
    // as AutoCAD's MINSERT.
    for col in 0..insert.column_count.max(1) {
        for row in 0..insert.row_count.max(1) {
            let ox = insert.column_spacing * col as f64;
            let oy = insert.row_spacing * row as f64;
            // base_point removal, then scale, then rotate, then place.
            let local = Affine {
                a: cos * sx,
                b: sin * sx,
                c: -sin * sy,
                d: cos * sy,
                e: insert.location.x + cos * ox - sin * oy - (cos * sx * block.base_point.x - sin * sy * block.base_point.y),
                f: insert.location.y + sin * ox + cos * oy - (sin * sx * block.base_point.x + cos * sy * block.base_point.y),
            };
            let combined = xf.then(local);
            let layer = insert_layer.unwrap_or(&entity.common.layer);
            for inner in &block.entities {
                expand_entity(drawing, inner, combined, Some(layer), depth + 1, curve_tolerance, out);
            }
        }
    }
}

/// Converts every closed-profile-capable entity in `entities` into a flat
/// list of `LayeredPolygon`s (no parent/hole nesting yet - see
/// `build_polygon_tree`).
pub fn entities_to_polygons<'a>(
    entities: impl Iterator<Item = &'a Entity>,
    curve_tolerance: f64,
) -> Vec<LayeredPolygon> {
    entities
        .filter_map(|e| entity_to_polygon(e, curve_tolerance))
        .collect()
}

/// Converts one `TEXT`/`MTEXT` entity into a `TextAnnotation`, if it is one.
///
/// **DXF rotation-unit quirk, load-bearing, easy to get backwards**: `TEXT`'s
/// group code 50 (`rotation`) is in **degrees**, but `MTEXT`'s group code 50
/// (`rotation_angle`) is in **radians** - same group code, different unit,
/// per the DXF reference. `TextAnnotation::rotation_deg` always normalizes to
/// degrees regardless of source entity, so every other function in this
/// module (`rotate_layered_polygon`, export) can treat rotation uniformly
/// without needing to know which entity type a given text came from.
fn entity_to_text(entity: &Entity) -> Option<TextAnnotation> {
    match &entity.specific {
        EntityType::Text(text) => Some(TextAnnotation {
            position: Point::new(text.location.x, text.location.y),
            rotation_deg: text.rotation,
            height: text.text_height,
            value: text.value.clone(),
            is_multiline: false,
        }),
        EntityType::MText(mtext) => Some(TextAnnotation {
            position: Point::new(mtext.insertion_point.x, mtext.insertion_point.y),
            rotation_deg: mtext.rotation_angle.to_degrees(),
            height: mtext.initial_text_height,
            value: mtext.text.clone(),
            is_multiline: true,
        }),
        _ => None,
    }
}

/// Extracts every `TEXT`/`MTEXT` entity in `entities` - the counterpart to
/// `entities_to_polygons` for the entity kinds that carry no closed profile
/// of their own. Attach the result to a built polygon tree via
/// `attach_texts`.
pub fn entities_to_texts<'a>(entities: impl Iterator<Item = &'a Entity>) -> Vec<TextAnnotation> {
    entities.filter_map(entity_to_text).collect()
}

/// True if every point of `candidate` lies inside-or-on `container`'s
/// boundary (containment test used to build the parent/hole tree).
///
/// **Whole-polygon, not just the first point** - real CAD-exported cutouts
/// routinely share a coincident vertex or a touching edge with their parent
/// (LibreCAD/Onshape snapping, tangent-arc drill holes, etc.). Testing only
/// `candidate`'s first point meant a hole whose *first* vertex happened to
/// land exactly on the parent's boundary got rejected as "not contained" and
/// was promoted to a standalone root/part - confirmed against a real 137-loop
/// fixture where exactly the loops touching their parent's boundary escaped
/// this way. `point_in_polygon` returns `None` for "on the boundary /
/// coincident vertex" (ambiguous, not "outside") - this treats that as
/// contained (touching but not crossing), only `Some(false)` (strictly
/// outside) disqualifies a candidate.
fn contains(container: &[Point], candidate: &[Point]) -> bool {
    // `LayeredPolygon`'s fields are all `pub`, so a degenerate (empty-points)
    // entry is a valid `Vec<LayeredPolygon>` for a caller to hand the public
    // `build_polygon_tree` - an empty candidate trivially can't be "inside"
    // anything, no need to look at `container` at all.
    if candidate.is_empty() {
        return false;
    }
    let zero = Point::new(0.0, 0.0);
    candidate.iter().all(|&p| point_in_polygon(p, container, zero, None) != Some(false))
}

fn area_of(points: &[Point]) -> f64 {
    polygon_area(points).abs()
}

/// Finds the tightest (smallest-area) node in `nodes` (searched depth-first)
/// that contains `poly`, returning the path of child indices from `nodes`
/// down to it. `nodes` must already be free of any polygon smaller than
/// `poly` (see `build_polygon_tree`'s largest-to-smallest insertion order),
/// so "deepest match" is also "tightest match." Returns indices rather than
/// a reference so the caller can do a single mutable descent afterward -
/// recursing on `&mut` with a "try deeper, else use this node" fallback hits
/// a real borrow-checker limitation (the recursive call's lifetime pins the
/// whole subtree even on the path that doesn't use it).
fn find_container_path(nodes: &[LayeredPolygon], poly: &[Point]) -> Vec<usize> {
    match nodes.iter().position(|n| contains(&n.points, poly)) {
        Some(idx) => {
            let mut path = vec![idx];
            path.extend(find_container_path(&nodes[idx].children, poly));
            path
        }
        None => Vec::new(),
    }
}

fn get_mut_by_path<'a>(nodes: &'a mut [LayeredPolygon], path: &[usize]) -> &'a mut LayeredPolygon {
    let (&first, rest) = path.split_first().expect("path must be non-empty");
    let node = &mut nodes[first];
    if rest.is_empty() {
        node
    } else {
        get_mut_by_path(&mut node.children, rest)
    }
}

/// Builds the parent/hole tree for a flat set of closed profiles via
/// containment (mirrors `svgparser.js`'s parent/hole detection). Polygons
/// are nested arbitrarily deep - a hole containing a smaller "island" shape
/// becomes that island's parent, same as nested SVG paths.
pub fn build_polygon_tree(mut flat: Vec<LayeredPolygon>) -> Vec<LayeredPolygon> {
    // largest-area first, so every already-placed node is a valid (non-too-small) candidate parent
    flat.sort_by(|a, b| area_of(&b.points).total_cmp(&area_of(&a.points)));

    let mut roots: Vec<LayeredPolygon> = Vec::new();
    for poly in flat {
        let path = find_container_path(&roots, &poly.points);
        if path.is_empty() {
            roots.push(poly);
        } else {
            get_mut_by_path(&mut roots, &path).children.push(poly);
        }
    }
    roots
}

/// Attaches each text to whichever node in `roots` most tightly contains its
/// insertion point - reuses `find_container_path`'s exact containment/depth
/// logic (a single-point "candidate" is a degenerate case it already handles
/// correctly, since `contains` only ever reads the candidate's first point).
/// A text whose insertion point falls outside every profile is dropped - see
/// the module doc comment for why.
pub fn attach_texts(roots: &mut [LayeredPolygon], texts: Vec<TextAnnotation>) {
    for text in texts {
        let path = find_container_path(roots, std::slice::from_ref(&text.position));
        if !path.is_empty() {
            get_mut_by_path(roots, &path).texts.push(text);
        }
    }
}

/// Port of the plan's "oversized-part bbox check": true if `part`'s bounds
/// don't fit within `sheet_bounds` in either dimension (part can never be
/// placed on this sheet in any rotation-free orientation).
pub fn is_oversized(part: &[Point], sheet_bounds: Bounds) -> bool {
    match get_polygon_bounds(part) {
        Some(b) => b.width > sheet_bounds.width || b.height > sheet_bounds.height,
        None => false,
    }
}

/// Port of `getOversizedParts`'s "either orientation" check
/// (`frontend/ui/services/nesting.service.js`): true only if `part` is too
/// large for `sheet_bounds` at *every* angle in the `rotations`-step grid,
/// not just its as-imported orientation - a long thin part that's wider
/// than the sheet at rotation 0 but fits fine rotated 90 degrees must not
/// read as oversized. `is_oversized` itself stays the single-orientation
/// primitive this builds on.
pub fn is_oversized_at_any_rotation(part: &[Point], sheet_bounds: Bounds, rotations: u32) -> bool {
    let step = 2.0 * std::f64::consts::PI / rotations.max(1) as f64;
    (0..rotations.max(1)).all(|i| {
        let angle = step * i as f64;
        let (sin, cos) = angle.sin_cos();
        let rotated: Vec<Point> = part.iter().map(|p| Point::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos)).collect();
        is_oversized(&rotated, sheet_bounds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dxf::entities::{Arc, Circle as DxfCircle, EntityCommon, LwPolyline};
    use dxf::{LwPolylineVertex, Point as DxfPoint};

    fn entity(layer: &str, specific: EntityType) -> Entity {
        Entity {
            common: EntityCommon {
                layer: layer.to_string(),
                ..Default::default()
            },
            specific,
        }
    }

    #[test]
    fn circle_entity_tessellates_and_carries_is_circle_metadata() {
        let e = entity(
            "CUT",
            EntityType::Circle(DxfCircle {
                center: DxfPoint::new(5.0, 5.0, 0.0),
                radius: 3.0,
                ..Default::default()
            }),
        );

        let poly = entity_to_polygon(&e, 0.01).expect("circle should convert");
        assert_eq!(poly.layer, "CUT");
        let circle = poly.is_circle.expect("circle metadata expected");
        assert_eq!(circle, Circle { cx: 5.0, cy: 5.0, r: 3.0 });

        let area = polygon_area(&poly.points).abs();
        let expected = std::f64::consts::PI * 3.0 * 3.0;
        // an inscribed polygon under-approximates a circle's area by construction;
        // a tight curve_tolerance (0.01) should keep that error under 1%
        assert!((area - expected).abs() / expected < 0.01);
    }

    #[test]
    fn full_sweep_arc_is_treated_as_a_circle() {
        let e = entity(
            "0",
            EntityType::Arc(Arc {
                center: DxfPoint::new(0.0, 0.0, 0.0),
                radius: 2.0,
                start_angle: 0.0,
                end_angle: 360.0,
                ..Default::default()
            }),
        );

        let poly = entity_to_polygon(&e, 0.1).expect("full-sweep arc should convert");
        assert!(poly.is_circle.is_some());
    }

    #[test]
    fn partial_arc_is_not_a_closed_profile() {
        let e = entity(
            "0",
            EntityType::Arc(Arc {
                center: DxfPoint::new(0.0, 0.0, 0.0),
                radius: 2.0,
                start_angle: 0.0,
                end_angle: 90.0,
                ..Default::default()
            }),
        );
        assert!(entity_to_polygon(&e, 0.1).is_none());
    }

    #[test]
    fn open_lwpolyline_is_not_a_closed_profile() {
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 0.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 10.0, y: 0.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(false);
        let e = entity("0", EntityType::LwPolyline(poly));
        assert!(entity_to_polygon(&e, 0.1).is_none());
    }

    /// The real-world export quirk confirmed against
    /// github.com/christianp/aperiodic-monotile's `hat-monotile.dxf`: closed
    /// bit unset, but the first vertex's coordinates are manually repeated
    /// as the last. Must still convert, and the duplicate must not survive
    /// into the output (a triangle has 3 points, not 4).
    #[test]
    fn open_lwpolyline_that_repeats_its_first_point_is_still_closed() {
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 0.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 10.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 5.0, y: 10.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 0.0, y: 0.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(false);
        let e = entity("0", EntityType::LwPolyline(poly));
        let converted = entity_to_polygon(&e, 0.1).expect("should convert despite the unset closed bit");
        assert_eq!(converted.points.len(), 3, "the duplicated closing point must be dropped");
    }

    #[test]
    fn closed_rectangular_lwpolyline_converts_with_no_bulge() {
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 0.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 10.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 10.0, y: 5.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 0.0, y: 5.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(true);
        let e = entity("PROFILE", EntityType::LwPolyline(poly));

        let converted = entity_to_polygon(&e, 0.1).expect("closed rectangle should convert");
        assert_eq!(converted.layer, "PROFILE");
        assert_eq!(converted.points.len(), 4);
        assert!((polygon_area(&converted.points).abs() - 50.0).abs() < 1e-9);
    }

    #[test]
    fn bulge_segment_tessellates_a_half_circle_with_correct_area_contribution() {
        // A closed "D" shape: straight line from (-1,0) to (1,0), then a bulge=1
        // (180 degree, i.e. semicircular) arc back from (1,0) to (-1,0) - bulge=1
        // means the included angle is 4*atan(1) = 180 degrees exactly.
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: -1.0, y: 0.0, bulge: 0.0, ..Default::default() },
                LwPolylineVertex { x: 1.0, y: 0.0, bulge: 1.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(true);
        let e = entity("0", EntityType::LwPolyline(poly));

        let converted = entity_to_polygon(&e, 0.001).expect("D-shape should convert");
        let area = polygon_area(&converted.points).abs();
        // half-disk of radius 1: area = pi/2
        assert!((area - std::f64::consts::FRAC_PI_2).abs() < 0.01, "area was {area}");
    }

    /// Regression test for a sign bug in `tessellate_bulge`'s center
    /// calculation: a non-180-degree bulge (e.g. a quarter-circle rounded
    /// corner, very common on real cut profiles) was computed around the
    /// *wrong* center - the other circle of the same radius that also
    /// happens to pass through both endpoints, curving inward instead of
    /// outward. The bulge=1 (exact semicircle) test above didn't catch this
    /// because that case is sign-degenerate (sagitta == radius in
    /// magnitude, so the sign error multiplies by zero either way).
    #[test]
    fn quarter_circle_bulge_tessellates_around_the_correct_center() {
        // bulge = tan(90deg / 4) = tan(22.5deg): a CCW quarter-circle arc
        // from (1,0) to (0,1), which must be centered at the origin (not at
        // (1,1), the wrong center the sign bug produced).
        let bulge = (std::f64::consts::FRAC_PI_8).tan();
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 1.0, y: 0.0, bulge, ..Default::default() },
                LwPolylineVertex { x: 0.0, y: 1.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(true);
        let e = entity("0", EntityType::LwPolyline(poly));

        let converted = entity_to_polygon(&e, 0.001).expect("quarter-circle profile should convert");
        for p in &converted.points {
            let dist_from_origin = (p.x * p.x + p.y * p.y).sqrt();
            assert!((dist_from_origin - 1.0).abs() < 0.01, "point {p:?} isn't on the unit circle (dist {dist_from_origin})");
        }
    }

    /// A `SPLINE` whose answer is known by construction: the standard
    /// four-arc cubic B-spline circle. Its area has to come out as pi r
    /// squared, which nothing about the evaluator could fake.
    #[test]
    fn a_spline_circle_encloses_the_area_a_circle_should() {
        const R: f64 = 100.0;
        // The standard control polygon for a circle as four cubic Bezier
        // quarter-arcs, with knot multiplicity 3 at each join and the first
        // point repeated to close - the same 13-point/17-knot structure
        // LibreCAD writes, which is what makes this a real check on the
        // evaluator rather than on a hand-rolled approximation. (Two
        // half-circle segments, the tempting shortcut, is a poor enough fit
        // to be 1.8% out on area on its own.)
        const K: f64 = 0.552_284_749_830_793_4; // 4/3 * (sqrt(2) - 1)
        let c = |x: f64, y: f64| dxf::Point::new(x * R, y * R, 0.0);
        let control = vec![
            c(1.0, 0.0), c(1.0, K), c(K, 1.0), c(0.0, 1.0),
            c(-K, 1.0), c(-1.0, K), c(-1.0, 0.0),
            c(-1.0, -K), c(-K, -1.0), c(0.0, -1.0),
            c(K, -1.0), c(1.0, -K), c(1.0, 0.0),
        ];
        let knots = vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 4.0, 4.0, 4.0, 4.0];
        let spline = dxf::entities::Spline { degree_of_curve: 3, control_points: control, knot_values: knots, ..Default::default() };

        let points = tessellate_spline(&spline, 0.01);
        assert!(points.len() > 20, "expected a real tessellation, got {}", points.len());
        let area = crate::polygon::polygon_area(&points).abs();
        let expected = std::f64::consts::PI * R * R;
        assert!(
            (area - expected).abs() / expected < 0.005,
            "a spline circle of radius {R} should enclose {expected:.0}, got {area:.0}"
        );
    }

    /// Degree 1 is a polyline in spline clothing, so the curve must pass
    /// exactly through every control point - the cheapest possible check that
    /// de Boor is indexing its knot spans correctly rather than merely
    /// producing something smooth.
    #[test]
    fn a_degree_one_spline_is_its_own_control_polygon() {
        let spline = dxf::entities::Spline {
            degree_of_curve: 1,
            control_points: vec![dxf::Point::new(0.0, 0.0, 0.0), dxf::Point::new(10.0, 0.0, 0.0), dxf::Point::new(10.0, 5.0, 0.0)],
            knot_values: vec![0.0, 0.0, 1.0, 2.0, 2.0],
            ..Default::default()
        };
        let points = tessellate_spline(&spline, 0.1);
        for corner in [Point::new(0.0, 0.0), Point::new(10.0, 0.0), Point::new(10.0, 5.0)] {
            assert!(points.iter().any(|p| p.within_distance(corner, 1e-6)), "control point {corner:?} missing from {points:?}");
        }
        for p in &points {
            let on_first = p.y.abs() < 1e-6 && (0.0..=10.0).contains(&p.x);
            let on_second = (p.x - 10.0).abs() < 1e-6 && (0.0..=5.0).contains(&p.y);
            assert!(on_first || on_second, "degree 1 must stay on the control polygon, got {p:?}");
        }
    }

    /// A knot vector that does not match the control points is not a spline
    /// this can evaluate. It must decline rather than panic or, worse, invent
    /// a shape - some exporter's malformed entity is not material.
    #[test]
    fn a_spline_with_an_impossible_knot_vector_is_declined_not_guessed_at() {
        let bad = dxf::entities::Spline {
            degree_of_curve: 3,
            control_points: vec![dxf::Point::new(0.0, 0.0, 0.0), dxf::Point::new(1.0, 1.0, 0.0)],
            knot_values: vec![0.0, 1.0],
            ..Default::default()
        };
        assert!(tessellate_spline(&bad, 0.1).is_empty());
        assert!(tessellate_spline(&dxf::entities::Spline::default(), 0.1).is_empty());
    }

    /// **The file this was written for.** `CURVY.dxf` is two closed cubic
    /// splines and nothing else - no polylines, no arcs - and before this the
    /// importer found no geometry in it at all. The small one sits inside the
    /// big one, so it has to come back as a hole rather than a second part.
    #[test]
    fn a_drawing_made_only_of_splines_imports_as_a_profile_with_its_hole() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/curvy.dxf");
        let drawing = dxf::Drawing::load_file(path).expect("fixture should parse");
        let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), 0.02));

        assert_eq!(tree.len(), 1, "one outer profile, not two loose shapes");
        let shape = &tree[0];
        assert_eq!(shape.children.len(), 1, "the inner spline is a hole in the outer one");

        let bounds = crate::polygon::get_polygon_bounds(&shape.points).expect("has points");
        assert!((bounds.width - 477.7).abs() < 1.0, "width {:.2}", bounds.width);
        assert!((bounds.height - 327.0).abs() < 1.0, "height {:.2}", bounds.height);

        // The hole is a circle drawn as a spline: 99.33mm across encloses
        // 7750mm2, and a tessellation that is wrong in any interesting way
        // will not land on that.
        let hole = crate::polygon::polygon_area(&shape.children[0].points).abs();
        assert!((hole - 7750.0).abs() < 40.0, "the round hole should enclose ~7750mm2, got {hole:.0}");
        assert!(polygon_material_area(shape) < crate::polygon::polygon_area(&shape.points).abs(), "the hole must be subtracted from the material area");
    }

    /// `real_boundary` exists so a rounded-corner part can be written back
    /// out on export as a real arc instead of `points`' tessellated
    /// approximation - it must carry the exact original vertex/bulge list,
    /// independent of `curve_tolerance` (a loose tolerance still needs the
    /// real geometry retained for a caller that wants it).
    #[test]
    fn closed_lwpolyline_with_a_bulge_retains_its_real_boundary() {
        let bulge = (std::f64::consts::FRAC_PI_8).tan();
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 1.0, y: 0.0, bulge, ..Default::default() },
                LwPolylineVertex { x: 0.0, y: 1.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(true);
        let e = entity("0", EntityType::LwPolyline(poly));

        let converted = entity_to_polygon(&e, 0.001).expect("quarter-circle profile should convert");
        let real_boundary = converted.real_boundary.expect("closed LWPOLYLINE should retain its real boundary");
        assert_eq!(real_boundary.len(), 2);
        assert_eq!(real_boundary[0].point, Point::new(1.0, 0.0));
        assert!((real_boundary[0].bulge - bulge).abs() < 1e-12);
        assert_eq!(real_boundary[1].point, Point::new(0.0, 1.0));
        assert_eq!(real_boundary[1].bulge, 0.0);
    }

    #[test]
    fn rotate_and_shift_move_real_boundary_points_but_leave_bulge_unchanged() {
        let bulge = 0.5;
        let mut poly = LwPolyline {
            vertices: vec![
                LwPolylineVertex { x: 1.0, y: 0.0, bulge, ..Default::default() },
                LwPolylineVertex { x: 0.0, y: 1.0, bulge: 0.0, ..Default::default() },
            ],
            ..Default::default()
        };
        poly.set_is_closed(true);
        let e = entity("0", EntityType::LwPolyline(poly));
        let converted = entity_to_polygon(&e, 0.1).expect("should convert");

        let rotated = rotate_layered_polygon(&converted, 90.0);
        let rb = rotated.real_boundary.expect("rotation must preserve real_boundary");
        assert!((rb[0].point.x - 0.0).abs() < 1e-9, "x was {}", rb[0].point.x);
        assert!((rb[0].point.y - 1.0).abs() < 1e-9, "y was {}", rb[0].point.y);
        assert_eq!(rb[0].bulge, bulge, "bulge is chord-relative, must be unchanged by rotation");

        let shifted = shift_layered_polygon(&converted, 10.0, 20.0);
        let sb = shifted.real_boundary.expect("shift must preserve real_boundary");
        assert!((sb[0].point.x - 11.0).abs() < 1e-9);
        assert!((sb[0].point.y - 20.0).abs() < 1e-9);
        assert_eq!(sb[0].bulge, bulge, "bulge is chord-relative, must be unchanged by translation");
    }

    #[test]
    fn build_polygon_tree_nests_a_hole_and_an_island_inside_it() {
        // Outer 20x20 square, a 10x10 hole square inside it, and a 2x2 island inside the hole.
        let outer = LayeredPolygon::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(20.0, 0.0),
                Point::new(20.0, 20.0),
                Point::new(0.0, 20.0),
            ],
            "CUT".into(),
            None,
        );
        let hole = LayeredPolygon::new(
            vec![
                Point::new(5.0, 5.0),
                Point::new(15.0, 5.0),
                Point::new(15.0, 15.0),
                Point::new(5.0, 15.0),
            ],
            "DRILL".into(),
            None,
        );
        let island = LayeredPolygon::new(
            vec![
                Point::new(9.0, 9.0),
                Point::new(11.0, 9.0),
                Point::new(11.0, 11.0),
                Point::new(9.0, 11.0),
            ],
            "CUT".into(),
            None,
        );

        let tree = build_polygon_tree(vec![island, outer, hole]);

        assert_eq!(tree.len(), 1, "only the outer square should be a root");
        let root = &tree[0];
        assert_eq!(root.children.len(), 1, "hole should nest directly under the outer square");
        let nested_hole = &root.children[0];
        assert_eq!(nested_hole.layer, "DRILL");
        assert_eq!(nested_hole.children.len(), 1, "island should nest under the hole, not the outer square");
        assert_eq!(nested_hole.children[0].layer, "CUT");
    }

    #[test]
    fn is_oversized_flags_a_part_bigger_than_the_sheet() {
        let part = [
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let sheet = Bounds { x: 0.0, y: 0.0, width: 50.0, height: 50.0 };
        assert!(is_oversized(&part, sheet));

        let small_sheet_fitting_part = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!(!is_oversized(&small_sheet_fitting_part, sheet));
    }

    /// The exact case `is_oversized` alone gets wrong: a long thin part
    /// that's wider than the sheet at rotation 0 but fits fine rotated 90
    /// degrees.
    #[test]
    fn is_oversized_at_any_rotation_tries_rotating_before_giving_up() {
        let long_thin_part = [Point::new(0.0, 0.0), Point::new(80.0, 0.0), Point::new(80.0, 10.0), Point::new(0.0, 10.0)];
        let sheet = Bounds { x: 0.0, y: 0.0, width: 50.0, height: 100.0 };

        assert!(is_oversized(&long_thin_part, sheet), "80x10 shouldn't fit a 50-wide sheet at rotation 0");
        assert!(!is_oversized_at_any_rotation(&long_thin_part, sheet, 4), "rotated 90 degrees it's 10x80, which fits a 50x100 sheet");
    }

    fn square_layered(x: f64, y: f64, size: f64, layer: &str) -> LayeredPolygon {
        LayeredPolygon::new(
            vec![Point::new(x, y), Point::new(x + size, y), Point::new(x + size, y + size), Point::new(x, y + size)],
            layer.to_string(),
            None,
        )
    }

    #[test]
    fn text_entity_converts_with_degrees_rotation_unchanged() {
        let e = entity(
            "LABEL",
            EntityType::Text(dxf::entities::Text {
                location: DxfPoint::new(3.0, 4.0, 0.0),
                value: "PART-42".to_string(),
                rotation: 90.0,
                text_height: 2.5,
                ..Default::default()
            }),
        );

        let text = entity_to_text(&e).expect("TEXT should convert");
        assert_eq!(text.value, "PART-42");
        assert_eq!(text.position, Point::new(3.0, 4.0));
        assert_eq!(text.rotation_deg, 90.0, "TEXT's own rotation is already in degrees - no conversion should happen");
        assert_eq!(text.height, 2.5);
        assert!(!text.is_multiline);
    }

    #[test]
    fn mtext_entity_converts_radians_rotation_to_degrees() {
        // MTEXT's group code 50 is in RADIANS, unlike TEXT's - this is the
        // load-bearing quirk entity_to_text's doc comment calls out.
        let e = entity(
            "LABEL",
            EntityType::MText(dxf::entities::MText {
                insertion_point: DxfPoint::new(1.0, 2.0, 0.0),
                text: "HELLO".to_string(),
                rotation_angle: std::f64::consts::FRAC_PI_2, // 90 degrees, in radians
                initial_text_height: 5.0,
                ..Default::default()
            }),
        );

        let text = entity_to_text(&e).expect("MTEXT should convert");
        assert_eq!(text.value, "HELLO");
        assert!((text.rotation_deg - 90.0).abs() < 1e-9, "rotation_deg was {}", text.rotation_deg);
        assert!(text.is_multiline);
    }

    #[test]
    fn non_text_entity_does_not_convert() {
        let e = entity("0", EntityType::Circle(DxfCircle { center: DxfPoint::origin(), radius: 1.0, ..Default::default() }));
        assert!(entity_to_text(&e).is_none());
    }

    #[test]
    fn attach_texts_attaches_to_the_tightest_containing_shape() {
        // Outer 20x20 square with a 4x4 part inside it - a text sitting
        // inside the small part must attach there, not to the outer square,
        // even though both contain the text's position.
        let mut roots = vec![square_layered(0.0, 0.0, 20.0, "SHEET")];
        roots[0].children.push(square_layered(2.0, 2.0, 4.0, "CUT"));

        let text = TextAnnotation { position: Point::new(3.0, 3.0), rotation_deg: 0.0, height: 1.0, value: "X".into(), is_multiline: false };
        attach_texts(&mut roots, vec![text]);

        assert!(roots[0].texts.is_empty(), "text belongs to the inner part, not the outer sheet");
        assert_eq!(roots[0].children[0].texts.len(), 1, "text should attach to the smaller containing part");
        assert_eq!(roots[0].children[0].texts[0].value, "X");
    }

    #[test]
    fn attach_texts_drops_a_text_with_no_containing_shape() {
        let mut roots = vec![square_layered(0.0, 0.0, 10.0, "CUT")];
        let text = TextAnnotation { position: Point::new(500.0, 500.0), rotation_deg: 0.0, height: 1.0, value: "LOST".into(), is_multiline: false };

        attach_texts(&mut roots, vec![text]); // must not panic

        assert!(roots[0].texts.is_empty());
    }

    #[test]
    fn rotate_layered_polygon_rotates_the_attached_texts_position_and_angle() {
        let mut poly = square_layered(-1.0, -1.0, 2.0, "CUT"); // centered on origin
        poly.texts.push(TextAnnotation { position: Point::new(1.0, 0.0), rotation_deg: 0.0, height: 1.0, value: "T".into(), is_multiline: false });

        let rotated = rotate_layered_polygon(&poly, 90.0);

        let text = &rotated.texts[0];
        assert!(text.position.x.abs() < 1e-9, "x was {}", text.position.x);
        assert!((text.position.y - 1.0).abs() < 1e-9, "y was {}", text.position.y);
        assert!((text.rotation_deg - 90.0).abs() < 1e-9, "a part's own rotation should accumulate onto the text's rotation");
    }

    #[test]
    fn shift_layered_polygon_shifts_the_attached_texts_position_only() {
        let mut poly = square_layered(0.0, 0.0, 10.0, "CUT");
        poly.texts.push(TextAnnotation { position: Point::new(1.0, 1.0), rotation_deg: 45.0, height: 1.0, value: "T".into(), is_multiline: false });

        let shifted = shift_layered_polygon(&poly, 5.0, -2.0);

        let text = &shifted.texts[0];
        assert_eq!(text.position, Point::new(6.0, -1.0));
        assert_eq!(text.rotation_deg, 45.0, "shifting must not touch rotation");
    }

    /// The only non-obvious part of `mirror_layered_polygon`: a reversed
    /// bulge list must still describe the same physical arcs, mirrored. Both
    /// a wrong sign and a wrong index shift produce a plausible-looking
    /// polygon that bulges the wrong way on export, which no area/bounds
    /// check would catch.
    #[test]
    fn mirroring_preserves_arcs_and_winding() {
        let v = vec![
            RealVertex { point: Point::new(0.0, 0.0), bulge: 0.4 },
            RealVertex { point: Point::new(10.0, 3.0), bulge: -0.2 },
        ];
        let mut poly = LayeredPolygon::new(vec![Point::new(0.0, 0.0), Point::new(10.0, 3.0), Point::new(10.0, 0.0)], "0".into(), None);
        poly.real_boundary = Some(v.clone());
        let m = mirror_layered_polygon(&poly);

        // Winding survives (reflection alone would flip the sign).
        assert!((polygon_area(&poly.points) - polygon_area(&m.points)).abs() < 1e-9);

        // Every arc of the mirrored boundary is the mirror image of the
        // original arc it came from, traversed backwards.
        let mb = m.real_boundary.unwrap();
        for j in 0..mb.len() {
            let k = (j + 1) % mb.len();
            let got = tessellate_bulge(mb[j].point, mb[k].point, mb[j].bulge, 0.01);
            let src = mb.len() - 1 - j;
            let src_next = (src + 1) % v.len();
            let expected: Vec<Point> =
                tessellate_bulge(v[src_next].point, v[(src_next + 1) % v.len()].point, v[src_next].bulge, 0.01).iter().rev().map(|p| Point::new(-p.x, p.y)).collect();
            assert_eq!(got.len(), expected.len());
            for (a, b) in got.iter().zip(&expected) {
                assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9, "arc {j}: {a:?} != {b:?}");
            }
        }
    }

    // --- POLYLINE (the older entity) -----------------------------------

    fn polyline_entity(layer: &str, verts: &[(f64, f64, f64)], closed: bool) -> Entity {
        let mut poly = dxf::entities::Polyline::default();
        poly.set_is_closed(closed);
        poly.__vertices_and_handles = verts
            .iter()
            .map(|&(x, y, bulge)| {
                let v = dxf::entities::Vertex { location: DxfPoint::new(x, y, 0.0), bulge, ..Default::default() };
                (v, dxf::Handle::empty())
            })
            .collect();
        entity(layer, EntityType::Polyline(poly))
    }

    #[test]
    fn closed_polyline_converts_the_same_as_the_equivalent_lwpolyline() {
        let square = [(0.0, 0.0, 0.0), (10.0, 0.0, 0.0), (10.0, 10.0, 0.0), (0.0, 10.0, 0.0)];
        let from_polyline = entity_to_polygon(&polyline_entity("CUT", &square, true), 0.1).expect("closed POLYLINE is a profile");

        let mut lw = LwPolyline {
            vertices: square.iter().map(|&(x, y, bulge)| LwPolylineVertex { x, y, bulge, ..Default::default() }).collect(),
            ..Default::default()
        };
        lw.set_is_closed(true);
        let from_lwpolyline = entity_to_polygon(&entity("CUT", EntityType::LwPolyline(lw)), 0.1).expect("closed LWPOLYLINE is a profile");

        assert_eq!(from_polyline.points, from_lwpolyline.points);
        assert_eq!(from_polyline.layer, "CUT");
        assert_eq!(from_polyline.real_boundary, from_lwpolyline.real_boundary);
    }

    #[test]
    fn polyline_bulge_survives_into_real_boundary_and_tessellation() {
        // Half-disc: straight edge back along the bottom, semicircular top.
        let verts = [(0.0, 0.0, 1.0), (10.0, 0.0, 0.0)];
        let poly = entity_to_polygon(&polyline_entity("0", &verts, true), 0.01).expect("closed POLYLINE is a profile");

        let boundary = poly.real_boundary.as_ref().expect("a POLYLINE keeps its bulge list for real-arc export");
        assert_eq!(boundary.len(), 2);
        assert_eq!(boundary[0].bulge, 1.0);

        // Area of a d=10 half disc, within tessellation error.
        let expected = std::f64::consts::PI * 25.0 / 2.0;
        assert!((polygon_area(&poly.points).abs() - expected).abs() < 0.5, "got {}", polygon_area(&poly.points).abs());
    }

    #[test]
    fn three_dimensional_polylines_are_rejected_rather_than_flattened() {
        let mut poly = dxf::entities::Polyline::default();
        poly.set_is_closed(true);
        poly.set_is_3d_polyline(true);
        poly.__vertices_and_handles = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]
            .iter()
            .map(|&(x, y)| {
                let v = dxf::entities::Vertex { location: DxfPoint::new(x, y, 5.0), ..Default::default() };
                (v, dxf::Handle::empty())
            })
            .collect();
        assert!(entity_to_polygon(&entity("0", EntityType::Polyline(poly)), 0.1).is_none());
    }

    // --- LINE / ARC chaining -------------------------------------------

    fn line(layer: &str, from: (f64, f64), to: (f64, f64)) -> Entity {
        entity(
            layer,
            EntityType::Line(dxf::entities::Line {
                p1: DxfPoint::new(from.0, from.1, 0.0),
                p2: DxfPoint::new(to.0, to.1, 0.0),
                ..Default::default()
            }),
        )
    }

    #[test]
    fn four_scrambled_lines_chain_into_one_square_ring() {
        // Deliberately out of order, and two of them drawn "backwards" - a
        // real drawing has no guaranteed entity order or edge direction.
        let entities = vec![
            line("CUT", (10.0, 10.0), (10.0, 0.0)), // right edge, reversed
            line("CUT", (0.0, 0.0), (10.0, 0.0)),   // bottom
            line("CUT", (0.0, 10.0), (0.0, 0.0)),   // left, reversed
            line("CUT", (10.0, 10.0), (0.0, 10.0)), // top
        ];
        let rings = entities_to_polygons_chained(entities.iter(), 0.1);
        assert_eq!(rings.len(), 1, "the four edges form exactly one ring");
        assert_eq!(rings[0].layer, "CUT");
        assert_eq!(rings[0].points.len(), 4, "a square ring has 4 vertices, with no repeated closing point");
        assert!((polygon_area(&rings[0].points).abs() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn lines_and_partial_arcs_chain_into_one_slot_profile() {
        // A 20x10 slot: two straight flanks plus a semicircular cap at each
        // end. Area = 10x20 rectangle + a d=10 circle.
        let arc = |layer: &str, cx: f64, start: f64, end: f64| {
            entity(
                layer,
                EntityType::Arc(Arc {
                    center: DxfPoint::new(cx, 0.0, 0.0),
                    radius: 5.0,
                    start_angle: start,
                    end_angle: end,
                    ..Default::default()
                }),
            )
        };
        // DXF arcs always sweep CCW from start to end, so the right-hand cap
        // is 270 -> 90 (through 0 degrees) and the left-hand one is 90 -> 270
        // (through 180). Swapping those would carve the caps *into* the slot.
        let entities = vec![
            line("CUT", (0.0, 5.0), (20.0, 5.0)),
            arc("CUT", 20.0, 270.0, 90.0),
            line("CUT", (20.0, -5.0), (0.0, -5.0)),
            arc("CUT", 0.0, 90.0, 270.0),
        ];
        let rings = entities_to_polygons_chained(entities.iter(), 0.01);
        assert_eq!(rings.len(), 1);
        let expected = 20.0 * 10.0 + std::f64::consts::PI * 25.0;
        assert!((polygon_area(&rings[0].points).abs() - expected).abs() < 1.0, "got {}", polygon_area(&rings[0].points).abs());
    }

    #[test]
    fn endpoints_within_the_join_radius_chain_but_a_real_gap_does_not() {
        let nearly = |gap: f64| {
            vec![
                line("0", (0.0, 0.0), (10.0, 0.0)),
                line("0", (10.0, 0.0), (10.0, 10.0)),
                line("0", (10.0, 10.0), (0.0, 10.0)),
                line("0", (0.0, 10.0 + gap), (0.0, 0.0)),
            ]
        };
        // Sloppy CAD endpoints well inside the tolerance still close.
        assert_eq!(entities_to_polygons_chained(nearly(0.05).iter(), 0.1).len(), 1);
        // A genuine gap does not - and the open chain is dropped, not
        // fabricated into a ring.
        assert!(entities_to_polygons_chained(nearly(5.0).iter(), 0.1).is_empty());
    }

    #[test]
    fn chaining_never_joins_edges_across_layers() {
        // Two squares sharing every coordinate, drawn on different layers -
        // if chaining ignored layers, these eight edges would produce garbage
        // instead of two clean rings.
        let mut entities = Vec::new();
        for layer in ["CUT", "DRILL"] {
            entities.push(line(layer, (0.0, 0.0), (10.0, 0.0)));
            entities.push(line(layer, (10.0, 0.0), (10.0, 10.0)));
            entities.push(line(layer, (10.0, 10.0), (0.0, 10.0)));
            entities.push(line(layer, (0.0, 10.0), (0.0, 0.0)));
        }
        let rings = entities_to_polygons_chained(entities.iter(), 0.1);
        assert_eq!(rings.len(), 2);
        let mut layers: Vec<&str> = rings.iter().map(|r| r.layer.as_str()).collect();
        layers.sort_unstable();
        assert_eq!(layers, vec!["CUT", "DRILL"]);
        for ring in &rings {
            assert!((polygon_area(&ring.points).abs() - 100.0).abs() < 1e-9);
        }
    }

    #[test]
    fn a_lone_partial_arc_still_is_not_a_profile() {
        let arc = entity(
            "0",
            EntityType::Arc(Arc {
                center: DxfPoint::new(0.0, 0.0, 0.0),
                radius: 5.0,
                start_angle: 0.0,
                end_angle: 90.0,
                ..Default::default()
            }),
        );
        assert!(entity_to_polygon(&arc, 0.1).is_none(), "one quarter arc is not a closed shape on its own");
        let entities = vec![arc];
        assert!(entities_to_polygons_chained(entities.iter(), 0.1).is_empty(), "and chaining must not invent a chord to close it");
    }

    // --- INSERT / block expansion --------------------------------------

    fn square_block(drawing: &mut Drawing, name: &str, layer: &str, size: f64) {
        let mut block = dxf::Block { name: name.to_string(), ..Default::default() };
        let mut poly = LwPolyline {
            vertices: [(0.0, 0.0), (size, 0.0), (size, size), (0.0, size)]
                .iter()
                .map(|&(x, y)| LwPolylineVertex { x, y, ..Default::default() })
                .collect(),
            ..Default::default()
        };
        poly.set_is_closed(true);
        block.entities.push(entity(layer, EntityType::LwPolyline(poly)));
        drawing.add_block(block);
    }

    fn insert(name: &str, layer: &str, at: (f64, f64), rotation: f64, scale: f64) -> Entity {
        entity(
            layer,
            EntityType::Insert(dxf::entities::Insert {
                name: name.to_string(),
                location: DxfPoint::new(at.0, at.1, 0.0),
                rotation,
                x_scale_factor: scale,
                y_scale_factor: scale,
                z_scale_factor: scale,
                ..Default::default()
            }),
        )
    }

    #[test]
    fn inserts_expand_into_transformed_copies_of_their_block() {
        let mut drawing = Drawing::new();
        square_block(&mut drawing, "PART", "CUT", 10.0);
        drawing.add_entity(insert("PART", "CUT", (100.0, 0.0), 0.0, 1.0));
        drawing.add_entity(insert("PART", "CUT", (0.0, 50.0), 90.0, 2.0));

        let expanded = expand_inserts(&drawing, 0.1);
        let rings = entities_to_polygons_chained(expanded.iter(), 0.1);
        assert_eq!(rings.len(), 2, "each INSERT places one copy of the block");

        let mut areas: Vec<f64> = rings.iter().map(|r| polygon_area(&r.points).abs()).collect();
        areas.sort_by(f64::total_cmp);
        assert!((areas[0] - 100.0).abs() < 1e-6, "unscaled copy keeps its area");
        assert!((areas[1] - 400.0).abs() < 1e-6, "the 2x copy is 4x the area");

        // The unrotated copy really is where the INSERT put it.
        let placed = rings.iter().find(|r| (polygon_area(&r.points).abs() - 100.0).abs() < 1e-6).unwrap();
        let bounds = get_polygon_bounds(&placed.points).unwrap();
        assert!((bounds.x - 100.0).abs() < 1e-6, "got x {}", bounds.x);
    }

    #[test]
    fn block_geometry_on_layer_zero_inherits_the_inserts_layer() {
        let mut drawing = Drawing::new();
        square_block(&mut drawing, "ON_ZERO", "0", 10.0);
        square_block(&mut drawing, "ON_NAMED", "ETCH", 10.0);
        drawing.add_entity(insert("ON_ZERO", "CUT", (0.0, 0.0), 0.0, 1.0));
        drawing.add_entity(insert("ON_NAMED", "CUT", (100.0, 0.0), 0.0, 1.0));

        let expanded = expand_inserts(&drawing, 0.1);
        let layers: Vec<&str> = expanded.iter().map(|e| e.common.layer.as_str()).collect();
        assert!(layers.contains(&"CUT"), "layer-0 block geometry takes the INSERT's layer, got {layers:?}");
        assert!(layers.contains(&"ETCH"), "geometry on a named layer keeps it, got {layers:?}");
    }

    #[test]
    fn minsert_arrays_expand_to_one_copy_per_grid_cell() {
        let mut drawing = Drawing::new();
        square_block(&mut drawing, "PART", "CUT", 5.0);
        let mut ins = insert("PART", "CUT", (0.0, 0.0), 0.0, 1.0);
        if let EntityType::Insert(i) = &mut ins.specific {
            i.column_count = 2;
            i.row_count = 3;
            i.column_spacing = 20.0;
            i.row_spacing = 20.0;
        }
        drawing.add_entity(ins);

        let rings = entities_to_polygons_chained(expand_inserts(&drawing, 0.1).iter(), 0.1);
        assert_eq!(rings.len(), 6, "a 2x3 MINSERT is six copies");
        // Six distinct positions, not six copies stacked on each other.
        let mut corners: Vec<(i64, i64)> = rings
            .iter()
            .map(|r| {
                let b = get_polygon_bounds(&r.points).unwrap();
                (b.x.round() as i64, b.y.round() as i64)
            })
            .collect();
        corners.sort_unstable();
        corners.dedup();
        assert_eq!(corners.len(), 6);
    }

    #[test]
    fn a_self_referencing_block_terminates_instead_of_hanging() {
        let mut drawing = Drawing::new();
        let mut block = dxf::Block { name: "LOOP".to_string(), ..Default::default() };
        let mut poly = LwPolyline {
            vertices: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)].iter().map(|&(x, y)| LwPolylineVertex { x, y, ..Default::default() }).collect(),
            ..Default::default()
        };
        poly.set_is_closed(true);
        block.entities.push(entity("0", EntityType::LwPolyline(poly)));
        block.entities.push(insert("LOOP", "0", (1.0, 1.0), 0.0, 1.0));
        drawing.add_block(block);
        drawing.add_entity(insert("LOOP", "CUT", (0.0, 0.0), 0.0, 1.0));

        // The real assertion is that this returns at all.
        let expanded = expand_inserts(&drawing, 0.1);
        assert!(expanded.len() <= MAX_INSERT_DEPTH, "recursion is capped, got {}", expanded.len());
    }
}
