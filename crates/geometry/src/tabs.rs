//! Bridges ("tabs") between small neighbouring parts - **new scope, not a
//! port**. A small part cut free on a router moves under the bit or is thrown
//! off the bed; a short uncut strip to its neighbour holds it. Post-nest only:
//! nothing here affects placement.
//!
//! `find_bridges` proposes where; `unite` merges bridged parts and their
//! bridges into one cut outline on export (Illustrator's "Unite"), so the
//! cutter simply goes around the strip.

use std::collections::HashMap;

use clipper2::FillRule;

use crate::clipper::{difference_polygons, intersection_polygons, offset_round, union_polygons};
use crate::dxf_import::{attach_texts, build_polygon_tree, LayeredPolygon};
use crate::point::Point;
use crate::polygon::{get_polygon_bounds, polygon_area, Bounds};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabSettings {
    /// A part is "small" when the longest side of its bounding box is below this (mm).
    pub threshold: f64,
    /// Width of the uncut strip (mm).
    pub width: f64,
    /// Router bit diameter (mm).
    pub bit: f64,
    /// Most bridges any one part may get.
    pub per_part: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bridge {
    /// Part ids, `a < b`.
    pub a: usize,
    pub b: usize,
    /// The strip plus its rounded junctions, overlapping both parts so
    /// `union(parts, patch)` leaves no seam. What export unites.
    pub patch: Vec<Vec<Point>>,
    /// `patch` minus both parts: just what sits in the gap. What the canvas draws.
    pub visible: Vec<Vec<Point>>,
}

/// `parts`: (id, outer contour already rotated and positioned on the sheet), one sheet.
pub fn find_bridges(parts: &[(usize, Vec<Point>)], settings: &TabSettings) -> Vec<Bridge> {
    // ponytail: "adjacent" = within 2 bit diameters, a heuristic; tune if bridges are missed or span too far
    let gap_limit = 2.0 * settings.bit;

    let small: Vec<(usize, &[Point], Bounds)> = parts
        .iter()
        .filter_map(|(id, pts)| {
            let b = get_polygon_bounds(pts)?;
            (b.width.max(b.height) < settings.threshold).then_some((*id, pts.as_slice(), b))
        })
        .collect();

    // (gap, lower id, higher id, point on lower, point on higher)
    let mut candidates: Vec<(f64, usize, usize, Point, Point)> = Vec::new();
    for i in 0..small.len() {
        for j in i + 1..small.len() {
            let (ida, a, ba) = &small[i];
            let (idb, b, bb) = &small[j];
            // bbox distance is a lower bound on the outline distance, so this never drops a real pair
            let dx = (bb.x - (ba.x + ba.width)).max(ba.x - (bb.x + bb.width)).max(0.0);
            let dy = (bb.y - (ba.y + ba.height)).max(ba.y - (bb.y + bb.height)).max(0.0);
            if (dx * dx + dy * dy).sqrt() > gap_limit {
                continue;
            }
            let Some((pa, pb, gap)) = closest(a, b) else {
                continue;
            };
            // below the bit the cutter cannot pass between them, so there is nothing to hold
            if gap < settings.bit || gap > gap_limit {
                continue;
            }
            let (lo, hi, p_lo, p_hi) = if ida < idb {
                (*ida, *idb, pa, pb)
            } else {
                (*idb, *ida, pb, pa)
            };
            candidates.push((gap, lo, hi, p_lo, p_hi));
        }
    }

    candidates.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));

    let outlines: HashMap<usize, &[Point]> = small.iter().map(|(id, pts, _)| (*id, *pts)).collect();

    let spacing = settings.width + settings.bit;
    let mut count: HashMap<usize, usize> = HashMap::new();
    let mut anchors: HashMap<usize, Vec<Point>> = HashMap::new();
    let clear = |anchors: &HashMap<usize, Vec<Point>>, id: usize, p: Point| {
        anchors
            .get(&id)
            .is_none_or(|v| v.iter().all(|q| q.distance_to(p) >= spacing))
    };

    let mut out = Vec::new();
    for (_, a, b, pa, pb) in candidates {
        let used = |id: usize| count.get(&id).copied().unwrap_or(0);
        if used(a) >= settings.per_part || used(b) >= settings.per_part {
            continue;
        }
        if !clear(&anchors, a, pa) || !clear(&anchors, b, pb) {
            continue;
        }
        let Some(corners) = strip_corners(pa, pb, settings.width) else {
            continue;
        };
        let (Some(&a_pts), Some(&b_pts)) = (outlines.get(&a), outlines.get(&b)) else {
            continue;
        };
        let Some((patch, visible)) = bridge_patch(a_pts, b_pts, corners, pa, pb, settings) else {
            continue;
        };
        *count.entry(a).or_insert(0) += 1;
        *count.entry(b).or_insert(0) += 1;
        anchors.entry(a).or_default().push(pa);
        anchors.entry(b).or_default().push(pb);
        out.push(Bridge { a, b, patch, visible });
    }
    out
}

