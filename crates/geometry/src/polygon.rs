//! Port of the non-NFP portions of main/util/geometryutil.js: floating-point
//! comparison helpers, polygon bounds/area/point-in-polygon, and rotation.
//! See nfp.rs for the NFP-tracing algorithm (noFitPolygon and friends).

use crate::point::Point;

/// Floating point comparison tolerance. Floating point error is likely to be
/// above 1 epsilon.
pub const TOL: f64 = 1e-9;

/// Port of `_almostEqual`. JS's `!tolerance` treats an explicit 0 the same as
/// "not passed" (falls back to TOL) - preserved here since callers rely on it.
pub fn almost_equal(a: f64, b: f64, tolerance: Option<f64>) -> bool {
    let tol = match tolerance {
        Some(t) if t != 0.0 => t,
        _ => TOL,
    };
    (a - b).abs() < tol
}

/// Port of `_withinDistance`.
pub fn within_distance(p1: Point, p2: Point, distance: f64) -> bool {
    let dx = p1.x - p2.x;
    let dy = p1.y - p2.y;
    dx * dx + dy * dy < distance * distance
}

/// Port of `_normalizeVector` (geometryutil.js's private helper, distinct from
/// vector.ts's Vector class - it operates on plain {x,y} pairs, same as here).
pub fn normalize_vector(v: Point) -> Point {
    if almost_equal(v.x * v.x + v.y * v.y, 1.0, None) {
        return v; // given vector was already a unit vector
    }
    let len = v.x.hypot(v.y);
    Point::new(v.x / len, v.y / len)
}

/// Port of `_onSegment`: true if `p` lies on segment AB, excluding endpoints.
pub fn on_segment(a: Point, b: Point, p: Point, tolerance: Option<f64>) -> bool {
    let tol = tolerance.unwrap_or(TOL);

    // vertical line
    if almost_equal(a.x, b.x, Some(tol)) && almost_equal(p.x, a.x, Some(tol)) {
        return !almost_equal(p.y, b.y, Some(tol))
            && !almost_equal(p.y, a.y, Some(tol))
            && p.y < b.y.max(a.y).max(tol)
            && p.y > b.y.min(a.y).min(tol);
    }

    // horizontal line
    if almost_equal(a.y, b.y, Some(tol)) && almost_equal(p.y, a.y, Some(tol)) {
        return !almost_equal(p.x, b.x, Some(tol))
            && !almost_equal(p.x, a.x, Some(tol))
            && p.x < b.x.max(a.x)
            && p.x > b.x.min(a.x);
    }

    // range check
    if (p.x < a.x && p.x < b.x)
        || (p.x > a.x && p.x > b.x)
        || (p.y < a.y && p.y < b.y)
        || (p.y > a.y && p.y > b.y)
    {
        return false;
    }

    // exclude end points
    if (almost_equal(p.x, a.x, Some(tol)) && almost_equal(p.y, a.y, Some(tol)))
        || (almost_equal(p.x, b.x, Some(tol)) && almost_equal(p.y, b.y, Some(tol)))
    {
        return false;
    }

    let cross = (p.y - a.y) * (b.x - a.x) - (p.x - a.x) * (b.y - a.y);
    if cross.abs() > tol {
        return false;
    }

    let dot = (p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y);
    if dot < 0.0 || almost_equal(dot, 0.0, Some(tol)) {
        return false;
    }

    let len2 = (b.x - a.x) * (b.x - a.x) + (b.y - a.y) * (b.y - a.y);
    if dot > len2 || almost_equal(dot, len2, Some(tol)) {
        return false;
    }

    true
}

