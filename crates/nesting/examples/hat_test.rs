//! One-off benchmark against a real third-party DXF fixture: the aperiodic
//! "hat" monotile (github.com/christianp/aperiodic-monotile/hat-monotile.dxf,
//! copied to `tests/fixtures/hat-monotile.dxf`), N copies on a single
//! 500x500mm sheet, no margin/spacing. The user ran this same job on the
//! "supernesting" online tool at N=2/20/252 and got 0.62%/6.24%/78.57%
//! utilisation respectively - this checks whether this project's NFP+GA
//! pipeline can match or beat that on the same job.
//!
//! Usage: `cargo run --release -p nesting --example hat_test -- [seconds] [rotations] [population_size] [mutation_rate] [placement_type] [part_count]`
//! (defaults: 60s, 12 rotations, population 20, mutation 10, gravity, 252
//! parts). The hat is a 13-sided non-rectangular shape, so the
//! rectangular-parts-prefer-90-degrees quirk documented for the other
//! fixtures doesn't apply here - but **`rotations=2` is the actual
//! standout, not a wider grid**: `compare_supernesting` (a separate example)
//! measured supernesting's own reference layouts directly and found every
//! placed hat, across all three fixtures, uses just one of two rotations
//! exactly 180 degrees apart (no mirroring). A wider grid (6/12/24) only
//! helps if something downstream actually *measures* which of those angles
//! fits best at each specific spot before committing to one - which
//! `place_parts` didn't do for a sheet's 2nd part onward until it gained a
//! real per-position rotation search (every configured angle scored via
//! `try_place_part_on_sheet`'s real contact/area metric, keeping the best -
//! previously only a sheet's very first part got any rotation comparison at
//! all). With that search in place, `rotations=2` + `tightfit` reproduces
//! supernesting's 0.62%/6.24%/78.57% exactly (0 unplaced) for N=2/20/252, on
//! generation 1 - no GA evolution even needed for this particular
//! tessellation. `GravityCorrective` does *not* reach the same figure at
//! `rotations=2` (plateaus at 75.76% for N=252): it deliberately skips the
//! contact-search for a sheet's 2nd part (cheap `Gravity` scoring instead,
//! see its own doc comment), which is fine for a mixed-shape job with few
//! parts per sheet but throws away exactly the search this tessellation
//! depends on getting right early - `tightfit` is the one to reach for here.
//!
//! `part_count` should match one of the real
//! `tests/fixtures/supernesting {2,20,252}part(s) 500x500.dxf` reference
//! layouts (see `compare_supernesting` for how their ground-truth
//! utilisation numbers were measured directly from those files rather than
//! assumed) - `target_utilisation_pct` looks up the matching reference
//! figure for whichever `part_count` is requested, `0.0` (no target, just
//! reports) for anything else.
//!
//! `placement_type` is one of `gravity`/`box`/`convexhull`/`tightfit`/
//! `gravitytightfit`/`gravitycorrective` - `tightfit` (`PlacementType::TightFit`)
//! was added specifically because
//! `Gravity`/`Box`/`ConvexHull` all plateaued around 70-71% utilisation
//! regardless of rotation/population/mutation tuning: they score by the
//! aggregate bounding shape of everything placed so far, never by how
//! snugly a candidate touches its immediate neighbor - exactly the wrong
//! proxy for this concave, interlocking tile shape.

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
use nesting::placement::{place_parts, NestPart, PlaceResult, PlacedPart, PlacementConfig, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};

const SHEET_SIZE: f64 = 500.0;
const CURVE_TOLERANCE: f64 = 0.1;

