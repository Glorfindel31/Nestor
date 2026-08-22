//! Multi-sheet consistency benchmark: when one shape is nested many times
//! across many sheets, do all the sheets pack as well as the best one?
//!
//! **Why this exists and `hat_bench` doesn't answer it.** Every other
//! benchmark here reports one aggregate utilisation, and an aggregate is
//! exactly the number that hides the failure being chased: a run of 33 sheets
//! where 30 sit at 66% and 3 at 75% reports ~67% and looks merely mediocre,
//! when in fact the engine *found* a 75% arrangement and then failed to reuse
//! it on the other 30 sheets. Those nine points are not a tuning problem, they
//! are ~3 whole sheets of material, and no single-number benchmark can see
//! them.
//!
//! So this one reports the **distribution**: every sheet's own utilisation,
//! the spread between best and worst, and - the number to actually watch -
//! how many sheets a perfect replication of the best sheet would have saved.
//! That last figure is the size of the prize, in sheets, and it is what any
//! fix here has to move.
//!
//! `hat_bench` is deliberately untouched and remains the speed benchmark: one
//! sheet, fixed work, fixed answer, time as the only variable. This is its
//! opposite - many sheets, quality as the only variable, time incidental.
//!
//! Scenarios, all on 2440x1220mm stock at 5mm margin and 5mm spacing:
//!
//! | name    | parts                        | sheets offered |
//! |---------|------------------------------|----------------|
//! | `two`   | `two.dxf` x50                | 100            |
//! | `three` | `three.dxf` x50              | 100            |
//! | `both`  | `two.dxf` x50 + `three.dxf` x50 | 200         |
//!
//! The sheet counts are headroom, not targets - the engine uses as many as it
//! needs and the rest are never touched. Over-providing is deliberate: a run
//! that runs *out* of sheets leaves parts unplaced for a structural reason,
//! which confounds the packing-quality question this is asking.
//!
//! Usage:
//!   cargo run --release -p nesting --example sheet_spread             # all
//!   cargo run --release -p nesting --example sheet_spread -- two
//!   cargo run --release -p nesting --example sheet_spread -- both 10 4
//!                                                              ^gens ^rotations
//!
//! Release mode is not optional; a debug build is ~20x slower.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use dxf::Drawing;
use geometry::clearance::{prepare_part, prepare_sheet};
use geometry::dxf_import::{build_polygon_tree, entities_to_polygons, LayeredPolygon};
use geometry::point::Point;
use geometry::polygon::{get_polygon_bounds, polygon_area};
use nesting::dispatch;
use nesting::ga::{GaConfig, GeneticAlgorithm};
use nesting::placement::{PlacementConfig, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};
use nesting::spread::Spread;

const SHEET_W: f64 = 2440.0;
const SHEET_H: f64 = 1220.0;
/// Margin/spacing, set once from the command line. Overridable because the
/// reference tool was measured at several combinations, and the comparison is
/// meaningless unless ours runs at the same one.
static CLEARANCE: std::sync::OnceLock<(f64, f64)> = std::sync::OnceLock::new();

fn margin() -> f64 {
    CLEARANCE.get().copied().unwrap_or((5.0, 5.0)).0
}

fn spacing() -> f64 {
    CLEARANCE.get().copied().unwrap_or((5.0, 5.0)).1
}
const CURVE_TOLERANCE: f64 = 0.1;
const SEED: u64 = 0;

/// Defaults chosen to be a realistic job rather than a tuned one: this is
/// measuring what a user actually gets, so a config nobody would pick would
/// prove nothing. Overridable from the command line.
const DEFAULT_GENERATIONS: usize = 5;
const DEFAULT_ROTATIONS: u32 = 4;
const POPULATION: usize = 10;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Loads every closed profile from a fixture and returns the largest by area.
///
/// Largest-by-area rather than "assert there is exactly one": these fixtures
/// are real drawings and may carry interior features or stray geometry, and a
/// hard assert would make the benchmark unusable the moment someone re-exports
/// one with an extra layer.
/// Every closed profile in a fixture, largest first.
fn load_profiles(name: &str) -> Vec<LayeredPolygon> {
    let fixture = repo_root().join("tests/fixtures").join(name);
    let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let mut tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    assert!(!tree.is_empty(), "no closed profile found in {}", fixture.display());
    tree.sort_by(|a, b| polygon_area(&b.points).abs().total_cmp(&polygon_area(&a.points).abs()));
    tree
}

