//! DXF entity -> polygon-tree conversion (replaces SVG import per the scope
//! change recorded in docs/PORT_STATUS.md). Does the same shape of work
//! `svgparser.js` did for SVG - closed-profile detection, parent/hole
//! containment nesting, `.isCircle` metadata for the circular-hole NFP fast
//! path (see circular_nfp.rs) - just against DXF entities instead of an SVG
//! DOM, via the `dxf` crate.
//!
//! Currently supported: `LWPOLYLINE` (closed, including bulge/arc segments),
//! `CIRCLE`, and full-sweep `ARC` (treated as a circle, same as SVG's
//! isCircle handling for a `<circle>`-equivalent full arc). Bare `LINE`/
//! partial-`ARC` networks that only form a closed profile once their
//! endpoints are chained together are **not** supported yet - that needs a
//! separate edge-graph-joining algorithm and is tracked as a follow-up in
//! docs/PORT_STATUS.md rather than attempted half-correct here. The older
//! heavyweight `POLYLINE` entity (pre-LWPOLYLINE, vertices as separate linked
//! entities) is also not yet supported.
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

fn lwpolyline_to_points(verts: &[dxf::LwPolylineVertex], is_closed: bool, tolerance: f64) -> Vec<Point> {
    let mut points = Vec::with_capacity(verts.len());
    let n = verts.len();

    for i in 0..n {
        let p0 = Point::new(verts[i].x, verts[i].y);
        points.push(p0);

        // only emit the arc between this vertex and the next if there IS a
        // next vertex to connect to (the last vertex only connects onward
        // when the polyline is closed, wrapping back to vertex 0)
        let has_next = i + 1 < n || is_closed;
        if has_next && verts[i].bulge != 0.0 {
            let next = &verts[if i + 1 < n { i + 1 } else { 0 }];
            let p1 = Point::new(next.x, next.y);
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
fn closes_itself_by_duplicate_point(poly: &dxf::entities::LwPolyline) -> bool {
    if poly.is_closed() {
        return false;
    }
    let verts = &poly.vertices;
    match (verts.first(), verts.last()) {
        (Some(first), Some(last)) if verts.len() >= 4 => (first.x - last.x).abs() < 1e-9 && (first.y - last.y).abs() < 1e-9,
        _ => false,
    }
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
        EntityType::LwPolyline(poly) if poly.is_closed() => {
            let points = lwpolyline_to_points(&poly.vertices, true, curve_tolerance);
            if points.len() < 3 {
                return None;
            }
            Some(LayeredPolygon { real_boundary: Some(real_boundary_from_vertices(&poly.vertices)), ..LayeredPolygon::new(points, layer, None) })
        }
        EntityType::LwPolyline(poly) if closes_itself_by_duplicate_point(poly) => {
            // Drop the redundant closing vertex (identical to the first)
            // before treating the loop as closed - otherwise it'd produce a
            // zero-length final edge back to vertex 0.
            let verts = &poly.vertices[..poly.vertices.len() - 1];
            let points = lwpolyline_to_points(verts, true, curve_tolerance);
            if points.len() < 3 {
                return None;
            }
            Some(LayeredPolygon { real_boundary: Some(real_boundary_from_vertices(verts)), ..LayeredPolygon::new(points, layer, None) })
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

/// True if `candidate`'s first point lies inside `container` (containment
/// test used to build the parent/hole tree - matches the "point-in-polygon"
/// approach `svgparser.js` used for SVG parent/hole detection).
fn contains(container: &[Point], candidate: &[Point]) -> bool {
    // `LayeredPolygon`'s fields are all `pub`, so a degenerate (empty-points)
    // entry is a valid `Vec<LayeredPolygon>` for a caller to hand the public
    // `build_polygon_tree` - an empty candidate trivially can't be "inside"
    // anything, no need to look at `container` at all.
    let Some(&first) = candidate.first() else { return false };
    let zero = Point::new(0.0, 0.0);
    point_in_polygon(first, container, zero, None) == Some(true)
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
}
