//! Direct A/B comparison against `hat_test.rs`: identical GA/placement
//! config, identical part_count/sheet, the *only* difference is the
//! geometry source - DXF import (`entities_to_polygons`) vs SVG import
//! (`geometry::svg_import::parse_svg`) of the same real-world hat tile
//! (`tests/fixtures/hat-monotile.dxf` vs `hat-monotile.svg`).
//!
//! **Known, investigated gap - not an SVG import bug.** At `part_count=252`
//! this currently reports DXF at 78.57% (matches `hat_test.rs`'s documented
//! target exactly) but SVG at ~69%, even though `svg_fixtures.rs` already
//! proves the two source shapes are byte-for-byte congruent (same area,
//! same winding, same vertex sequence once translated to a common origin).
//! Isolated by hand (see this session's investigation, not kept as
//! permanent test code here): translating the SVG-derived shape to
//! numerically match DXF's own coordinate values reproduces 78.57% exactly,
//! generation-by-generation identical to the DXF run - but an arbitrary
//! translation of the *DXF*-derived shape stays at 78.57% too, while
//! several different arbitrary translations of the *SVG*-derived shape all
//! land somewhere in the high-60s/low-70s, never at 78.57%, unless forced
//! to match DXF's own values. That rules out "any translation destabilizes
//! this tessellation" - the fragility is specific to the exact
//! floating-point coordinate *values* each file happens to contain (DXF's
//! own rounding vs SVG's own rounding of the same nominal design), not
//! shape correctness or import format. This is the same kind of
//! razor's-edge placement-engine sensitivity `hat_test.rs`'s own doc
//! comment already documents extensively for this specific tessellation
//! (`rotations=6` producing bad angles, `rotations=8+` being a strict
//! downgrade, etc.) - a pre-existing characteristic of the delicate
//! tightfit contact-search's floating-point tie-breaking, not something
//! this session's SVG import work introduced. Root-causing *that*
//! tie-break sensitivity (likely somewhere in the NFP search-start-point
//! selection or the tightfit scorer's own comparisons) is a separate,
//! open-ended investigation from porting SVG import itself.
//!
//! Usage: `cargo run --release -p nesting --example hat_test_svg -- [seconds] [part_count]`
//! (defaults: 60s, 252 parts) - always rotations=2, population=20,
//! mutation=10, tightfit, seed=0, matching `hat_test.rs`'s own documented
//! best-known config for this tessellation.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dxf::Drawing;
use geometry::clearance::{prepare_part, prepare_sheet};
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, polygon_material_area, LayeredPolygon};
use geometry::point::Point;
use geometry::polygon::polygon_area;
use nesting::cache::NfpCache;
use nesting::dispatch;
use nesting::ga::{is_better_nest, GaConfig, GeneticAlgorithm};
use nesting::placement::{PlaceResult, PlacementConfig, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};

const SHEET_SIZE: f64 = 500.0;
const CURVE_TOLERANCE: f64 = 0.1;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_dxf_hat() -> LayeredPolygon {
    let fixture = repo_root().join("tests/fixtures/hat-monotile.dxf");
    let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let flat = entities_to_polygons(drawing.entities(), CURVE_TOLERANCE);
    let tree = build_polygon_tree(flat);
    assert_eq!(tree.len(), 1, "expected exactly one closed profile in hat-monotile.dxf, got {}", tree.len());
    tree.into_iter().next().unwrap()
}

fn load_svg_hat() -> LayeredPolygon {
    let fixture = repo_root().join("tests/fixtures/hat-monotile.svg");
    let text = std::fs::read_to_string(&fixture).unwrap_or_else(|e| panic!("couldn't read {}: {e}", fixture.display()));
    let flat = geometry::svg_import::parse_svg(&text, CURVE_TOLERANCE, None).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let tree = build_polygon_tree(flat);
    assert_eq!(tree.len(), 1, "expected exactly one closed profile in hat-monotile.svg, got {}", tree.len());
    tree.into_iter().next().unwrap()
}

