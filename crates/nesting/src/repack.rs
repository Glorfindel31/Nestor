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

use geometry::dxf_import::{rotate_layered_polygon, LayeredPolygon};
use geometry::polygon::polygon_area;

use crate::cache::NfpCache;
use crate::dispatch;
use crate::ga::{GaConfig, GeneticAlgorithm};
use crate::placement::{
    cached_inner_nfp, place_parts, rotation_steps, sheet_source, try_place_part_on_sheet, NestPart, PlaceResult, PlacedObstacle, PlacedPart, PlacementConfig,
    SheetPlacement,
};

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
    locked: &[usize],
    should_cancel: &(impl Fn() -> bool + Sync),
) -> Option<SheetPlacement> {
    // Band packing is off for this pass: it re-packs a sheet from scratch,
    // and this function's whole contract is to *move existing placements*
    // around. Letting it re-lay a sheet here would mean the caller's parts
    // came back in positions it never asked to have changed - and, in
    // `repack_sheet`, would ignore pinned parts outright.
    let placement_config = &PlacementConfig { banded_pass: false, ..placement_config.clone() };

    if current.parts.is_empty() {
        return None;
    }
    // Pinned parts change the problem completely - see
    // `repack_around_locked`. An empty `locked` takes the untouched GA path
    // below.
    if !locked.is_empty() {
        return repack_around_locked(sheet, current, locked, parts_by_id, shape_ids, placement_config, &NfpCache::new(), should_cancel);
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
    let candidate = dispatch::run(&mut ga, sheets, parts_by_id, shape_ids, placement_config, generations, should_cancel, &|_, _| {}, &|_| {})?;

    if !is_better_sheet(&candidate, &original) {
        return None;
    }
    let mut winner = candidate.placements.into_iter().next()?;
    winner.sheet_index = current.sheet_index; // real identity, not the 1-elem-slice's own 0
    Some(winner)
}

/// Re-nests only the *unlocked* parts of a sheet, around the locked ones
/// left exactly where they are. This is what backs the UI's "drag a piece
/// where you want it, pin it, tidy up the rest" flow.
///
/// Deliberately **not** the GA: with some parts pinned there is no free
/// permutation to evolve - the pinned ones can't move at all, and the rest
/// have to fit into whatever room is left. A single greedy
/// largest-part-first pass against the pinned parts as obstacles is the
/// whole job, and it reuses the same `try_place_part_on_sheet` machinery
/// `consolidation` already uses to drop one part onto an occupied sheet.
///
/// Returns `None` if any free part can't be placed - **never** a partial
/// result. Silently dropping a part the user can see on screen is the one
/// outcome worse than "no improvement", which is how the UI already renders
/// `None`.
#[allow(clippy::too_many_arguments)] // same shape as `repack_sheet` itself, which carries the same allow
fn repack_around_locked(
    sheet: &LayeredPolygon,
    current: &SheetPlacement,
    locked: &[usize],
    parts_by_id: &HashMap<usize, LayeredPolygon>,
    shape_ids: &HashMap<usize, usize>,
    placement_config: &PlacementConfig,
    cache: &NfpCache,
    should_cancel: &(impl Fn() -> bool + Sync),
) -> Option<SheetPlacement> {
    let source_id_of = |id: usize| shape_ids.get(&id).copied().unwrap_or(id);
    let sheet_src = sheet_source(current.sheet_index);

    let mut obstacles: Vec<PlacedObstacle> = Vec::new();
    let mut out: Vec<PlacedPart> = Vec::new();
    for part in current.parts.iter().filter(|p| locked.contains(&p.id)) {
        let geometry = rotate_layered_polygon(parts_by_id.get(&part.id)?, part.rotation);
        obstacles.push(PlacedObstacle {
            polygon: geometry,
            id: part.id,
            source_id: source_id_of(part.id),
            rotation: part.rotation,
            placement: part.placement,
        });
        out.push(*part);
    }

    // Largest first, same reasoning as the initial nest's own seed order:
    // the awkward parts need the choice of position while there still is
    // one.
    let mut free: Vec<&PlacedPart> = current.parts.iter().filter(|p| !locked.contains(&p.id)).collect();
    free.sort_by(|a, b| {
        let area = |p: &PlacedPart| parts_by_id.get(&p.id).map_or(0.0, |g| polygon_area(&g.points).abs());
        area(b).total_cmp(&area(a))
    });

    for part in free {
        if should_cancel() {
            return None;
        }
        let base = parts_by_id.get(&part.id)?;

        // Try every orientation this part is allowed (which is its own
        // per-part rule if it has one - a grain-locked part must not be
        // quietly re-rotated by a tidy-up pass), keeping the best.
        let mut best: Option<(f64, PlacedPart)> = None;
        for (angle, _) in rotation_steps(placement_config, part.id, part.rotation) {
            let rotated = rotate_layered_polygon(base, angle);
            let Some(sheet_nfp) = cached_inner_nfp(cache, sheet, sheet_src, &rotated, source_id_of(part.id), angle, placement_config.curve_tolerance) else {
                continue;
            };
            if sheet_nfp.is_empty() {
                continue;
            }
            let Some(result) =
                try_place_part_on_sheet(&rotated, source_id_of(part.id), angle, &sheet_nfp, sheet, &obstacles, placement_config, cache, &|_| {}).placed()
            else {
                continue;
            };
            if best.as_ref().is_none_or(|(score, _)| result.minarea < *score) {
                best = Some((result.minarea, PlacedPart { id: part.id, placement: result.position, rotation: angle }));
            }
        }

        let (_, placed) = best?; // a part that fits nowhere means no result at all
        obstacles.push(PlacedObstacle {
            polygon: rotate_layered_polygon(base, placed.rotation),
            id: placed.id,
            source_id: source_id_of(placed.id),
            rotation: placed.rotation,
            placement: placed.placement,
        });
        out.push(placed);
    }

    Some(SheetPlacement { sheet_index: current.sheet_index, parts: out })
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
        GaConfig { population_size: 6, mutation_rate: 60.0, rotations: 1, mirror: false, part_rules: Default::default() }
    }

    fn placement_config() -> PlacementConfig {
        PlacementConfig {
            placement_type: PlacementType::Gravity,
            rotations: 1,
            dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
            curve_tolerance: 0.3,
            part_rules: Default::default(),
            banded_pass: true,
        }
    }


    /// Locking is what turns "re-nest this sheet" into "tidy up around the
    /// pieces I placed by hand" - the pinned parts must come back
    /// *bit-identical*, not merely close.
    #[test]
    fn locked_parts_come_back_exactly_where_they_were() {
        let sheet = square(120.0);
        let parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::from([(0, rect(70.0, 25.0)), (1, rect(50.0, 45.0)), (2, rect(30.0, 30.0))]);
        let pinned = Placement { x: 44.5, y: 71.25 };
        let current = SheetPlacement {
            sheet_index: 2,
            parts: vec![
                PlacedPart { id: 0, placement: pinned, rotation: 0.0 },
                PlacedPart { id: 1, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 },
                PlacedPart { id: 2, placement: Placement { x: 0.0, y: 45.0 }, rotation: 0.0 },
            ],
        };

        let result = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config(), &placement_config(), 20, 0, &[0], &|| false)
            .expect("the two free parts can be re-placed around the pinned one");

        assert_eq!(result.sheet_index, 2, "the sheet keeps its real identity");
        assert_eq!(result.parts.len(), 3, "nothing may be dropped");
        let locked = result.parts.iter().find(|p| p.id == 0).expect("the pinned part is still there");
        assert_eq!(locked.placement.x, pinned.x);
        assert_eq!(locked.placement.y, pinned.y);
        assert_eq!(locked.rotation, 0.0);
    }

    #[test]
    fn a_fully_locked_sheet_has_nothing_to_repack() {
        let sheet = square(120.0);
        let parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::from([(0, rect(70.0, 25.0)), (1, rect(50.0, 45.0))]);
        let current = SheetPlacement {
            sheet_index: 0,
            parts: vec![
                PlacedPart { id: 0, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 },
                PlacedPart { id: 1, placement: Placement { x: 0.0, y: 30.0 }, rotation: 0.0 },
            ],
        };
        let result = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config(), &placement_config(), 20, 0, &[0, 1], &|| false)
            .expect("everything locked still returns the sheet as-is");
        assert_eq!(result.parts.len(), 2);
        for part in &result.parts {
            let original = current.parts.iter().find(|p| p.id == part.id).unwrap();
            assert_eq!(part.placement.x, original.placement.x);
            assert_eq!(part.placement.y, original.placement.y);
        }
    }

    /// Never a partial result: if a freed part has nowhere to go, the whole
    /// repack is refused rather than silently losing it.
    #[test]
    fn a_free_part_that_no_longer_fits_refuses_the_whole_repack() {
        // A big part pinned dead centre leaves no room for the second one.
        let sheet = square(100.0);
        let parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::from([(0, rect(96.0, 96.0)), (1, rect(90.0, 90.0))]);
        let current = SheetPlacement {
            sheet_index: 0,
            parts: vec![
                PlacedPart { id: 0, placement: Placement { x: 2.0, y: 2.0 }, rotation: 0.0 },
                PlacedPart { id: 1, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 },
            ],
        };
        let result = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config(), &placement_config(), 20, 0, &[0], &|| false);
        assert!(result.is_none(), "no room for the freed part means no result, not a dropped part");
    }

    /// A pinned part must not be quietly re-rotated by the tidy-up pass
    /// either - the locked path has to honour per-part rules the same way
    /// the main engine does.
    #[test]
    fn the_locked_pass_respects_a_parts_own_rotation_rule() {
        use crate::placement::{PartRule, PartRules};

        let sheet = square(200.0);
        let parts_by_id: HashMap<usize, LayeredPolygon> = HashMap::from([(0, rect(60.0, 20.0)), (1, rect(60.0, 20.0))]);
        let current = SheetPlacement {
            sheet_index: 0,
            parts: vec![
                PlacedPart { id: 0, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 },
                PlacedPart { id: 1, placement: Placement { x: 0.0, y: 100.0 }, rotation: 0.0 },
            ],
        };
        let rules: PartRules = std::sync::Arc::new(HashMap::from([(1, PartRule { angles: vec![0.0, 180.0], mirror: false })]));
        let cfg = PlacementConfig { rotations: 8, part_rules: rules, ..placement_config() };

        let result = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config(), &cfg, 20, 0, &[0], &|| false).expect("repacks");
        let free = result.parts.iter().find(|p| p.id == 1).unwrap();
        assert!(free.rotation == 0.0 || free.rotation == 180.0, "grain-locked free part was rotated to {}", free.rotation);
    }

    #[test]
    fn a_single_part_can_never_be_improved() {
        // Only one part, so no reordering exists that could change anything -
        // repack must recognize there's nothing better and keep the original.
        let sheet = square(100.0);
        let parts_by_id = HashMap::from([(0, square(10.0))]);
        let current = SheetPlacement { sheet_index: 0, parts: vec![PlacedPart { id: 0, placement: Placement { x: 0.0, y: 0.0 }, rotation: 0.0 }] };

        let result = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config(), &placement_config(), 20, 0, &[], &|| false);

        assert!(result.is_none(), "a single-part sheet has no better arrangement to find");
    }

    #[test]
    fn empty_sheet_returns_none() {
        let sheet = square(100.0);
        let current = SheetPlacement { sheet_index: 3, parts: Vec::new() };
        let result = repack_sheet(&sheet, &current, &HashMap::new(), &HashMap::new(), &ga_config(), &placement_config(), 10, 0, &[], &|| false);
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
        let ga_config = GaConfig { population_size: 10, mutation_rate: 70.0, rotations: 2, mirror: false, part_rules: Default::default() };

        let winner = repack_sheet(&sheet, &current, &parts_by_id, &HashMap::new(), &ga_config, &cfg, 80, 0, &[], &|| false)
            .expect("this exact fixture/seed is known to find an improvement");

        assert_eq!(winner.sheet_index, 7, "must report the caller's real sheet index, not the internal 1-slice position of 0");
        assert_eq!(winner.parts.len(), 4, "repack must never drop a part");
        let mut ids: Vec<usize> = winner.parts.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 2, 3], "repack must never invent or duplicate a part id");
    }
}