/// Reference utilisation measured directly off supernesting's own reference
/// layouts (see `compare_supernesting`) for whichever `part_count` this run
/// was asked for - `0.0` (no target, just reports) for anything else.
fn target_utilisation_pct(part_count: usize) -> f64 {
    match part_count {
        2 => 0.62,
        20 => 6.24,
        252 => 78.57,
        _ => 0.0,
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let run_seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let rotations: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);
    let population_size: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let mutation_rate: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10.0);
    let placement_type = match args.next().as_deref() {
        Some("box") => PlacementType::Box,
        Some("convexhull") => PlacementType::ConvexHull,
        Some("tightfit") => PlacementType::TightFit,
        Some("gravitytightfit") => PlacementType::GravityTightFit,
        Some("gravitycorrective") => PlacementType::GravityCorrective,
        Some("gravity") | None => PlacementType::Gravity,
        Some(other) => panic!("unknown placement_type {other:?} - expected gravity/box/convexhull/tightfit/gravitytightfit/gravitycorrective"),
    };
    let part_count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(252);
    let target_utilisation_pct = target_utilisation_pct(part_count);

    let fixture = repo_root().join("tests/fixtures/hat-monotile.dxf");
    let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let flat = entities_to_polygons(drawing.entities(), CURVE_TOLERANCE);
    let tree = build_polygon_tree(flat);
    assert_eq!(tree.len(), 1, "expected exactly one closed profile in hat-monotile.dxf, got {}", tree.len());
    let hat = &tree[0];

    let raw_area = polygon_area(&hat.points).abs();
    println!("hat shape: {} vertices, raw area {:.2}mm2", hat.points.len(), raw_area);

    // No margin, no spacing - both a true no-op at 0.0 (see
    // geometry::clearance's module doc), included for parity with the real
    // pipeline rather than bypassing it.
    let padded_points = prepare_part(&hat.points, 0.0).expect("hat shape should offset cleanly at zero spacing");
    let padded_hat = LayeredPolygon { points: padded_points, layer: hat.layer.clone(), is_circle: None, children: hat.children.clone(), texts: hat.texts.clone(), real_boundary: None };

    let mut parts_by_id = std::collections::HashMap::new();
    // All 252 instances share one source id (0) - they're all copies of the
    // single imported hat shape, so every pairwise NFP/obstacle-NFP
    // computation between any two of them should be a cache hit after the
    // first, instead of each of the ~251*252/2 distinct id pairs
    // recomputing identical geometry from scratch. This is the actual fix
    // for why the first run of this benchmark only completed ~2.6
    // generations in 60s.
    let mut shape_ids = std::collections::HashMap::new();
    for id in 0..part_count {
        parts_by_id.insert(id, padded_hat.clone());
        shape_ids.insert(id, 0usize);
    }
    let adam: Vec<usize> = (0..part_count).collect();

    let sheet_raw = vec![Point::new(0.0, 0.0), Point::new(SHEET_SIZE, 0.0), Point::new(SHEET_SIZE, SHEET_SIZE), Point::new(0.0, SHEET_SIZE)];
    let sheet_points = prepare_sheet(&sheet_raw, 0.0, 0.0).expect("500x500 sheet should be usable at zero margin/spacing");
    let sheet = LayeredPolygon { points: sheet_points, layer: "SHEET".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None };
    let sheets = vec![sheet]; // exactly one sheet - this is a single-sheet packing-density benchmark, not a multi-sheet job

    let placement_config = PlacementConfig { placement_type, rotations, dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD, curve_tolerance: CURVE_TOLERANCE };
    let ga_config = GaConfig { population_size, mutation_rate, rotations };
    let mut ga = GeneticAlgorithm::new(adam, ga_config, Vec::new(), 0);

    println!(
        "running: {part_count} hats on a {SHEET_SIZE}x{SHEET_SIZE}mm sheet, placement={placement_type:?}, rotations={rotations}, budget={run_seconds}s, target={target_utilisation_pct}%"
    );

    // Manual generation loop (mirroring `dispatch::run`'s own internals)
    // instead of calling it directly, so every improving result can be
    // recorded as a visualization frame - `dispatch::run` only ever hands
    // back the final winner, not the progression that got there.
    let deadline = Instant::now() + Duration::from_secs(run_seconds);
    let should_cancel = || Instant::now() >= deadline;
    let start = Instant::now();
    let cache = NfpCache::new();
    let mut best: Option<PlaceResult> = None;
    let mut history: Vec<(usize, f64, PlaceResult)> = Vec::new();
    let mut generation = 0usize;

    while !should_cancel() {
        generation += 1;
        let results = dispatch::run_generation(&mut ga, &sheets, &parts_by_id, &shape_ids, &placement_config, &should_cancel, &|_, _| {}, &cache);
        for evaluated in results {
            let result = evaluated.result;
            if best.as_ref().is_none_or(|b| is_better_nest(&result, b)) {
                let elapsed_s = start.elapsed().as_secs_f64();
                println!(
                    "  gen {generation}: sheets={}, unplaced={}, util={:.2}% ({elapsed_s:.1}s elapsed)",
                    result.placements.len(),
                    result.unplaced_count,
                    result.utilisation
                );
                best = Some(result.clone());
                history.push((generation, elapsed_s, result));
            }
        }
        if should_cancel() {
            break;
        }
    }
    let elapsed = start.elapsed().as_secs_f64();

    match best {
        Some(r) => {
            let placed = part_count - r.unplaced_count;
            let sheet_area = polygon_material_area(&sheets[0]);
            let placed_area: f64 = r.placements.iter().flat_map(|s| &s.parts).map(|_| raw_area).sum();
            let utilisation_of_placed = (placed_area / sheet_area) * 100.0;
            println!(
                "done in {elapsed:.1}s: placed {placed}/{part_count}, utilisation={utilisation_of_placed:.2}% (target {target_utilisation_pct}%), reported_utilisation={:.2}%",
                r.utilisation
            );
            if target_utilisation_pct <= 0.0 {
                println!("(no reference target for {part_count} parts - just reporting)");
            } else if utilisation_of_placed >= target_utilisation_pct {
                println!("BEAT/MATCHED the target.");
            } else {
                println!("below target by {:.2} points.", target_utilisation_pct - utilisation_of_placed);
            }

            write_history_json(&hat.points, SHEET_SIZE, &history);
        }
        None => println!("run returned no result (unexpected - no cancellation should have prevented at least one generation)"),
    }

    // Step-by-step capture: one direct, single-pass `place_parts` call (no
    // GA - decreasing-area order is a no-op tiebreak here since every part
    // is the same size), observing each individual part's placement via
    // `on_part_placed` as it happens - not generation-level "best so far"
    // jumps like `history` above, the literal one-part-at-a-time view.
    let step_parts: Vec<NestPart> = (0..part_count).map(|id| NestPart { id, source_id: 0, polygon: padded_hat.clone(), rotation: 0.0 }).collect();
    let step_cache = NfpCache::new();
    let steps: std::sync::Mutex<Vec<Vec<PlacedPart>>> = std::sync::Mutex::new(Vec::new());
    let _ = place_parts(&sheets, step_parts, &placement_config, &step_cache, &|| false, &|_sheet_idx, p: &PlacedPart| {
        let mut frames = steps.lock().expect("single-threaded call, lock never contested");
        let mut snapshot = frames.last().cloned().unwrap_or_default();
        snapshot.push(*p);
        frames.push(snapshot);
    }, &|_, _, _| {});
    write_steps_json(&hat.points, SHEET_SIZE, raw_area, part_count, &steps.into_inner().expect("single-threaded call, lock never poisoned"));
}

