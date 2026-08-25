//! Port of main/util/HullPolygon.ts's `hull()` (Andrew's monotone chain
//! convex hull algorithm, based on d3-polygon). Only `hull()` is ported -
//! the original also has `area`, `centroid`, `contains`, `length` methods,
//! but grepping the whole Electron repo shows zero call sites for any of
//! them; only `.hull()` is ever called (from `deepnest.js`'s
//! `getHull`/`simplifyPolygon` and `background.js`).

use crate::point::Point;

fn cross(a: Point, b: Point, c: Point) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Assumes `points` is already sorted lexicographically by (x, y). Returns
/// indices into `points`, in left-to-right order, forming the upper hull.
fn compute_upper_hull_indexes(points: &[Point]) -> Vec<usize> {
    let n = points.len();
    let mut indexes: Vec<usize> = vec![0, 1];
    let mut size = 2usize;

    for i in 2..n {
        while size > 1 && cross(points[indexes[size - 2]], points[indexes[size - 1]], points[i]) <= 0.0 {
            size -= 1;
        }
        if size == indexes.len() {
            indexes.push(i);
        } else {
            indexes[size] = i;
        }
        size += 1;
    }

    indexes.truncate(size);
    indexes
}

/// Port of `HullPolygon.hull`: the convex hull of `points`, in
/// counterclockwise order. `None` if fewer than 3 points are given.
pub fn hull(points: &[Point]) -> Option<Vec<Point>> {
    let n = points.len();
    if n < 3 {
        return None;
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| points[a].x.total_cmp(&points[b].x).then(points[a].y.total_cmp(&points[b].y)));

    let sorted_points: Vec<Point> = order.iter().map(|&i| points[i]).collect();
    let flipped_points: Vec<Point> = sorted_points.iter().map(|p| Point::new(p.x, -p.y)).collect();

    let upper_indexes = compute_upper_hull_indexes(&sorted_points);
    let lower_indexes = compute_upper_hull_indexes(&flipped_points);

    // compute_upper_hull_indexes always returns >= 2 indices: `size` starts
    // at 2 and its inner while loop stops at `size > 1`, so every iteration
    // of the outer `for i in 2..n` loop leaves it >= 2 after the trailing
    // `size += 1` - and `hull`'s own `n < 3` guard above guarantees at least
    // one such iteration runs. Never panics.
    let skip_left = lower_indexes[0] == upper_indexes[0];
    let skip_right = *lower_indexes.last().unwrap() == *upper_indexes.last().unwrap();

    let mut result = Vec::new();
    for &i in upper_indexes.iter().rev() {
        result.push(points[order[i]]);
    }
    let lower_start = if skip_left { 1 } else { 0 };
    let lower_end = lower_indexes.len() - usize::from(skip_right);
    for &i in &lower_indexes[lower_start..lower_end] {
        result.push(points[order[i]]);
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_fewer_than_three_points() {
        assert!(hull(&[Point::new(0.0, 0.0), Point::new(1.0, 1.0)]).is_none());
    }

    #[test]
    fn hull_of_a_square_with_an_interior_point_excludes_the_interior_point() {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
            Point::new(5.0, 5.0), // interior, must be excluded
        ];
        let h = hull(&points).expect("hull should exist");
        assert_eq!(h.len(), 4);
        assert!(!h.contains(&Point::new(5.0, 5.0)));
        for corner in &points[0..4] {
            assert!(h.contains(corner));
        }
    }

    #[test]
    fn hull_of_a_triangle_is_the_triangle_itself() {
        let points = [Point::new(0.0, 0.0), Point::new(4.0, 0.0), Point::new(2.0, 3.0)];
        let h = hull(&points).expect("hull should exist");
        assert_eq!(h.len(), 3);
    }
}

