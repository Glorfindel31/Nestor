//! The band packer against real fixture geometry, not synthetic rectangles.
//!
//! `banded`'s own unit tests all pass on clean shapes built in code, and the
//! module was still producing overlapping, off-sheet placements the moment it
//! saw a real part. That gap is the whole reason this file exists: padded
//! outlines, bevelled corners, holes and coordinates that do not start at the
//! origin are all things a hand-built test triangle quietly does not have.

use dxf::Drawing;
use geometry::clearance::{prepare_part, prepare_sheet};
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, rotate_layered_polygon, shift_layered_polygon, LayeredPolygon};
use geometry::point::Point;
use geometry::polygon::{get_polygon_bounds, polygon_area};
use nesting::banded::pack_sheet;
use nesting::placement::{has_material_outside_sheet, has_material_overlap, NestPart};

const CURVE_TOLERANCE: f64 = 0.3;
const SPACING: f64 = 6.0;
const MARGIN: f64 = 0.0;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn rect(w: f64, h: f64) -> Vec<Point> {
    vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)]
}

/// The real parts, padded exactly as `run_nest` pads them.
fn real_parts(copies: usize) -> Vec<NestPart> {
    let drawing = Drawing::load_file(fixture("two.dxf")).expect("two.dxf should parse");
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    let mut parts = Vec::new();
    let mut id = 0;
    for (source_id, shape) in tree.iter().enumerate() {
        let padded = prepare_part(&shape.points, SPACING).expect("should offset");
        let polygon = LayeredPolygon { points: padded, real_boundary: None, ..shape.clone() };
        for _ in 0..copies {
            parts.push(NestPart { id, source_id, polygon: polygon.clone(), rotation: 0.0 });
            id += 1;
        }
    }
    parts
}

fn usable_sheet() -> LayeredPolygon {
    let points = prepare_sheet(&rect(2440.0, 1220.0), MARGIN, SPACING).expect("sheet should offset");
    LayeredPolygon { points, layer: "sheet".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None }
}

/// Places every part the band packer chose, exactly as `place_parts` would.
fn materialise(parts: &[NestPart], result: &nesting::banded::BandedSheet) -> Vec<LayeredPolygon> {
    result
        .placed
        .iter()
        .map(|p| {
            let part = parts.iter().find(|q| q.id == p.id).expect("placed id must exist");
            let rotated = rotate_layered_polygon(&part.polygon, p.rotation);
            shift_layered_polygon(&rotated, p.placement.x, p.placement.y)
        })
        .collect()
}

/// **The invariant.** Whatever the band packer produces has to be legal, or it
/// is worse than useless - it replaces a valid greedy sheet with an invalid
/// one, and the only thing that notices is the audit at export time.
#[test]
fn band_packed_real_parts_do_not_overlap_each_other() {
    let parts = real_parts(12);
    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let Some(result) = pack_sheet(bounds, &parts, CURVE_TOLERANCE) else {
        panic!("the band packer placed nothing on a 2440x1220 sheet");
    };
    assert!(!result.placed.is_empty());

    let placed = materialise(&parts, &result);
    for i in 0..placed.len() {
        for j in (i + 1)..placed.len() {
            assert!(
                !has_material_overlap(&placed[i], &placed[j]),
                "parts {} and {} overlap ({:?} vs {:?}) - {} parts placed",
                result.placed[i].id,
                result.placed[j].id,
                get_polygon_bounds(&placed[i].points),
                get_polygon_bounds(&placed[j].points),
                placed.len()
            );
        }
    }
}

#[test]
fn band_packed_real_parts_stay_on_the_sheet() {
    let parts = real_parts(12);
    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE).expect("should place something");

    for (placed, meta) in materialise(&parts, &result).iter().zip(result.placed.iter()) {
        assert!(
            !has_material_outside_sheet(placed, &sheet),
            "part {} escapes the sheet: {:?} vs sheet {:?}",
            meta.id,
            get_polygon_bounds(&placed.points),
            bounds
        );
    }
}

/// **The target, not the current state.** The band packer only earns its place
/// if it beats the greedy pass on the shapes greedy is worst at: greedy
/// reaches 14 parts (76.5%) on this sheet, and the reference tool reaches 16
/// (88%). Today this places 12.
///
/// The blocker is measured and specific. Pairing is exact on clean geometry -
/// two synthetic right triangles pair at density 1.000 - but the real padded
/// parts pair at 0.937, giving an 837x433 box where the geometry allows about
/// 789x433. That 48mm matters out of all proportion to its size: a 789-wide
/// box fits three across a 2446mm sheet, an 837-wide box fits only two, so the
/// excess costs an entire column per band. The padding is the cause -
/// `offset_bevel` on the triangle's 28.5-degree tip fattens it well beyond a
/// uniform 3mm ring, so the two copies no longer tile their own box.
///
/// Ignored rather than deleted: it is the specification for finishing this,
/// and the number in the failure message is the progress bar.
#[test]
#[ignore = "band packer reaches 12 parts; needs 16 to beat greedy - see the doc comment"]
fn band_packing_beats_the_greedy_ceiling_on_pairable_parts() {
    let parts = real_parts(12);
    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE).expect("should place something");

    let sheet_area = polygon_area(&sheet.points).abs();
    let utilisation = result.area / sheet_area * 100.0;
    println!("banded: {} parts, {utilisation:.1}% of the usable sheet", result.placed.len());
    assert!(
        result.placed.len() >= 16,
        "expected at least 16 parts (the two-band layout), got {} at {utilisation:.1}%",
        result.placed.len()
    );
}