/// Polygon rings, as Clipper returns them.
type Rings = Vec<Vec<Point>>;

/// The strip plus its rounded junctions, as `(patch, visible)`.
///
/// Closing (grow by `r`, then shrink by `r`, `r` = bit radius) fills every
/// concave corner where the strip meets a part with a radius-`r` arc. A router
/// bit cannot follow a square inside corner, but it can follow an arc of its
/// own radius, so that arc is the tightest curve the cut can actually take.
/// The closed shape is then clipped to the strip's zone, a rectangle wider than
/// the strip by `1.5 r` on each side. Without the clip, closing would round
/// every concave corner of both parts, not just the ones the strip meets.
fn bridge_patch(
    a: &[Point],
    b: &[Point],
    corners: [Point; 4],
    pa: Point,
    pb: Point,
    settings: &TabSettings,
) -> Option<(Rings, Rings)> {
    let r = settings.bit / 2.0;
    let local = largest(union_polygons(&[a.to_vec(), b.to_vec()], &[corners.to_vec()], FillRule::NonZero).ok()?)?;
    let grown = largest(offset_round(&local, r))?;
    let closed = largest(offset_round(&grown, -r))?;
    let zone = rect(pa, pb, settings.width / 2.0 + 1.5 * r, settings.width / 2.0)?;
    let patch = intersection_polygons(&[closed], &[zone.to_vec()], FillRule::NonZero).ok()?;
    if patch.is_empty() {
        return None;
    }
    let visible = difference_polygons(&patch, &[a.to_vec(), b.to_vec()], FillRule::NonZero).ok()?;
    Some((patch, open(visible, settings.width.min(settings.bit) / 4.0)))
}

/// Morphological opening (shrink by `eps`, grow back): drops everything
/// thinner than `2 * eps`. Closing puts the outline on the true curve while
/// a part's own outline is a chord tessellation, so `patch - parts` carries
/// zero-width crescents along every edge in the zone - harmless to the
/// export (the union swallows them), but drawn as a line their hairpin turns
/// make egui's mitred joins shoot needles across the whole sheet.
fn open(rings: Rings, eps: f64) -> Rings {
    rings
        .into_iter()
        .flat_map(|mut ring| {
            // Clipper reads a clockwise ring as a hole and would grow it.
            // `polygon_area` is deepnest's: negative means counter-clockwise.
            if polygon_area(&ring) > 0.0 {
                ring.reverse();
            }
            offset_round(&ring, -eps)
        })
        .flat_map(|ring| offset_round(&ring, eps))
        .filter(|ring| polygon_area(ring).abs() > eps * eps)
        .collect()
}

/// The path with the largest absolute area, `None` when there are none.
fn largest(paths: Vec<Vec<Point>>) -> Option<Vec<Point>> {
    paths
        .into_iter()
        .max_by(|x, y| polygon_area(x).abs().total_cmp(&polygon_area(y).abs()))
}

