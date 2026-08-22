//! Pattern replication: optimise **one** sheet hard, then stamp it as many
//! times as the remaining quantities allow, and repeat on what is left.
//!
//! **Why this exists.** `dispatch::run` searches one permutation of the whole
//! job and lets `place_parts` fill sheets from it in order. Every sheet is
//! therefore solved once, in passing, with whatever parts the gene happened to
//! leave for it - and the search budget is spread across a permutation of
//! hundreds of parts. Measured on the reference job (200 parts, 2440x1220,
//! margin 0 / spacing 6) that gives sheets scattered from 49% to 82%.
//!
//! The commercial tool we measure against does something structurally
//! different, and it is visible in its own report: five *patterns* with a
//! Duplicate column summing to thirteen sheets, every one of them at 87.9%.
//! It is not finding better placements than we can - on the interlocking
//! fixture, where band packing cannot help and it is placement quality alone,
//! the two engines tie to within 11mm2 out of 389 711. It is spending its
//! whole search budget on *one* sheet instead of on a 200-part permutation,
//! and then reusing the answer.
//!
//! That is what this module does:
//!
//! 1. Nest a single sheet from everything still unplaced.
//! 2. Work out how many times that arrangement repeats given what is left.
//! 3. Emit it that many times, subtract, and go again.
//!
//! **What it deliberately does not do.** No attempt to choose *which* parts
//! make the best pattern - the single-sheet nest picks whatever fits best from
//! the whole remaining pool, which is the same choice `place_parts` already
//! makes, just concentrated. And no back-tracking: a pattern, once emitted, is
//! not revisited. `consolidation::refine_consolidation` still runs afterwards
//! in the caller and can drain a half-empty tail sheet.
//!
//! The last round is where the tail lands: once no pattern repeats more than
//! once, this degenerates into "nest one sheet at a time", which is strictly
//! what the old path did per sheet anyway.

use std::collections::HashMap;

use geometry::dxf_import::LayeredPolygon;

use crate::consolidation::recompute_totals;
use crate::dispatch::{self, MIRROR_ID_BIT};
use crate::ga::{GaConfig, GeneticAlgorithm};
use crate::placement::{PlaceResult, PlacedPart, PlacementConfig, SheetPlacement};

/// A part id with its mirror bit stripped - the identity that decides which
/// pool a copy can be drawn from.
fn base(id: usize) -> usize {
    id & !MIRROR_ID_BIT
}

/// The shape a part id belongs to, mirror-insensitive.
///
/// `shape_ids` registers a mirrored copy under `source ^ MIRROR_ID_BIT`, so
/// asking it directly would file a shape and its own flipped copy as two
/// different shapes - and then a pattern that used one could never be told it
/// may draw from the other's pool.
fn source_of(id: usize, shape_ids: &HashMap<usize, usize>) -> Option<usize> {
    shape_ids.get(&base(id)).map(|&s| base(s))
}