/// Diagnostic: how much of the pairing loss is the clearance padding?
///
/// If the true outlines pair tightly and only the padded ones do not, the fix
/// is structural - pair first, pad the composite - rather than a better search.
/// If neither pairs tightly, the shape simply is not a clean half-box and no
/// amount of searching will find one.
#[test]
fn report_how_much_padding_costs_the_pairing() {
    let drawing = Drawing::load_file(fixture("two.dxf")).expect("two.dxf should parse");
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    let shape = &tree[0];

    let true_area = polygon_area(&shape.points).abs();
    let tb = get_polygon_bounds(&shape.points).expect("has points");
    let padded_points = prepare_part(&shape.points, SPACING).expect("should offset");
    let padded_area = polygon_area(&padded_points).abs();
    let pb = get_polygon_bounds(&padded_points).expect("has points");

    println!("true   {:.1}x{:.1} area {:.0}  (half its box = {:.0})", tb.width, tb.height, true_area, tb.width * tb.height / 2.0);
    println!("padded {:.1}x{:.1} area {:.0}  (half its box = {:.0})", pb.width, pb.height, padded_area, pb.width * pb.height / 2.0);
    println!("padding added {:.0} mm2; a uniform {:.1}mm ring would add ~{:.0}", padded_area - true_area, SPACING / 2.0, perimeter(&shape.points) * SPACING / 2.0);

    // Pair each version and report the density the packer can reach.
    for (label, points) in [("true", shape.points.clone()), ("padded", padded_points.clone())] {
        let poly = LayeredPolygon { points, real_boundary: None, ..shape.clone() };
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: poly.clone(), rotation: 0.0 },
            NestPart { id: 1, source_id: 0, polygon: poly, rotation: 0.0 },
        ];
        let bounds = get_polygon_bounds(&rect(5000.0, 5000.0)).expect("has points");
        if let Some(r) = pack_sheet(bounds, &parts, CURVE_TOLERANCE) {
            println!("{label}: packed {} parts, area {:.0}", r.placed.len(), r.area);
        }
    }
}

fn perimeter(points: &[Point]) -> f64 {
    (0..points.len()).map(|i| points[i].distance_to(points[(i + 1) % points.len()])).sum()
}

/// Does the shape pair *at all*, with no clearance in play? If it does, any
/// failure to pair the real job is about the padding round-trip; if it does
/// not, the shape is simply not a clean half-box and the band packer can never
/// help it.
#[test]
fn report_whether_the_bare_shape_pairs() {
    let drawing = Drawing::load_file(fixture("two.dxf")).expect("two.dxf should parse");
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));

    for (i, shape) in tree.iter().enumerate() {
        let b = get_polygon_bounds(&shape.points).expect("has points");
        let area = polygon_area(&shape.points).abs();
        // Round-trip: pad then un-pad, to see what the band packer recovers.
        let padded = prepare_part(&shape.points, SPACING).expect("offsets");
        let recovered = geometry::clipper::offset_bevel(&padded, -SPACING / 2.0).into_iter().next().unwrap_or_default();
        let rb = get_polygon_bounds(&recovered).unwrap_or(b);
        println!(
            "src {i}: true {:.1}x{:.1} area {:.0} ({:.1}% of half-box, {} pts) | recovered {:.1}x{:.1} area {:.0} ({} pts)",
            b.width, b.height, area, area / (b.width * b.height / 2.0) * 100.0, shape.points.len(),
            rb.width, rb.height, polygon_area(&recovered).abs(), recovered.len()
        );

        // Pair the bare shape with zero clearance.
        let poly = LayeredPolygon { points: shape.points.clone(), real_boundary: None, ..shape.clone() };
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: poly.clone(), rotation: 0.0 },
            NestPart { id: 1, source_id: 0, polygon: poly, rotation: 0.0 },
        ];
        let big = get_polygon_bounds(&rect(6000.0, 6000.0)).expect("has points");
        if let Some(r) = pack_sheet(big, &parts, CURVE_TOLERANCE) {
            println!("   bare pair -> {} parts placed", r.placed.len());
        }
    }
}

/// Reads the NFP convention off a case whose answer is known by inspection:
/// two identical 10x10 squares, A anchored at (100, 200). Wherever the NFP
/// says B may go, B must end up exactly touching A - so the printed bounds
/// tell us directly whether an NFP point is B's origin, B's first vertex, or
/// something else again.
#[test]
fn report_the_nfp_reference_convention() {
    let square = |x: f64, y: f64| LayeredPolygon {
        points: vec![Point::new(x, y), Point::new(x + 10.0, y), Point::new(x + 10.0, y + 10.0), Point::new(x, y + 10.0)],
        layer: "0".into(),
        is_circle: None,
        children: Vec::new(),
        texts: Vec::new(),
        real_boundary: None,
    };
    let a = square(100.0, 200.0);
    let b = square(100.0, 200.0);
    let nfp = geometry::obstacle_nfp::obstacle_nfp(&a, &b, CURVE_TOLERANCE).expect("squares have an NFP");
    let nb = get_polygon_bounds(&nfp.outer).expect("nfp has points");
    println!("A at (100,200) 10x10; B identical");
    println!("NFP bounds: x {:.1}..{:.1}, y {:.1}..{:.1}", nb.x, nb.x + nb.width, nb.y, nb.y + nb.height);
    println!("  if these are B-origin positions, expect 90..110 / 190..210");
    println!("  if these are B-first-vertex positions, expect 190..210 / 390..410");
    println!("  if these are pure offsets,           expect -10..10 / -10..10");
}
