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
    let Some(result) = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()) else {
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
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()).expect("should place something");

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

/// **What the band packer actually reaches on real geometry, and the ceiling
/// it cannot pass.** Greedy reaches 14 parts (76.5%) on this sheet; the band
/// packer reaches 15 at 85.1%, so it earns its place - but not the 16 the
/// reference tool's report implies.
///
/// Sixteen is not reachable here, and the reason is the shape, not the packer.
/// `two.dxf`'s parts look like right triangles and are not: the apex sits part
/// way along the top edge, so a 180-degree copy tiles a *parallelogram*, never
/// the bounding box. Probed directly - align the two boxes and step the copy
/// perpendicular to the long edge - the two padded outlines still overlap at
/// 40mm of separation, so the 784x433 pair box the two-band layout would need
/// simply does not exist. The real Pareto front of pair boxes runs from
/// 785x465 to 845x430, and `min(width + height) = 1250` against a usable sheet
/// height of 1226: one band of each orientation cannot stack, whatever the
/// search does.
///
/// So this asserts what is achievable and guards it. Getting past it needs
/// common-line pairing (the reference's own 776.5x422.4 pattern unit is the
/// bare part box, i.e. zero clearance on the shared cut) - see `PLAN.md` 2.1 -
/// not a better band search.
#[test]
fn band_packing_beats_the_greedy_ceiling_on_pairable_parts() {
    let parts = real_parts(12);
    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()).expect("should place something");

    let sheet_area = polygon_area(&sheet.points).abs();
    let utilisation = result.area / sheet_area * 100.0;
    println!("banded: {} parts, {utilisation:.1}% of the usable sheet", result.placed.len());
    assert!(
        result.placed.len() >= 15,
        "expected at least 15 parts (greedy reaches 14), got {} at {utilisation:.1}%",
        result.placed.len()
    );
}

