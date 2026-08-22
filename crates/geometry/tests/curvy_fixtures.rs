//! Cross-format parity for **curved** geometry: `curvy.dxf` against
//! `curvy.svg`, the same artwork saved two ways.
//!
//! This is the independent check on `dxf_import::tessellate_spline`. The DXF
//! stores the shape as clamped cubic B-splines - control points, knot vectors,
//! de Boor evaluation; the SVG stores it as cubic Beziers in one `<path>`,
//! written by different software. Nothing is shared between the two code
//! paths, so if the B-spline evaluation mishandled a knot span, or picked the
//! wrong span at a segment join, the two would disagree here and nowhere else.
//!
//! **Why this compares shape descriptors rather than vertices**, unlike
//! `svg_fixtures.rs`. That test compares `one.dxf`/`one.svg` edge vector by
//! edge vector at engine tolerance, and it can, because both files store the
//! same 13 explicit vertices. These two store *curves*, which each importer
//! tessellates on its own terms - different point counts, different vertex
//! positions, both correct. There is no common vertex set to compare, so the
//! comparison has to be over quantities a tessellation converges on:
//! proportions, not coordinates.
//!
//! The descriptors are all scale-free on purpose, because the two files are
//! not the same size - see `a_unitless_svg_is_scaled_by_a_fallback_that_can_be_wrong`
//! for why, and for the real trap that sits behind it.

use dxf::Drawing;
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, LayeredPolygon};
use geometry::polygon::{get_polygon_bounds, polygon_area};
use geometry::svg_import::parse_svg;

const CURVE_TOLERANCE: f64 = 0.02;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn from_dxf() -> LayeredPolygon {
    let drawing = Drawing::load_file(fixture("curvy.dxf")).expect("curvy.dxf should parse");
    let mut tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    assert_eq!(tree.len(), 1, "curvy.dxf is one profile with a hole in it");
    tree.remove(0)
}

/// Through the same two steps `commands::import_svg` uses - `parse_svg`
/// deliberately returns a flat list and leaves containment nesting to
/// `build_polygon_tree`, exactly as `entities_to_polygons` does.
fn from_svg() -> LayeredPolygon {
    let text = std::fs::read_to_string(fixture("curvy.svg")).expect("curvy.svg should read");
    let flat = parse_svg(&text, CURVE_TOLERANCE, None).expect("curvy.svg should parse");
    let mut tree = build_polygon_tree(flat);
    assert_eq!(tree.len(), 1, "the round subpath must nest as a hole, not stand alone as a second part");
    tree.remove(0)
}

/// Scale-free descriptions of an outline: how square it is, how much of its
/// own box it fills, and how convoluted its boundary is. A wrong curve
/// evaluation moves at least one of these.
struct Shape {
    aspect: f64,
    /// Area over bounding-box area.
    fill: f64,
    /// Perimeter over sqrt(area) - dimensionless, and the one that reacts to
    /// a boundary that wanders where it should not.
    convolution: f64,
}

fn describe(points: &[geometry::point::Point]) -> Shape {
    let bounds = get_polygon_bounds(points).expect("has points");
    let area = polygon_area(points).abs();
    let perimeter: f64 = (0..points.len()).map(|i| points[i].distance_to(points[(i + 1) % points.len()])).sum();
    Shape { aspect: bounds.width / bounds.height, fill: area / (bounds.width * bounds.height), convolution: perimeter / area.sqrt() }
}

/// The outer profile is the same shape from both files.
#[test]
fn the_spline_outline_matches_the_bezier_one() {
    let dxf = describe(&from_dxf().points);
    let svg = describe(&from_svg().points);

    assert!((dxf.aspect - svg.aspect).abs() < 1e-3, "aspect ratio: dxf {:.5} vs svg {:.5}", dxf.aspect, svg.aspect);
    assert!((dxf.fill - svg.fill).abs() < 1e-3, "area as a fraction of the bounding box: dxf {:.5} vs svg {:.5}", dxf.fill, svg.fill);
    assert!(
        (dxf.convolution - svg.convolution).abs() / dxf.convolution < 0.01,
        "boundary convolution: dxf {:.5} vs svg {:.5}",
        dxf.convolution,
        svg.convolution
    );
}

