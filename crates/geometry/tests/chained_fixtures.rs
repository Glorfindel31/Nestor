//! `dxf_import::entities_to_polygons_chained` against a real file.
//!
//! **Why this fixture exists.** The chainer handles the drawings whose
//! profiles are not a single entity: loose `LINE`s and partial `ARC`s that
//! only enclose anything once their endpoints are joined up. That is an
//! ordinary way for a CAD package to export, and it is a completely separate
//! algorithm from the per-entity conversion beside it - but the fixture that
//! covered it was removed with the old fixture set, and none of
//! `one`/`two`/`three`/`curvy` has a single `LINE` entity in it.
//!
//! It got more exposed, not less, when `SPLINE` import landed: a spline that
//! does not return to its own start is handed to this same chainer, so a real
//! drawing whose profile is a spline meeting a couple of lines goes straight
//! through here. `curvy.dxf`'s two splines are both closed, so it does not
//! cover it.
//!
//! `chained.dxf` is deliberately minimal and hand-written: four loose lines
//! that form a 100 x 40 rectangle - one of them drawn backwards, because a
//! real drawing does not care which way round a line was drawn - and a "D"
//! made of three lines and a half-circle `ARC`, so a curved segment is
//! chained too.

use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, entities_to_polygons_chained};
use geometry::polygon::{get_polygon_bounds, polygon_area};

const CURVE_TOLERANCE: f64 = 0.1;

fn drawing() -> dxf::Drawing {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/chained.dxf");
    dxf::Drawing::load_file(&path).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", path.display()))
}

/// The premise. If the plain per-entity pass ever starts finding profiles in
/// this file, the fixture has stopped testing what it is for.
#[test]
fn no_single_entity_in_this_file_is_a_profile() {
    let d = drawing();
    let found = entities_to_polygons(d.entities(), CURVE_TOLERANCE);
    assert!(found.is_empty(), "expected no closed profile without chaining, got {}", found.len());
}

#[test]
fn loose_lines_and_arcs_chain_into_two_closed_profiles() {
    let d = drawing();
    let mut found = entities_to_polygons_chained(d.entities(), CURVE_TOLERANCE);
    assert_eq!(found.len(), 2, "expected the rectangle and the D");

    found.sort_by(|a, b| {
        let (x, y) = (get_polygon_bounds(&a.points).expect("bounds"), get_polygon_bounds(&b.points).expect("bounds"));
        x.x.total_cmp(&y.x)
    });

    let rect = get_polygon_bounds(&found[0].points).expect("bounds");
    assert!((rect.width - 100.0).abs() < 1e-6, "rectangle width was {}", rect.width);
    assert!((rect.height - 40.0).abs() < 1e-6, "rectangle height was {}", rect.height);
    assert!((polygon_area(&found[0].points).abs() - 4000.0).abs() < 1e-6, "the rectangle should enclose exactly its own box");

    // The D: a 60 x 60 box, and an area of 60x30 plus a half-disc of r=30.
    // Tessellated, so compared against the tolerance rather than exactly.
    let d_shape = get_polygon_bounds(&found[1].points).expect("bounds");
    assert!((d_shape.width - 90.0).abs() < 0.05, "D width was {}", d_shape.width);
    assert!((d_shape.height - 60.0).abs() < 0.05, "D height was {}", d_shape.height);
    let expected = 60.0 * 60.0 + std::f64::consts::PI * 30.0 * 30.0 / 2.0;
    let area = polygon_area(&found[1].points).abs();
    assert!((area - expected).abs() / expected < 0.01, "D area was {area:.1}, expected about {expected:.1}");

    // Layer identity has to survive the chaining, not just the geometry -
    // a profile that loses its layer is a profile that cannot be cut.
    assert!(found.iter().all(|p| p.layer == "CUT"), "layers were {:?}", found.iter().map(|p| &p.layer).collect::<Vec<_>>());
}

/// Two separate profiles, neither inside the other, must come out as two
/// top-level shapes - not one with the other as its hole.
#[test]
fn chained_profiles_are_siblings_not_holes() {
    let tree = build_polygon_tree(entities_to_polygons_chained(drawing().entities(), CURVE_TOLERANCE));
    assert_eq!(tree.len(), 2);
    assert!(tree.iter().all(|p| p.children.is_empty()), "neither profile contains the other");
}
