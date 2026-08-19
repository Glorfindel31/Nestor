//! Per-generation dispatch, replacing `main/deepnest.js`'s `launchWorkers`:
//! a 100ms-poll loop that IPC-dispatched one individual at a time to up to
//! `config.threads` separate Electron `BrowserWindow` worker processes,
//! tracking each one's `processing`/`fitness` state by hand to know when a
//! slot freed up.
//!
//! **Replaced by construction, not ported 1:1**: `rayon`'s parallel
//! iteration is synchronous - it blocks the calling thread until every item
//! finishes - so there's no polling loop, no `processing` flag, no manually
//! tracking how many workers are busy. One `par_iter()` call evaluates an
//! entire generation's population at once, capped by rayon's own thread
//! pool instead of a hand-rolled `running < config.threads` check. This is
//! what `docs/PORT_STATUS.md`'s Phase 4 row means by "eliminates the
//! ~7500-buffered-insert IPC flood by construction" - there's no IPC here
//! at all, every worker is a thread in the same process sharing memory.
//!
//! **Not ported here**: progress/log event plumbing (the original's
//! `eventEmitter.send(...)` calls), thread-count tuning against
//! `config.threads` (rayon's default global pool is used as-is - Phase 9's
//! "verify, don't assume" row is where that gets checked against real data,
//! not guessed at here), and `widenRotationsIfStalled`/`refineStalledBest`
//! (need `run`'s caller to track stagnation across calls - left to whatever
//! wraps this loop, same reasoning as the sheets `docs/PORT_STATUS.md` Phase
//! 4 row this superseded).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

use geometry::dxf_import::LayeredPolygon;

use crate::cache::NfpCache;
use crate::ga::{is_better_nest, GeneticAlgorithm};
use crate::placement::{place_parts, NestPart, PlaceResult, PlacementConfig};

/// Set in a part id to mean "the mirrored (flipped-over) copy of this part".
/// `src-tauri`'s `expand_parts` registers both variants in `parts_by_id`/
/// `shape_ids` under `id` and `id ^ MIRROR_ID_BIT`, so a mirrored gene needs
/// no separate geometry pipeline - it *is* a part, with its own shape
/// identity (and therefore its own NFP cache entries, which it must have:
/// the mirror of a shape has a different NFP against everything).
/// XOR, not addition, so decoding an already-mirrored id (a repack of an
/// existing nest re-feeds its own placement ids back through here) toggles
/// instead of running off the end of the map.
pub const MIRROR_ID_BIT: usize = 1 << 20;

/// Unpacks a rotation gene into `(part id, real angle)` - see
/// `ga::random_angle` for the encoding (`>= 360` means "mirrored").
#[must_use]
pub fn decode_rotation(id: usize, rotation: f64) -> (usize, f64) {
    if rotation >= 360.0 {
        (id ^ MIRROR_ID_BIT, rotation - 360.0)
    } else {
        (id, rotation)
    }
}

/// One individual's genes (`placement`/`rotation`, the same shape
/// `ga::Individual` carries) alongside the `PlaceResult` they produced -
/// `run_generation` returns these instead of bare `PlaceResult`s because
/// `GeneticAlgorithm::generation()` (called at the end of this same
/// function, once every individual has a fitness) replaces `ga.population`
/// with the *next* generation before returning, so a caller can't look a
/// gene back up from `ga.population` afterward. A caller that wants to
/// replay exactly how a particular result was built (e.g. re-running
/// `place_parts` a second time with tracing hooks live) needs the gene
/// captured here, at evaluation time, or not at all.
#[derive(Clone, Debug)]
pub struct EvaluatedIndividual {
    pub placement: Vec<usize>,
    pub rotation: Vec<f64>,
    pub result: PlaceResult,
}

