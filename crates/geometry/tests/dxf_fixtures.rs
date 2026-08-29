//! Integration tests against the real DXF fixtures in `tests/fixtures/`.
//!
//! These are actual CAD exports, not synthetic geometry, and that is the
//! whole point: every bug these have caught (holes escaping their parent,
//! circle metadata being dropped, simplification eating a profile's area)
//! came from a property real files have and hand-built test polygons do not.
//!
//! What each fixture is for:
//!
//! | fixture     | what makes it useful                                        |
//! |-------------|-------------------------------------------------------------|
//! | `one.dxf`   | the hat monotile - one exact 13-vertex interlocking tile     |
//! | `two.dxf`   | 4 parts, 12 circular drill holes nested under 2 of them, two layers |
//! | `three.dxf` | a 718-point profile, i.e. real curve-tessellated geometry    |
//!
//! Counts asserted below were read off the files with
//! `cargo run -p geometry --example inspect_fixture -- <file>`, not guessed
//! and then relaxed until they passed.
//!
//! **Known coverage gap**: nothing here exercises
//! `entities_to_polygons_chained` (profiles drawn as loose `LINE`/`ARC`
//! networks that only close once endpoints are joined). The fixture that
//! covered it was removed, and none of the three above contains `LINE`
//! entities. The code path is still live and still used by import - it just
//! has no fixture-backed test any more. Restore one if that path is ever
//! touched.

use dxf::Drawing;
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons};
use geometry::inner_nfp::inner_nfp;
use geometry::point::Point;
use geometry::polygon::polygon_area;
use geometry::simplify_polygon::{simplify_polygon, SimplifyConfig};

const CURVE_TOLERANCE: f64 = 0.01;

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn load(name: &str) -> Vec<geometry::dxf_import::LayeredPolygon> {
    let drawing = Drawing::load_file(fixture_path(name)).unwrap_or_else(|e| panic!("{name} should parse: {e}"));
    entities_to_polygons(drawing.entities(), CURVE_TOLERANCE)
}

/// Layer identity and circle metadata must survive import - the two things
/// the whole DXF-only scope change exists to preserve.
#[test]
fn circle_metadata_and_layer_identity_survive_import() {
    let polygons = load("two.dxf");
    assert_eq!(polygons.len(), 16, "expected 16 closed profiles, got {}", polygons.len());

    let layers: std::collections::HashSet<&str> = polygons.iter().map(|p| p.layer.as_str()).collect();
    assert!(layers.contains("VISIBLE"), "expected a `VISIBLE` layer, got {layers:?}");
    assert!(layers.len() >= 2, "layer identity collapsed to one layer: {layers:?}");

    // Every circle-derived polygon must carry its metadata: the circular-NFP
    // fast path keys off exactly this, and losing it silently downgrades
    // every drill hole to a generic polygon.
    let circles: Vec<_> = polygons.iter().filter(|p| p.is_circle.is_some()).collect();
    assert_eq!(circles.len(), 12, "expected 12 circles, got {}", circles.len());
    for c in &circles {
        let circle = c.is_circle.expect("filtered on is_circle");
        assert!(circle.r > 0.0, "circle radius should be positive, got {}", circle.r);
        assert!(c.points.len() >= 3, "circle should tessellate to at least a triangle");
    }
}

/// Regression test for a real containment bug in `dxf_import::contains`: it
/// used to test only a candidate loop's *first* vertex and treated
/// `point_in_polygon`'s "on the boundary" (`None`) as "not contained". Real
/// CAD exports often have a cutout sharing a coincident vertex with its
/// parent, so such a hole was promoted to a standalone root - and then nested
/// as if it were its own part, which is how you cut a hole out of the middle
/// of a sheet.
///
/// A *higher* outer-part count than expected is the signature of that bug
/// coming back.
#[test]
fn every_cutout_nests_under_its_real_parent_rather_than_escaping_as_a_part() {
    let tree = build_polygon_tree(load("two.dxf"));

    assert_eq!(tree.len(), 4, "expected exactly 4 outer parts, got {} - a higher count means cutouts are escaping as standalone parts again", tree.len());
    let holes: usize = tree.iter().map(|p| p.children.len()).sum();
    assert_eq!(holes, 12, "all 12 circles must sit under a parent, got {holes}");

    for part in &tree {
        for hole in &part.children {
            assert!(hole.is_circle.is_some(), "every hole in this fixture is a drilled circle");
            assert!(hole.children.is_empty(), "this fixture nests exactly one level deep (outline + holes), found an island under a hole");
        }
    }
}