/// Merges already-positioned `shapes` and the `bridges` joining them into the
/// united outline(s), keeping every shape's children (holes, etch, drill).
pub fn unite(shapes: Vec<LayeredPolygon>, patches: &[Vec<Point>]) -> Vec<LayeredPolygon> {
    if shapes.is_empty() {
        return Vec::new();
    }
    let subject: Vec<Vec<Point>> = shapes.iter().map(|s| s.points.clone()).collect();
    let clip: Vec<Vec<Point>> = patches.to_vec();
    let Ok(united) = union_polygons(&subject, &clip, FillRule::NonZero) else {
        return shapes;
    };

    let layer = shapes[0].layer.clone();
    let mut flat: Vec<LayeredPolygon> = united
        .into_iter()
        .map(|path| LayeredPolygon::new(path, layer.clone(), None))
        .collect();

    let mut texts = Vec::new();
    for shape in &shapes {
        texts.extend(shape.texts.iter().cloned());
        flatten_children(&shape.children, &mut flat);
    }

    // the outer outlines are replaced by the union, but their holes must be re-nested
    // under whichever united outline now contains them
    let mut roots = build_polygon_tree(flat);
    attach_texts(&mut roots, texts);
    // Clipper only speaks polylines, so this restores the arcs (original part arcs and
    // the new fillets) for DXF export. `points` stay the polyline for SVG and the canvas.
    for root in &mut roots {
        root.real_boundary = Some(crate::arc_fit::fit_arcs(&root.points, 1e-3));
    }
    roots
}

/// Pushes every descendant of `children` onto `out`, each with its own children
/// cleared - `build_polygon_tree` rebuilds the nesting from containment.
fn flatten_children(children: &[LayeredPolygon], out: &mut Vec<LayeredPolygon>) {
    for child in children {
        let mut node = child.clone();
        node.children.clear();
        out.push(node);
        flatten_children(&child.children, out);
    }
}

/// The strip between two anchor points, `width` wide and extended by half its
/// width past each end so it overlaps both outlines. `None` when the anchors
/// coincide (no direction to take).
fn strip_corners(pa: Point, pb: Point, width: f64) -> Option<[Point; 4]> {
    rect(pa, pb, width / 2.0, width / 2.0)
}

/// A rectangle along `pa..pb`, `2 * half_width` wide, extended by `overhang`
/// past each end. `None` when the anchors coincide.
fn rect(pa: Point, pb: Point, half_width: f64, overhang: f64) -> Option<[Point; 4]> {
    let (dx, dy) = (pb.x - pa.x, pb.y - pa.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-12 {
        return None;
    }
    let u = Point::new(dx / len, dy / len);
    let n = Point::new(-u.y, u.x);
    let start = Point::new(pa.x - u.x * overhang, pa.y - u.y * overhang);
    let end = Point::new(pb.x + u.x * overhang, pb.y + u.y * overhang);
    let h = half_width;
    Some([
        Point::new(start.x + n.x * h, start.y + n.y * h),
        Point::new(start.x - n.x * h, start.y - n.y * h),
        Point::new(end.x - n.x * h, end.y - n.y * h),
        Point::new(end.x + n.x * h, end.y + n.y * h),
    ])
}

type Cand = (Point, Point, f64);

fn keep(slot: &mut Option<Cand>, cand: Cand) {
    if slot.is_none_or(|s| cand.2 < s.2) {
        *slot = Some(cand);
    }
}

/// Point on segment `s0..s1` nearest `p`, and the distance to it.
fn project(p: Point, s0: Point, s1: Point) -> (Point, f64) {
    let d = Point::new(s1.x - s0.x, s1.y - s0.y);
    let len2 = d.x * d.x + d.y * d.y;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.x - s0.x) * d.x + (p.y - s0.y) * d.y) / len2).clamp(0.0, 1.0)
    };
    let foot = Point::new(s0.x + d.x * t, s0.y + d.y * t);
    (foot, p.distance_to(foot))
}

