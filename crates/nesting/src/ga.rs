//! Port of `main/deepnest.js`'s `GeneticAlgorithm` class and `isBetterNest`.
//! Pure algorithm - no dispatch/threading here (that's `nesting::dispatch`,
//! not yet built); `generation()` just needs every individual's `fitness`
//! already populated by whatever ran placement for this generation.
//!
//! **A gene is a part id (`usize`), not a part object.** The original's
//! `individual.placement` is an array of references to the actual part
//! objects (shared, not deep-copied, by `.slice(0)`); `mate`'s crossover
//! containment check (`contains(gene, id)`) only ever looks at `.id`
//! anyway. Since part geometry (`geometry::dxf_import::LayeredPolygon`) is
//! comparatively expensive to clone (points plus recursive hole children),
//! and the GA reorders/mutates/mates potentially hundreds of individuals a
//! generation, carrying ids instead of geometry keeps every GA operation a
//! cheap `Vec<usize>` shuffle - real part lookup by id is the caller's job
//! (`nesting::placement::NestPart`), same separation of concerns as the
//! original had via object reference vs. `.id`, just made explicit.
//!
//! **`widenRotationsIfStalled` is now ported, but split across two places**:
//! this module only exposes `set_rotations` (widening what future mutations
//! can draw from); the stagnation counter that decides *when* to call it
//! lives in `app/src/commands.rs`'s generation loop, the caller that
//! actually persists across many `dispatch::run_generation` calls (this
//! module and `nesting::dispatch` both only ever see one generation at a
//! time). **`refineStalledBest` is still not ported** - it re-runs
//! consolidation against a stalled run's current champion, which needs the
//! same generation-loop-level caller `widenRotationsIfStalled` just gained,
//! but doing so hasn't been requested yet.

use std::cell::Cell;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::placement::{PartRules, PlaceResult};

/// `background.js`'s `isBetterNest`: ranks nest results for display - fewer
/// unplaced parts first (a trial that leaves parts out is never "better"
/// just for using fewer sheets or packing tighter), then fewer sheets, then
/// higher utilisation. Deliberately not the raw GA fitness score, which
/// bundles in things (gravity/box/hull shape penalties, edge-merge savings)
/// that don't map to "which result would I actually want to export".
/// Simpler than the original: `PlaceResult`'s fields are always concrete
/// (`usize`/`Vec`/`f64`), so there's no `a.unplacedCount || 0`-style
/// undefined-coercion to replicate.
#[must_use]
pub fn is_better_nest(a: &PlaceResult, b: &PlaceResult) -> bool {
    if a.unplaced_count != b.unplaced_count {
        return a.unplaced_count < b.unplaced_count;
    }
    let a_sheets = a.placements.len();
    let b_sheets = b.placements.len();
    if a_sheets != b_sheets {
        return a_sheets < b_sheets;
    }
    a.utilisation > b.utilisation
}

#[derive(Clone, Debug)]
pub struct GaConfig {
    pub population_size: usize,
    /// Percentage, 0-100 (matches the original's `config.mutationRate`
    /// convention - used as `0.01 * mutation_rate` throughout).
    pub mutation_rate: f64,
    pub rotations: u32,
    /// Let the search also try each part flipped over (mirrored). Doubles
    /// the angle grid a rotation gene draws from - see `random_angle` for
    /// the encoding and `nesting::dispatch` for where a mirrored gene turns
    /// into mirrored geometry.
    pub mirror: bool,
    /// Per-part orientation constraints - the same map
    /// `PlacementConfig::part_rules` carries, so a gene and the placement it
    /// produces can never disagree about what a part is allowed to do. Empty
    /// (the default) means every part draws from the global grid, exactly as
    /// before this field existed.
    pub part_rules: PartRules,
}

/// One individual: a permutation of part ids (`placement`) and a parallel
/// per-part rotation angle (`rotation`). `fitness` starts unset - the
/// (future) dispatch loop runs placement for this individual's
/// placement/rotation genes and fills it in; `generation()` requires it.
#[derive(Clone, Debug)]
pub struct Individual {
    pub placement: Vec<usize>,
    pub rotation: Vec<f64>,
    pub fitness: Option<f64>,
}

fn random_angles(ids: &[usize], rotations: u32, mirror: bool, rules: &PartRules, rng: &mut impl Rng) -> Vec<f64> {
    ids.iter().map(|&id| random_angle(id, rotations, mirror, rules, rng)).collect()
}

