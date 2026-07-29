//! Post-nest "cleaning pass": re-packs one sheet's already-placed parts, in
//! place, using the same engine/config the main run used - no new placement
//! type, no directional bias. Distinct from `consolidation::refine_consolidation`,
//! which *relocates* parts between already-open sheets; this never touches
//! any other sheet, only ever rearranges a single sheet's own parts.
//!
//! **Why this can't reuse `ga::is_better_nest`/`consolidation::recompute_totals`
//! for its accept/reject decision**, even though both exist for exactly this
//! kind of "is candidate better than original" comparison elsewhere in this
//! crate: `PlaceResult::utilisation` is `total_placed_area / total_usable_sheet_area`
//! - for a *fixed* set of parts placed on a *fixed* sheet, that ratio is
//! identical no matter how those parts are arranged, since it only depends on
//! their combined area, never their positions. A tightly clustered layout and
//! a scattered-but-still-valid one of the exact same parts score identically
//! on utilisation, so a comparison built on it (like `is_better_nest`) can
//! never tell them apart - it would always see a tie and always keep the
//! original, making the whole repack pass a silent no-op. `PlaceResult::fitness`
//! doesn't have this problem: its last term folds in the final placed part's
//! Gravity/TightFit positioning score (effectively, the resulting cluster's
//! bounding box or contact area), which *does* vary with arrangement - see
//! `is_better_sheet` below.

use std::collections::HashMap;

use geometry::dxf_import::LayeredPolygon;

use crate::cache::NfpCache;
use crate::dispatch;
use crate::ga::{GaConfig, GeneticAlgorithm};
use crate::placement::{place_parts, NestPart, PlaceResult, PlacementConfig, SheetPlacement};

/// Same shape as `ga::is_better_nest` (unplaced count first, tie-broken by
/// a second metric), but with `fitness` (lower wins) standing in for
/// `utilisation` - see this module's doc comment for why utilisation can't
/// do this job for a same-part-set repack. A tie keeps `original` (`<`, not
/// `<=`), matching the "never reject the original for no reason" requirement.
fn is_better_sheet(candidate: &PlaceResult, original: &PlaceResult) -> bool {
    if candidate.unplaced_count != original.unplaced_count {
        return candidate.unplaced_count < original.unplaced_count;
    }
    candidate.fitness < original.fitness
}