fn load_part(name: &str) -> LayeredPolygon {
    let fixture = repo_root().join("tests/fixtures").join(name);
    let drawing = Drawing::load_file(&fixture).unwrap_or_else(|e| panic!("couldn't parse {}: {e}", fixture.display()));
    let tree = build_polygon_tree(entities_to_polygons(drawing.entities(), CURVE_TOLERANCE));
    assert!(!tree.is_empty(), "no closed profile found in {}", fixture.display());
    tree.into_iter().max_by(|a, b| polygon_area(&a.points).abs().total_cmp(&polygon_area(&b.points).abs())).expect("non-empty")
}

fn rect(w: f64, h: f64) -> Vec<Point> {
    vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)]
}

/// One shape to nest, and how many copies.
struct PartSpec {
    file: &'static str,
    quantity: usize,
    /// Nest *every* closed profile in the file as its own part, `quantity`
    /// copies each - not just the largest one.
    ///
    /// This exists because taking only the largest silently turned a
    /// four-variant job into a one-variant one, and then compared the result
    /// against a reference tool that had nested all four. A fixture is a file,
    /// not a shape, and defaulting to "the biggest thing in it" is a good way
    /// to benchmark something nobody asked for.
    all_profiles: bool,
}

struct Scenario {
    name: &'static str,
    parts: Vec<PartSpec>,
    sheet_copies: usize,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario { name: "two", parts: vec![PartSpec { file: "two.dxf", quantity: 50, all_profiles: false }], sheet_copies: 100 },
        Scenario { name: "three", parts: vec![PartSpec { file: "three.dxf", quantity: 50, all_profiles: false }], sheet_copies: 100 },
        Scenario {
            name: "both",
            parts: vec![PartSpec { file: "two.dxf", quantity: 50, all_profiles: false }, PartSpec { file: "three.dxf", quantity: 50, all_profiles: false }],
            sheet_copies: 200,
        },
        // The apples-to-apples reference comparison: all four profiles in
        // two.dxf, 50 copies each = 200 parts, which is exactly the job a
        // commercial nester was measured on at 13 sheets / ~87.9%.
        Scenario { name: "ref", parts: vec![PartSpec { file: "two.dxf", quantity: 50, all_profiles: true }], sheet_copies: 100 },
    ]
}