/// Closest approach between two closed outlines as `(point on a, point on b, gap)`.
///
/// Every vertex is tried against every edge of the other outline, which covers
/// the true minimum. A tie between a corner and a face is common (two boxes side
/// by side: corner-to-face and face-to-face are both the gap), so when the best
/// edge pair is near-parallel and overlaps, its mid-edge pair wins - the strip
/// lands mid-edge, not at a corner.
fn closest(a: &[Point], b: &[Point]) -> Option<(Point, Point, f64)> {
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let mut best: Option<Cand> = None;
    let mut best_mid: Option<Cand> = None;
    for i in 0..a.len() {
        let (a0, a1) = (a[i], a[(i + 1) % a.len()]);
        let da = Point::new(a1.x - a0.x, a1.y - a0.y);
        let la = (da.x * da.x + da.y * da.y).sqrt();
        if la < 1e-12 {
            continue;
        }
        let ua = Point::new(da.x / la, da.y / la);
        for j in 0..b.len() {
            let (b0, b1) = (b[j], b[(j + 1) % b.len()]);
            let db = Point::new(b1.x - b0.x, b1.y - b0.y);
            let lb = (db.x * db.x + db.y * db.y).sqrt();
            if lb < 1e-12 {
                continue;
            }
            let ub = Point::new(db.x / lb, db.y / lb);

            for v in [a0, a1] {
                let (foot, d) = project(v, b0, b1);
                keep(&mut best, (v, foot, d));
            }
            for v in [b0, b1] {
                let (foot, d) = project(v, a0, a1);
                keep(&mut best, (foot, v, d));
            }

            if (ua.x * ub.y - ua.y * ub.x).abs() < 0.05 {
                let t0 = (b0.x - a0.x) * ua.x + (b0.y - a0.y) * ua.y;
                let t1 = (b1.x - a0.x) * ua.x + (b1.y - a0.y) * ua.y;
                let lo = t0.min(t1).max(0.0);
                let hi = t0.max(t1).min(la);
                if hi > lo {
                    let t = (lo + hi) / 2.0;
                    let pa = Point::new(a0.x + ua.x * t, a0.y + ua.y * t);
                    let (pb, d) = project(pa, b0, b1);
                    keep(&mut best_mid, (pa, pb, d));
                }
            }
        }
    }
    match (best, best_mid) {
        (Some(c), Some(m)) if m.2 <= c.2 + 1e-6 => Some(m),
        (c, _) => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<Point> {
        vec![
            Point::new(x, y),
            Point::new(x + s, y),
            Point::new(x + s, y + s),
            Point::new(x, y + s),
        ]
    }

    fn settings(per_part: usize) -> TabSettings {
        TabSettings {
            threshold: 50.0,
            width: 3.0,
            bit: 6.0,
            per_part,
        }
    }

    fn shoelace(poly: &[Point]) -> f64 {
        let n = poly.len();
        let twice: f64 = (0..n)
            .map(|i| {
                let (p, q) = (poly[i], poly[(i + 1) % n]);
                p.x * q.y - q.x * p.y
            })
            .sum();
        (twice / 2.0).abs()
    }

    /// (min x, min y, max x, max y) over every vertex of every polygon.
    fn bbox(polys: &[Vec<Point>]) -> (f64, f64, f64, f64) {
        let pts = polys.iter().flatten();
        (
            pts.clone().map(|p| p.x).fold(f64::MAX, f64::min),
            pts.clone().map(|p| p.y).fold(f64::MAX, f64::min),
            pts.clone().map(|p| p.x).fold(f64::MIN, f64::max),
            pts.map(|p| p.y).fold(f64::MIN, f64::max),
        )
    }

    #[test]
    fn two_squares_get_one_mid_edge_bridge() {
        let parts = vec![(0, square(0.0, 0.0, 20.0)), (1, square(28.0, 0.0, 20.0))];
        let bridges = find_bridges(&parts, &settings(2));
        assert_eq!(bridges.len(), 1);
        assert_eq!((bridges[0].a, bridges[0].b), (0, 1));

        // visible: the strip in the gap plus its fillets, symmetric about the strip centre y = 10
        let (vx0, vy0, vx1, vy1) = bbox(&bridges[0].visible);
        let cy = (vy0 + vy1) / 2.0;
        assert!((cy - 10.0).abs() < 1e-3, "visible centre y {cy}");
        assert!(vx0 <= 20.0 + 1e-3 && vx1 >= 28.0 - 1e-3, "visible x span {vx0}..{vx1}");

        // strip alone is 8 x 3 = 24; fillets add a little, bounded by 4 corners of r^2 * 2
        let r = 3.0;
        let area: f64 = bridges[0].visible.iter().map(|p| shoelace(p)).sum();
        assert!(area > 3.0 * 8.0, "visible area {area}");
        assert!(area < 3.0 * 8.0 + 4.0 * r * r * 2.0, "visible area {area}");

        // patch overlaps both squares so the union has no seam
        let (px0, _, px1, _) = bbox(&bridges[0].patch);
        assert!(px0 < 20.0 && px1 > 28.0, "patch x span {px0}..{px1}");
    }

    #[test]
    fn gap_below_bit_gets_no_bridge() {
        let parts = vec![(0, square(0.0, 0.0, 20.0)), (1, square(24.0, 0.0, 20.0))];
        assert!(find_bridges(&parts, &settings(2)).is_empty());
    }

    #[test]
    fn big_part_is_not_bridged() {
        let parts = vec![(0, square(0.0, 0.0, 100.0)), (1, square(108.0, 0.0, 20.0))];
        assert!(find_bridges(&parts, &settings(2)).is_empty());
    }

    #[test]
    fn per_part_cap_limits_middle_square() {
        let parts = vec![
            (0, square(0.0, 0.0, 20.0)),
            (1, square(28.0, 0.0, 20.0)),
            (2, square(56.0, 0.0, 20.0)),
        ];
        assert_eq!(find_bridges(&parts, &settings(1)).len(), 1);
    }

    #[test]
    fn unite_keeps_holes_under_one_outline() {
        let parts = vec![(0, square(0.0, 0.0, 20.0)), (1, square(28.0, 0.0, 20.0))];
        let bridges = find_bridges(&parts, &settings(2));
        let corners: Vec<Vec<Point>> = bridges.iter().flat_map(|b| b.patch.clone()).collect();

        let mut s0 = LayeredPolygon::new(square(0.0, 0.0, 20.0), "CUT".into(), None);
        s0.children
            .push(LayeredPolygon::new(square(8.0, 8.0, 4.0), "HOLE".into(), None));
        let mut s1 = LayeredPolygon::new(square(28.0, 0.0, 20.0), "CUT".into(), None);
        s1.children
            .push(LayeredPolygon::new(square(36.0, 8.0, 4.0), "HOLE".into(), None));

        let roots = unite(vec![s0, s1], &corners);
        assert_eq!(roots.len(), 1);
        assert!(shoelace(&roots[0].points) > 800.0);
        assert_eq!(roots[0].children.len(), 2);
        assert!(roots[0].children.iter().all(|c| c.layer == "HOLE"));

        // NEEDS arc_fit::fit_arcs (todo!() until the other worker lands it): the fillets
        // must come back as bulge arcs in the real boundary
        let boundary = roots[0].real_boundary.as_ref().expect("real_boundary set by unite");
        assert!(boundary.iter().any(|v| v.bulge != 0.0), "no arcs in united outline");
    }

    /// Two coarse 12-gons (a "circle" as an import tessellates one): closing
    /// puts the outline on the true curve, off the chords, and the drawn
    /// bridge must not carry the resulting hairline slivers - drawn, those
    /// become needles across the sheet.
    #[test]
    fn the_drawn_bridge_has_no_slivers() {
        let gon = |cx: f64| -> Vec<Point> {
            (0..12).map(|k| {
                let t = std::f64::consts::TAU * k as f64 / 12.0;
                Point::new(cx + 10.0 * t.cos(), 10.0 * t.sin())
            }).collect()
        };
        let parts = vec![(0, gon(0.0)), (1, gon(28.0))];
        let bridges = find_bridges(&parts, &settings(2));
        assert_eq!(bridges.len(), 1);
        // A sliver is a hairpin: the outline doubles back on itself. That
        // near-zero inside angle is what egui's mitred stroke turns into a
        // needle, so it is what this checks for.
        let sharpest = bridges[0]
            .visible
            .iter()
            .flat_map(|ring| {
                let n = ring.len();
                (0..n).map(move |i| {
                    let (p, q, r) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
                    let (u, v) = (Point::new(p.x - q.x, p.y - q.y), Point::new(r.x - q.x, r.y - q.y));
                    (u.x * v.y - u.y * v.x).atan2(u.x * v.x + u.y * v.y).abs()
                })
            })
            .fold(f64::MAX, f64::min);
        assert!(sharpest > 10f64.to_radians(), "a hairpin vertex survived: {} deg", sharpest.to_degrees());
        let area: f64 = bridges[0].visible.iter().map(|r| polygon_area(r).abs()).sum();
        assert!(area > 3.0 * 6.0, "the strip itself survives the cleanup, area {area}");
    }
}