/// One rotation gene. With `mirror` off this is the plain `k * 360/rotations`
/// grid; with it on the grid is drawn twice as long, so an angle of `360` or
/// more means "the same angle, but on the flipped-over part" (`angle - 360`).
///
/// ponytail: folding the mirror flag into the angle instead of adding a
/// parallel `Vec<bool>` gene keeps every crossover/mutation/elitism path in
/// this module (and `EvaluatedIndividual`, and every caller that replays a
/// gene) untouched - one gene vector still carries the whole per-part
/// decision. `nesting::dispatch::decode_rotation` is the single place that
/// unpacks it.
///
/// `rotations.max(1)`: `rotations: 0` would make `rng.gen_range(0..0)` panic
/// (an empty range) - `PlacementConfig`'s own use of `rotations` already
/// guards this exact case the same way (`placement.rs`'s `config.rotations.max(1)`
/// calls); `GaConfig` needs the same floor, since this is `pub` API reachable
/// without going through `app`'s own `rotations == 0` request
/// validation (a test, a future caller, `nesting`'s own bench harness).
fn random_angle(id: usize, rotations: u32, mirror: bool, rules: &PartRules, rng: &mut impl Rng) -> f64 {
    let rule = rules.get(&(id & !crate::dispatch::MIRROR_ID_BIT));
    let mirror = rule.map_or(mirror, |r| r.mirror);
    match rule.filter(|r| !r.angles.is_empty()) {
        // Unconstrained: one `gen_range` over the same slot count as before,
        // so an unconstrained run draws the identical random sequence it
        // always did.
        None => {
            let rotations = rotations.max(1);
            let slots = if mirror { rotations * 2 } else { rotations };
            (rng.gen_range(0..slots) as f64) * (360.0 / rotations as f64)
        }
        // Constrained: draw from the allowed set instead of the grid, with
        // the same "upper half means mirrored" encoding on top, so
        // `dispatch::decode_rotation` needs no idea any of this exists.
        Some(rule) => {
            let n = rule.angles.len();
            let slot = rng.gen_range(0..if mirror { n * 2 } else { n });
            rule.angles[slot % n] + if slot >= n { 360.0 } else { 0.0 }
        }
    }
}

#[derive(Debug)]
pub struct GeneticAlgorithm {
    pub population: Vec<Individual>,
    config: GaConfig,
    // `Cell`, not a plain field, deliberately: it lets every RNG-consuming
    // method below keep taking `&self` (unchanged signatures, no call-site
    // churn) while still advancing a real, continuing random sequence each
    // call - see `next_rng`'s own doc comment for why a `&mut self` design
    // (the more obvious choice) doesn't actually work here.
    next_seed: Cell<u64>,
}

impl GeneticAlgorithm {
    /// `adam` is the initial (e.g. decreasing-area-sorted) part-id order;
    /// stays unmutated as `population[0]`. `extra_seeds` are alternate
    /// starting orders (same alternate-sort-order idea the original uses) -
    /// any whose length doesn't match `adam`'s is dropped, same as the
    /// original's `.filter(o => o && o.length === adam.length)`.
    ///
    /// `seed` makes the *entire* run's randomness (initial rotation angles,
    /// every `mutate`/`mate`/parent-selection roll across every generation)
    /// fully reproducible: the same `seed` with the same `adam`/`config`/
    /// `extra_seeds` always produces the exact same sequence of individuals.
    /// Deliberately not left to `rand::thread_rng()` (OS entropy, different
    /// every process run) - comparing placement strategies needs to isolate
    /// "did this scoring change actually help" from "did this run just
    /// happen to get a luckier random population," which thread_rng's
    /// unrepeatable seed made impossible to tell apart.
    pub fn new(adam: Vec<usize>, config: GaConfig, extra_seeds: Vec<Vec<usize>>, seed: u64) -> Self {
        let adam_len = adam.len();
        let mut ga = GeneticAlgorithm { population: Vec::new(), config, next_seed: Cell::new(seed) };

        let mut rng = ga.next_rng();
        ga.population.push(Individual {
            rotation: random_angles(&adam, ga.config.rotations, ga.config.mirror, &ga.config.part_rules, &mut rng),
            placement: adam,
            fitness: None,
        });

        for seed in extra_seeds.into_iter().filter(|s| s.len() == adam_len) {
            if ga.population.len() >= ga.config.population_size {
                break;
            }
            let mut rng = ga.next_rng();
            ga.population.push(Individual {
                rotation: random_angles(&seed, ga.config.rotations, ga.config.mirror, &ga.config.part_rules, &mut rng),
                placement: seed,
                fitness: None,
            });
        }

        while ga.population.len() < ga.config.population_size {
            let mutant = ga.mutate(&ga.population[0]);
            ga.population.push(mutant);
        }
        ga
    }