/// **The row-step invariant.** A row advances by the distance at which a unit
/// can repeat, not by the width of its bounding box.
///
/// Two of these triangles paired at 180 degrees form a parallelogram whose box
/// is ~62mm wider than the lattice it tiles, because the slanted end of one
/// copy slots into the next. Advancing by the box width wastes that overhang
/// on every unit, which on this sheet is the difference between 2 pair-boxes
/// across and 3 - 14 parts against 16, 77.1% against 88.1%. 16 is also
/// exactly what the commercial nester this job was measured against puts on
/// the same sheet, at the same 6mm spacing, so it is a real ceiling and not a
/// number picked to match the current code.
///
/// One profile only: mixing shapes lets the band packer fill a tail with
/// something else and hides whether the step itself is being used.
#[test]
fn a_row_advances_by_the_lattice_step_not_the_box_width() {
    let parts: Vec<NestPart> = real_parts(20).into_iter().filter(|p| p.source_id == 1).collect();
    assert!(!parts.is_empty(), "two.dxf should have a second profile");
    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()).expect("should place something");

    let utilisation = result.area / polygon_area(&sheet.points).abs() * 100.0;
    assert!(result.placed.len() >= 16, "expected 16 parts on the sheet, got {} at {utilisation:.1}%", result.placed.len());

    // ...and legally. A step measured too short is exactly the failure this
    // buys, and it would show up as more parts, not fewer.
    let placed = materialise(&parts, &result);
    for i in 0..placed.len() {
        assert!(!has_material_outside_sheet(&placed[i], &sheet), "part {i} hangs off the sheet");
        for j in (i + 1)..placed.len() {
            assert!(!has_material_overlap(&placed[i], &placed[j]), "parts {i} and {j} overlap");
        }
    }
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
        if let Some(r) = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()) {
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
        if let Some(r) = pack_sheet(big, &parts, CURVE_TOLERANCE, None, &Default::default()) {
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
    let square = |x: f64, y: f64| LayeredPolygon::new(vec![Point::new(x, y), Point::new(x + 10.0, y), Point::new(x + 10.0, y + 10.0), Point::new(x, y + 10.0)], "0".into(), None);
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

/// **The bug this file existed for and still missed.** `place_parts` rotates
/// every part by its own `NestPart::rotation` *before* the band packer sees it,
/// but a `PlacedPart::rotation` is read downstream as an angle applied to the
/// part's original outline. So the band packer has to report `part.rotation`
/// plus whatever it chose, and reporting only its own choice is undetectable
/// on a fixture where every part sits at 0 degrees - which every other test
/// here uses. With rotations enabled the real engine put all eight parts of a
/// nest off the sheet.
///
/// Here the parts arrive exactly as `place_parts` hands them over: polygon
/// already turned, `rotation` recording by how much. Materialising from the
/// *original* outline at the reported angle is what the exporter and the audit
/// both do, so that is what gets checked.
#[test]
fn placements_are_reported_at_an_absolute_rotation() {
    const BASE: f64 = 37.0;
    let originals = real_parts(12);
    let parts: Vec<NestPart> = originals
        .iter()
        .map(|p| NestPart { polygon: rotate_layered_polygon(&p.polygon, BASE), rotation: BASE, ..p.clone() })
        .collect();

    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()).expect("should place something");
    assert!(!result.placed.is_empty());

    let placed: Vec<LayeredPolygon> = result
        .placed
        .iter()
        .map(|p| {
            let original = originals.iter().find(|q| q.id == p.id).expect("placed id must exist");
            let rotated = rotate_layered_polygon(&original.polygon, p.rotation);
            shift_layered_polygon(&rotated, p.placement.x, p.placement.y)
        })
        .collect();

    for (i, part) in placed.iter().enumerate() {
        assert!(!has_material_outside_sheet(part, &sheet), "part {} escapes the sheet", result.placed[i].id);
        for (j, other) in placed.iter().enumerate().skip(i + 1) {
            assert!(!has_material_overlap(part, other), "parts {} and {} overlap", result.placed[i].id, result.placed[j].id);
        }
    }
}

/// **A concave part has to be paired on its outline, not on its hull.**
///
/// `nestTest03.dxf` is a 150x280 rectangle with a concave bite taken out of one
/// diagonal, and that bite is the entire reason two copies can interlock: it is
/// where the next part's corner goes. A convex hull fills it in, so pairing on
/// the hull cannot see it at all.
///
/// The cost is a whole sheet. On the hull the best pair box is 160x525
/// (density 0.862), only two bands fit in 1505, and the sheet reaches **48**
/// parts - precisely the bounding-box ceiling, i.e. the concavity bought
/// nothing whatsoever. On the true outline the pair is 160x485 (0.933), a third
/// band fits, and the sheet takes **52**; over the 250-part job that is 6
/// sheets against 5, which is what the commercial nester gets.
///
/// **54, not 52,** since `pack_sheet` seeds its search with the best uniform
/// band plan: nine 160-tall bands of three pairs each is the answer, and the
/// depth-first search alone never backtracked far enough to try it. 54 is also
/// exactly what the commercial nester puts on this sheet (81.98%), so this is
/// the real ceiling and not a lucky number.
///
/// Reverting `shell_of`'s point-count branch to always hull fails this at 48;
/// dropping the `uniform_plan` seed fails it at 52.
#[test]
fn a_concave_part_pairs_on_its_outline_not_its_hull() {
    const JOB_SPACING: f64 = 5.0;
    let drawing = Drawing::load_file(fixture("nestTest03.dxf")).expect("nestTest03.dxf should parse");
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    let mut parts = Vec::new();
    for (source_id, shape) in tree.iter().enumerate() {
        let padded = prepare_part(&shape.points, JOB_SPACING).expect("should offset");
        let polygon = LayeredPolygon { points: padded, real_boundary: None, ..shape.clone() };
        for _ in 0..60 {
            parts.push(NestPart { id: parts.len(), source_id, polygon: polygon.clone(), rotation: 0.0 });
        }
    }

    let points = prepare_sheet(&rect(1500.0, 1500.0), 0.0, JOB_SPACING).expect("sheet should offset");
    let sheet = LayeredPolygon { points, layer: "sheet".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None };
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()).expect("should place something");

    // The bounding-box ceiling is 48 - see the doc comment. Anything at or
    // below it means the interlock was thrown away, whatever the cause.
    assert!(result.placed.len() >= 54, "expected 54 parts, got {} (52 means the uniform-plan seed is gone, 48 means it paired on the hull)", result.placed.len());

    // A denser sheet is only worth having if it is a legal one.
    let placed = materialise(&parts, &result);
    for (poly, meta) in placed.iter().zip(result.placed.iter()) {
        assert!(!has_material_outside_sheet(poly, &sheet), "part {} escapes the sheet", meta.id);
    }
    for i in 0..placed.len() {
        for j in (i + 1)..placed.len() {
            assert!(!has_material_overlap(&placed[i], &placed[j]), "parts {} and {} overlap", result.placed[i].id, result.placed[j].id);
        }
    }
}