/// Port of `_lineIntersect`: intersection of AB and EF, or `None` if there is
/// no intersection or a numerical error. If `infinite` is set, AB/EF are
/// treated as infinite lines rather than finite segments.
pub fn line_intersect(a: Point, b: Point, e: Point, f: Point, infinite: bool) -> Option<Point> {
    let a1 = b.y - a.y;
    let b1 = a.x - b.x;
    let c1 = b.x * a.y - a.x * b.y;
    let a2 = f.y - e.y;
    let b2 = e.x - f.x;
    let c2 = f.x * e.y - e.x * f.y;

    let denom = a1 * b2 - a2 * b1;

    let x = (b1 * c2 - b2 * c1) / denom;
    let y = (a2 * c1 - a1 * c2) / denom;

    if !x.is_finite() || !y.is_finite() {
        return None;
    }

    if !infinite {
        // coincident points do not count as intersecting
        if (a.x - b.x).abs() > TOL
            && (if a.x < b.x {
                x < a.x || x > b.x
            } else {
                x > a.x || x < b.x
            })
        {
            return None;
        }
        if (a.y - b.y).abs() > TOL
            && (if a.y < b.y {
                y < a.y || y > b.y
            } else {
                y > a.y || y < b.y
            })
        {
            return None;
        }
        if (e.x - f.x).abs() > TOL
            && (if e.x < f.x {
                x < e.x || x > f.x
            } else {
                x > e.x || x < f.x
            })
        {
            return None;
        }
        if (e.y - f.y).abs() > TOL
            && (if e.y < f.y {
                y < e.y || y > f.y
            } else {
                y > e.y || y < f.y
            })
        {
            return None;
        }
    }

    Some(Point::new(x, y))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Port of `getPolygonBounds`.
pub fn get_polygon_bounds(polygon: &[Point]) -> Option<Bounds> {
    if polygon.len() < 3 {
        return None;
    }

    let mut xmin = polygon[0].x;
    let mut xmax = polygon[0].x;
    let mut ymin = polygon[0].y;
    let mut ymax = polygon[0].y;

    for p in &polygon[1..] {
        if p.x > xmax {
            xmax = p.x;
        } else if p.x < xmin {
            xmin = p.x;
        }
        if p.y > ymax {
            ymax = p.y;
        } else if p.y < ymin {
            ymin = p.y;
        }
    }

    Some(Bounds {
        x: xmin,
        y: ymin,
        width: xmax - xmin,
        height: ymax - ymin,
    })
}

/// Port of `pointInPolygon`. `offset` is added to every polygon vertex before
/// testing (mirrors JS's `polygon.offsetx`/`offsety`). Returns `None` if the
/// point lies exactly on a vertex or edge (JS returns `null` there too).
pub fn point_in_polygon(
    point: Point,
    polygon: &[Point],
    offset: Point,
    tolerance: Option<f64>,
) -> Option<bool> {
    if polygon.len() < 3 {
        return None;
    }
    let tol = tolerance.unwrap_or(TOL);

    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let pi = Point::new(polygon[i].x + offset.x, polygon[i].y + offset.y);
        let pj = Point::new(polygon[j].x + offset.x, polygon[j].y + offset.y);

        if almost_equal(pi.x, point.x, Some(tol)) && almost_equal(pi.y, point.y, Some(tol)) {
            return None;
        }

        if on_segment(pi, pj, point, Some(tol)) {
            return None;
        }

        if almost_equal(pi.x, pj.x, Some(tol)) && almost_equal(pi.y, pj.y, Some(tol)) {
            j = i;
            continue;
        }

        let intersect = (pi.y > point.y) != (pj.y > point.y)
            && point.x < (pj.x - pi.x) * (point.y - pi.y) / (pj.y - pi.y) + pi.x;
        if intersect {
            inside = !inside;
        }
        j = i;
    }

    Some(inside)
}

/// Port of `polygonArea`. A negative area indicates counter-clockwise winding.
#[must_use]
pub fn polygon_area(polygon: &[Point]) -> f64 {
    if polygon.is_empty() {
        return 0.0;
    }
    let mut area = 0.0;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        area += (polygon[j].x + polygon[i].x) * (polygon[j].y - polygon[i].y);
        j = i;
    }
    0.5 * area
}

/// Port of `isRectangle`.
#[must_use]
pub fn is_rectangle(poly: &[Point], tolerance: Option<f64>) -> bool {
    let Some(bb) = get_polygon_bounds(poly) else {
        return false;
    };
    let tol = tolerance.unwrap_or(TOL);

    poly.iter().all(|p| {
        (almost_equal(p.x, bb.x, Some(tol)) || almost_equal(p.x, bb.x + bb.width, Some(tol)))
            && (almost_equal(p.y, bb.y, Some(tol)) || almost_equal(p.y, bb.y + bb.height, Some(tol)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_area_returns_signed_area_of_a_simple_square() {
        let square = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        assert!((polygon_area(&square).abs() - 100.0).abs() < 1e-6);
    }

    /// The `len() - 1` seed underflows on an empty slice. Clipper boolean
    /// ops can hand back a degenerate path, and every caller here reaches
    /// for `.abs()` on the result rather than checking the input first.
    #[test]
    fn an_empty_polygon_has_zero_area_rather_than_panicking() {
        assert_eq!(polygon_area(&[]), 0.0);
        assert_eq!(polygon_area(&[Point::new(1.0, 2.0)]), 0.0);
    }

    #[test]
    fn get_polygon_bounds_computes_the_bounding_box() {
        let poly = [
            Point::new(-5.0, 2.0),
            Point::new(8.0, 2.0),
            Point::new(8.0, 20.0),
            Point::new(-5.0, 20.0),
        ];
        let bounds = get_polygon_bounds(&poly).unwrap();
        assert_eq!(
            bounds,
            Bounds {
                x: -5.0,
                y: 2.0,
                width: 13.0,
                height: 18.0
            }
        );
    }

    #[test]
    fn almost_equal_respects_tolerance() {
        assert!(almost_equal(1.0, 1.0000001, Some(1e-3)));
        assert!(!almost_equal(1.0, 1.1, Some(1e-3)));
    }
}