/// Evaluates every individual in the current generation's population in
/// parallel, assigns each one's `fitness`, then advances the GA
/// (`GeneticAlgorithm::generation()`) - unless `should_cancel` cut this
/// generation short, see below. Returns the `PlaceResult` for every
/// individual actually (re-)placed this call - useful for a caller that
/// wants to track the best-so-far result, not just the best fitness (see
/// `is_better_nest`'s doc comment for why those can rank differently).
///
/// Individuals that already carry a `fitness` are skipped, not
/// re-evaluated. The only way that happens is `GeneticAlgorithm::generation`'s
/// elitism (`new_population = vec![self.population[0].clone()]`), which
/// carries the surviving elite's already-correct fitness forward unchanged
/// - its genes didn't change, so its placement result didn't either.
/// Matches the original (`launchWorkers`'s dispatch loop skips any
/// individual whose `.fitness` is already truthy) and skips a real,
/// otherwise-redundant placement computation every generation.
///
/// `should_cancel` is checked once per individual, right before that
/// individual's own placement work starts - not while one is already
/// running; there's no way to abort a `place_parts` call already in flight
/// without restructuring the placement engine itself. Once it returns
/// true, every individual not yet started returns early instead of running
/// a real placement. For a population bigger than the thread pool's
/// parallelism (the common case), that means only the currently in-flight
/// batch - bounded by thread count, not population size - still has to
/// finish, not the whole generation. This is what makes a caller's
/// per-generation cancel check (checking `should_cancel` again between
/// calls to this function) actually responsive instead of only ever
/// taking effect at generation boundaries.
///
/// A cancelled call leaves some individuals without a fitness, so
/// `GeneticAlgorithm::generation()` - which requires every individual's
/// fitness to already be set - is skipped rather than called on a
/// half-evaluated population (it would panic). The caller is expected to
/// stop calling `run_generation` once it observes cancellation, not to
/// treat a cut-short call as a normal completed generation.
///
/// `on_individual_placed(done, total)` is called once up front with
/// `(0, total)` before any placement starts, then again after each
/// individual's `place_parts` call returns - `total` is how many
/// individuals this call will actually evaluate (population size minus any
/// already-fitness'd elite). A single individual's placement is real,
/// possibly tens-of-seconds work against non-trivial real geometry, and
/// without this a caller has no signal at all between "generation started"
/// and "generation finished", which reads as the whole run having stalled
/// even though it's working the entire time.
///
/// `cache` should be the same `NfpCache` for the whole run (every
/// individual, every generation) - see `place_parts`'s own doc comment for
/// why. Every individual in a generation shares the one cache (behind its
/// own internal `Mutex`, safe across `par_iter`'s parallel threads).
#[must_use]
pub fn run_generation(
    ga: &mut GeneticAlgorithm,
    sheets: &[LayeredPolygon],
    parts_by_id: &HashMap<usize, LayeredPolygon>,
    shape_ids: &HashMap<usize, usize>,
    placement_config: &PlacementConfig,
    should_cancel: &(impl Fn() -> bool + Sync),
    on_individual_placed: &(impl Fn(usize, usize) + Sync),
    cache: &NfpCache,
) -> Vec<EvaluatedIndividual> {
    let total = ga.population.iter().filter(|ind| ind.fitness.is_none()).count();
    on_individual_placed(0, total);
    let completed = AtomicUsize::new(0);

    let evaluated: Vec<(usize, EvaluatedIndividual)> = ga
        .population
        .par_iter()
        .enumerate()
        .filter(|(_, individual)| individual.fitness.is_none())
        .filter_map(|(idx, individual)| {
            if should_cancel() {
                return None;
            }
            let nest_parts: Vec<NestPart> = individual
                .placement
                .iter()
                .zip(&individual.rotation)
                .map(|(&gene_id, &gene_rotation)| {
                    let (id, rotation) = decode_rotation(gene_id, gene_rotation);
                    // A caller's `parts_by_id` doesn't have to cover the
                    // mirrored variants (`repack_sheet` passes one sheet's
                    // parts, whatever the config says) - fall back to the
                    // un-flipped part at the same angle instead of panicking
                    // on a variant nobody registered.
                    let id = if parts_by_id.contains_key(&id) { id } else { gene_id };
                    NestPart {
                        id,
                        source_id: shape_ids.get(&id).copied().unwrap_or(id),
                        polygon: parts_by_id.get(&id).expect("every gene id must have a matching part").clone(),
                        rotation,
                    }
                })
                .collect();
            // `?`: `place_parts` returns `None` if `should_cancel` fired
            // partway through its own work - same "not evaluated" outcome
            // as the check above, just discovered mid-placement instead of
            // before starting. This is what makes cancellation a real kill
            // switch rather than only ever skipping individuals that
            // hadn't started yet: `place_parts` itself bails out of its
            // per-part loop within a fraction of a second of the flag
            // flipping, instead of running to completion regardless.
            let result = place_parts(sheets, nest_parts, placement_config, cache, should_cancel, &|_, _| {}, &|_, _, _| {})?;
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            on_individual_placed(done, total);
            Some((idx, EvaluatedIndividual { placement: individual.placement.clone(), rotation: individual.rotation.clone(), result }))
        })
        .collect();

    for (idx, evaluated) in &evaluated {
        ga.population[*idx].fitness = Some(evaluated.result.fitness);
    }

    let results: Vec<EvaluatedIndividual> = evaluated.into_iter().map(|(_, evaluated)| evaluated).collect();
    if ga.population.iter().all(|ind| ind.fitness.is_some()) {
        ga.generation();
    }
    results
}