/// **What a banded sheet *reports* has to be as legal as what it placed.**
///
/// `place_parts` carries a part's rotation from sheet to sheet, so by the time
/// a later sheet is packed `NestPart::polygon` is already turned by
/// `NestPart::rotation`. A `PlacedPart::rotation`, though, is absolute - the
/// angle from the part's original outline - which is what `banded` reports and
/// what every consumer reconstructs geometry with.
///
/// Accepting a banded sheet rebuilt its obstacle list by turning the
/// already-turned polygon by that absolute angle, landing it at
/// `base + absolute` and counting the base twice. Nothing read that list
/// afterwards, so it stayed invisible until the sheet top-up did - and then
/// placed parts into space the real geometry was already occupying, which only
/// the export-time audit noticed.
///
/// Feeding parts in pre-rotated is what makes this deterministic: with every
/// base rotation at zero, doubling it changes nothing.
#[test]
fn a_banded_sheet_reports_placements_that_do_not_overlap() {
    const JOB_SPACING: f64 = 5.0;
    let mut originals: Vec<LayeredPolygon> = Vec::new();
    for name in ["nestTest03.dxf", "nestTest01.dxf"] {
        let drawing = Drawing::load_file(fixture(name)).expect("fixture should parse");
        for shape in build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE)) {
            let padded = prepare_part(&shape.points, JOB_SPACING).expect("should offset");
            originals.push(LayeredPolygon { points: padded, real_boundary: None, ..shape });
        }
    }

    let mut parts = Vec::new();
    for (source_id, polygon) in originals.iter().enumerate() {
        for copy in 0..40 {
            // Half the copies arrive already turned, which is the state a
            // second or third sheet always sees in a real run.
            parts.push(NestPart { id: parts.len(), source_id, polygon: polygon.clone(), rotation: if copy % 2 == 0 { 0.0 } else { 90.0 } });
        }
    }

    let points = prepare_sheet(&rect(1500.0, 1500.0), 0.0, JOB_SPACING).expect("sheet should offset");
    let sheet = LayeredPolygon { points, layer: "sheet".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None };
    let sheets = vec![sheet.clone(); 8];
    let config = nesting::placement::PlacementConfig {
        placement_type: nesting::placement::PlacementType::TightFit,
        rotations: 4,
        dominant_part_area_threshold: nesting::placement::DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
        curve_tolerance: CURVE_TOLERANCE,
        part_rules: Default::default(),
        banded_pass: true,
    };
    let cache = nesting::cache::NfpCache::default();
    let result = nesting::placement::place_parts(&sheets, parts.clone(), &config, &cache, &|| false, &|_, _| {}, &|_, _, _| {}).expect("should place");

    for sheet_placement in &result.placements {
        // Exactly how a consumer rebuilds it: the *original* outline, turned by
        // the reported absolute angle, moved to the reported position.
        let placed: Vec<LayeredPolygon> = sheet_placement
            .parts
            .iter()
            .map(|p| {
                let original = &originals[parts.iter().find(|q| q.id == p.id).expect("placed id exists").source_id];
                shift_layered_polygon(&rotate_layered_polygon(original, p.rotation), p.placement.x, p.placement.y)
            })
            .collect();
        for i in 0..placed.len() {
            for j in (i + 1)..placed.len() {
                assert!(
                    !has_material_overlap(&placed[i], &placed[j]),
                    "sheet {}: reported placements of parts {} and {} overlap",
                    sheet_placement.sheet_index,
                    sheet_placement.parts[i].id,
                    sheet_placement.parts[j].id
                );
            }
        }
    }
}