/// Re-packs one sheet's current parts, in place, using the exact same
/// engine/config the main run used. Returns `Some(better)` only if strictly
/// better than `current` (`is_better_sheet`); `None` means "keep the
/// original" - a caller should leave `current` untouched in that case.
///
/// `should_cancel`/`seed`/`generations` behave the same as they do for a
/// normal `dispatch::run` call - this is a small, self-contained GA search
/// (fresh `NfpCache`, single sheet), not a hand-rolled variant of one.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn repack_sheet(
    sheet: &LayeredPolygon,
    current: &SheetPlacement,
    parts_by_id: &HashMap<usize, LayeredPolygon>,
    shape_ids: &HashMap<usize, usize>,
    ga_config: &GaConfig,
    placement_config: &PlacementConfig,
    generations: usize,
    seed: u64,
    should_cancel: &(impl Fn() -> bool + Sync),
) -> Option<SheetPlacement> {
    if current.parts.is_empty() {
        return None;
    }
    let sheets = std::slice::from_ref(sheet); // never spills to another sheet

    // Baseline: replay the sheet's current order/rotations through one
    // deterministic place_parts pass (no GA) to get an honest, directly
    // comparable `fitness` for the layout as it stands today -
    // `SheetPlacement` itself doesn't carry a fitness number. place_parts
    // has no RNG of its own, so the same order/rotations/sheet/config
    // reproduces the exact placement that originally happened here.
    let original_parts: Vec<NestPart> = current
        .parts
        .iter()
        .filter_map(|p| {
            parts_by_id.get(&p.id).map(|poly| NestPart {
                id: p.id,
                source_id: shape_ids.get(&p.id).copied().unwrap_or(p.id),
                polygon: poly.clone(),
                rotation: p.rotation,
            })
        })
        .collect();
    if original_parts.len() != current.parts.len() {
        return None; // a referenced part id is missing from parts_by_id - nothing safe to compare against
    }

    let baseline_cache = NfpCache::new();
    let original = place_parts(sheets, original_parts, placement_config, &baseline_cache, should_cancel, &|_, _| {}, &|_, _, _| {})?;
    if original.unplaced_count != 0 {
        return None; // shouldn't happen (these parts already fit today), but never trust the replay blindly
    }

    let adam: Vec<usize> = current.parts.iter().map(|p| p.id).collect();
    // Deliberately *not* warm-starting population[0]'s rotation genes from
    // current.parts' actual rotations, despite looking like an obvious
    // improvement (an earlier review flagged exactly this as a gap) -
    // measured it directly against this module's own "finds and applies a
    // strictly better arrangement" fixture and it's a real regression, not
    // a neutral change: seed 0 finds fitness 14850 starting from
    // GeneticAlgorithm::new's own random rotation roll, but gets stuck
    // exactly at the 14955 baseline when population[0] starts tied to it
    // instead. A population[0] identical to `original` gives mutation/
    // crossover a worse starting diversity than a random one for this
    // search, not a better one - keep the random init.
    let mut ga = GeneticAlgorithm::new(adam, ga_config.clone(), Vec::new(), seed);
    let candidate = dispatch::run(&mut ga, sheets, parts_by_id, shape_ids, placement_config, generations, should_cancel, &|_, _| {})?;

    if !is_better_sheet(&candidate, &original) {
        return None;
    }
    let mut winner = candidate.placements.into_iter().next()?;
    winner.sheet_index = current.sheet_index; // real identity, not the 1-elem-slice's own 0
    Some(winner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{Placement, PlacedPart, PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};
    use geometry::point::Point;

    fn square(size: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![Point::new(0.0, 0.0), Point::new(size, 0.0), Point::new(size, size), Point::new(0.0, size)],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    fn ga_config() -> GaConfig {
        GaConfig { population_size: 6, mutation_rate: 60.0, rotations: 1 }
    }

    fn placement_config() -> PlacementConfig {
        PlacementConfig {
            placement_type: PlacementType::Gravity,
            rotations: 1,
            dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
            curve_tolerance: 0.3,
        }
    }

    #[test]
    fn a_single_part_can_never_be_improved() {
        // Only one part, so no reordering exists that could change anything -
        // repack must recognize there's nothing better and keep the original.
        let sheet = square(100.0);
        let parts_by_id = HashMap::from([(0, square(10.0))]);
        let current = SheetPlacement { sheet_index: 0, parts: vec![PlacedPart { id: 0, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 }] };

        let result = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config(), &placement_config(), 20, 0, &|| false);

        assert!(result.is_none(), "a single-part sheet has no better arrangement to find");
    }

    #[test]
    fn empty_sheet_returns_none() {
        let sheet = square(100.0);
        let current = SheetPlacement { sheet_index: 3, parts: Vec::new() };
        let result = repack_sheet(&sheet, &current, &HashMap::new(), &HashMap::new(), &ga_config(), &placement_config(), 10, 0, &|| false);
        assert!(result.is_none());
    }

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

    #[test]
    fn finds_and_applies_a_strictly_better_arrangement() {
        // 4 differently-shaped rectangles on a sheet with just enough slack
        // that ordering genuinely changes how tightly they cluster (found by
        // sweeping placement types/orders/seeds against this exact fixture -
        // see git history for the sweep if this ever needs re-deriving).
        let sheet = square(120.0);
        let parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::from([(0, rect(70.0, 25.0)), (1, rect(50.0, 45.0)), (2, rect(30.0, 30.0)), (3, rect(20.0, 60.0))]);
        let current = SheetPlacement {
            sheet_index: 7,
            parts: [1, 3, 0, 2].iter().map(|&id| PlacedPart { id, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 }).collect(),
        };
        let mut cfg = placement_config();
        cfg.rotations = 2;
        let ga_config = GaConfig { population_size: 10, mutation_rate: 70.0, rotations: 2 };

        let winner = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config, &cfg, 80, 0, &|| false)
            .expect("this exact fixture/seed is known to find an improvement");

        assert_eq!(winner.sheet_index, 7, "must report the caller's real sheet index, not the internal 1-slice position of 0");
        assert_eq!(winner.parts.len(), 4, "repack must never drop a part");
        let mut ids: Vec<usize> = winner.parts.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 2, 3], "repack must never invent or duplicate a part id");
    }
}
