//! Performance-bottleneck-finding benchmark: repeated timed nest runs
//! against a real DXF fixture, on the sheet size/margin/spacing a real
//! job uses, logged via `nesting::benchmark_log` in the same two-file
//! shape `main/benchmarkLogger.js` established (a per-generation detail
//! log, `nest-benchmark.log`, and a per-run summary CSV,
//! `nest-benchmark-runs.csv`, both at the repo root).
//!
//! Usage: `cargo run --release -p nesting --example bench -- [num_runs] [run_seconds] [placement_type]`
//! (defaults: 40 runs, 60s each, gravity - pass small values to smoke-test
//! first, release mode matters a lot here, this is a perf measurement).
//! `placement_type` is one of `gravity`/`box`/`convexhull`/`tightfit`/
//! `gravitytightfit`/`gravitycorrective`.
//!
//! **Grid-sweep mode**: `cargo run --release -p nesting --example bench -- grid`
//! runs a fixed-generations (5) sweep across `placement_type` x
//! `population_size` x `rotations` x `dominant_part_area_threshold`
//! combinations instead of the single fixed config above, answering "which
//! settings find the best arrangement (fewest sheets, highest utilisation)
//! fastest" rather than "how fast is one fixed config." Each combination
//! gets its own fresh `GeneticAlgorithm`/`NfpCache` (same as the normal
//! per-run loop below - a cache doesn't carry over between combinations,
//! since a real user changing these settings between runs wouldn't share
//! one either). Results go to `nest-grid-results.csv`, one row per
//! combination, so they can be sorted/compared after the sweep finishes.
//!
//! Sheet is a plain 2440x1220mm rectangle with a 3mm margin and 6.5mm
//! spacing applied via `geometry::clearance::prepare_sheet`/`prepare_part`
//! (see that module's doc comment for the margin-vs-spacing derivation).
//! Parts are every closed profile in *both* `tests/fixtures/FLAT.dxf` and
//! `tests/fixtures/FLAT-struck.dxf` (two real cut layouts, combined into one
//! part pool - ids continue across the second file rather than restarting
//! at 0, so they stay unique). Holes aren't re-offset - the padding is a
//! keep-out zone around the *outside* of a part for inter-part clearance,
//! unrelated to interior features.
//!
//! `SHEET_COPIES` is asserted at startup to have real headroom over the
//! computed minimum (total part area / sheet area, at the ~90% packing
//! efficiency real runs actually achieve) instead of being a bare guess - a
//! previous pass used a guessed 40, which turned out to be below the true
//! minimum for `FLAT.dxf` alone, leaving parts structurally unable to fit
//! regardless of GA quality and confounding the "why doesn't more compute
//! time help" result. Panics with the real numbers instead of silently
//! under-providing sheets again.
//!
//! Each run gets a fresh `GeneticAlgorithm` and calls
//! `dispatch::run_generation` in a loop until `run_seconds` of wall clock
//! have elapsed, logging every generation's timing/fitness/placement
//! stats - this is real per-generation throughput data (rayon-parallel,
//! same as an actual run), not a synthetic microbenchmark, and is exactly
//! the shape of information needed to see whether NfpCache's absence (see
//! `docs/PORT_STATUS.md`'s Phase 4 row - built but not wired into the
//! placement pipeline yet) is actually the dominant cost here or not.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dxf::Drawing;
use geometry::clearance::{prepare_part, prepare_sheet};
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, LayeredPolygon};
use nesting::benchmark_log::{append_benchmark_line, append_run_summary_row, git_revision};
use nesting::cache::NfpCache;
use nesting::dispatch;
use nesting::ga::{GaConfig, GeneticAlgorithm};
use nesting::placement::{PlacementConfig, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};

