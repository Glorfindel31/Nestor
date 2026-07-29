//! Port of `background.js`'s `getOuterNfp(A, B, inside=false, config)` - the
//! obstacle-avoidance NFP used when fitting a part around already-placed
//! parts on a sheet. Always takes the pure-JS Minkowski path (never the
//! addon/frame-trick `inside=true` branch, which `tryPlacePartOnSheet` never
//! calls) - see `crate::clipper::outer_nfp` for that half.
//!
//! The one extra piece over a plain `outer_nfp` call: A's own holes are
//! additional opportunities for B (B could nest entirely inside one of them),
//! computed per-hole via `inner_nfp` and attached as `children` - restored by
//! the caller as valid placement space after subtracting the obstacle's outer
//! NFP (which otherwise wrongly excludes those positions, since Minkowski
//! collision math only sees A's outer boundary).

use crate::dxf_import::LayeredPolygon;
use crate::inner_nfp::inner_nfp;
use crate::clipper::outer_nfp;
use crate::point::Point;
use crate::polygon::get_polygon_bounds;

#[derive(Clone, Debug, PartialEq)]
pub struct ObstacleNfp {
    pub outer: Vec<Point>,
    pub children: Vec<Vec<Point>>,
}

/// `a` is the stationary already-placed obstacle, `b` is the part being
/// fitted around it. Returns `None` if the underlying Minkowski difference
/// fails (degenerate input), matching `getOuterNfp`'s `null` return.
pub fn obstacle_nfp(a: &LayeredPolygon, b: &LayeredPolygon, curve_tolerance: f64) -> Option<ObstacleNfp> {
    let outer = outer_nfp(&a.points, &b.points)?;

    let mut children = Vec::new();
    if !a.children.is_empty() {
        if let Some(b_bounds) = get_polygon_bounds(&b.points) {
            for hole in &a.children {
                let Some(hole_bounds) = get_polygon_bounds(&hole.points) else {
                    continue;
                };
                if hole_bounds.width > b_bounds.width && hole_bounds.height > b_bounds.height {
                    if let Some(fits) = inner_nfp(hole, b, curve_tolerance) {
                        children.extend(fits);
                    }
                }
            }
        }
    }

    Some(ObstacleNfp { outer, children })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn holeless_obstacle_has_no_children() {
        let a = square_layered(0.0, 0.0, 10.0);
        let b = square_layered(0.0, 0.0, 2.0);
        let result = obstacle_nfp(&a, &b, 0.1).expect("nfp should exist");
        assert!(result.children.is_empty());
        let bounds = get_polygon_bounds(&result.outer).unwrap();
        assert!((bounds.width - 12.0).abs() < 1e-6);
    }

    #[test]
    fn obstacle_with_a_hole_big_enough_for_b_gets_a_restore_region() {
        // 20x20 obstacle with a 10x10 hole in the middle - big enough for a
        // 2x2 part `b` to nest inside.
        let hole = LayeredPolygon {
            points: vec![
                Point::new(5.0, 5.0),
                Point::new(15.0, 5.0),
                Point::new(15.0, 15.0),
                Point::new(5.0, 15.0),
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

        let result = obstacle_nfp(&a, &b, 0.1).expect("nfp should exist");
        assert_eq!(result.children.len(), 1);
    }

    #[test]
    fn obstacle_with_a_hole_too_small_for_b_gets_no_restore_region() {
        let hole = LayeredPolygon {
            points: vec![
                Point::new(9.0, 9.0),
                Point::new(11.0, 9.0),
                Point::new(11.0, 11.0),
                Point::new(9.0, 11.0),
            ],
            layer: "DRILL".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };
        let mut a = square_layered(0.0, 0.0, 20.0);
        a.children.push(hole);
        let b = square_layered(0.0, 0.0, 5.0);

        let result = obstacle_nfp(&a, &b, 0.1).expect("nfp should exist");
        assert!(result.children.is_empty());
    }
}