    /// Hands out a fresh, deterministically-seeded RNG and advances the
    /// counter it draws from - every call in one `GeneticAlgorithm`'s
    /// lifetime gets a *different* seed (so calls don't all repeat the same
    /// first-few-draws), but the whole sequence is 100% reproducible given
    /// the constructor's own `seed`. `&self`, not `&mut self` (via `Cell`'s
    /// interior mutability): the obvious alternative - storing one `StdRng`
    /// field directly and taking `&mut self` in `mutate`/`mate`/
    /// `random_weighted_individual` - breaks every existing call site
    /// shaped like `ga.mutate(&ga.population[0])` (an immutable borrow of
    /// `ga.population` alongside a mutable borrow of `ga` in the same
    /// expression), which is exactly how `generation()` and this
    /// constructor both already call it.
    fn next_rng(&self) -> StdRng {
        let seed = self.next_seed.get();
        self.next_seed.set(seed.wrapping_add(1));
        StdRng::seed_from_u64(seed)
    }

    /// Port of half of `widenRotationsIfStalled`'s effect: widens the
    /// rotation grid `mutate`'s rotation-reroll draws from for every future
    /// mutation, without touching any already-existing individual's current
    /// rotation genes (same as the original - widening only ever changes
    /// what *future* mutations can pick, not what's already in the
    /// population). The stagnation tracking itself (counting generations
    /// since the last improvement, deciding *when* to call this) is the
    /// caller's job - `nesting::dispatch`'s per-generation loop is the thing
    /// that persists across calls, not anything in this module.
    pub fn set_rotations(&mut self, rotations: u32) {
        self.config.rotations = rotations;
    }

    /// Returns a mutated copy of `individual` at the configured mutation
    /// rate. Three operators, same as the original: adjacent-swap (per
    /// part), rotation-reroll (per part, rate capped independently - see
    /// `ROTATION_MUTATION_RATE_CAP`'s doc below), and a single whole-individual
    /// relocate (moves one part to a random different slot in one step).
    #[must_use]
    pub fn mutate(&self, individual: &Individual) -> Individual {
        // The shared NFP cache is keyed by {sourceA, sourceB, rotationA,
        // rotationB} - a cache hit needs the exact same rotation pair seen
        // before. Rerolling rotation at the same rate as part-order mutation
        // means a mutation_rate tuned aggressively for order exploration
        // (60-96%, seen in real benchmark runs) rerolls nearly every part's
        // rotation every generation too, so the cache never saturates -
        // measured ~7,400 NFP cache misses per individual on individual #1
        // of a run, STILL ~7,360 on average 1,200+ individuals later, never
        // converging. Capping the rotation-reroll roll independently lets
        // order exploration stay as aggressive as the user wants without
        // also destroying cache locality.
        const ROTATION_MUTATION_RATE_CAP: f64 = 15.0;
        let rotation_mutation_chance = 0.01 * self.config.mutation_rate.min(ROTATION_MUTATION_RATE_CAP);
        let swap_chance = 0.01 * self.config.mutation_rate;
        // Same `rotations: 0` guard as `random_angles` above - `rotations`
        // reaches this function straight from caller-supplied `GaConfig`.
        let rotations = self.config.rotations.max(1);

        let mut rng = self.next_rng();
        let mut clone = individual.clone();
        clone.fitness = None;

        for i in 0..clone.placement.len() {
            if rng.gen::<f64>() < swap_chance {
                let j = i + 1;
                // Deliberately swaps only `placement`, not `rotation` -
                // preserved exactly from the original, which has the same
                // asymmetry. A part's rotation is tied to its *slot*
                // through a swap, not carried along with the part into its
                // new slot; each slot still gets its own independent
                // rotation-reroll chance right below regardless of what
                // just moved into it.
                if j < clone.placement.len() {
                    clone.placement.swap(i, j);
                }
            }
            if rng.gen::<f64>() < rotation_mutation_chance {
                clone.rotation[i] = random_angle(clone.placement[i], rotations, self.config.mirror, &self.config.part_rules, &mut rng);
            }
        }

        // Secondary, coarser-grained operator: the adjacent swap above can
        // only move a part one slot per mutation event, so escaping a bad
        // ordering takes many generations of accumulated small swaps.
        // Relocate removes one part and reinserts it at a random different
        // slot in a single step - a bigger jump. Rolled once per individual
        // (not once per part like the swap above), since it's more
        // disruptive.
        if clone.placement.len() > 1 && rng.gen::<f64>() < swap_chance {
            let from = rng.gen_range(0..clone.placement.len());
            let mut to = rng.gen_range(0..clone.placement.len() - 1);
            if to >= from {
                to += 1;
            }
            let id = clone.placement.remove(from);
            clone.placement.insert(to, id);
            let rot = clone.rotation.remove(from);
            clone.rotation.insert(to, rot);
        }

        clone
    }

