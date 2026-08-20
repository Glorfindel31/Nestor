//! Port of `main/nfpDb.ts`'s `NfpCache`: an in-memory cache for the
//! (relatively expensive) NFP computations `geometry::obstacle_nfp` and
//! `geometry::inner_nfp` produce, keyed by `crate::cache_key::nfp_cache_key`
//! (Phase 2's unification of what were two separately-maintained key
//! formats).
//!
//! **Threading, a real change from the original, not a 1:1 port**: the
//! Electron app ran one independent, unshared `NfpCache` per background
//! window (6-8 parallel `BrowserWindow` processes, zero cross-window cache
//! sharing - see the plan's Phase 4 scope). This port shares ONE cache
//! across every rayon worker thread instead, behind a `Mutex` (see
//! `docs/PORT_STATUS.md`'s Phase 4 row: "sharded map or plain mutex - verify
//! contention before reaching for anything fancier"). A plain `Mutex` is the
//! honest default until real contention data says otherwise.
//!
//! **Entry-count bookkeeping simplification**: the original hand-tracks an
//! `entryCount` field to avoid `Object.keys(db).length` being O(n) per
//! insert; Rust's `HashMap::len()` is already O(1), so that field isn't
//! needed here.
//!
//! **Clone-on-read/write, simplified by ownership**: the original must
//! explicitly deep-copy every cached NFP on both insert and read
//! (`new Point(p.x, p.y)` per point) because a live NFP-tracing call mutates
//! `Point.marked` in place on whatever array reference it's given - a cache
//! hit returning a shared reference would let one caller's tracing corrupt
//! what a later caller reads. Rust's `Point` is `Copy`, so `Vec<Point>` /
//! `Vec<Vec<Point>>`'s ordinary `.clone()` already produces an independent
//! copy with its own `marked` flags; no custom clone method needed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use geometry::point::Point;

use crate::cache_key::{nfp_cache_key, NfpKey, SourceId};

/// Matches `nfpDb.ts`'s `MAX_CACHE_ENTRIES`: above this many entries, stop
/// caching new NFPs (existing entries are kept, just no more added). Bounded
/// memory beats an out-of-memory crash on a long multi-part run.
pub const MAX_CACHE_ENTRIES: usize = 50_000;

/// A cached NFP result - either the obstacle/outer shape (an outer loop plus
/// any hole-restore regions, matching `geometry::obstacle_nfp::ObstacleNfp`)
/// or the inner-fit shape (a flat list of valid-placement regions, matching
/// `geometry::inner_nfp::inner_nfp`'s return type). Mirrors `nfpDb.ts`'s
/// `Nfp | Nfp[]` union - disambiguated there by a caller-supplied `inner`
/// boolean flag, made an explicit, self-describing enum here instead.
#[derive(Clone, Debug)]
pub enum CachedNfp {
    Outer { outer: Vec<Point>, children: Vec<Vec<Point>> },
    Inner(Vec<Vec<Point>>),
}

/// Shared NFP cache. Cheap to construct (`NfpCache::default()`); wrap in an
/// `Arc` to share across rayon worker threads once Phase 4's dispatch loop
/// exists.
///
/// Each key maps to an `Arc<OnceLock<_>>` "slot", not a plain value - this is
/// what makes `get_or_compute` (the only way in or out) stampede-proof.
/// A first-generation version exposed `has`/`find`/`insert` separately, which
/// left the actual NFP computation happening *between* two lock
/// acquisitions, unlocked: under `dispatch::run_generation`'s `par_iter()`,
/// several rayon threads wanting the same key at once could all miss
/// together, all compute the same (expensive) NFP independently, and race to
/// insert - wasted, duplicated work that defeated the whole point of sharing
/// one cache across threads. A `OnceLock` per key fixes this structurally:
/// the map lock is only ever held long enough to fetch-or-create a key's
/// slot; the actual computation runs outside that lock, and
/// `OnceLock::get_or_init` itself guarantees only the first caller for a
/// given key actually runs `compute`- every other concurrent caller for
/// that same key just blocks until it finishes, then shares the result.
///
/// **This blocking is deliberate and load-bearing, not a bug to "fix" by
/// letting racers duplicate work instead** - confirmed the hard way earlier
/// in this cache's history: a real 170-part/120-sheet job's NFP computation
/// (the Minkowski diff behind `geometry::obstacle_nfp`, on the real
/// post-clearance-offset part shapes) costs single-digit-to-tens of
/// milliseconds per call, not microseconds - see `geometry::clipper::
/// offset_bevel`'s doc comment for why the *input* polygons were far larger
/// than expected (~56 points from a 4-point original) until that was fixed.
/// A non-blocking, duplicate-on-race version of this cache was tried against
/// that same real job and measured *slower* end to end (175s vs 128s for 5
/// generations) - with computation this expensive, letting several threads
/// each redo it independently costs far more than any of them blocking to
/// share one result.
#[derive(Default, Debug)]
pub struct NfpCache {
    db: Mutex<HashMap<NfpKey, Arc<OnceLock<Option<Arc<CachedNfp>>>>>>,
    /// Total `get_or_compute` calls, hits included - see `lookups`.
    lookups: std::sync::atomic::AtomicUsize,
}