/// Nests `adam` onto `sheets` by finding and repeating whole-sheet patterns.
///
/// Same signature shape as `dispatch::run` plus the sheet list, and returns the
/// same `PlaceResult`, so a caller can swap between the two.
///
/// `generations` is per *pattern*, not for the whole job. That is the point:
/// each round gets the full budget on one sheet.
#[must_use]
pub fn run(
    sheets: &[LayeredPolygon],
    adam: Vec<usize>,
    parts_by_id: &HashMap<usize, LayeredPolygon>,
    shape_ids: &HashMap<usize, usize>,
    ga_config: &GaConfig,
    placement_config: &PlacementConfig,
    generations: usize,
    seed: u64,
    should_cancel: &(impl Fn() -> bool + Sync),
) -> Option<PlaceResult> {
    if sheets.is_empty() || adam.is_empty() {
        return None;
    }

    // Unplaced ids, grouped by shape. A pattern says "one of shape 3 goes
    // here"; which physical copy fills that slot is arbitrary, because every
    // copy of a shape is the same geometry.
    let mut pool: HashMap<usize, Vec<usize>> = HashMap::new();
    for &id in &adam {
        if let Some(source) = source_of(id, shape_ids) {
            pool.entry(source).or_default().push(id);
        }
    }

    let mut placements: Vec<SheetPlacement> = Vec::new();
    let mut sheet_cursor = 0usize;
    let mut round = 0u64;

    while sheet_cursor < sheets.len() && pool.values().any(|v| !v.is_empty()) {
        if should_cancel() {
            break;
        }

        // --- 1. Nest one sheet from everything still unplaced.
        let remaining: Vec<usize> = {
            let mut v: Vec<usize> = pool.values().flatten().copied().collect();
            // `adam`'s order is load-bearing (`expand_parts` sorts by
            // decreasing area, which is the seed the GA starts from), and
            // draining a HashMap loses it.
            v.sort_by_key(|id| adam.iter().position(|a| a == id).unwrap_or(usize::MAX));
            v
        };
        let one_sheet = [sheets[sheet_cursor].clone()];
        let mut ga = GeneticAlgorithm::new(remaining.clone(), ga_config.clone(), Vec::new(), seed.wrapping_add(round));
        let Some(attempt) = dispatch::run(&mut ga, &one_sheet, parts_by_id, shape_ids, placement_config, generations, should_cancel, &|_, _| {}) else {
            break;
        };
        let Some(pattern) = attempt.placements.into_iter().find(|p| !p.parts.is_empty()) else {
            break;
        };
        round += 1;
        if std::env::var("NEST_PATTERNS_DEBUG").is_ok_and(|v| v != "0") {
            eprintln!("  pattern {round}: {} part(s)", pattern.parts.len());
        }

        // --- 2. How many times does it repeat?
        // Per shape, how many slots the pattern wants against how many copies
        // are left. The tightest shape decides, and the sheets left cap it.
        let mut need: HashMap<usize, usize> = HashMap::new();
        for part in &pattern.parts {
            if let Some(source) = source_of(part.id, shape_ids) {
                *need.entry(source).or_default() += 1;
            }
        }
        let repeats = need
            .iter()
            .map(|(source, &wanted)| pool.get(source).map_or(0, Vec::len) / wanted.max(1))
            .min()
            .unwrap_or(0)
            .min(sheets.len() - sheet_cursor)
            .max(1);

        // --- 3. Stamp it, drawing fresh copies for each repeat.
        let mut stamped = 0usize;
        for _ in 0..repeats {
            let mut parts: Vec<PlacedPart> = Vec::with_capacity(pattern.parts.len());
            for slot in &pattern.parts {
                let Some(source) = source_of(slot.id, shape_ids) else { continue };
                let Some(id) = pool.get_mut(&source).and_then(Vec::pop) else { continue };
                // Keep the slot's mirror bit, not the drawn copy's: the
                // pattern decided this position holds a flipped part, and
                // `parts_by_id` has the flipped geometry under that bit.
                parts.push(PlacedPart { id: base(id) | (slot.id & MIRROR_ID_BIT), placement: slot.placement, rotation: slot.rotation });
            }
            if parts.is_empty() {
                break;
            }
            let short = parts.len() < pattern.parts.len();
            placements.push(SheetPlacement { sheet_index: sheet_cursor, parts });
            sheet_cursor += 1;
            stamped += 1;
            // The pool ran dry part way through this copy - the repeat count
            // said it would not, so stop rather than stamping further short
            // sheets.
            if short || sheet_cursor >= sheets.len() {
                break;
            }
        }
        if stamped == 0 {
            break;
        }
    }

    if placements.is_empty() {
        return None;
    }

    let unplaced_ids: Vec<usize> = pool.into_values().flatten().collect();
    let totals = recompute_totals(&placements, parts_by_id, sheets);
    Some(PlaceResult {
        placements,
        // Same shape as `place_parts`'s own: sheets used, less how full they
        // are, so lower is better and opening a sheet always costs more than
        // any packing gain within one.
        fitness: sheet_cursor as f64 - totals.utilisation / 100.0,
        area: totals.total_placed_area,
        total_area: totals.total_usable_sheet_area,
        utilisation: totals.utilisation,
        unplaced_count: unplaced_ids.len(),
        unplaced_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};
    use geometry::point::Point;

    fn rect(w: f64, h: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    fn configs() -> (GaConfig, PlacementConfig) {
        (
            GaConfig { population_size: 4, mutation_rate: 10.0, rotations: 1, mirror: false, part_rules: Default::default() },
            PlacementConfig {
                placement_type: PlacementType::TightFit,
                rotations: 1,
                dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
                curve_tolerance: 0.3,
                part_rules: Default::default(),
                banded_pass: false,
            },
        )
    }

    /// A job of identical parts has exactly one sensible pattern, and it must
    /// be *stamped* rather than re-solved: every sheet the same, and the parts
    /// spread across them instead of piling onto the first.
    #[test]
    fn one_pattern_is_reused_across_every_sheet_it_fits() {
        let sheets: Vec<LayeredPolygon> = (0..4).map(|_| rect(100.0, 100.0)).collect();
        let parts_by_id: HashMap<usize, LayeredPolygon> = (0..16).map(|i| (i, rect(40.0, 40.0))).collect();
        let shape_ids: HashMap<usize, usize> = (0..16).map(|i| (i, 0)).collect();
        let (ga, placement) = configs();

        let result = run(&sheets, (0..16).collect(), &parts_by_id, &shape_ids, &ga, &placement, 2, 0, &|| false).expect("should nest");

        assert_eq!(result.unplaced_count, 0, "16 parts, 4 per sheet, 4 sheets - everything fits");
        assert!(result.placements.iter().all(|s| s.parts.len() == 4), "every sheet holds the full pattern");
        assert_eq!(result.placements.len(), 4, "four sheets used");
        // Every sheet is the same pattern: same count, same positions.
        let first: Vec<(i64, i64)> = result.placements[0].parts.iter().map(|p| (p.placement.x as i64, p.placement.y as i64)).collect();
        for sheet in &result.placements[1..] {
            let here: Vec<(i64, i64)> = sheet.parts.iter().map(|p| (p.placement.x as i64, p.placement.y as i64)).collect();
            assert_eq!(here, first, "every stamped sheet must be the same arrangement");
        }
    }

    /// **No part may be placed twice, and none may be lost.** Stamping draws
    /// fresh copies out of a pool for each repeat, which is exactly where a
    /// duplicated or dropped id would come from - and a duplicate is a part
    /// that gets cut once but billed twice.
    #[test]
    fn every_part_is_placed_exactly_once() {
        let sheets: Vec<LayeredPolygon> = (0..6).map(|_| rect(100.0, 100.0)).collect();
        let mut parts_by_id: HashMap<usize, LayeredPolygon> = (0..10).map(|i| (i, rect(40.0, 40.0))).collect();
        let mut shape_ids: HashMap<usize, usize> = (0..10).map(|i| (i, 0)).collect();
        for i in 10..17 {
            parts_by_id.insert(i, rect(30.0, 30.0));
            shape_ids.insert(i, 1);
        }
        let (ga, placement) = configs();

        let result = run(&sheets, (0..17).collect(), &parts_by_id, &shape_ids, &ga, &placement, 2, 0, &|| false).expect("should nest");

        let mut seen: Vec<usize> = result.placements.iter().flat_map(|s| s.parts.iter().map(|p| base(p.id))).collect();
        let placed = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), placed, "a part was placed twice");
        assert_eq!(placed + result.unplaced_count, 17, "every part must be placed or accounted unplaced");
    }

    /// Running out of sheets must stop cleanly and report the shortfall, not
    /// index past the end of the sheet list.
    #[test]
    fn a_job_bigger_than_the_stock_reports_what_did_not_fit() {
        let sheets = vec![rect(100.0, 100.0)];
        let parts_by_id: HashMap<usize, LayeredPolygon> = (0..12).map(|i| (i, rect(40.0, 40.0))).collect();
        let shape_ids: HashMap<usize, usize> = (0..12).map(|i| (i, 0)).collect();
        let (ga, placement) = configs();

        let result = run(&sheets, (0..12).collect(), &parts_by_id, &shape_ids, &ga, &placement, 2, 0, &|| false).expect("should place something");
        assert_eq!(result.placements.len(), 1);
        assert_eq!(result.placements[0].parts.len() + result.unplaced_count, 12);
        assert!(result.unplaced_count > 0, "eight of these cannot fit on one sheet");
    }

    #[test]
    fn nothing_to_nest_is_none_rather_than_an_empty_result() {
        let (ga, placement) = configs();
        assert!(run(&[rect(100.0, 100.0)], Vec::new(), &HashMap::new(), &HashMap::new(), &ga, &placement, 1, 0, &|| false).is_none());
        assert!(run(&[], vec![0], &HashMap::new(), &HashMap::new(), &ga, &placement, 1, 0, &|| false).is_none());
    }
}