/// Real curve-tessellated geometry through the simplification pipeline:
/// self-intersection cleanup, offset-shell re-merge and axis straightening
/// all run here against profiles that actually came out of CAD, including
/// `three.dxf`'s 718-point outline.
///
/// The assertion is deliberately loose on *how much* simplification changes a
/// profile and strict on it not destroying one - the pipeline is allowed to
/// reshape a polygon, but a profile that loses half its area has stopped
/// being the part someone drew.
#[test]
fn simplify_polygon_survives_every_real_cut_profile() {
    let config = SimplifyConfig { curve_tolerance: 0.1, use_convex_hull: false };
    let mut checked = 0;

    for fixture in ["one.dxf", "two.dxf", "three.dxf"] {
        let polygons = load(fixture);
        for (i, profile) in polygons.iter().filter(|p| p.is_circle.is_none()).enumerate() {
            let original_area = polygon_area(&profile.points).abs();
            if original_area < 1e-6 {
                continue; // degenerate source polygon - nothing to simplify
            }
            let (result, _holes) = simplify_polygon(&profile.points, false, &config);
            assert!(result.len() >= 3, "{fixture} profile {i} simplified to fewer than 3 points");

            let ratio = polygon_area(&result).abs() / original_area;
            assert!((0.5..1.5).contains(&ratio), "{fixture} profile {i}: area changed too much (ratio {ratio})");
            checked += 1;
        }
    }
    assert!(checked >= 6, "expected to check several real profiles, only checked {checked}");
}

/// The general inner-NFP fallback - the plan's flagged "hardest sub-problem" -
/// against genuine container-with-holes cases rather than the synthetic
/// square-with-one-hole in `inner_nfp.rs`'s own unit tests.
///
/// The result is data-dependent (a probe may or may not fit), so what is
/// asserted is that the computation completes rather than panicking, which is
/// the failure mode this path has actually had.
#[test]
fn inner_nfp_general_fallback_works_against_real_drilled_profiles() {
    let tree = build_polygon_tree(load("two.dxf"));

    let drilled: Vec<_> = tree.iter().filter(|p| p.children.len() >= 2).collect();
    assert!(!drilled.is_empty(), "expected at least one profile with multiple drill holes");

    let probe = geometry::dxf_import::LayeredPolygon::new(vec![Point::new(0.0, 0.0), Point::new(0.5, 0.0), Point::new(0.5, 0.5), Point::new(0.0, 0.5)], "0".into(), None);

    let mut checked = 0;
    for profile in drilled.iter().take(10) {
        let _ = inner_nfp(profile, &probe, 0.1);
        checked += 1;
    }
    assert!(checked > 0);
}

/// The hat is an exactly-interlocking aperiodic tile, so its geometry is
/// razor-edged: a rounding change anywhere in import moves it. This pins the
/// shape the whole `hat_bench` baseline rests on.
#[test]
fn the_hat_fixture_imports_as_one_exact_thirteen_vertex_tile() {
    let tree = build_polygon_tree(load("one.dxf"));
    assert_eq!(tree.len(), 1, "the hat fixture is a single profile");
    assert_eq!(tree[0].points.len(), 13, "the hat is a 13-vertex polykite");
    assert!(tree[0].children.is_empty(), "the hat has no holes");

    let area = polygon_area(&tree[0].points).abs();
    assert!((area - 779.4).abs() < 0.5, "hat area drifted: {area}");
}

/// `fillets.dxf` is a 60x40 plate whose curves are deliberately small: 2mm
/// corner radii, a 1mm-radius square hole and a 0.75mm notch. At the app's
/// default 0.3mm `curve_tolerance` a flat error budget lets every one of them
/// collapse to a two-chord approximation - the plate came in 1.25mm2 light
/// and the notch lost 15% of its area. `arc_step_angle`'s relative-sagitta
/// cap is what keeps the error proportional to each arc's own radius, so this
/// asserts the imported area at the *loose* tolerance, which is the setting
/// the bug lived at.
#[test]
fn small_fillets_survive_a_loose_curve_tolerance() {
    let drawing = Drawing::load_file(fixture_path("fillets.dxf")).expect("fillets.dxf must parse");
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), 0.3));
    assert_eq!(tree.len(), 1, "the plate is one profile with two holes");

    // 60x40 less four quarter-circles of r=2 cut off its corners.
    let true_area = 60.0 * 40.0 - 4.0 * (4.0 - std::f64::consts::PI);
    let area = polygon_area(&tree[0].points).abs();
    assert!((area - true_area).abs() < true_area * 0.001, "outline area {area} is off {true_area} by more than 0.1%");

    // The 0.75mm-radius notch: the smallest curve in the file, and the one a
    // flat tolerance mangles worst.
    let notch = tree[0].children.iter().min_by(|a, b| polygon_area(&a.points).abs().total_cmp(&polygon_area(&b.points).abs())).expect("the plate has holes");
    let true_notch = 1.5 * 1.5 + std::f64::consts::PI * 0.75 * 0.75;
    let notch_area = polygon_area(&notch.points).abs();
    assert!((notch_area - true_notch).abs() < true_notch * 0.02, "notch area {notch_area} is off {true_notch} by more than 2%");
}
