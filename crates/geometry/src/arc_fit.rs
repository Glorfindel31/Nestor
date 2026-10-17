//! Recovers real arcs from a tessellated closed outline - **new scope, not a
//! port**. A Clipper boolean (`tabs::unite`) only speaks polylines, so a part
//! that went through one comes back with its arcs chopped into segments; a
//! router then follows a faceted path. This turns runs of co-circular
//! vertices back into DXF bulge arcs.

use crate::dxf_import::RealVertex;
use crate::point::Point;

/// Largest turn an arc vertex may have. A tessellated circle steps by a small
/// angle per vertex; anything at or above this is a real corner, which an arc
/// can never pass through.
const MAX_ARC_STEP: f64 = std::f64::consts::FRAC_PI_6; // 30 degrees

/// Turns at or below this are straight: a line, not an arc.
const COLLINEAR: f64 = 1e-9;

/// `points`: a closed polyline (last point not repeated). Returns the same
/// outline as DXF `(point, bulge)` vertices: `bulge = tan(θ/4)` of the arc
/// leaving that vertex, positive = CCW, `0.0` for a straight segment - the
/// convention `dxf_import::tessellate_bulge` reads.
pub fn fit_arcs(points: &[Point], tol: f64) -> Vec<RealVertex> {
    // Zero-length edges have no direction, so they would give meaningless
    // turns and centres. Dropping them keeps the same outline.
    let mut pts: Vec<Point> = Vec::with_capacity(points.len());
    for &p in points {
        if pts.last().is_none_or(|q| q.distance_to(p) > 1e-12) {
            pts.push(p);
        }
    }
    while pts.len() > 1 && pts[0].distance_to(pts[pts.len() - 1]) <= 1e-12 {
        pts.pop();
    }

    let n = pts.len();
    if n < 3 {
        return pts.into_iter().map(|point| RealVertex { point, bulge: 0.0 }).collect();
    }

    let raw: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b, c) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let (ux, uy) = (b.x - a.x, b.y - a.y);
            let (vx, vy) = (c.x - b.x, c.y - b.y);
            (ux * vy - uy * vx).atan2(ux * vx + uy * vy)
        })
        .collect();

    // Start the walk where no arc can wrap across it. A real corner is the
    // ideal start. A smooth outline has none, so start at the smallest turn:
    // a tangent join between a line and an arc, which is half an arc step.
    // Starting mid-arc would split that arc in two. Only the vertex count
    // suffers, not correctness.
    let by_abs = |a: &(usize, &f64), b: &(usize, &f64)| a.1.abs().total_cmp(&b.1.abs());
    let indexed: Vec<(usize, &f64)> = raw.iter().enumerate().collect();
    let has_corner = raw.iter().any(|t| t.abs() >= MAX_ARC_STEP);
    let s = if has_corner {
        indexed.iter().copied().max_by(by_abs)
    } else {
        indexed.iter().copied().min_by(by_abs)
    }
    .map_or(0, |(i, _)| i);

    let q: Vec<Point> = (0..n).map(|k| pts[(s + k) % n]).collect();
    let t: Vec<f64> = (0..n).map(|k| raw[(s + k) % n]).collect();

    // Walk indices 0..=n in the rotated order. Index n is vertex 0 again, so
    // the last run may close on the start, but no run wraps past it.
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        // ponytail: O(n^2) run growth; outlines here are a few hundred points.
        let mut best: Option<(usize, Point)> = None;
        let mut sign = 0.0_f64;
        let mut next_interior = i + 1;
        let mut j = i + 3;
        while j <= n {
            // Vertices i+1 ..= j-1 are interior to the candidate run. Admit any
            // not yet checked. Once one fails it stays interior, so stop.
            let mut interior_ok = true;
            while next_interior < j {
                let turn = t[next_interior];
                if !(turn.abs() > COLLINEAR && turn.abs() < MAX_ARC_STEP) {
                    interior_ok = false;
                    break;
                }
                if sign == 0.0 {
                    sign = turn.signum();
                } else if turn.signum() != sign {
                    interior_ok = false;
                    break;
                }
                next_interior += 1;
            }
            if !interior_ok {
                break;
            }
            match fit_circle(&q, i, j, tol) {
                Some(centre) => best = Some((j, centre)),
                None => break,
            }
            j += 1;
        }

        match best {
            Some((j, c)) => {
                // Sum the signed angle swept at the centre. Consecutive steps
                // are each under 30 degrees with one sign, so the sum is the
                // real sweep and the bulge matches the arc's direction.
                let theta: f64 = (i..j)
                    .map(|k| {
                        let (a, b) = (q[k % n], q[(k + 1) % n]);
                        let (ax, ay) = (a.x - c.x, a.y - c.y);
                        let (bx, by) = (b.x - c.x, b.y - c.y);
                        (ax * by - ay * bx).atan2(ax * bx + ay * by)
                    })
                    .sum();
                out.push(RealVertex { point: q[i], bulge: (theta / 4.0).tan() });
                i = j;
            }
            None => {
                out.push(RealVertex { point: q[i], bulge: 0.0 });
                i += 1;
            }
        }
    }
    out
}