fn run(scenario: &Scenario, generations: usize, rotations: u32) {
    println!("\n=== scenario '{}' ===", scenario.name);

    let sheet_points = rect(SHEET_W, SHEET_H);
    let sheet_usable = prepare_sheet(&sheet_points, margin(), spacing()).expect("sheet should offset cleanly");
    let sheet_area = polygon_area(&sheet_usable).abs();
    let sheet = LayeredPolygon { points: sheet_usable, layer: "sheet".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None };
    let sheets: Vec<LayeredPolygon> = (0..scenario.sheet_copies).map(|_| sheet.clone()).collect();

    let mut parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::new();
    let mut shape_ids: HashMap<usize, usize> = HashMap::new();
    let mut true_area_by_id: HashMap<usize, f64> = HashMap::new();
    let mut adam: Vec<usize> = Vec::new();
    let mut total_true_area = 0.0;
    let mut next_id = 0usize;

    let expanded: Vec<(&PartSpec, LayeredPolygon)> = scenario
        .parts
        .iter()
        .flat_map(|spec| {
            let shapes = if spec.all_profiles { load_profiles(spec.file) } else { vec![load_part(spec.file)] };
            shapes.into_iter().map(move |s| (spec, s))
        })
        .collect();

    for (source_id, (spec, shape)) in expanded.iter().enumerate() {
        let bounds = get_polygon_bounds(&shape.points).expect("part has points");
        let true_area = polygon_area(&shape.points).abs();
        let padded = prepare_part(&shape.points, spacing()).expect("part should offset cleanly");
        let part = LayeredPolygon { points: padded, layer: shape.layer.clone(), is_circle: None, children: shape.children.clone(), texts: shape.texts.clone(), real_boundary: None };
        println!(
            "  {:<14}: {:.1} x {:.1} mm, area {:.0} mm^2, x{} copies ({:.1}% of a sheet each)",
            format!("{} #{}", spec.file, source_id + 1),
            bounds.width,
            bounds.height,
            true_area,
            spec.quantity,
            true_area / sheet_area * 100.0
        );

        for _ in 0..spec.quantity {
            parts_by_id.insert(next_id, part.clone());
            shape_ids.insert(next_id, source_id);
            true_area_by_id.insert(next_id, true_area);
            adam.push(next_id);
            total_true_area += true_area;
            next_id += 1;
        }
    }

    // The floor nothing can beat: total part area over sheet area. Printed so
    // a utilisation figure can be read against what was ever achievable rather
    // than against 100%.
    let theoretical_min_sheets = (total_true_area / sheet_area).ceil() as usize;
    println!("  {} part(s) total, {:.2} sheets' worth of material (absolute floor: {theoretical_min_sheets} sheets)", adam.len(), total_true_area / sheet_area);
    assert!(
        scenario.sheet_copies > theoretical_min_sheets,
        "scenario '{}' offers {} sheets but needs at least {theoretical_min_sheets} - parts would be unplaceable for a structural reason, not a packing one",
        scenario.name,
        scenario.sheet_copies
    );

    let placement_config = PlacementConfig {
        placement_type: PlacementType::TightFit,
        rotations,
        curve_tolerance: CURVE_TOLERANCE,
        dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
        part_rules: Default::default(),
    };
    let ga_config = GaConfig { population_size: POPULATION, mutation_rate: 10.0, rotations, mirror: false, part_rules: Default::default() };
    // `NEST_INTERLEAVE=1` round-robins the seed order across part types
    // instead of listing every copy of type 1, then every copy of type 2, and
    // so on. `place_parts` consumes the gene in order and closes a sheet when
    // nothing more fits, so a grouped seed fills each sheet from a single type
    // by construction - which is exactly the segregation being investigated.
    // This is a measurement switch, not a proposed fix; it exists to test
    // whether seed order is the cause.
    if std::env::var("NEST_INTERLEAVE").is_ok_and(|v| v != "0") {
        let mut by_source: Vec<Vec<usize>> = vec![Vec::new(); expanded.len()];
        for &id in &adam {
            by_source[shape_ids[&id]].push(id);
        }
        let mut interleaved = Vec::with_capacity(adam.len());
        let mut round = 0;
        while interleaved.len() < adam.len() {
            for bucket in &by_source {
                if let Some(&id) = bucket.get(round) {
                    interleaved.push(id);
                }
            }
            round += 1;
        }
        adam = interleaved;
        println!("  (seed order interleaved across {} part types)", expanded.len());
    }

    let mut ga = GeneticAlgorithm::new(adam, ga_config, Vec::new(), SEED);

    let started = Instant::now();
    let result = dispatch::run(&mut ga, &sheets, &parts_by_id, &shape_ids, &placement_config, generations, &|| false, &|_, _| {});
    let elapsed = started.elapsed();

    let Some(result) = result else {
        println!("  NO RESULT - nothing placed at all");
        return;
    };

    let spread = Spread::of(&result, &true_area_by_id, sheet_area);
    println!("
  {generations} generation(s) x population {POPULATION}, rotations {rotations}, in {elapsed:.2?}");
    print!("{}", spread.report(result.unplaced_count));

    // Which shapes actually shared a sheet. The distribution alone says the
    // sheets differ; this says *why* - a sheet holding one shape only, when
    // another shape would have fitted in its leftovers, is the engine
    // segregating shapes rather than mixing them, which is a different fault
    // from packing any one shape badly.
    if expanded.len() > 1 {
        println!("
  per-sheet composition (count by source shape):");
        let mut tally: HashMap<Vec<usize>, usize> = HashMap::new();
        for placement in &result.placements {
            if placement.parts.is_empty() {
                continue;
            }
            let mut counts = vec![0usize; expanded.len()];
            for p in &placement.parts {
                if let Some(&src) = shape_ids.get(&p.id) {
                    counts[src] += 1;
                }
            }
            *tally.entry(counts).or_default() += 1;
        }
        let mut rows: Vec<_> = tally.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for (counts, sheets) in rows {
            let detail: Vec<String> = counts.iter().enumerate().filter(|(_, n)| **n > 0).map(|(i, n)| format!("{n}x#{}", i + 1)).collect();
            println!("    {sheets:3} sheet(s): {}", detail.join(" + "));
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "all".into());
    let generations: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_GENERATIONS);
    let rotations: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_ROTATIONS);
    let m: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let sp: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let _ = CLEARANCE.set((m, sp));

    println!("sheet_spread: {SHEET_W:.0}x{SHEET_H:.0}mm stock, margin {}mm, spacing {}mm, seed {SEED}", margin(), spacing());

    let all = scenarios();
    let selected: Vec<&Scenario> = if which == "all" { all.iter().collect() } else { all.iter().filter(|s| s.name == which).collect() };
    assert!(
        !selected.is_empty(),
        "unknown scenario '{which}' - expected one of: all, {}",
        all.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
    );

    for scenario in selected {
        run(scenario, generations, rotations);
    }
}
