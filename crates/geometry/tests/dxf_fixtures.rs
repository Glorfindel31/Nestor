//! Integration test against a real DXF fixture (copied from the Electron
//! repo's tests/assets/FLAT.dxf into tests/fixtures/ per the plan's repo
//! structure) - a real laser/CNC-cut sheet layout with a `drilling` layer
//! containing thousands of small circles, exactly the scenario that drove
//! the DXF-only, layer-retaining scope change recorded in docs/PORT_STATUS.md.

use dxf::Drawing;
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons};
use geometry::inner_nfp::inner_nfp;
use geometry::point::Point;
use geometry::polygon::polygon_area;
use geometry::simplify_polygon::{simplify_polygon, SimplifyConfig};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn loads_and_converts_the_flat_dxf_fixture() {
    let drawing = Drawing::load_file(fixture_path("FLAT.dxf")).expect("FLAT.dxf should parse");

    let entity_count = drawing.entities().count();
    assert!(entity_count > 3000, "expected thousands of entities, got {entity_count}");

    let polygons = entities_to_polygons(drawing.entities(), 0.01);
    assert!(!polygons.is_empty(), "expected at least some closed profiles");

    // the fixture is known (via manual inspection) to use these three layers
    let layers: std::collections::HashSet<&str> = polygons.iter().map(|p| p.layer.as_str()).collect();
    assert!(layers.contains("drilling"), "expected a `drilling` layer, got {layers:?}");

    // every circle-derived polygon must carry isCircle metadata with a positive radius
    let circle_polys: Vec<_> = polygons.iter().filter(|p| p.is_circle.is_some()).collect();
    assert!(!circle_polys.is_empty(), "expected circle entities to convert with isCircle metadata");
    for p in &circle_polys {
        let c = p.is_circle.unwrap();
        assert!(c.r > 0.0, "circle radius should be positive, got {}", c.r);
        assert!(p.points.len() >= 3, "circle should tessellate to at least a triangle");
    }
}

#[test]
fn loads_the_struck_flat_dxf_fixture_without_error() {
    let drawing = Drawing::load_file(fixture_path("FLAT-struck.dxf")).expect("FLAT-struck.dxf should parse");
    let polygons = entities_to_polygons(drawing.entities(), 0.01);
    assert!(!polygons.is_empty());
}

#[test]
fn simplify_polygon_runs_without_panicking_on_every_real_cut_profile() {
    // Regression check against real, non-synthetic geometry: the 99
    // LWPOLYLINE-derived cut profiles in FLAT.dxf (excludes the 3079 CIRCLE
    // drill holes, checked separately above) exercise self-intersection
    // cleanup, offset reversal, and axis straightening against actual
    // laser-cut part outlines, not just synthetic squares.
    let drawing = Drawing::load_file(fixture_path("FLAT.dxf")).expect("FLAT.dxf should parse");
    let polygons = entities_to_polygons(drawing.entities(), 0.01);
    let profiles: Vec<_> = polygons.iter().filter(|p| p.is_circle.is_none()).collect();
    assert!(profiles.len() > 50, "expected dozens of cut profiles, got {}", profiles.len());

    let config = SimplifyConfig { curve_tolerance: 0.1, use_convex_hull: false };
    for (i, profile) in profiles.iter().enumerate() {
        let original_area = polygon_area(&profile.points).abs();
        if original_area < 1e-6 {
            continue; // degenerate/zero-area source polygon, nothing meaningful to simplify
        }

        let (result, _holes) = simplify_polygon(&profile.points, false, &config);
        assert!(result.len() >= 3, "profile {i} simplified to fewer than 3 points");

        let result_area = polygon_area(&result).abs();
        let ratio = result_area / original_area;
        assert!(
            (0.5..1.5).contains(&ratio),
            "profile {i}: area changed too much ({original_area} -> {result_area}, ratio {ratio})"
        );
    }
}

#[test]
fn inner_nfp_general_fallback_works_against_real_drilled_profiles() {
    // FLAT.dxf's 99 cut profiles each have their drill-hole circles nested as
    // real children (per the polygon-tree test), giving genuine
    // container-with-holes cases for the "hardest sub-problem" general
    // fallback - not just the synthetic square-with-one-hole test in
    // inner_nfp.rs's own unit tests.
    let drawing = Drawing::load_file(fixture_path("FLAT.dxf")).expect("FLAT.dxf should parse");
    let polygons = entities_to_polygons(drawing.entities(), 0.01);
    let tree = build_polygon_tree(polygons);

    let drilled: Vec<_> = tree.iter().filter(|p| p.children.len() >= 2).collect();
    assert!(!drilled.is_empty(), "expected at least one profile with multiple drill holes");

    let mut checked = 0;
    for profile in drilled.iter().take(10) {
        // a tiny probe part - small enough that it should fit somewhere
        // inside the profile without necessarily colliding with every hole
        let probe = geometry::dxf_import::LayeredPolygon {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(0.5, 0.0),
                Point::new(0.5, 0.5),
                Point::new(0.0, 0.5),
            ],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        };

        // must not panic - the actual result (Some or None) is real data
        // dependent, but the computation itself must complete cleanly
        let _ = inner_nfp(profile, &probe, 0.1);
        checked += 1;
    }
    assert!(checked > 0);
}