/// **The first sheet has to be about the part at the front of the queue.**
///
/// `place_parts` fills sheets in gene order, seeded largest-area-first, and its
/// greedy pass always puts `parts[0]` down first. The band packer has no such
/// rule - left free it packs whichever shape makes the densest sheet, which on
/// a job of very unequal parts means spending the small ones early and
/// stranding the big one at the end, alone, with nothing left to fill around
/// it.
///
/// Here the queue starts with the 880x720 part and the 120x300 rectangle is
/// what the bands would rather have: free, they take the first sheet with 30 of
/// the rectangle and the big part does not surface until the third, by which
/// point it is nearly alone. Anchored, it goes down first and the fill packs the
/// small parts into the gaps around it. On the full 800-part job that is 33
/// sheets against 32.
///
/// It takes all four shapes to reproduce: with only the big part and one small
/// one, the greedy pass already wins the first sheet and the bands never get
/// the chance to strand anything.
#[test]
fn the_first_sheet_places_the_shape_at_the_front_of_the_queue() {
    const JOB_SPACING: f64 = 5.0;
    let mut shapes: Vec<LayeredPolygon> = Vec::new();
    // Decreasing area - the seed order `dto::expand_parts` produces, and so the
    // order `place_parts` works its queue in.
    for name in ["nestTest04.dxf", "nestTest01.dxf", "nestTest02.dxf", "nestTest03.dxf"] {
        let drawing = Drawing::load_file(fixture(name)).expect("fixture should parse");
        let shape = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE)).into_iter().next().expect("one profile");
        let padded = prepare_part(&shape.points, JOB_SPACING).expect("should offset");
        shapes.push(LayeredPolygon { points: padded, real_boundary: None, ..shape });
    }

    let mut parts = Vec::new();
    for (source_id, polygon) in shapes.iter().enumerate() {
        for _ in 0..if source_id == 0 { 6 } else { 30 } {
            parts.push(NestPart { id: parts.len(), source_id, polygon: polygon.clone(), rotation: 0.0 });
        }
    }

    let points = prepare_sheet(&rect(1500.0, 1500.0), 0.0, JOB_SPACING).expect("sheet should offset");
    let sheet = LayeredPolygon { points, layer: "sheet".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None };
    let sheets = vec![sheet; 20];
    let config = nesting::placement::PlacementConfig {
        placement_type: nesting::placement::PlacementType::TightFit,
        rotations: 4,
        dominant_part_area_threshold: nesting::placement::DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
        curve_tolerance: CURVE_TOLERANCE,
        part_rules: Default::default(),
        banded_pass: true,
    };
    let cache = nesting::cache::NfpCache::default();
    let result = nesting::placement::place_parts(&sheets, parts.clone(), &config, &cache, &|| false, &|_, _| {}, &|_, _, _| {}).expect("should place");

    let first = result.placements.first().expect("at least one sheet");
    let big_on_first = first.parts.iter().filter(|p| parts.iter().find(|q| q.id == p.id).expect("id exists").source_id == 0).count();
    assert!(big_on_first > 0, "the big part never reached the first sheet - the band packer took it with the small shape instead");
}

/// **The last copy of a shape is reported at an absolute rotation too.**
///
/// `build_units` has two exits, and the `available < 2` one - taken when only
/// one copy of a shape is left, which is every late sheet of a real job -
/// returned its member angle *relative* to the polygon it was handed while its
/// box arithmetic described the absolute geometry. The caller then rotated the
/// part to the reported angle and dropped it at a position computed for a
/// different one: on `nestTest04.dxf` under `--placement box` an 880x720 part
/// landed 1320mm from where the band said it would, straight off a 1500x1500
/// sheet.
///
/// Invisible until the GA had turned a part, and invisible while a shape still
/// had two copies to pair - which is why the sibling test above, at twelve
/// copies, passes either way. One copy each is the whole point of this one.
///
/// Reverting `into_absolute` at the `available < 2` return fails this.
#[test]
fn the_last_copy_of_a_shape_is_reported_at_an_absolute_rotation() {
    const BASE: f64 = 90.0;
    let originals = real_parts(1);
    let parts: Vec<NestPart> = originals
        .iter()
        .map(|p| NestPart { polygon: rotate_layered_polygon(&p.polygon, BASE), rotation: BASE, ..p.clone() })
        .collect();

    let sheet = usable_sheet();
    let bounds = get_polygon_bounds(&sheet.points).expect("sheet has points");
    let result = pack_sheet(bounds, &parts, CURVE_TOLERANCE, None, &Default::default()).expect("should place something");
    assert!(!result.placed.is_empty());

    for p in &result.placed {
        let original = originals.iter().find(|q| q.id == p.id).expect("placed id must exist");
        let moved = shift_layered_polygon(&rotate_layered_polygon(&original.polygon, p.rotation), p.placement.x, p.placement.y);
        assert!(!has_material_outside_sheet(&moved, &sheet), "part {} escapes the sheet at rotation {}", p.id, p.rotation);
    }
}
