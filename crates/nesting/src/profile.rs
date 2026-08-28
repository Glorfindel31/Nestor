//! Coarse phase timers for the placement hot path.
//!
//! Added after the first optimisation attempt on the hat benchmark
//! (`examples/hat_bench.rs`) moved the clock by ~1%. The reasoning behind
//! that attempt was sound and the measurement it rested on was real -
//! 3.7 million cache lookups serving six distinct values - but "this happens
//! a lot" is not the same claim as "this is where the time goes", and only
//! one of those two can be settled by counting calls.
//!
//! So: atomics, not a sampling profiler, because the interesting regions are
//! known by name and a nanosecond counter per region is enough to rank them.
//! Two relaxed atomic adds per measured region against work measured in
//! microseconds is not something this benchmark can see.
//!
//! Always compiled, never wired into the app - `record` is only called from
//! `placement`'s hot path and only ever read by a benchmark that asks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// One measured region: how long, and how many times.
pub struct Phase {
    name: &'static str,
    nanos: AtomicU64,
    calls: AtomicU64,
}

impl Phase {
    const fn new(name: &'static str) -> Self {
        Self { name, nanos: AtomicU64::new(0), calls: AtomicU64::new(0) }
    }

    /// Times `f`, attributing its wall time to this phase. Wall time, not
    /// CPU time: these regions run inside `rayon`'s parallel iteration, so
    /// the totals across phases will exceed the run's own wall clock by
    /// roughly the thread count. Their *ratio* is what ranks them.
    pub fn time<T>(&self, f: impl FnOnce() -> T) -> T {
        // Off unless asked for. The measured regions are called ~17 million
        // times on the hat benchmark, and two `Instant::now()` calls plus two
        // atomics apiece is ~1s of thread time - enough that leaving the
        // timers always-on would have the profiler showing up in its own
        // numbers, and enough to make a "speedup" look bigger than it is.
        if !enabled() {
            return f();
        }
        let started = Instant::now();
        let out = f();
        self.nanos.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
        out
    }
}

/// Set `NEST_PROFILE=1` to collect phase timings. Read once and cached -
/// this is checked on every measured region.
#[must_use]
pub fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("NEST_PROFILE").is_ok_and(|v| v != "0"))
}

pub static OBSTACLE_NFP_LOOKUP: Phase = Phase::new("obstacle nfp lookup");
pub static OBSTACLE_SHIFT: Phase = Phase::new("obstacle nfp shift");
pub static CLIP_DIFFERENCE: Phase = Phase::new("clipper difference");
pub static CLIP_UNION: Phase = Phase::new("clipper union");
pub static INNER_NFP_LOOKUP: Phase = Phase::new("inner nfp lookup");
pub static CANDIDATE_SCORING: Phase = Phase::new("candidate scoring");
pub static CONTACT_INTERSECT: Phase = Phase::new("  of which: clip");
pub static CONTACT_PREP: Phase = Phase::new("  of which: prep");
pub static ROTATE_PART: Phase = Phase::new("rotate part");
pub static BANDED_PACK: Phase = Phase::new("banded pack_sheet");
pub static BANDED_CATALOGUE: Phase = Phase::new("  of which: catalogue");
pub static BANDED_SEARCH: Phase = Phase::new("  of which: search");
pub static BANDED_FINGERPRINT: Phase = Phase::new("  of which: fingerprint");
pub static BANDED_UNIFORM: Phase = Phase::new("  of which: uniform");
pub static OVERLAP_VALIDATE: Phase = Phase::new("overlap validation");
pub static TRY_PLACE: Phase = Phase::new("TOTAL try_place_part");
pub static OBSTACLE_NFP_COMPUTE: Phase = Phase::new("  of which: compute");

static ALL: &[&Phase] =
    &[&OBSTACLE_NFP_LOOKUP, &OBSTACLE_SHIFT, &CLIP_DIFFERENCE, &CLIP_UNION, &INNER_NFP_LOOKUP, &CANDIDATE_SCORING, &CONTACT_INTERSECT, &CONTACT_PREP, &ROTATE_PART, &OBSTACLE_NFP_COMPUTE, &BANDED_PACK, &OVERLAP_VALIDATE, &TRY_PLACE, &BANDED_CATALOGUE, &BANDED_SEARCH, &BANDED_UNIFORM, &BANDED_FINGERPRINT];

/// A plain event count, for questions of the form "how often is X already
/// known" - no timing, just a tally.
pub struct Counter {
    name: &'static str,
    hits: AtomicU64,
}

impl Counter {
    const fn new(name: &'static str) -> Self {
        Self { name, hits: AtomicU64::new(0) }
    }

    pub fn add(&self, n: u64) {
        if enabled() {
            self.hits.fetch_add(n, Ordering::Relaxed);
        }
    }
}

/// Candidate positions scored, in total.
pub static CANDIDATES_TOTAL: Counter = Counter::new("candidates scored");
/// ...of which the same position was already scored on the previous attempt
/// for this shape and rotation.
pub static CANDIDATES_REPEATED: Counter = Counter::new("  repeated position");
/// ...of which are also far enough from every newly placed obstacle that the
/// previous score is provably still correct. This is the memo hit rate an
/// incremental contact cache would actually get.
pub static CANDIDATES_UNAFFECTED: Counter = Counter::new("  and score unchanged");

/// Band plans served from `banded::PLAN_CACHE` / searched from scratch.
pub static PLAN_CACHE_HIT: Counter = Counter::new("band plan cache hit");
pub static PLAN_CACHE_MISS: Counter = Counter::new("band plan searched");

pub static ACC_NFP_MISS: Counter = Counter::new("accumulator nfp miss (hits shared cache)");
static COUNTERS: &[&Counter] = &[&CANDIDATES_TOTAL, &CANDIDATES_REPEATED, &CANDIDATES_UNAFFECTED, &PLAN_CACHE_HIT, &PLAN_CACHE_MISS, &ACC_NFP_MISS];

/// Every counter as `(name, hits)`, in declaration order.
#[must_use]
pub fn counters() -> Vec<(&'static str, u64)> {
    COUNTERS.iter().map(|c| (c.name, c.hits.load(Ordering::Relaxed))).collect()
}

/// Every phase as `(name, seconds, calls)`, busiest first.
#[must_use]
pub fn report() -> Vec<(&'static str, f64, u64)> {
    let mut rows: Vec<_> = ALL
        .iter()
        .map(|p| (p.name, p.nanos.load(Ordering::Relaxed) as f64 / 1e9, p.calls.load(Ordering::Relaxed)))
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows
}