    /// Single-point crossover, producing two children. Uses plain
    /// `Vec::contains` on part ids for the "does this child already have
    /// this part" check - the original's local `contains(gene, id)` helper,
    /// now just what `Vec<usize>::contains` already does.
    ///
    /// Requires `male`/`female` to have the same gene length - always true
    /// for the only real caller (`generation()`, which draws both parents
    /// from the same `population`, where every individual is guaranteed the
    /// same length by construction), but not enforced by the type system
    /// since this is a `pub` method. A mismatched pair panics via
    /// out-of-bounds slicing below rather than a clean error.
    #[must_use]
    pub fn mate(&self, male: &Individual, female: &Individual) -> (Individual, Individual) {
        // A real assert, not debug_assert!: `mate` is `pub`, callable with
        // any two individuals by a caller other than `generation()` (which
        // is the only call site that currently guarantees equal length by
        // construction). In a release build a debug_assert compiles out
        // entirely, leaving a mismatched pair to fail via an unexplained
        // out-of-bounds panic several lines down instead of this clear
        // message at the actual violated precondition.
        assert_eq!(male.placement.len(), female.placement.len(), "mate() requires male/female to have the same gene length");

        let mut rng = self.next_rng();
        let r: f64 = rng.gen::<f64>().clamp(0.1, 0.9);
        let cutpoint = (r * (male.placement.len() as f64 - 1.0)).round() as usize;

        let mut gene1 = male.placement[..cutpoint].to_vec();
        let mut rot1 = male.rotation[..cutpoint].to_vec();
        let mut gene2 = female.placement[..cutpoint].to_vec();
        let mut rot2 = female.rotation[..cutpoint].to_vec();

        for i in 0..female.placement.len() {
            if !gene1.contains(&female.placement[i]) {
                gene1.push(female.placement[i]);
                rot1.push(female.rotation[i]);
            }
        }
        for i in 0..male.placement.len() {
            if !gene2.contains(&male.placement[i]) {
                gene2.push(male.placement[i]);
                rot2.push(male.rotation[i]);
            }
        }

        (
            Individual { placement: gene1, rotation: rot1, fitness: None },
            Individual { placement: gene2, rotation: rot2, fitness: None },
        )
    }

    /// Advances to the next generation in place. Requires every current
    /// individual's `fitness` to already be set (the caller ran placement
    /// for each one) - same implicit precondition the original has, just
    /// enforced with `.expect(..)` instead of silently sorting on
    /// `undefined - undefined = NaN`.
    pub fn generation(&mut self) {
        self.population
            .sort_by(|a, b| a.fitness.expect("fitness must be set before generation()").total_cmp(&b.fitness.unwrap()));

        // fittest individual is preserved in the new generation (elitism)
        let mut new_population = vec![self.population[0].clone()];

        while new_population.len() < self.population.len() {
            let male_idx = self.random_weighted_individual(None);
            let female_idx = self.random_weighted_individual(Some(male_idx));

            let (child1, child2) = self.mate(&self.population[male_idx], &self.population[female_idx]);
            new_population.push(self.mutate(&child1));
            if new_population.len() < self.population.len() {
                new_population.push(self.mutate(&child2));
            }
        }

        self.population = new_population;
    }