/// The angle, in degrees, that this outline has to be turned *by* for its
/// minimum-area bounding rectangle to sit axis-aligned. Always in `[0, 90)`,
/// because a rectangle maps onto itself every quarter turn.
///
/// **Why this exists.** The rotation grid a nest searches is `k * 360/n`
/// measured from the part's own file orientation, so a part drawn on the
/// diagonal is only ever offered diagonal placements. Measured on
/// `nestTest01.dxf` at 250 copies (1500x1500, spacing 5): the file as drawn
/// nests to 11 sheets at 23 parts each; the identical geometry saved out of
/// CAD rotated by 37 degrees nests to **13 sheets at 20 each**, an 18% worse
/// answer for a part that is the same shape. The commercial nester returns 11
/// either way. Turning every imported outline by this angle is what makes the
/// grid mean the same thing regardless of how the drawing happened to be
/// saved.
///
/// Rotating calipers, the naive way: the minimum-area rectangle of a convex
/// polygon always has a side flush with one of its edges, so trying every hull
/// edge finds it exactly. That is O(h^2) on the hull's point count, which is
/// tens of points on real parts and runs once per shape at import.
#[must_use]
pub fn min_area_rect_angle(points: &[Point]) -> f64 {
    let Some(h) = hull(points) else { return 0.0 };
    if h.len() < 3 {
        return 0.0;
    }
    let mut best = (f64::INFINITY, 0.0);
    for i in 0..h.len() {
        let (a, b) = (h[i], h[(i + 1) % h.len()]);
        let theta = (b.y - a.y).atan2(b.x - a.x);
        let (sin, cos) = (-theta).sin_cos();
        let (mut x0, mut x1, mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for p in &h {
            let (x, y) = (p.x * cos - p.y * sin, p.x * sin + p.y * cos);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let area = (x1 - x0) * (y1 - y0);
        if area < best.0 {
            best = (area, -theta.to_degrees().rem_euclid(90.0));
        }
    }
    best.1.rem_euclid(90.0)
}

#[cfg(test)]
mod min_area_rect_angle_tests {
    use super::*;

    fn turn(points: &[Point], degrees: f64) -> Vec<Point> {
        let (sin, cos) = degrees.to_radians().sin_cos();
        points.iter().map(|p| Point::new(p.x * cos - p.y * sin, p.x * sin + p.y * cos)).collect()
    }

    /// An axis-aligned part must be left exactly where it is, or normalising
    /// on import would move every drawing that is already correct.
    #[test]
    fn an_axis_aligned_outline_needs_no_turn() {
        let rect = [Point::new(0.0, 0.0), Point::new(280.0, 0.0), Point::new(280.0, 150.0), Point::new(0.0, 150.0)];
        assert!(min_area_rect_angle(&rect).abs() < 1e-9, "got {}", min_area_rect_angle(&rect));
    }

    /// The whole point: turning the drawing must not change where the part
    /// ends up once the angle is applied back.
    #[test]
    fn turning_the_drawing_is_undone_by_the_angle_it_reports() {
        let part = [Point::new(0.0, 0.0), Point::new(280.0, 0.0), Point::new(280.0, 150.0), Point::new(90.0, 150.0), Point::new(0.0, 60.0)];
        for drawn_at in [7.0, 37.0, 45.0, 63.0, 122.0] {
            let turned = turn(&part, drawn_at);
            let back = turn(&turned, min_area_rect_angle(&turned));
            let bounds = crate::polygon::get_polygon_bounds(&back).expect("has points");
            let want = crate::polygon::get_polygon_bounds(&part).expect("has points");
            assert!(
                (bounds.width - want.width).abs() < 1e-6 && (bounds.height - want.height).abs() < 1e-6
                    || (bounds.width - want.height).abs() < 1e-6 && (bounds.height - want.width).abs() < 1e-6,
                "drawn at {drawn_at}: got {}x{}, want {}x{} (either way round)",
                bounds.width,
                bounds.height,
                want.width,
                want.height
            );
        }
    }
}