impl NfpCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A poisoned `Mutex` (some thread panicked while holding the lock)
    /// would otherwise make every future access from every other thread
    /// panic too, permanently, for the rest of the run - a single bad NFP
    /// computation shouldn't take down a shared cache every other rayon
    /// worker thread depends on. Recovering the guard (a stale-but-still
    /// internally-consistent map, since a `HashMap` can't be left in a
    /// half-mutated state by a panic between operations) is a far smaller
    /// problem than that.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<NfpKey, Arc<OnceLock<Option<Arc<CachedNfp>>>>>> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Returns the cached value for this key, computing it via `compute` on
    /// a genuine miss - see the struct doc comment for why concurrent
    /// callers requesting the same key can't stampede each other here.
    /// Returns a shared `Arc`, not an owned copy. The original had to deep-
    /// copy on every read because its NFP tracing mutated `Point.marked` in
    /// place on whatever array it was handed; nothing in this port mutates a
    /// cached NFP, and cloning one per lookup meant copying an entire polygon
    /// 3.7 million times on the hat benchmark to serve six distinct values.
    /// A caller that genuinely needs to mutate one clones it itself.
    ///
    /// Past `MAX_CACHE_ENTRIES`, a genuinely new key is computed directly,
    /// uncached and uncoalesced, rather than growing the map further -
    /// matches the original API's "no-op past the cap for new keys" cap
    /// behavior; an already-present key (inserted before the cap was hit)
    /// keeps working normally.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_compute(
        &self,
        a: SourceId,
        b: SourceId,
        a_rotation: f64,
        b_rotation: f64,
        a_flipped: bool,
        b_flipped: bool,
        compute: impl FnOnce() -> Option<CachedNfp>,
    ) -> Option<Arc<CachedNfp>> {
        self.lookups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let key = nfp_cache_key(a, b, a_rotation, b_rotation, a_flipped, b_flipped);
        let slot = {
            let mut db = self.lock();
            if let Some(existing) = db.get(&key) {
                Arc::clone(existing)
            } else if db.len() >= MAX_CACHE_ENTRIES {
                drop(db);
                return compute().map(Arc::new);
            } else {
                let slot = Arc::new(OnceLock::new());
                db.insert(key, Arc::clone(&slot));
                slot
            }
        };
        slot.get_or_init(|| compute().map(Arc::new)).clone()
    }

    /// How many times `get_or_compute` has been called - hits included, not
    /// just the misses `stats` counts. The gap between the two is the point:
    /// on the hat benchmark six cached NFPs serve millions of lookups, so
    /// what the per-lookup path costs (a `format!`ed `String` key, a global
    /// `Mutex`, a full clone of the cached polygon) dominates what computing
    /// an NFP costs. Sizing that needs the real number, not a guess.
    #[must_use]
    pub fn lookups(&self) -> usize {
        self.lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[must_use]
    pub fn stats(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    use super::*;

    fn sample_outer() -> CachedNfp {
        CachedNfp::Outer {
            outer: vec![Point::new(0.0, 0.0), Point::new(1.0, 0.0), Point::new(1.0, 1.0)],
            children: Vec::new(),
        }
    }

    fn panics_if_called() -> Option<CachedNfp> {
        panic!("compute should not run - this key should already be cached");
    }

    #[test]
    fn a_miss_computes_and_caches_the_result() {
        let cache = NfpCache::new();
        let found = cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, || Some(sample_outer())).expect("should be cached");
        match &*found {
            CachedNfp::Outer { outer, .. } => assert_eq!(outer.len(), 3),
            CachedNfp::Inner(_) => panic!("wrong variant"),
        }
        assert_eq!(cache.stats(), 1);

        // a second call for the same key must not call `compute` again
        let found_again = cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, panics_if_called).expect("should still be cached");
        match &*found_again {
            CachedNfp::Outer { outer, .. } => assert_eq!(outer.len(), 3),
            CachedNfp::Inner(_) => panic!("wrong variant"),
        }
    }

    #[test]
    fn geometrically_identical_rotations_share_a_cache_entry() {
        let cache = NfpCache::new();
        let _ = cache.get_or_compute(SourceId::part(1), SourceId::part(2), 360.0, 0.0, false, false, || Some(sample_outer()));
        // 0.0 normalizes to the same key as 360.0 - must hit, not recompute.
        assert!(cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, panics_if_called).is_some());
    }

    #[test]
    fn a_none_result_is_also_cached_not_recomputed_every_time() {
        let cache = NfpCache::new();
        let calls = AtomicUsize::new(0);
        let compute = || {
            calls.fetch_add(1, Ordering::Relaxed);
            None
        };
        assert!(cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, compute).is_none());
        assert!(cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, compute).is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 1, "a failed computation should still be cached, not retried every call");
    }

    #[test]
    fn returned_copies_are_independent_of_the_cached_entry() {
        let cache = NfpCache::new();
        let mut first = (*cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, || Some(sample_outer())).unwrap()).clone();
        if let CachedNfp::Outer { outer, .. } = &mut first {
            outer[0].marked = true;
        }

        let second = cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, panics_if_called).unwrap();
        let second = &*second;
        if let CachedNfp::Outer { outer, .. } = second {
            assert!(!outer[0].marked, "mutating one returned copy must not affect another");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn stops_caching_new_keys_past_the_entry_cap_but_keeps_existing_ones() {
        let cache = NfpCache::new();
        for i in 0..MAX_CACHE_ENTRIES {
            let _ = cache.get_or_compute(SourceId::part(i), SourceId::part(2), 0.0, 0.0, false, false, || Some(sample_outer()));
        }
        assert_eq!(cache.stats(), MAX_CACHE_ENTRIES);

        let calls = AtomicUsize::new(0);
        let compute = || {
            calls.fetch_add(1, Ordering::Relaxed);
            Some(sample_outer())
        };
        let _ = cache.get_or_compute(SourceId::part(999_999), SourceId::part(2), 0.0, 0.0, false, false, compute);
        assert_eq!(cache.stats(), MAX_CACHE_ENTRIES, "cap should block a brand-new key from being cached");
        let _ = cache.get_or_compute(SourceId::part(999_999), SourceId::part(2), 0.0, 0.0, false, false, compute);
        assert_eq!(calls.load(Ordering::Relaxed), 2, "a key that never got cached (past the cap) must recompute every call");

        // an existing key (cached before the cap was reached) still hits.
        assert!(cache.get_or_compute(SourceId::part(0), SourceId::part(2), 0.0, 0.0, false, false, panics_if_called).is_some());
    }

    /// The actual regression test for the cache-stampede bug: many threads
    /// requesting the exact same key at the same moment must share ONE
    /// computation, not each independently compute and race to store their
    /// own result. Proven by a shared counter that only a genuine first-and-
    /// only caller would leave at 1.
    #[test]
    fn concurrent_requests_for_the_same_key_are_coalesced_into_one_computation() {
        let cache = Arc::new(NfpCache::new());
        let calls = Arc::new(AtomicUsize::new(0));
        const THREADS: usize = 16;
        let barrier = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let calls = Arc::clone(&calls);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait(); // maximize actual overlap, not just "eventually all called"
                    cache.get_or_compute(SourceId::part(1), SourceId::part(2), 0.0, 0.0, false, false, || {
                        calls.fetch_add(1, Ordering::Relaxed);
                        // Hold the "computation" open briefly so the other
                        // threads' get_or_compute calls actually land while
                        // this one is still in flight, not after it's done.
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        Some(sample_outer())
                    })
                })
            })
            .collect();

        for handle in handles {
            assert!(handle.join().unwrap().is_some());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1, "16 concurrent callers for the same key should produce exactly 1 computation, not up to 16");
    }
}
