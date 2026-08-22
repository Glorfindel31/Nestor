//! The standard nesting benchmark: fixed work, fixed answer, time as the
//! only variable.
//!
//! `hat_test.rs`/`hat_test_svg.rs` are *time-boxed* - they run for N seconds
//! and report how far they got. That makes them useless as a speed
//! regression test: a faster engine gets more generations done, so the
//! result changes and the runtime doesn't. This one inverts that. It runs a
//! fixed number of generations from a fixed seed and asserts the exact best
//! result, so the only thing an optimisation can move is the clock.
//!
//! Determinism comes from three things already true of the engine, none of
//! them added here: `GeneticAlgorithm` is seeded (`StdRng`, seed 0, not
//! `thread_rng`), `dispatch::run_generation` collects its `par_iter` results
//! into an index-ordered `Vec` so rayon's scheduling can't reorder them, and
//! the shared `NfpCache` only ever affects how long a lookup takes, never
//! what it returns. The one non-deterministic input was the wall-clock
//! deadline, and this file simply doesn't have one (`should_cancel` is
//! `|| false`).
//!
//! The hat monotile is the fixture on purpose: it is an exactly
//! interlocking aperiodic tile, so the packing quality is razor-edged and
//! any accidental change in the geometry pipeline moves the number. See
//! `hat_test_svg.rs`'s module comment for the 8.4e-9 fixture-precision bug
//! that this sensitivity caught once already.
//!
//! Usage: `cargo run --release -p nesting --example hat_bench [-- generations part_count]`
//! Defaults: 3 generations, 252 parts. **Release mode is not optional** -
//! a debug build is ~20x slower and measures nothing useful.
//!
//! Config is `hat_test.rs`'s documented best-known one for this
//! tessellation: rotations=2, population=20, mutation=10, TightFit, seed 0,
//! 500x500 sheet, zero margin/spacing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dxf::Drawing;
use geometry::clearance::{prepare_part, prepare_sheet};
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, LayeredPolygon};
use geometry::point::Point;
use nesting::cache::NfpCache;
use nesting::dispatch;
use nesting::ga::{is_better_nest, GaConfig, GeneticAlgorithm};
use nesting::placement::{PlaceResult, PlacementConfig, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};

const SHEET_SIZE: f64 = 500.0;
const CURVE_TOLERANCE: f64 = 0.1;
const SEED: u64 = 0;

/// The answer this benchmark must keep producing, recorded from the first
/// run of this file. An optimisation that changes any of these has changed
/// the *nest*, not just its speed - which may well be an improvement, but it
/// is not a speedup and must not be reported as one. Re-record deliberately,
/// in its own commit, with the reason.
struct Expected {
    generations: usize,
    part_count: usize,
    sheets: usize,
    unplaced: usize,
    utilisation: f64,
}