/// The hole is the same hole, in the same place, and it is round in both.
///
/// The round hole is the sharpest part of this test: a circle's area is
/// `pi/4` of its bounding box, and a B-spline evaluation that is wrong in an
/// interesting way will not land on 0.7854 by accident.
#[test]
fn the_round_hole_matches_and_is_actually_round() {
    let (dxf, svg) = (from_dxf(), from_svg());
    assert_eq!(dxf.children.len(), 1, "curvy.dxf's inner spline is a hole");
    assert_eq!(svg.children.len(), 1, "curvy.svg's second subpath is a hole");

    for (label, shape) in [("dxf", &dxf), ("svg", &svg)] {
        let outer = get_polygon_bounds(&shape.points).expect("has points");
        let hole = get_polygon_bounds(&shape.children[0].points).expect("has points");
        let hole_fill = polygon_area(&shape.children[0].points).abs() / (hole.width * hole.height);
        assert!(
            (hole_fill - std::f64::consts::FRAC_PI_4).abs() < 0.002,
            "{label}: the hole is a circle, so it must fill pi/4 of its box, got {hole_fill:.5}"
        );
        assert!((hole.width / hole.height - 1.0).abs() < 1e-3, "{label}: a circle's box is square, got {:.5}", hole.width / hole.height);

        // Same size and same position relative to the part that contains it.
        let relative_size = hole.width / outer.width;
        let centre_x = (hole.x + hole.width / 2.0 - outer.x) / outer.width;
        let centre_y = (hole.y + hole.height / 2.0 - outer.y) / outer.height;
        assert!((relative_size - 0.20796).abs() < 1e-3, "{label}: hole size relative to the part: {relative_size:.5}");
        assert!((centre_x - 0.38827).abs() < 1e-3, "{label}: hole centre x: {centre_x:.5}");
        assert!((centre_y - 0.55756).abs() < 1e-3, "{label}: hole centre y: {centre_y:.5}");
    }
}

/// **The two files are not the same size, and the reason is a trap worth
/// pinning down.**
///
/// `curvy.svg` carries a `viewBox` and no `width`/`height`, so nothing in the
/// file says how big the drawing is in the real world. `parse_svg` falls back
/// to treating user units as CSS pixels at 96dpi: 1354.04 units becomes
/// 358.25mm.
///
/// But this file's units are PostScript points, at 72dpi - 1354.04pt really is
/// the DXF's 477.63mm. So the fallback lands the part at exactly three
/// quarters of its true size, and nothing in the file can tell the importer
/// otherwise. This is precisely why the app prompts for a unit on every SVG
/// import, and why DXF stays the primary path: its coordinates are already
/// millimetres and need no guess at all.
///
/// Asserted so neither half can drift silently. The 96dpi fallback resizes
/// every unitless SVG anyone has imported; the 4/3 is this fixture's own
/// reminder that a clean import is not the same thing as a correctly-sized one.
#[test]
fn a_unitless_svg_is_scaled_by_a_fallback_that_can_be_wrong() {
    let dxf = get_polygon_bounds(&from_dxf().points).expect("has points");
    let svg = get_polygon_bounds(&from_svg().points).expect("has points");

    // The documented fallback: the 1354.04-unit viewBox read as 96dpi pixels.
    assert!((svg.width - 1354.04 * 25.4 / 96.0).abs() < 0.5, "expected the 96dpi pixel fallback, got {:.2}mm", svg.width);

    // ...which is 3/4 of the truth for this file, because its units are points.
    let ratio = dxf.width / svg.width;
    assert!((ratio - 96.0 / 72.0).abs() < 0.01, "expected the part to come in 3/4 size, got a ratio of {ratio:.4}");

    // Uniformly, though - which is what keeps it the same shape.
    assert!(
        (dxf.width / svg.width - dxf.height / svg.height).abs() < 1e-3,
        "scaling must be uniform: {:.5} horizontally vs {:.5} vertically",
        dxf.width / svg.width,
        dxf.height / svg.height
    );
}

/// The other half of the fallback: it has to be *reported*, not just
/// documented in a test. A part that imports cleanly at 3/4 size is only
/// catchable by the person who drew it, so the app says so out loud - and
/// this is what proves the flag it says it on actually fires.
#[test]
fn a_guessed_size_is_flagged_as_guessed() {
    let svg = std::fs::read_to_string(fixture("curvy.svg")).expect("curvy.svg should read");
    assert!(geometry::svg_import::size_is_guessed(&svg), "curvy.svg has a viewBox and no width/height - its size is a guess");

    // A file that does say how big it is must not be flagged, or the warning
    // becomes noise on every import and stops being read.
    let sized = svg.replacen("<svg", "<svg width=\"100mm\" height=\"100mm\"", 1);
    assert!(sized.contains("width="), "the fixture rewrite must have taken");
    assert!(!geometry::svg_import::size_is_guessed(&sized), "an SVG with a real width/height is not a guess");

    // Nor must anything that is not a parseable SVG at all: it is about to
    // fail for a better reason.
    assert!(!geometry::svg_import::size_is_guessed("not xml"));
}