fn run_job(label: &str, hat: LayeredPolygon, part_count: usize, run_seconds: u64) {
    let raw_area = polygon_area(&hat.points).abs();
    println!("[{label}] hat shape: {} vertices, raw area {:.4}mm2, layer={:?}, is_circle={:?}", hat.points.len(), raw_area, hat.layer, hat.is_circle);

    let padded_points = prepare_part(&hat.points, 0.0).expect("hat shape should offset cleanly at zero spacing");
    let padded_hat = LayeredPolygon { points: padded_points, layer: hat.layer.clone(), is_circle: None, children: hat.children.clone(), texts: hat.texts.clone() };

    let mut parts_by_id = std::collections::HashMap::new();
    let mut shape_ids = std::collections::HashMap::new();
    for id in 0..part_count {
        parts_by_id.insert(id, padded_hat.clone());
        shape_ids.insert(id, 0usize);
    }
    let adam: Vec<usize> = (0..part_count).collect();

    let sheet_raw = vec![Point::new(0.0, 0.0), Point::new(SHEET_SIZE, 0.0), Point::new(SHEET_SIZE, SHEET_SIZE), Point::new(0.0, SHEET_SIZE)];
    let sheet_points = prepare_sheet(&sheet_raw, 0.0, 0.0).expect("500x500 sheet should be usable at zero margin/spacing");
    let sheet = LayeredPolygon { points: sheet_points, layer: "SHEET".into(), is_circle: None, children: Vec::new(), texts: Vec::new() };
    let sheets = vec![sheet];

    let rotations = 2u32;
    let population_size = 20usize;
    let mutation_rate = 10.0f64;
    let placement_config = PlacementConfig { placement_type: PlacementType::TightFit, rotations, dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD, curve_tolerance: CURVE_TOLERANCE };
    let ga_config = GaConfig { population_size, mutation_rate, rotations };
    let mut ga = GeneticAlgorithm::new(adam, ga_config, Vec::new(), 0);

    let deadline = Instant::now() + Duration::from_secs(run_seconds);
    let should_cancel = || Instant::now() >= deadline;
    let start = Instant::now();
    let cache = NfpCache::new();
    let mut best: Option<PlaceResult> = None;
    let mut generation = 0usize;

    while !should_cancel() {
        generation += 1;
        let results = dispatch::run_generation(&mut ga, &sheets, &parts_by_id, &shape_ids, &placement_config, &should_cancel, &|_, _| {}, &cache);
        for evaluated in results {
            let result = evaluated.result;
            if best.as_ref().is_none_or(|b| is_better_nest(&result, b)) {
                let elapsed_s = start.elapsed().as_secs_f64();
                println!("[{label}] gen {generation}: sheets={}, unplaced={}, util={:.2}% ({elapsed_s:.1}s)", result.placements.len(), result.unplaced_count, result.utilisation);
                best = Some(result);
            }
        }
        if should_cancel() {
            break;
        }
    }

    match best {
        Some(r) => {
            let placed = part_count - r.unplaced_count;
            let sheet_area = polygon_material_area(&sheets[0]);
            let placed_area: f64 = r.placements.iter().flat_map(|s| &s.parts).map(|_| raw_area).sum();
            let utilisation_of_placed = (placed_area / sheet_area) * 100.0;
            println!(
                "[{label}] DONE: placed {placed}/{part_count}, utilisation={utilisation_of_placed:.2}% (reported={:.2}%)\n",
                r.utilisation
            );
        }
        None => println!("[{label}] no result\n"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let run_seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let part_count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(252);

    let dxf_hat = load_dxf_hat();
    let svg_hat = load_svg_hat();

    // Sanity: prove the two source shapes are congruent (same area/winding)
    // before even running the nest, so a mismatch below can't be blamed on
    // something this check would have already caught.
    let dxf_area = polygon_area(&dxf_hat.points);
    let svg_area = polygon_area(&svg_hat.points);
    println!("dxf signed area: {dxf_area:.4}, svg signed area: {svg_area:.4}\n");
    assert!((dxf_area.abs() - svg_area.abs()).abs() < 0.01, "unsigned areas differ");
    assert_eq!(dxf_area.signum(), svg_area.signum(), "winding differs");

    run_job("DXF", dxf_hat, part_count, run_seconds);
    run_job("SVG", svg_hat, part_count, run_seconds);
}