/// Runs `generations` full generations, tracking the best result seen
/// across all of them via `is_better_nest` (not raw GA fitness, which
/// bundles in placement-strategy shape penalties that don't map to "which
/// result would I actually want"). `None` only if `generations == 0` (or a
/// cancellation lands before generation 1 produces anything).
///
/// Takes `should_cancel`/`on_individual_placed` the same way
/// `run_generation` does (forwarded straight through, plus checked once
/// more here between generations) - a caller reaching for this top-level
/// entry point instead of hand-rolling the generation loop `run_generation`
/// leaves to its wrapper still gets real cancellation responsiveness, not
/// a silently-hardcoded `|| false` that discards it. Pass `&|| false` and
/// `&|_, _| {}` explicitly if a caller genuinely doesn't want either.
#[must_use]
pub fn run(
    ga: &mut GeneticAlgorithm,
    sheets: &[LayeredPolygon],
    parts_by_id: &HashMap<usize, LayeredPolygon>,
    shape_ids: &HashMap<usize, usize>,
    placement_config: &PlacementConfig,
    generations: usize,
    should_cancel: &(impl Fn() -> bool + Sync),
    on_individual_placed: &(impl Fn(usize, usize) + Sync),
) -> Option<PlaceResult> {
    // Fresh cache for this call - `run` (unlike `run_generation`) is a
    // whole self-contained nest attempt, so there's no longer-lived run for
    // it to share a cache with (matches `place_parts`'s own doc comment:
    // one cache per whole run, not per generation).
    let cache = NfpCache::new();
    let mut best: Option<PlaceResult> = None;
    for _ in 0..generations {
        if should_cancel() {
            break;
        }
        let results = run_generation(ga, sheets, parts_by_id, shape_ids, placement_config, should_cancel, on_individual_placed, &cache);
        for evaluated in results {
            if best.as_ref().is_none_or(|b| is_better_nest(&evaluated.result, b)) {
                best = Some(evaluated.result);
            }
        }
        // `run_generation` may have been cut short mid-population by the
        // same flag - stop here rather than starting another generation on
        // a population it deliberately left half-evaluated.
        if should_cancel() {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ga::GaConfig;
    use crate::placement::{PlacementType, DEFAULT_DOMINANT_PART_AREA_THRESHOLD};
    use geometry::point::Point;

    fn square(size: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(size, 0.0),
                Point::new(size, size),
                Point::new(0.0, size),
            ],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    fn placement_config() -> PlacementConfig {
        PlacementConfig {
            placement_type: PlacementType::Gravity,
            rotations: 1,
            dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
            curve_tolerance: 0.3,
        }
    }

    fn setup(part_count: usize) -> (GeneticAlgorithm, Vec<LayeredPolygon>, HashMap<usize, LayeredPolygon>) {
        let parts_by_id: HashMap<usize, LayeredPolygon> = (0..part_count).map(|id| (id, square(10.0))).collect();
        let adam: Vec<usize> = (0..part_count).collect();
        let ga = GeneticAlgorithm::new(adam, GaConfig { population_size: 6, mutation_rate: 20.0, rotations: 1, mirror: false }, Vec::new(), 0);
        let sheets = vec![square(100.0)];
        (ga, sheets, parts_by_id)
    }

    #[test]
    fn run_generation_evaluates_new_individuals_and_skips_the_carried_over_elite() {
        let (mut ga, sheets, parts_by_id) = setup(4);
        let cfg = placement_config();

        // first call: every individual is freshly constructed with no
        // fitness yet, so all of them get placed.
        let cache = NfpCache::new();
        let results = run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, &|| false, &|_, _| {}, &cache);
        assert_eq!(results.len(), 6);
        for evaluated in &results {
            assert_eq!(evaluated.result.unplaced_count, 0, "4 small squares should all fit on one 100x100 sheet");
        }

        // generation() already replaced ga.population with the next
        // generation's population by the time this returns - same size.
        // population[0] is the surviving elite (a literal clone of the
        // fittest individual just evaluated), so it carries its fitness
        // forward unchanged; every other slot is a freshly mated/mutated
        // child with no fitness yet, matching the original (a newly bred
        // individual has no fitness until it's placed).
        assert_eq!(ga.population.len(), 6);
        assert!(ga.population[0].fitness.is_some(), "elite survivor should carry its fitness forward");
        for individual in &ga.population[1..] {
            assert!(individual.fitness.is_none());
        }

        // second call: the carried-over elite must NOT be re-placed - only
        // population_size - 1 fresh results this time.
        let results2 = run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, &|| false, &|_, _| {}, &cache);
        assert_eq!(results2.len(), 5, "the elite survivor should be skipped, not re-placed");
    }

    #[test]
    fn run_generation_stops_early_and_skips_advancing_the_ga_when_cancelled() {
        let (mut ga, sheets, parts_by_id) = setup(4);
        let cfg = placement_config();

        let population_before = ga.population.clone();
        let results = run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, &|| true, &|_, _| {}, &NfpCache::new());

        assert!(results.is_empty(), "cancelling before any individual starts should place none of them");
        assert!(
            ga.population.iter().zip(&population_before).all(|(a, b)| a.placement == b.placement),
            "generation() must not have run - the population should be untouched"
        );
        for individual in &ga.population {
            assert!(individual.fitness.is_none(), "no individual should have been evaluated");
        }
    }

    #[test]
    fn run_generation_reports_progress_once_up_front_and_once_per_individual() {
        let (mut ga, sheets, parts_by_id) = setup(4);
        let cfg = placement_config();

        let ticks = std::sync::Mutex::new(Vec::new());
        let results = run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, &|| false, &|done, total| {
            ticks.lock().unwrap().push((done, total));
        }, &NfpCache::new());

        let ticks = ticks.into_inner().unwrap();
        // one upfront (0, total) call before any placement, then one per
        // individual actually placed - 6 individuals fresh (population_size
        // from setup's GaConfig), so 7 calls total.
        assert_eq!(ticks.len(), 1 + results.len());
        assert_eq!(ticks[0], (0, results.len()));
        // done counts strictly increase 1..=total across the remaining calls
        // (order isn't guaranteed across parallel individuals, but the
        // shared counter is atomic, so every value 1..=total appears exactly
        // once).
        let mut dones: Vec<usize> = ticks[1..].iter().map(|&(done, _)| done).collect();
        dones.sort_unstable();
        assert_eq!(dones, (1..=results.len()).collect::<Vec<_>>());
    }

    #[test]
    fn run_tracks_the_best_result_across_multiple_generations() {
        let (mut ga, sheets, parts_by_id) = setup(5);
        let cfg = placement_config();

        let best = run(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, 3, &|| false, &|_, _| {}).expect("3 generations should produce a best result");

        assert_eq!(best.unplaced_count, 0);
        assert!(best.fitness.is_finite());
    }

    #[test]
    fn run_returns_none_for_zero_generations() {
        let (mut ga, sheets, parts_by_id) = setup(3);
        let cfg = placement_config();
        assert!(run(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, 0, &|| false, &|_, _| {}).is_none());
    }

    /// Regression test: `run()` used to hardcode `&|| false` internally, so
    /// a caller reaching for this top-level API instead of hand-rolling
    /// `run_generation`'s own loop (as `src-tauri`'s `run_nest_with_progress`
    /// does) silently got no cancellation at all - the exact bug class
    /// commit `7c6334b` ("Fix UI freeze on long nest runs") already fixed
    /// once, reintroduced by this one function.
    #[test]
    fn run_actually_stops_when_should_cancel_fires() {
        let (mut ga, sheets, parts_by_id) = setup(5);
        let cfg = placement_config();

        // Cancel immediately - if `run()` ignored this (as it used to),
        // `generations: 1000` would still run to completion.
        let best = run(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &cfg, 1000, &|| true, &|_, _| {});

        assert!(best.is_none(), "an immediate cancel should stop run() before any generation produces a result");
    }

    /// `config.mirror` is per-run, but `parts_by_id` isn't guaranteed to
    /// carry the mirrored variants (a single-sheet repack passes only that
    /// sheet's parts) - a mirrored gene must then degrade to the plain part,
    /// not panic on a missing id.
    #[test]
    fn a_mirrored_gene_falls_back_when_its_variant_is_not_registered() {
        let (_, sheets, parts_by_id) = setup(3);
        let adam: Vec<usize> = (0..3).collect();
        let mut ga = GeneticAlgorithm::new(adam, GaConfig { population_size: 6, mutation_rate: 20.0, rotations: 1, mirror: true }, Vec::new(), 0);
        let results = run_generation(&mut ga, &sheets, &parts_by_id, &HashMap::new(), &placement_config(), &|| false, &|_, _| {}, &NfpCache::new());
        assert!(!results.is_empty());
        for r in &results {
            assert!(r.rotation.iter().any(|&a| a >= 360.0), "the mirror-widened grid must actually produce mirrored genes");
            for sheet in &r.result.placements {
                for p in &sheet.parts {
                    assert!(parts_by_id.contains_key(&p.id), "placed id must exist in the map it came from");
                }
            }
        }
    }
}
