//! Integration test against the real `one.svg` fixture - the SVG
//! counterpart to `one.dxf` (see `dxf_fixtures.rs`). Both files
//! describe the exact same real-world 13-sided "hat" aperiodic monotile, so
//! importing each and translating to a common origin (its own bounding-box
//! corner) must produce the identical point sequence - same winding, same
//! vertex order, not just the same area/bounding box. This is a direct
//! regression test for a real bug: SVG's Y-down coordinate system versus
//! this codebase's Y-up convention (DXF, Clipper2, NFP tracing) silently
//! mirrored every imported SVG shape and reversed its winding - same
//! unsigned area, opposite `polygon_area` sign - until `svg_import::
//! parse_svg` started negating the Y scale in its base transform.
//!
//! It is also the regression test for a second, subtler bug. This test used
//! to compare vertices at `0.01` - seven decades looser than `TOL` - and so
//! reported the two fixtures as congruent while the SVG's own coordinates
//! were rounded to 8 decimals, leaving its edge lengths wrong by up to
//! 8.4e-9. For an exactly-interlocking tessellation that is a *different
//! shape*, and it cost ~10 points of utilisation on the hat benchmark
//! (`crates/nesting/examples/hat_test_svg.rs`) while looking for all the
//! world like a floating-point tie-break bug in the placement engine.
//! **Compare at engine tolerance, or this class of bug hides here again.**

use dxf::Drawing;
use geometry::dxf_import::entities_to_polygons;
use geometry::point::Point;
use geometry::polygon::{get_polygon_bounds, polygon_area, TOL};
use geometry::svg_import::parse_svg;

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn normalize(points: &[Point]) -> Vec<(f64, f64)> {
    let b = get_polygon_bounds(points).unwrap();
    points.iter().map(|p| (p.x - b.x, p.y - b.y)).collect()
}

#[test]
fn svg_import_matches_dxf_import_for_the_same_real_world_hat_tile() {
    let drawing = Drawing::load_file(fixture_path("one.dxf")).expect("one.dxf should parse");
    let dxf_flat = entities_to_polygons(drawing.entities(), 0.1);
    assert_eq!(dxf_flat.len(), 1, "expected exactly one closed profile in one.dxf");

    let svg_text = std::fs::read_to_string(fixture_path("one.svg")).expect("one.svg should read");
    let svg_flat = parse_svg(&svg_text, 0.1, None).expect("one.svg should parse");
    assert_eq!(svg_flat.len(), 1, "expected exactly one closed profile in one.svg");

    let dxf_area = polygon_area(&dxf_flat[0].points).abs();
    let svg_area = polygon_area(&svg_flat[0].points).abs();
    assert!((dxf_area - svg_area).abs() < 0.01, "dxf area {dxf_area} vs svg area {svg_area}");

    // Same winding *sign*, not just the same unsigned area - a mirrored
    // import keeps area identical while reversing vertex order.
    let dxf_signed = polygon_area(&dxf_flat[0].points);
    let svg_signed = polygon_area(&svg_flat[0].points);
    assert_eq!(dxf_signed.signum(), svg_signed.signum(), "SVG and DXF import must agree on winding direction (dxf {dxf_signed}, svg {svg_signed})");

    // Same vertex sequence, not just the same winding sign - translate both
    // to their own bounding-box corner and compare point-for-point.
    let dxf_norm = normalize(&dxf_flat[0].points);
    let svg_norm = normalize(&svg_flat[0].points);
    assert_eq!(dxf_norm.len(), svg_norm.len());
    for (i, (d, s)) in dxf_norm.iter().zip(svg_norm.iter()).enumerate() {
        assert!((d.0 - s.0).abs() < 0.01 && (d.1 - s.1).abs() < 0.01, "vertex {i}: dxf {d:?} vs svg {s:?}");
    }

    // The one that actually matters: edge *vectors* at engine tolerance.
    // Translation-invariant (so it tests shape, not position) and tight
    // enough to catch a fixture that was merely rounded - which is exactly
    // the bug that hid here before. See this file's module doc.
    let edges = |points: &[Point]| -> Vec<(f64, f64)> {
        (0..points.len())
            .map(|i| {
                let next = points[(i + 1) % points.len()];
                (next.x - points[i].x, next.y - points[i].y)
            })
            .collect()
    };
    for (i, (d, s)) in edges(&dxf_flat[0].points).iter().zip(edges(&svg_flat[0].points).iter()).enumerate() {
        let err = (d.0 - s.0).hypot(d.1 - s.1);
        assert!(err < TOL, "edge {i} differs by {err:.3e}, over the engine's own {TOL:.0e} tolerance: dxf {d:?} vs svg {s:?}");
    }
}