/// Centre of the circle through `q[i]`, its midpoint index, and `q[j]`, if
/// every vertex `i..=j` lies within `tol` of it. `None` when the three
/// points are (near-)collinear or the run is off the circle.
fn fit_circle(q: &[Point], i: usize, j: usize, tol: f64) -> Option<Point> {
    let n = q.len();
    let (a, m, c) = (q[i], q[((i + j) / 2) % n], q[j % n]);
    let (bx, by) = (m.x - a.x, m.y - a.y);
    let (cx, cy) = (c.x - a.x, c.y - a.y);
    let d = 2.0 * (bx * cy - by * cx);
    if d.abs() <= 1e-12 * (bx.hypot(by) * cx.hypot(cy)) {
        return None;
    }
    let (b2, c2) = (bx * bx + by * by, cx * cx + cy * cy);
    let centre = Point::new(a.x + (cy * b2 - by * c2) / d, a.y + (bx * c2 - cx * b2) / d);
    let r = a.distance_to(centre);
    (i..=j)
        .all(|k| (q[k % n].distance_to(centre) - r).abs() <= tol)
        .then_some(centre)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two straight 40mm sides joined by 24-segment semicircles of radius 10,
    /// centred (0,0) and (40,0). CCW. Straight sides are implicit between the
    /// two arcs' endpoints.
    fn stadium() -> Vec<Point> {
        let mut pts = Vec::new();
        for &(cx, start) in &[(40.0, -90.0), (0.0, 90.0)] {
            for k in 0..=24 {
                let a = (start + 7.5 * k as f64).to_radians();
                pts.push(Point::new(cx + 10.0 * a.cos(), 10.0 * a.sin()));
            }
        }
        pts
    }

    #[test]
    fn square_stays_straight() {
        let sq = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let out = fit_arcs(&sq, 0.01);
        assert_eq!(out.len(), 4, "{out:?}");
        assert!(out.iter().all(|v| v.bulge == 0.0), "{out:?}");
    }

    #[test]
    fn duplicate_points_are_dropped() {
        let sq = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let out = fit_arcs(&sq, 0.01);
        assert_eq!(out.len(), 4, "{out:?}");
        assert!(out.iter().all(|v| v.bulge == 0.0), "{out:?}");
    }

    #[test]
    fn stadium_becomes_two_semicircles() {
        let out = fit_arcs(&stadium(), 0.01);
        assert_eq!(out.len(), 4, "{out:?}");
        let arcs = out.iter().filter(|v| (v.bulge - 1.0).abs() < 1e-6).count();
        let lines = out.iter().filter(|v| v.bulge.abs() < 1e-9).count();
        assert_eq!((arcs, lines), (2, 2), "{out:?}");
    }

    #[test]
    fn clockwise_stadium_gives_negative_bulges() {
        let mut pts = stadium();
        pts.reverse();
        let out = fit_arcs(&pts, 0.01);
        assert_eq!(out.len(), 4, "{out:?}");
        let arcs = out.iter().filter(|v| (v.bulge + 1.0).abs() < 1e-6).count();
        assert_eq!(arcs, 2, "{out:?}");
    }

    #[test]
    fn zigzag_is_not_an_arc() {
        let zig = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 5.0),
            Point::new(20.0, 0.0),
            Point::new(30.0, 5.0),
            Point::new(40.0, 0.0),
            Point::new(50.0, 5.0),
        ];
        let out = fit_arcs(&zig, 0.01);
        assert_eq!(out.len(), 6, "{out:?}");
        assert!(out.iter().all(|v| v.bulge == 0.0), "{out:?}");
    }
}