    /// Returns an index into `self.population`, weighted toward the front
    /// (lower fitness = more likely) - an index rather than a reference or
    /// clone, so `exclude` compares by position instead of needing
    /// reference/value identity (the original excludes via
    /// `pop.indexOf(exclude)` on a shallow-copied array, i.e. also
    /// position/reference based, not equality-based - ids can repeat across
    /// individuals in principle, so comparing by value would be wrong here
    /// too).
    fn random_weighted_individual(&self, exclude: Option<usize>) -> usize {
        let indices: Vec<usize> = (0..self.population.len()).filter(|&i| Some(i) != exclude).collect();
        let n = indices.len();
        let mut rng = self.next_rng();
        let rand: f64 = rng.gen();

        let mut lower = 0.0;
        let weight = 1.0 / n as f64;
        let mut upper = weight;

        for (i, &idx) in indices.iter().enumerate() {
            if rand > lower && rand < upper {
                return idx;
            }
            lower = upper;
            upper += 2.0 * weight * ((n - i) as f64 / n as f64);
        }

        indices[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> GaConfig {
        GaConfig { population_size: 8, mutation_rate: 10.0, rotations: 4, mirror: false, part_rules: Default::default() }
    }

    fn adam(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    fn same_ids(placement: &[usize], expected_len: usize) -> bool {
        let mut sorted = placement.to_vec();
        sorted.sort_unstable();
        sorted == (0..expected_len).collect::<Vec<_>>()
    }

    /// Regression test: `rotations: 0` used to panic immediately inside
    /// `GeneticAlgorithm::new` (`random_angles`'s `rng.gen_range(0..0)` on
    /// an empty range) and again in `mutate`. `app` validates against
    /// this at its own request boundary, but `GaConfig`/`GeneticAlgorithm`
    /// are `pub` API reachable without going through that check at all.
    #[test]
    fn rotations_zero_does_not_panic() {
        let cfg = GaConfig { population_size: 4, mutation_rate: 50.0, rotations: 0, mirror: false, part_rules: Default::default() };
        let ga = GeneticAlgorithm::new(adam(4), cfg, Vec::new(), 0);
        assert_eq!(ga.population.len(), 4);
        for individual in &ga.population {
            assert!(individual.rotation.iter().all(|&r| r == 0.0), "rotations: 0 should fall back to a single 0-degree angle");
        }

        // mutate() must not panic either - roll it enough times to hit both
        // the swap and rotation-reroll branches at least once.
        for _ in 0..20 {
            let _ = ga.mutate(&ga.population[0]);
        }
    }

    /// Regression test for `set_rotations` (the `widenRotationsIfStalled`
    /// port's other half - see the module doc comment): with `rotations: 1`
    /// every angle is forced to exactly 0.0 degrees, so if `mutate`'s
    /// rotation-reroll ever produces anything else, the grid it's drawing
    /// from must have actually widened, not just been recorded.
    #[test]
    fn set_rotations_widens_the_grid_mutate_draws_from() {
        let mut ga = GeneticAlgorithm::new(adam(6), GaConfig { population_size: 4, mutation_rate: 100.0, rotations: 1, mirror: false, part_rules: Default::default() }, Vec::new(), 0);
        for _ in 0..20 {
            let mutant = ga.mutate(&ga.population[0]);
            assert!(mutant.rotation.iter().all(|&r| r == 0.0), "rotations: 1 should never produce a non-zero angle");
        }

        ga.set_rotations(4);
        let saw_nonzero = (0..50).any(|_| ga.mutate(&ga.population[0]).rotation.iter().any(|&r| r != 0.0));
        assert!(saw_nonzero, "widened rotations should let mutate draw a non-zero angle at least once in 50 tries");
    }

    #[test]
    fn population_is_filled_to_the_configured_size() {
        let ga = GeneticAlgorithm::new(adam(6), config(), Vec::new(), 0);
        assert_eq!(ga.population.len(), 8);
    }

    #[test]
    fn adam_survives_unmutated_as_population_zero() {
        let a = adam(6);
        let ga = GeneticAlgorithm::new(a.clone(), config(), Vec::new(), 0);
        assert_eq!(ga.population[0].placement, a);
    }

    #[test]
    fn extra_seeds_are_used_when_they_match_adams_length() {
        let mut cfg = config();
        cfg.population_size = 3;
        let seeds = vec![vec![5, 4, 3, 2, 1, 0], vec![1, 2, 3, 4, 5, 6, 7]]; // 2nd is wrong length, dropped
        let ga = GeneticAlgorithm::new(adam(6), cfg, seeds, 0);
        assert_eq!(ga.population.len(), 3);
        assert_eq!(ga.population[1].placement, vec![5, 4, 3, 2, 1, 0]);
    }

    #[test]
    fn every_individual_is_a_valid_permutation_with_a_rotation_per_part() {
        let ga = GeneticAlgorithm::new(adam(7), config(), Vec::new(), 0);
        for ind in &ga.population {
            assert!(same_ids(&ind.placement, 7));
            assert_eq!(ind.rotation.len(), 7);
            for &r in &ind.rotation {
                assert!(r >= 0.0 && r < 360.0);
                assert_eq!((r / 90.0).round() * 90.0, r, "rotation {r} not on the 4-way grid");
            }
        }
    }

    #[test]
    fn mutate_preserves_the_part_set() {
        let ga = GeneticAlgorithm::new(adam(10), config(), Vec::new(), 0);
        for _ in 0..20 {
            let mutant = ga.mutate(&ga.population[0]);
            assert!(same_ids(&mutant.placement, 10));
            assert_eq!(mutant.rotation.len(), 10);
            assert!(mutant.fitness.is_none());
        }
    }

    #[test]
    fn mate_produces_two_full_permutations_from_the_parents() {
        let ga = GeneticAlgorithm::new(adam(8), config(), Vec::new(), 0);
        let male = &ga.population[0];
        let female = ga.mutate(male);
        for _ in 0..20 {
            let (c1, c2) = ga.mate(male, &female);
            assert!(same_ids(&c1.placement, 8));
            assert!(same_ids(&c2.placement, 8));
            assert_eq!(c1.rotation.len(), 8);
            assert_eq!(c2.rotation.len(), 8);
        }
    }

    #[test]
    fn generation_keeps_population_size_and_elitism() {
        let mut ga = GeneticAlgorithm::new(adam(6), config(), Vec::new(), 0);
        for (i, ind) in ga.population.iter_mut().enumerate() {
            ind.fitness = Some(100.0 - i as f64); // last-indexed starts fittest
        }
        // sort will put the individual with fitness 100.0-7=93.0... just assert the elite survives
        let elite = ga.population.iter().min_by(|a, b| a.fitness.unwrap().total_cmp(&b.fitness.unwrap())).unwrap().placement.clone();

        ga.generation();

        assert_eq!(ga.population.len(), 8);
        assert_eq!(ga.population[0].placement, elite);
        for ind in &ga.population {
            assert!(same_ids(&ind.placement, 6));
        }
    }

    #[test]
    #[should_panic(expected = "fitness must be set")]
    fn generation_panics_if_fitness_is_missing() {
        let mut ga = GeneticAlgorithm::new(adam(4), config(), Vec::new(), 0);
        ga.generation();
    }

    fn result(unplaced: usize, sheets: usize, utilisation: f64) -> PlaceResult {
        PlaceResult {
            placements: (0..sheets)
                .map(|i| crate::placement::SheetPlacement { sheet_index: i, parts: Vec::new() })
                .collect(),
            fitness: 0.0,
            area: 0.0,
            total_area: 0.0,
            utilisation,
            unplaced_count: unplaced,
            unplaced_ids: Vec::new(),
        }
    }

    #[test]
    fn is_better_nest_prefers_fewer_unplaced_parts_above_all_else() {
        // fewer_unplaced has more sheets and worse utilisation than its
        // rival, but 0 unplaced parts still wins outright.
        let fewer_unplaced = result(0, 5, 10.0);
        assert!(is_better_nest(&fewer_unplaced, &result(1, 1, 99.0)));
        assert!(!is_better_nest(&result(1, 1, 99.0), &fewer_unplaced));
    }

    #[test]
    fn is_better_nest_then_prefers_fewer_sheets() {
        assert!(is_better_nest(&result(0, 1, 10.0), &result(0, 2, 99.0)));
    }

    #[test]
    fn is_better_nest_finally_prefers_higher_utilisation() {
        assert!(is_better_nest(&result(0, 1, 80.0), &result(0, 1, 50.0)));
        assert!(!is_better_nest(&result(0, 1, 50.0), &result(0, 1, 80.0)));
    }
}