const SHEET_WIDTH: f64 = 2440.0;
const SHEET_HEIGHT: f64 = 1220.0;
const MARGIN: f64 = 3.0;
const SPACING: f64 = 6.5;
const SHEET_COPIES: usize = 120;
const CURVE_TOLERANCE: f64 = 0.3;
/// Real utilisation observed in prior runs on this fixture set was ~91%;
/// use a slightly more conservative 90% here so the startup check has a
/// small margin of its own rather than asserting against the exact figure.
const ASSUMED_PACKING_EFFICIENCY: f64 = 0.9;
const FIXTURES: &[&str] = &["FLAT.dxf", "FLAT-struck.dxf"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Loads every closed profile from every file in `FIXTURES`, combined into
/// one part pool - ids continue across files (a plain running counter)
/// rather than restarting at 0 per file, so they stay unique.
fn load_parts() -> HashMap<usize, LayeredPolygon> {
    let mut parts_by_id = HashMap::new();
    let mut next_id = 0usize;
    let mut skipped = 0usize;

    for name in FIXTURES {
        let fixture = repo_root().join("tests/fixtures").join(name);
        let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
        let flat = entities_to_polygons(drawing.entities(), CURVE_TOLERANCE);
        let tree = build_polygon_tree(flat);

        for root in tree {
            let Some(expanded_points) = prepare_part(&root.points, SPACING) else {
                skipped += 1;
                continue;
            };
            parts_by_id.insert(
                next_id,
                LayeredPolygon { points: expanded_points, layer: root.layer, is_circle: None, children: root.children, texts: root.texts, real_boundary: None },
            );
            next_id += 1;
        }
    }
    if skipped > 0 {
        eprintln!("warning: {skipped} profile(s) failed to offset for spacing and were skipped");
    }
    parts_by_id
}

fn build_sheet() -> LayeredPolygon {
    let raw = vec![
        geometry::point::Point::new(0.0, 0.0),
        geometry::point::Point::new(SHEET_WIDTH, 0.0),
        geometry::point::Point::new(SHEET_WIDTH, SHEET_HEIGHT),
        geometry::point::Point::new(0.0, SHEET_HEIGHT),
    ];
    // See `geometry::clearance`'s module doc for the margin/spacing
    // derivation (parts are padded by `prepare_part` in `load_parts` above;
    // the sheet's own inset here has to be net of that padding, which is
    // exactly what `prepare_sheet` computes).
    let points = prepare_sheet(&raw, MARGIN, SPACING).expect("margin/spacing should leave a usable sheet at these dimensions");
    LayeredPolygon { points, layer: "SHEET".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None }
}

/// Loads the fixture set, builds the sheet pool, and sanity-checks
/// `SHEET_COPIES` has real headroom - shared setup between the normal
/// fixed-config loop in `main()` and `run_grid()`'s sweep, so both measure
/// against the exact same parts/sheets.
fn setup_fixture() -> (HashMap<usize, LayeredPolygon>, Vec<usize>, Vec<LayeredPolygon>) {
    let parts_by_id = load_parts();
    let adam: Vec<usize> = {
        let mut ids: Vec<usize> = parts_by_id.keys().copied().collect();
        ids.sort_by(|&a, &b| {
            let area_a = geometry::polygon::polygon_area(&parts_by_id[&a].points).abs();
            let area_b = geometry::polygon::polygon_area(&parts_by_id[&b].points).abs();
            area_b.total_cmp(&area_a)
        });
        ids
    };
    let sheets: Vec<LayeredPolygon> = (0..SHEET_COPIES).map(|_| build_sheet()).collect();

    // Fail fast with the real numbers instead of silently under-providing
    // sheets again (see this file's module doc comment for the previous
    // pass that got this wrong with a bare guess of 40).
    let sheet_area = geometry::polygon::polygon_area(&sheets[0].points).abs();
    let total_part_area: f64 = adam.iter().map(|id| geometry::polygon::polygon_area(&parts_by_id[id].points).abs()).sum();
    let usable_capacity = SHEET_COPIES as f64 * sheet_area * ASSUMED_PACKING_EFFICIENCY;
    assert!(
        usable_capacity >= total_part_area,
        "SHEET_COPIES={SHEET_COPIES} isn't enough: {total_part_area:.0}mm2 of parts vs {usable_capacity:.0}mm2 of usable capacity \
         at {:.0}% packing efficiency ({:.1} sheets minimum at 100%, {:.1} at {:.0}%) - raise SHEET_COPIES",
        ASSUMED_PACKING_EFFICIENCY * 100.0,
        total_part_area / sheet_area,
        total_part_area / (sheet_area * ASSUMED_PACKING_EFFICIENCY),
        ASSUMED_PACKING_EFFICIENCY * 100.0,
    );

    (parts_by_id, adam, sheets)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();

    if first.as_deref() == Some("grid") {
        run_grid();
        return;
    }

    let num_runs: usize = first.and_then(|s| s.parse().ok()).unwrap_or(40);
    let run_seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let placement_type = match args.next().as_deref() {
        Some("box") => PlacementType::Box,
        Some("convexhull") => PlacementType::ConvexHull,
        Some("tightfit") => PlacementType::TightFit,
        Some("gravitytightfit") => PlacementType::GravityTightFit,
        Some("gravitycorrective") => PlacementType::GravityCorrective,
        Some("gravity") | None => PlacementType::Gravity,
        Some(other) => panic!("unknown placement_type {other:?} - expected gravity/box/convexhull/tightfit/gravitytightfit/gravitycorrective"),
    };

    let (parts_by_id, adam, sheets) = setup_fixture();

    let placement_config =
        PlacementConfig { placement_type, rotations: 4, dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD, curve_tolerance: CURVE_TOLERANCE };
    let ga_config = GaConfig { population_size: 10, mutation_rate: 10.0, rotations: 4 };

    let detail_log = repo_root().join("nest-benchmark.log");
    let runs_csv = repo_root().join("nest-benchmark-runs.csv");
    let runs_header = "timestamp,git_rev,run,elapsed_s,generations,parts_total,population_size,rotations,mutation_rate,placement_type,dominant_part_area_threshold,curve_tolerance,sheet_width,sheet_height,margin,spacing,best_fitness,sheets_used,parts_placed,parts_unplaced,utilisation_pct,avg_gen_ms,min_gen_ms,max_gen_ms";

    println!(
        "bench: {} parts, {} sheet copies ({SHEET_WIDTH}x{SHEET_HEIGHT}mm, margin {MARGIN}mm, spacing {SPACING}mm), {num_runs} runs x {run_seconds}s, rev={}",
        adam.len(),
        SHEET_COPIES,
        git_revision()
    );

    for run in 1..=num_runs {
        // seed = run - 1: run 1 uses seed 0, run 2 seed 1, etc. - each run
        // is its own fully reproducible trial (same seed always reproduces
        // the same result), while different runs sample different starting
        // populations, same as `rand::thread_rng()` used to but repeatably.
        let mut ga = GeneticAlgorithm::new(adam.clone(), ga_config.clone(), Vec::new(), (run - 1) as u64);
        // Fresh per run, shared across every generation within it - matches
        // real usage (one NfpCache per run_nest_command call, not per
        // generation), so this bench measures the same cache-hit-rate
        // shape a real run gets.
        let cache = NfpCache::new();
        let run_start = Instant::now();
        let deadline = Duration::from_secs(run_seconds);

        let mut generation = 0usize;
        let mut best_fitness = f64::INFINITY;
        let mut best_unplaced = usize::MAX;
        let mut best_sheets_used = 0usize;
        let mut best_utilisation = 0.0;
        let mut gen_times_ms: Vec<u128> = Vec::new();

        while run_start.elapsed() < deadline {
            let gen_start = Instant::now();
            let results = dispatch::run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &placement_config, &|| false, &|_, _| {}, &cache);
            let gen_ms = gen_start.elapsed().as_millis();
            generation += 1;
            gen_times_ms.push(gen_ms);

            for evaluated in &results {
                let r = &evaluated.result;
                if r.fitness < best_fitness {
                    best_fitness = r.fitness;
                    best_unplaced = r.unplaced_count;
                    best_sheets_used = r.placements.len();
                    best_utilisation = r.utilisation;
                }
            }

            append_benchmark_line(
                &detail_log,
                &format!(
                    "{},{},run={run},gen={generation},gen_ms={gen_ms},best_fitness={best_fitness:.0},unplaced={best_unplaced},sheets_used={best_sheets_used}",
                    unix_timestamp(),
                    git_revision(),
                ),
            );
            println!(
                "run {run}/{num_runs} gen {generation}: {gen_ms}ms, best_fitness={best_fitness:.0}, unplaced={best_unplaced}, sheets={best_sheets_used}, util={best_utilisation:.1}%"
            );
        }

        let avg_gen_ms = if gen_times_ms.is_empty() { 0.0 } else { gen_times_ms.iter().sum::<u128>() as f64 / gen_times_ms.len() as f64 };
        let min_gen_ms = gen_times_ms.iter().min().copied().unwrap_or(0);
        let max_gen_ms = gen_times_ms.iter().max().copied().unwrap_or(0);
        let parts_placed = adam.len().saturating_sub(best_unplaced.min(adam.len()));

        append_run_summary_row(
            &runs_csv,
            runs_header,
            &[
                unix_timestamp(),
                git_revision().to_string(),
                run.to_string(),
                format!("{:.1}", run_start.elapsed().as_secs_f64()),
                generation.to_string(),
                adam.len().to_string(),
                ga_config.population_size.to_string(),
                ga_config.rotations.to_string(),
                format!("{:.1}", ga_config.mutation_rate),
                format!("{:?}", placement_config.placement_type),
                format!("{:.2}", placement_config.dominant_part_area_threshold),
                format!("{:.2}", placement_config.curve_tolerance),
                SHEET_WIDTH.to_string(),
                SHEET_HEIGHT.to_string(),
                MARGIN.to_string(),
                SPACING.to_string(),
                format!("{best_fitness:.1}"),
                best_sheets_used.to_string(),
                parts_placed.to_string(),
                best_unplaced.to_string(),
                format!("{best_utilisation:.2}"),
                format!("{avg_gen_ms:.1}"),
                min_gen_ms.to_string(),
                max_gen_ms.to_string(),
            ],
        );

        println!("=== run {run}/{num_runs} done: {generation} generations in {:.1}s (avg {avg_gen_ms:.0}ms/gen) ===", run_start.elapsed().as_secs_f64());
    }

    println!("all runs complete. see {} and {}", detail_log.display(), runs_csv.display());
}