/// Writes every improving arrangement (generation, elapsed time, and every
/// placed part's `x`/`y`/`rotation`) to a JSON file for the frame-by-frame
/// SVG viewer artifact - the hat's own outline is written once (`hat_points`),
/// since every part is the same shape and the viewer applies each frame's
/// per-part transform via SVG's own `translate`/`rotate`, the same
/// rotate-then-shift composition `placement::place_parts` itself uses.
fn write_history_json(hat_points: &[Point], sheet_size: f64, history: &[(usize, f64, PlaceResult)]) {
    let points_json: String = hat_points.iter().map(|p| format!("[{:.3},{:.3}]", p.x, p.y)).collect::<Vec<_>>().join(",");

    let frames_json: String = history
        .iter()
        .map(|(generation, elapsed_s, result)| {
            let parts_json: String = result
                .placements
                .iter()
                .flat_map(|s| &s.parts)
                .map(|p| format!("[{:.3},{:.3},{:.3}]", p.placement.x, p.placement.y, p.rotation))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"generation\":{generation},\"elapsed_s\":{elapsed_s:.2},\"sheets_used\":{},\"unplaced\":{},\"utilisation\":{:.2},\"parts\":[{parts_json}]}}",
                result.placements.len(),
                result.unplaced_count,
                result.utilisation
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    let json = format!("{{\"sheet_size\":{sheet_size},\"hat_points\":[{points_json}],\"frames\":[{frames_json}]}}");

    let out_path = repo_root().join("hat_test_history.json");
    std::fs::write(&out_path, json).expect("should be able to write hat_test_history.json");
    println!("wrote {} frames to {}", history.len(), out_path.display());
}

/// Same JSON shape `write_history_json` produces (so the existing frame
/// viewer artifact needs no changes to read this instead) but one frame per
/// individual part placed, in order - `generation` is repurposed as a
/// 1-based part-placement index, `elapsed_s`/`sheets_used` are placeholders
/// (this is a single direct `place_parts` call, not a timed GA run).
fn write_steps_json(hat_points: &[Point], sheet_size: f64, single_part_area: f64, part_count: usize, steps: &[Vec<PlacedPart>]) {
    let points_json: String = hat_points.iter().map(|p| format!("[{:.3},{:.3}]", p.x, p.y)).collect::<Vec<_>>().join(",");
    let sheet_area = sheet_size * sheet_size;

    let frames_json: String = steps
        .iter()
        .enumerate()
        .map(|(idx, snapshot)| {
            let parts_json: String =
                snapshot.iter().map(|p| format!("[{:.3},{:.3},{:.3}]", p.placement.x, p.placement.y, p.rotation)).collect::<Vec<_>>().join(",");
            let utilisation = (snapshot.len() as f64 * single_part_area / sheet_area) * 100.0;
            format!(
                "{{\"generation\":{},\"elapsed_s\":0,\"sheets_used\":1,\"unplaced\":{},\"utilisation\":{utilisation:.2},\"parts\":[{parts_json}]}}",
                idx + 1,
                part_count - snapshot.len(),
            )
        })
        .collect::<Vec<_>>()
        .join(",");

    let json = format!("{{\"sheet_size\":{sheet_size},\"hat_points\":[{points_json}],\"frames\":[{frames_json}]}}");

    let out_path = repo_root().join("hat_test_steps.json");
    std::fs::write(&out_path, json).expect("should be able to write hat_test_steps.json");
    println!("wrote {} part-by-part steps to {}", steps.len(), out_path.display());
}
