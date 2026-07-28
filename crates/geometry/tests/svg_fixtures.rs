//! Integration test against the real `hat-monotile.svg` fixture - the SVG
//! counterpart to `hat-monotile.dxf` (see `dxf_fixtures.rs`). Both files
//! describe the exact same real-world 13-sided "hat" aperiodic monotile, so
//! importing each and translating to a common origin (its own bounding-box
//! corner) must produce the identical point sequence - same winding, same
//! vertex order, not just the same area/bounding box. This is a direct
//! regression test for a real bug: SVG's Y-down coordinate system versus
//! this codebase's Y-up convention (DXF, Clipper2, NFP tracing) silently
//! mirrored every imported SVG shape and reversed its winding - same
//! unsigned area, opposite `polygon_area` sign - until `svg_import::
//! parse_svg` started negating the Y scale in its base transform.

use dxf::Drawing;
use geometry::dxf_import::entities_to_polygons;
use geometry::point::Point;
use geometry::polygon::{get_polygon_bounds, polygon_area};
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
    let drawing = Drawing::load_file(fixture_path("hat-monotile.dxf")).expect("hat-monotile.dxf should parse");
    let dxf_flat = entities_to_polygons(drawing.entities(), 0.1);
    assert_eq!(dxf_flat.len(), 1, "expected exactly one closed profile in hat-monotile.dxf");

    let svg_text = std::fs::read_to_string(fixture_path("hat-monotile.svg")).expect("hat-monotile.svg should read");
    let svg_flat = parse_svg(&svg_text, 0.1, None).expect("hat-monotile.svg should parse");
    assert_eq!(svg_flat.len(), 1, "expected exactly one closed profile in hat-monotile.svg");

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
}