/// Fixed-generations sweep across `placement_type` x `population_size` x
/// `rotations` x `dominant_part_area_threshold` - see this file's module
/// doc comment for the "which settings find the best arrangement fastest"
/// question this answers, as distinct from `main()`'s "how fast is one
/// fixed config" loop above.
fn run_grid() {
    const GENERATIONS: usize = 5;
    let placement_types = [PlacementType::Gravity, PlacementType::Box, PlacementType::ConvexHull];
    let population_sizes = [6usize, 10, 20];
    let rotations_options = [1u32, 2, 4];
    let dominant_thresholds = [0.5f64, 0.9];

    let (parts_by_id, adam, sheets) = setup_fixture();
    let total_combos = placement_types.len() * population_sizes.len() * rotations_options.len() * dominant_thresholds.len();

    let grid_csv = repo_root().join("nest-grid-results.csv");
    let grid_header = "timestamp,git_rev,placement_type,population_size,rotations,dominant_threshold,gen1_ms,gen2_ms,gen3_ms,gen4_ms,gen5_ms,total_time_s,best_fitness,sheets_used,parts_placed,parts_unplaced,utilisation_pct";

    println!(
        "grid: {} parts, {} sheet copies, {GENERATIONS} generations/combo, {total_combos} combinations, rev={}",
        adam.len(),
        SHEET_COPIES,
        git_revision()
    );

    let mut combo_num = 0usize;
    for &placement_type in &placement_types {
        for &population_size in &population_sizes {
            for &rotations in &rotations_options {
                for &dominant_part_area_threshold in &dominant_thresholds {
                    combo_num += 1;

                    let placement_config = PlacementConfig { placement_type, rotations, dominant_part_area_threshold, curve_tolerance: CURVE_TOLERANCE };
                    let ga_config = GaConfig { population_size, mutation_rate: 10.0, rotations };
                    let mut ga = GeneticAlgorithm::new(adam.clone(), ga_config, Vec::new(), 0);
                    // Fresh per combination, not shared across them - a real
                    // user changing these settings between runs wouldn't
                    // share a cache either, and reusing one here would let
                    // an earlier combination's warm cache flatter a later
                    // combination's numbers.
                    let cache = NfpCache::new();

                    let mut best_fitness = f64::INFINITY;
                    let mut best_unplaced = usize::MAX;
                    let mut best_sheets_used = 0usize;
                    let mut best_utilisation = 0.0;
                    let mut gen_times_ms: Vec<u128> = Vec::with_capacity(GENERATIONS);

                    let combo_start = Instant::now();
                    for _ in 0..GENERATIONS {
                        let gen_start = Instant::now();
                        let results = dispatch::run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &placement_config, &|| false, &|_, _| {}, &cache);
                        gen_times_ms.push(gen_start.elapsed().as_millis());

                        for evaluated in &results {
                            let r = &evaluated.result;
                            if r.fitness < best_fitness {
                                best_fitness = r.fitness;
                                best_unplaced = r.unplaced_count;
                                best_sheets_used = r.placements.len();
                                best_utilisation = r.utilisation;
                            }
                        }
                    }
                    let total_time_s = combo_start.elapsed().as_secs_f64();
                    let parts_placed = adam.len().saturating_sub(best_unplaced.min(adam.len()));

                    println!(
                        "[{combo_num}/{total_combos}] placement={placement_type:?} pop={population_size} rot={rotations} dominant={dominant_part_area_threshold:.2}: \
                         {total_time_s:.1}s, fitness={best_fitness:.0}, sheets={best_sheets_used}, unplaced={best_unplaced}, util={best_utilisation:.1}%"
                    );

                    let gen_ms_cols: Vec<String> = (0..GENERATIONS).map(|i| gen_times_ms.get(i).map_or_else(String::new, u128::to_string)).collect();
                    let mut row = vec![
                        unix_timestamp(),
                        git_revision().to_string(),
                        format!("{placement_type:?}"),
                        population_size.to_string(),
                        rotations.to_string(),
                        format!("{dominant_part_area_threshold:.2}"),
                    ];
                    row.extend(gen_ms_cols);
                    row.extend([
                        format!("{total_time_s:.1}"),
                        format!("{best_fitness:.1}"),
                        best_sheets_used.to_string(),
                        parts_placed.to_string(),
                        best_unplaced.to_string(),
                        format!("{best_utilisation:.2}"),
                    ]);
                    append_run_summary_row(&grid_csv, grid_header, &row);
                }
            }
        }
    }

    println!("grid sweep complete: {total_combos} combinations. see {}", grid_csv.display());
}

/// No `chrono`/`time` dependency for one timestamp column - a fixed-offset
/// seconds-since-UNIX-epoch value is enough to line runs up chronologically
/// (which is all the original's ISO-string timestamp was for) without a new
/// dependency for it.
fn unix_timestamp() -> String {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs().to_string()).unwrap_or_else(|_| "0".to_string())
}