const BASELINE: Expected = Expected { generations: 3, part_count: 252, sheets: 1, unplaced: 0, utilisation: 78.565_824_631_324_11 };

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_hat() -> LayeredPolygon {
    let fixture = repo_root().join("tests/fixtures/one.dxf");
    let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    assert_eq!(tree.len(), 1, "expected exactly one closed profile in one.dxf, got {}", tree.len());
    tree.into_iter().next().unwrap()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let generations: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(BASELINE.generations);
    let part_count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(BASELINE.part_count);

    let hat = load_hat();
    let padded = prepare_part(&hat.points, 0.0).expect("hat shape should offset cleanly at zero spacing");
    let part = LayeredPolygon { points: padded, layer: hat.layer.clone(), is_circle: None, children: hat.children.clone(), texts: hat.texts.clone(), real_boundary: None };

    let mut parts_by_id = HashMap::new();
    let mut shape_ids = HashMap::new();
    for id in 0..part_count {
        parts_by_id.insert(id, part.clone());
        shape_ids.insert(id, 0usize);
    }

    let sheet_raw = vec![Point::new(0.0, 0.0), Point::new(SHEET_SIZE, 0.0), Point::new(SHEET_SIZE, SHEET_SIZE), Point::new(0.0, SHEET_SIZE)];
    let sheets = vec![LayeredPolygon {
        points: prepare_sheet(&sheet_raw, 0.0, 0.0).expect("500x500 sheet should be usable at zero margin/spacing"),
        layer: "SHEET".into(),
        is_circle: None,
        children: Vec::new(),
        texts: Vec::new(),
        real_boundary: None,
    }];

    let placement_config = PlacementConfig {
        placement_type: PlacementType::TightFit,
        rotations: 2,
        dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
        curve_tolerance: CURVE_TOLERANCE,
        part_rules: Default::default(), banded_pass: true };
    let ga_config = GaConfig { population_size: 20, mutation_rate: 10.0, rotations: 2, mirror: false, part_rules: Default::default() };
    let mut ga = GeneticAlgorithm::new((0..part_count).collect(), ga_config, Vec::new(), SEED);

    let cache = NfpCache::new();
    let never_cancel = || false;
    let mut best: Option<PlaceResult> = None;
    let mut per_generation: Vec<Duration> = Vec::with_capacity(generations);

    println!("hat_bench: {generations} generations x population 20, {part_count} parts, seed {SEED}");
    let total_start = Instant::now();
    for generation in 1..=generations {
        let started = Instant::now();
        let results = dispatch::run_generation(&mut ga, &sheets, &parts_by_id, &shape_ids, &placement_config, &never_cancel, &|_, _| {}, &cache);
        let elapsed = started.elapsed();
        per_generation.push(elapsed);
        for evaluated in results {
            if best.as_ref().is_none_or(|b| is_better_nest(&evaluated.result, b)) {
                best = Some(evaluated.result);
            }
        }
        let best_util = best.as_ref().map_or(0.0, |b| b.utilisation);
        println!("  gen {generation}: {:>7.3}s   best util {best_util:.4}%", elapsed.as_secs_f64());
    }
    let total = total_start.elapsed();

    let best = best.expect("a run of at least one generation always produces a result");
    println!();
    println!("TOTAL          {:.3}s", total.as_secs_f64());
    println!("slowest gen    {:.3}s", per_generation.iter().max().map_or(0.0, Duration::as_secs_f64));
    println!("fastest gen    {:.3}s", per_generation.iter().min().map_or(0.0, Duration::as_secs_f64));
    println!("nfp cache      {} entries, {} lookups ({:.0} per entry)", cache.stats(), cache.lookups(), cache.lookups() as f64 / cache.stats().max(1) as f64);
    println!("RESULT         sheets={} unplaced={} util={:.14}%", best.placements.len(), best.unplaced_count, best.utilisation);
    if nesting::profile::enabled() {
        println!("
phase                  seconds        calls   (summed across threads)");
        for (name, seconds, calls) in nesting::profile::report() {
            println!("  {name:<20} {seconds:>8.3}   {calls:>10}");
        }
        println!();
        let counters = nesting::profile::counters();
        let total = counters.first().map_or(0, |(_, n)| *n).max(1);
        for (name, hits) in &counters {
            println!("  {name:<22} {hits:>12}   {:>6.1}%", *hits as f64 / total as f64 * 100.0);
        }
    } else {
        println!("(set NEST_PROFILE=1 for a phase breakdown)");
    }

    // Only assert against the recorded baseline when the run actually used
    // the baseline's parameters - the arguments exist for exploring, and an
    // exploratory run has no recorded answer to check itself against.
    if generations != BASELINE.generations || part_count != BASELINE.part_count {
        println!("\n(no baseline for {generations} generations / {part_count} parts - result not checked)");
        return;
    }
    assert_eq!(best.placements.len(), BASELINE.sheets, "sheet count changed - this is a different nest, not a faster one");
    assert_eq!(best.unplaced_count, BASELINE.unplaced, "unplaced count changed - this is a different nest, not a faster one");
    assert!(
        (best.utilisation - BASELINE.utilisation).abs() < 1e-9,
        "utilisation changed: {:.14}% vs baseline {:.14}% - this is a different nest, not a faster one",
        best.utilisation,
        BASELINE.utilisation
    );
    println!("\nOK - matches baseline exactly.");
}
