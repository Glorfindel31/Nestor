//! Port of `background.js`'s single-threaded greedy per-sheet placement
//! loop: `placeParts` + `tryPlacePartOnSheet` + the three placement-type
//! scorers. Phase 3's first end-to-end milestone - no GA, no threads (see
//! `RUST-REWRITE-PLAN.md` and `docs/PORT_STATUS.md`'s Phase 3 table).
//!
//! Simplification vs. the original, not a functional change: the JS side
//! converts every polygon to Clipper's own integer coordinate space by hand
//! (`toClipperCoordinates`/`ScaleUpPath`/`toNestCoordinates`) because the old
//! flat `ClipperLib` API needed manually-oriented, pre-scaled paths. Our
//! `geometry::clipper` wrapper (`crates/geometry/src/clipper.rs`) already
//! does that scaling internally per call (`DeepnestScale`, x10^7) and its
//! boolean ops are true set operations that don't require caller-managed
//! winding for correctness (confirmed by `inner_nfp.rs`'s general fallback,
//! which already composes multiple same-side loops this same way) - so this
//! port works directly in plain `Point` coordinates throughout, with no
//! `nfpToClipperCoordinates`/`toNestCoordinates`-equivalent step needed.
//!
//! Deliberately **not** ported here: `config.mergeLines`'s edge-merge fitness
//! bonus (`mergedLength` in the original). It's an optional scoring nicety,
//! not required for the core placement loop or this milestone's
//! one-rectangle-on-one-sheet correctness goal; the `.exact` per-point
//! marking it depends on isn't tracked on `geometry::Point` yet either. Add
//! both together if/when the edge-merge bonus is needed.

use std::collections::{HashMap, HashSet};

use clipper2::FillRule;
use geometry::clipper::{difference_polygons, intersection_polygons, offset_bevel, union_polygons};
use geometry::dxf_import::{polygon_material_area, rotate_layered_polygon, shift_layered_polygon, LayeredPolygon};
use geometry::hull_polygon::hull;
use geometry::inner_nfp::inner_nfp;
use geometry::obstacle_nfp::obstacle_nfp;
use geometry::point::Point;
use geometry::polygon::{almost_equal, get_polygon_bounds, is_rectangle, polygon_area, Bounds};

use std::sync::Arc;

use crate::cache::{CachedNfp, NfpCache};
use crate::cache_key::SourceId;

/// NFP cache-key identity for a part. Callers pass a `source_id`
/// (`NestPart::source_id`/`PlacedObstacle::source_id`), not the per-instance
/// `id` - every quantity-expanded copy of the same original part shares one
/// `source_id`, so N identical-shape parts share cache entries instead of
/// each instance recomputing the same geometry from scratch (real, measured
/// cost for jobs with many identical parts - restores parity with the
/// original app's `.source`-keyed cache, see `docs/PORT_STATUS.md`'s
/// Phase 4 entry). Assigned once (`dto::expand_parts`) and stable for the
/// whole run, so the numeric value itself is a valid "source" string - just
/// prefixed so it can never collide with `sheet_source`'s ids (both are
/// otherwise small integers starting at 0).
pub(crate) fn part_source(source_id: usize) -> SourceId {
    SourceId::part(source_id)
}

/// NFP cache-key identity for a sheet: `place_parts` is always called with
/// the same `sheets` slice for the life of a run (every individual/
/// generation), so a sheet's index into that slice is just as stable an
/// identity as a part's id is.
pub(crate) fn sheet_source(index: usize) -> SourceId {
    SourceId::sheet(index)
}

/// `obstacle_nfp`, through `cache` - a cache hit skips the actual Minkowski
/// difference (`geometry::obstacle_nfp`'s real cost) entirely. Keyed by both
/// polygons' stable identity + rotation, not their post-rotation geometry -
/// the same (obstacle id, part id, obstacle rotation, part rotation)
/// combination recurs constantly across a GA run's many individuals and
/// generations (only the *order*/*which sheet* differs between them, not
/// this specific pair's shapes), which is exactly what made this the
/// dominant uncached cost (see `nesting::cache`'s own module doc for the
/// cache itself, built in an earlier phase but never wired into the actual
/// placement pipeline until now).
#[allow(clippy::too_many_arguments)]
fn cached_obstacle_nfp(
    cache: &NfpCache,
    obstacle: &LayeredPolygon,
    obstacle_id: usize,
    obstacle_rotation: f64,
    part: &LayeredPolygon,
    part_id: usize,
    part_rotation: f64,
    curve_tolerance: f64,
) -> Option<Arc<CachedNfp>> {
    let cached = cache.get_or_compute(part_source(obstacle_id), part_source(part_id), obstacle_rotation, part_rotation, false, false, || {
        obstacle_nfp(obstacle, part, curve_tolerance).map(|nfp| CachedNfp::Outer { outer: nfp.outer, children: nfp.children })
    })?;
    // The `Arc` is returned rather than unpacked into an owned `ObstacleNfp`
    // on purpose: this is the hottest call in the engine (3.7M times on the
    // hat benchmark) and its only consumer immediately copies the points it
    // needs into a shifted buffer anyway, so an owned copy here was pure waste.
    matches!(&*cached, CachedNfp::Outer { .. }).then_some(cached)
}

/// `inner_nfp`, through `cache` - same idea as `cached_obstacle_nfp` above.
/// `Arotation` is hardcoded to `0.0` for the lookup, matching
/// `cache_key`'s documented caller convention: the container (sheet) doesn't
/// rotate in this scenario, only the part being fitted into it does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cached_inner_nfp(
    cache: &NfpCache,
    sheet: &LayeredPolygon,
    sheet_src: SourceId,
    part: &LayeredPolygon,
    part_id: usize,
    part_rotation: f64,
    curve_tolerance: f64,
) -> Option<Vec<Vec<Point>>> {
    let cached = crate::profile::INNER_NFP_LOOKUP
        .time(|| cache.get_or_compute(sheet_src, part_source(part_id), 0.0, part_rotation, false, false, || inner_nfp(sheet, part, curve_tolerance).map(CachedNfp::Inner)))?;
    // Still cloned, unlike the obstacle path above: this runs once per
    // candidate rotation rather than once per already-placed obstacle, so it
    // is nowhere near the same order of call volume, and every caller wants
    // an owned region list it can hand around.
    match &*cached {
        CachedNfp::Inner(regions) => Some(regions.clone()),
        CachedNfp::Outer { .. } => None,
    }
}

/// `background.js`'s `DEFAULT_DOMINANT_PART_AREA_THRESHOLD`.
pub const DEFAULT_DOMINANT_PART_AREA_THRESHOLD: f64 = 0.9;

/// How far outward (mm) `PlacementType::TightFit` grows a candidate's own
/// footprint before measuring overlap with already-placed material/the
/// sheet edge - the "is this touching" probe width. Empirical starting
/// point, same order of magnitude as real spacing/margin values already
/// used elsewhere in this codebase (3-6.5mm in the `FLAT.dxf` benchmarks);
/// tune against a real job if this doesn't clearly help.
pub const TIGHT_FIT_PROBE_DISTANCE: f64 = 1.0;

/// True if two axis-aligned bounding boxes are within `distance` of each
/// other (touching or overlapping counts as within any non-negative
/// distance) - an exact cull, not an approximation: if this returns false,
/// the two boxes' contents genuinely cannot produce any overlap once each
/// is buffered outward by `distance`, so `PlacementType::TightFit` can skip
/// the real Clipper offset/intersection entirely for that pair.
pub fn bounds_within_distance(a: &Bounds, b: &Bounds, distance: f64) -> bool {
    a.x <= b.x + b.width + distance && b.x <= a.x + a.width + distance && a.y <= b.y + b.height + distance && b.y <= a.y + a.height + distance
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementType {
    Gravity,
    Box,
    ConvexHull,
    /// Scores a candidate by how much of a small buffer zone around its own
    /// boundary actually touches already-placed material or the sheet
    /// edge - genuine local contact, not the aggregate bounding shape of
    /// everything placed so far the other three types use. Added after the
    /// other three all plateaued around 70-71% utilisation on a real
    /// concave/interlocking-tile benchmark (the aperiodic "hat" monotile) -
    /// none of them directly reward a candidate for sitting snugly against
    /// its immediate neighbor, which is exactly what an interlocking shape
    /// needs to pack tightly. See `TIGHT_FIT_PROBE_DISTANCE`'s doc comment
    /// for the buffer-zone width this depends on.
    TightFit,
    /// `Gravity` picks the candidate (or set of near-tied candidates, by
    /// `Gravity`'s own bounding-measure), `TightFit`'s exact contact area
    /// breaks ties among them. Cheaper than pure `TightFit` (the expensive
    /// contact computation only runs on however many candidates are
    /// actually tied by the cheap metric, not every candidate) and more
    /// principled than `Gravity`'s own plain x-position tiebreak - "which of
    /// these equally-compact options sits snuggest" is a real geometric
    /// question, "which is further left" isn't. See
    /// `find_best_hybrid_candidate`.
    GravityTightFit,
    /// Two-phase: the sheet's second part (the first is handled by
    /// `place_parts`'s own dedicated first-part search - same multi-rotation
    /// contact-maximizing search as `TightFit`/`GravityTightFit`, not the
    /// plain top-left fast path, see that code's own doc comment for why
    /// this matters a lot for jobs where most sheets never reach a 3rd part)
    /// scores exactly like `Gravity` - cheap, and with only one neighbor on
    /// the sheet there's nothing for a contact-area search to meaningfully
    /// improve on yet. From the third part onward, scoring switches outright
    /// to `TightFit`'s real contact-area measure (not a tie-break like
    /// `GravityTightFit` - a full switch): a cheap aggregate-bounding-box
    /// heuristic stops being good enough once a sheet has real established
    /// neighbors worth fitting tightly against, so accuracy "corrects" it.
    /// Also opts into `place_parts`'s rotation-reuse cache: once a shape
    /// (`source_id`) has placed successfully at some rotation, a later part
    /// sharing that `source_id` tries that same rotation first instead of
    /// re-running the full multi-angle search from scratch (only applies
    /// from the second part onward - the dedicated first-part search always
    /// runs fresh, it doesn't consult this cache).
    GravityCorrective,
}

#[derive(Clone, Debug)]
pub struct PlacementConfig {
    pub placement_type: PlacementType,
    /// Number of rotation angles tried per part before giving up on a sheet
    /// (equal steps of `360/rotations` degrees). See `docs/PORT_STATUS.md`'s
    /// rotation-angle-grid quirk - kept as plain user-facing config here too.
    pub rotations: u32,
    pub dominant_part_area_threshold: f64,
    pub curve_tolerance: f64,
    /// Per-part orientation constraints. Empty (the default) means every
    /// part follows the global `rotations` grid and mirror switch, exactly
    /// as before this field existed - see `PartRule`.
    pub part_rules: PartRules,
    /// Also try a band/shelf layout per sheet and keep it when it beats the
    /// greedy pass - see `crate::banded`. Costs one extra bounding-box pass
    /// per sheet, which is cheap next to the NFP work already done.
    pub banded_pass: bool,
}

/// Per-part constraints on how a part may be oriented. Absent (no entry in
/// `PartRules` for a part id) means unconstrained - the global
/// `rotations` grid and the global mirror switch apply, exactly as before
/// this existed.
///
/// The driving case is grain direction: a part cut from material with a
/// visible grain, a coating or a printed face may only sit at, say, 0 or
/// 180 degrees, and may not be flipped over at all, while everything else in
/// the same job is free.
#[derive(Clone, Debug, PartialEq)]
pub struct PartRule {
    /// The only angles this part may be placed at, in degrees, already
    /// normalised to `[0, 360)` and deduped. Empty means unconstrained.
    pub angles: Vec<f64>,
    /// Whether this part may be mirrored, overriding the job-wide switch.
    pub mirror: bool,
}

/// Part id -> its constraint. `Arc` because it rides on `PlacementConfig`
/// and `GaConfig`, both of which are cloned per run and shared across
/// rayon's per-individual threads; the map itself never changes during a
/// run.
pub type PartRules = std::sync::Arc<HashMap<usize, PartRule>>;

/// The rotation angles a part may be tried at, in the order to try them,
/// each paired with **the rotation delta to apply to reach it from the
/// previous entry**. The first entry's delta is always `0.0` (the caller
/// already holds the part at `from`).
///
/// Deltas rather than just angles because the callers rotate incrementally
/// (`rotate_layered_polygon(&trial_polygon, delta)`), and because
/// `advance_rotation` wraps its angle at 360 while the rotation applied to
/// the geometry must stay a plain `step`. Recomputing the delta from wrapped
/// angles would silently turn a `+90` into a `-270` at the wraparound.
///
/// **An unconstrained part gets exactly the sequence the old fixed loop
/// produced** - `from`, then `advance_rotation` by `360/rotations`, that many
/// times - so nothing about an unconstrained run changes, bit for bit.
///
/// A constrained part's allowed set **replaces** the grid rather than
/// filtering it. Filtering would make 180 unreachable at `rotations: 3` and
/// leave a grain-locked part silently unplaceable; and the allowed angles
/// need not lie on the grid at all.
#[must_use]
pub fn rotation_steps(config: &PlacementConfig, part_id: usize, from: f64) -> Vec<(f64, f64)> {
    let rule = config.part_rules.get(&(part_id & !crate::dispatch::MIRROR_ID_BIT));
    match rule.filter(|r| !r.angles.is_empty()) {
        None => {
            let step = 360.0 / config.rotations.max(1) as f64;
            let mut out = Vec::with_capacity(config.rotations.max(1) as usize);
            let mut angle = from;
            for i in 0..config.rotations.max(1) {
                out.push((angle, if i == 0 { 0.0 } else { step }));
                angle = advance_rotation(angle, step);
            }
            out
        }
        Some(rule) => {
            // Start at whichever allowed angle is closest to where the part
            // already is, then walk the rest in cyclic order - the same
            // "carry on from the current orientation" shape the grid loop
            // has, so a part that already placed well at one allowed angle
            // tries that one first on the next sheet.
            let start = rule
                .angles
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let d = |x: f64| {
                        let raw = (x - from).rem_euclid(360.0);
                        raw.min(360.0 - raw)
                    };
                    d(**a).total_cmp(&d(**b))
                })
                .map_or(0, |(i, _)| i);

            let mut out = Vec::with_capacity(rule.angles.len());
            let mut previous = from;
            for k in 0..rule.angles.len() {
                let angle = rule.angles[(start + k) % rule.angles.len()];
                out.push((angle, (angle - previous).rem_euclid(360.0)));
                previous = angle;
            }
            out
        }
    }
}

/// Whether a part may be mirrored: its own rule if it has one, otherwise the
/// job-wide default. See `PartRule`.
#[must_use]
pub fn part_may_mirror(rules: &PartRules, part_id: usize, global: bool) -> bool {
    rules.get(&(part_id & !crate::dispatch::MIRROR_ID_BIT)).map_or(global, |r| r.mirror)
}

/// A part queued for nesting. `polygon`/`rotation` are replaced (not
/// mutated in place) each time a rotation retry fails, mirroring
/// `background.js`'s `parts[i] = r` - the part's current-best-tried rotation
/// carries over between sheets.
#[derive(Clone, Debug)]
pub struct NestPart {
    pub id: usize,
    /// Which original part definition this instance was expanded from
    /// (shared by every quantity-copy of the same part) - used for NFP
    /// cache-key identity instead of `id`, so N identical-shape copies with
    /// distinct `id`s still share cache entries. Distinct from `id` itself,
    /// which stays the per-instance identity used for final placement
    /// output/removal - see `docs/PORT_STATUS.md`'s Phase 4 entry on the
    /// original app's `.source`-keyed cache this restores parity with.
    pub source_id: usize,
    pub polygon: LayeredPolygon,
    pub rotation: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
}

/// One part's final resting place on a sheet. `id` is purely the caller's
/// own identity for the part (see `NestPart::id`) - nothing in this module
/// uses it as an internal key, since ids aren't guaranteed unique (quantity
/// > 1 of the same part shares an id, same as the JS original never assumed
/// otherwise either).
#[derive(Clone, Copy, Debug)]
pub struct PlacedPart {
    pub id: usize,
    pub placement: Placement,
    pub rotation: f64,
}

/// One already-placed obstacle `try_place_part_on_sheet` has to clear -
/// geometry plus enough identity (`id`, `rotation`) to build an NFP cache
/// key against it, and `placement` (where it actually sits) bundled in
/// rather than as a separate parallel slice - every call site already used
/// geometry and placement in lockstep, so keeping them apart was only ever
/// a chance for the two to drift out of sync.
#[derive(Clone, Debug)]
pub struct PlacedObstacle {
    pub polygon: LayeredPolygon,
    pub id: usize,
    /// Same shape-identity meaning as `NestPart::source_id` - used instead
    /// of `id` when building this obstacle's NFP cache key.
    pub source_id: usize,
    pub rotation: f64,
    pub placement: Placement,
}

#[derive(Clone, Debug)]
pub struct SheetPlacement {
    pub sheet_index: usize,
    pub parts: Vec<PlacedPart>,
}

#[derive(Clone, Debug)]
pub struct PlaceResult {
    pub placements: Vec<SheetPlacement>,
    pub fitness: f64,
    pub area: f64,
    pub total_area: f64,
    pub utilisation: f64,
    pub unplaced_count: usize,
    /// Which part ids never fit any sheet - same length/order as
    /// `unplaced_count` (`parts.len()` at the end of `place_parts`, below),
    /// just carrying the ids too so a caller can show the user *which*
    /// parts are missing, not just how many.
    pub unplaced_ids: Vec<usize>,
}

/// One rotation/position `try_place_part_on_sheet` (or the TightFit-family
/// first-part rotation search in `place_parts`) actually scored while
/// placing a part - not just the winner. `score` is always
/// `CandidateScore::area()`'s raw number (lower wins, same convention
/// `find_best_candidate` uses for every placement type, including
/// `TightFit`'s already-negated contact area) - a caller replaying these
/// doesn't need to know which placement type produced them to rank "better"
/// vs "worse". Only candidates that survived the "does this even land inside
/// the sheet" filter are recorded - an NFP vertex rejected before scoring
/// was never a real option, not a rejected one.
#[derive(Clone, Copy, Debug)]
pub struct CandidateTrace {
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
    pub score: f64,
    pub accepted: bool,
}

fn shift_points(points: &[Point], dx: f64, dy: f64) -> Vec<Point> {
    points.iter().map(|p| Point::new(p.x + dx, p.y + dy)).collect()
}

fn get_hull_or_fallback(points: &[Point]) -> Vec<Point> {
    hull(points).unwrap_or_else(|| points.to_vec())
}

/// Port of `hasMaterialOverlap`: true if `a` and `b` share any non-zero-area
/// material, after subtracting both polygons' own holes from the overlap.
pub fn has_material_overlap(a: &LayeredPolygon, b: &LayeredPolygon) -> bool {
    let intersection = match intersection_polygons(std::slice::from_ref(&a.points), std::slice::from_ref(&b.points), FillRule::NonZero) {
        Ok(r) if !r.is_empty() => r,
        _ => return false,
    };

    let mut holes: Vec<Vec<Point>> = a.children.iter().map(|c| c.points.clone()).collect();
    holes.extend(b.children.iter().map(|c| c.points.clone()));

    let intersection = if holes.is_empty() {
        intersection
    } else {
        match difference_polygons(&intersection, &holes, FillRule::NonZero) {
            Ok(r) => r,
            Err(_) => return true,
        }
    };

    // Bare `> 0.0` against a Clipper-derived (x10^7-scaled, boolean-op'd)
    // area, not a tolerance-guarded comparison - inherited as-is from the
    // original JS (`hasMaterialOverlap`'s own equivalent check), not an
    // oversight. A sub-pixel Clipper sliver could in principle read as
    // "real" overlap; left matching upstream behavior rather than
    // introducing a new epsilon the original never had.
    intersection.iter().any(|p| polygon_area(p).abs() > 0.0)
}

/// Port of `hasMaterialOutsideSheet`: true if any of `part` falls outside
/// `sheet`'s outer boundary, or overlaps one of the sheet's own holes.
pub fn has_material_outside_sheet(part: &LayeredPolygon, sheet: &LayeredPolygon) -> bool {
    let outside = match difference_polygons(std::slice::from_ref(&part.points), std::slice::from_ref(&sheet.points), FillRule::NonZero) {
        Ok(r) => r,
        Err(_) => return true,
    };
    if outside.iter().any(|p| polygon_area(p).abs() > 0.0) {
        return true;
    }

    sheet.children.iter().any(|hole| has_material_overlap(part, hole))
}

/// A candidate placement's fitness, shaped by which placement type produced
/// it - the enum (rather than a bare `area: f64, width: Option<f64>` pair)
/// makes "gravity/box candidates always carry a width, convex-hull
/// candidates never do" a compile-time fact instead of a runtime convention
/// `find_best_candidate` would otherwise have to trust its caller to uphold.
enum CandidateScore {
    Gravity { area: f64, width: f64 },
    Box { area: f64, width: f64 },
    ConvexHull { area: f64 },
    /// `area` is *negated* contact area (more contact = more negative), so
    /// `find_best_candidate`'s existing "smaller area wins" convention picks
    /// the candidate with the *most* contact unchanged - no new comparison
    /// logic needed, same reasoning `ConvexHull` already relies on.
    TightFit { area: f64 },
}

impl CandidateScore {
    fn area(&self) -> f64 {
        match *self {
            CandidateScore::Gravity { area, .. }
            | CandidateScore::Box { area, .. }
            | CandidateScore::ConvexHull { area }
            | CandidateScore::TightFit { area } => area,
        }
    }

    fn width(&self) -> Option<f64> {
        match self {
            CandidateScore::Gravity { width, .. } | CandidateScore::Box { width, .. } => Some(*width),
            CandidateScore::ConvexHull { .. } | CandidateScore::TightFit { .. } => None,
        }
    }
}

/// Contact against an already-placed *part* counts for more than contact
/// against the empty sheet border - see `tight_fit_contact_area`'s own doc
/// comment for the real scenario (a 3rd part jumping to the sheet's empty
/// opposite corner instead of extending the pair already placed) that
/// motivated weighting these differently instead of summing raw contact
/// area untouched.
const TIGHT_FIT_PART_CONTACT_WEIGHT: f64 = 2.0;
const TIGHT_FIT_SHEET_CONTACT_WEIGHT: f64 = 1.0;

/// Shared by `PlacementType::TightFit`'s own per-candidate scoring and
/// `find_best_hybrid_candidate`'s tie-break: the weighted contact area
/// between a candidate at `shiftvector` and its neighborhood, split into
/// `parts_neighborhood` (already-placed obstacles) and `sheet_neighborhood`
/// (the sheet's own border band), after culling each to only bounding-box-
/// nearby entries (see `bounds_within_distance`'s doc comment for why that
/// cull is exact, not an approximation).
///
/// Scored as `PART_WEIGHT * part_contact + SHEET_WEIGHT * sheet_contact`,
/// not just their sum with equal weight and not "whichever is bigger" -
/// touching a part outweighs touching the same area of empty sheet edge,
/// and touching *both* simultaneously always outscores either alone (both
/// terms are non-negative, so adding a second real contact never reduces
/// the total). Confirmed against a real 12-part mixed-size job where,
/// before this weighting existed, a 3rd part (the first one scored by pure
/// contact area, not `Gravity`) jumped to the sheet's empty opposite corner
/// instead of extending the two-part stack already placed - raw contact
/// against two full-length *empty* sheet walls exceeded the more modest
/// contact available by squeezing against the existing stack's exposed
/// edge, even though extending the existing cluster is what "tight fit"
/// should mean here.
/// A candidate part's probe-buffered outline, computed once per part shape
/// and reused for every candidate position of it.
///
/// The buffer is a Clipper `offset_bevel`, and offsetting is
/// translation-invariant: buffering a shape and then moving it gives the same
/// polygon as moving it and then buffering. The original did the latter,
/// which meant one Clipper offset *per candidate position* - 2.87 million of
/// them on the hat benchmark, 102s of thread time, ~99% of the run once the
/// NFP accumulation above was fixed. Buffering once and translating the
/// result is the same geometry for a rounding error's worth of the cost.
struct TightFitProbe {
    buffered: Vec<Vec<Point>>,
    /// The sheet's bounds, but only when the sheet is a plain rectangle -
    /// see `skips_sheet_contact`.
    rectangular_sheet: Option<Bounds>,
}

impl TightFitProbe {
    fn new(part: &LayeredPolygon, sheet: &LayeredPolygon) -> Self {
        let rectangular_sheet = if is_rectangle(&sheet.points, None) { get_polygon_bounds(&sheet.points) } else { None };
        Self { buffered: offset_bevel(&part.points, TIGHT_FIT_PROBE_DISTANCE), rectangular_sheet }
    }

    /// True when this candidate provably cannot touch the sheet border band,
    /// so the Clipper intersection against it can be skipped outright.
    ///
    /// The band is the ring *outside* the sheet (`sheet_border_band`), so a
    /// candidate contacts it exactly when its probe-buffered outline pokes
    /// out past the sheet edge. For a rectangular sheet that is settled by
    /// four comparisons: a candidate whose bounding box clears every edge by
    /// more than the probe distance cannot reach it.
    ///
    /// This matters because the band's own bounding box is the whole sheet,
    /// so the usual `bounds_within_distance` filter never rejects it - every
    /// candidate, including ones in the dead centre of the sheet, was paying
    /// for a Clipper intersection that could only ever return zero. Exact,
    /// not a heuristic: the skipped calls all returned 0.0.
    ///
    /// `None` (a non-rectangular sheet) falls through to the real
    /// intersection, which was always correct and stays that way.
    fn skips_sheet_contact(&self, candidate_bbox: &Bounds) -> bool {
        let Some(sheet) = self.rectangular_sheet else {
            return false;
        };
        candidate_bbox.x - TIGHT_FIT_PROBE_DISTANCE > sheet.x
            && candidate_bbox.y - TIGHT_FIT_PROBE_DISTANCE > sheet.y
            && candidate_bbox.x + candidate_bbox.width + TIGHT_FIT_PROBE_DISTANCE < sheet.x + sheet.width
            && candidate_bbox.y + candidate_bbox.height + TIGHT_FIT_PROBE_DISTANCE < sheet.y + sheet.height
    }
}

fn tight_fit_contact_area(
    probe: &TightFitProbe,
    shiftvector: Placement,
    part_bounds: Bounds,
    parts_neighborhood: &[(Bounds, Vec<Point>)],
    sheet_neighborhood: &[(Bounds, Vec<Point>)],
) -> f64 {
    let candidate_bbox = Bounds { x: part_bounds.x + shiftvector.x, y: part_bounds.y + shiftvector.y, width: part_bounds.width, height: part_bounds.height };
    let has_nearby = |neighborhood: &[(Bounds, Vec<Point>)]| neighborhood.iter().any(|(bounds, _)| bounds_within_distance(&candidate_bbox, bounds, TIGHT_FIT_PROBE_DISTANCE));
    if !has_nearby(parts_neighborhood) && !has_nearby(sheet_neighborhood) {
        return 0.0;
    }

    let buffered: Vec<Vec<Point>> = crate::profile::CONTACT_PREP.time(|| probe.buffered.iter().map(|region| shift_points(region, shiftvector.x, shiftvector.y)).collect());

    let contact_against = |neighborhood: &[(Bounds, Vec<Point>)]| -> f64 {
        let nearby: Vec<Vec<Point>> = crate::profile::CONTACT_PREP.time(|| {
            neighborhood
                .iter()
                .filter(|(bounds, _)| bounds_within_distance(&candidate_bbox, bounds, TIGHT_FIT_PROBE_DISTANCE))
                .map(|(_, poly)| poly.clone())
                .collect()
        });
        if nearby.is_empty() {
            return 0.0;
        }
        crate::profile::CONTACT_INTERSECT
            .time(|| intersection_polygons(&buffered, &nearby, FillRule::NonZero).map(|regions| regions.iter().map(|r| polygon_area(r).abs()).sum()).unwrap_or(0.0))
    };

    let sheet_contact = if probe.skips_sheet_contact(&candidate_bbox) { 0.0 } else { contact_against(sheet_neighborhood) };
    TIGHT_FIT_PART_CONTACT_WEIGHT * contact_against(parts_neighborhood) + TIGHT_FIT_SHEET_CONTACT_WEIGHT * sheet_contact
}

/// The sheet's own border, as an inward `TIGHT_FIT_PROBE_DISTANCE`-wide band
/// treated as a contact "obstacle" - so hugging a sheet edge/corner scores as
/// tight contact too, not just hugging another already-placed part. Shared
/// by `try_place_part_on_sheet`'s neighborhood construction (extended there
/// with already-placed parts) and `place_parts`'s first-part-on-a-sheet
/// tightest-rotation search below, where there are no already-placed parts
/// yet and this band is the *entire* neighborhood.
fn sheet_border_band(sheet: &LayeredPolygon) -> Vec<Vec<Point>> {
    let sheet_outer = offset_bevel(&sheet.points, TIGHT_FIT_PROBE_DISTANCE);
    difference_polygons(&sheet_outer, std::slice::from_ref(&sheet.points), FillRule::NonZero).unwrap_or_default()
}

/// `PlacementType::GravityTightFit`: `find_best_candidate` already gives the
/// single `Gravity`-best candidate; this widens that to every candidate
/// within tie tolerance of it (the same `almost_equal` notion of "tied"
/// `find_best_candidate`'s own x-tiebreak already uses), then - only if more
/// than one is actually tied - picks among just those by real contact area
/// instead of `find_best_candidate`'s plain x-position tiebreak. Falls back
/// to the plain `Gravity` champion untouched when nothing is tied with it,
/// so this never does more expensive work than pure `Gravity` needs for the
/// common case of a single clear winner.
fn find_best_hybrid_candidate(
    candidates: &[Candidate],
    excluded: &HashSet<usize>,
    probe: &TightFitProbe,
    part_bounds: Bounds,
    parts_neighborhood: &[(Bounds, Vec<Point>)],
    sheet_neighborhood: &[(Bounds, Vec<Point>)],
) -> Option<usize> {
    let champion_idx = find_best_candidate(candidates, excluded)?;
    let champion_area = candidates[champion_idx].score.area();

    let tied: Vec<usize> = (0..candidates.len())
        .filter(|idx| !excluded.contains(idx) && almost_equal(candidates[*idx].score.area(), champion_area, None))
        .collect();

    if tied.len() <= 1 {
        return Some(champion_idx);
    }

    tied.into_iter()
        .max_by(|&a, &b| {
            let contact_a = tight_fit_contact_area(probe, candidates[a].shiftvector, part_bounds, parts_neighborhood, sheet_neighborhood);
            let contact_b = tight_fit_contact_area(probe, candidates[b].shiftvector, part_bounds, parts_neighborhood, sheet_neighborhood);
            contact_a.total_cmp(&contact_b)
        })
        .or(Some(champion_idx))
}

struct Candidate {
    shiftvector: Placement,
    score: CandidateScore,
}

/// Port of `findBestCandidate`: replays the bar-climbing comparison the
/// scoring loop used, skipping already-`excluded` candidates. Must stay a
/// byte-for-byte match of the original comparison (including the
/// placement-type-independent x tiebreak) for deferred-validation retries to
/// reproduce what an interleaved validate-as-you-go loop would have picked.
fn find_best_candidate(candidates: &[Candidate], excluded: &HashSet<usize>) -> Option<usize> {
    let mut minarea: Option<f64> = None;
    let mut minwidth: Option<f64> = None;
    let mut minx: Option<f64> = None;
    let mut best: Option<usize> = None;

    for (idx, cand) in candidates.iter().enumerate() {
        if excluded.contains(&idx) {
            continue;
        }
        let area = cand.score.area();
        let x = cand.shiftvector.x;

        // No `.unwrap()`: the original relied on `minarea.is_none()` being
        // the *first* `||` operand and Rust short-circuiting past the
        // other operands' unwraps on the very first candidate - correct,
        // but only as long as nobody ever reorders these three operands. An
        // explicit `match` on `minarea` makes "nothing chosen yet" a real
        // branch instead of an implicit assumption, and the inner
        // `(Gravity, None)` combination - which the calling loop's own
        // invariants make unreachable in practice (`minwidth` is only ever
        // `None` for the whole call when every candidate is ConvexHull) -
        // degrades to a plain area comparison instead of panicking, rather
        // than asserting an invariant this function doesn't need to police.
        let take = match minarea {
            None => true,
            Some(current_minarea) => {
                let width_wins = match (&cand.score, minwidth) {
                    (CandidateScore::Gravity { width, .. }, Some(current_minwidth)) => {
                        *width < current_minwidth || (almost_equal(*width, current_minwidth, None) && area < current_minarea)
                    }
                    _ => area < current_minarea,
                };
                width_wins || minx.is_some_and(|current_minx| almost_equal(current_minarea, area, None) && x < current_minx)
            }
        };

        if take {
            minarea = Some(area);
            minwidth = cand.score.width();
            minx = Some(x);
            best = Some(idx);
        }
    }

    best
}

fn flush_pending_clips(final_nfp: &mut Vec<Vec<Point>>, pending_clips: &mut Vec<Vec<Point>>) -> bool {
    if pending_clips.is_empty() {
        return true;
    }
    match crate::profile::CLIP_DIFFERENCE.time(|| difference_polygons(final_nfp, pending_clips, FillRule::NonZero)) {
        Ok(result) => {
            *final_nfp = result;
            pending_clips.clear();
            true
        }
        Err(_) => false,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PlaceOnSheetResult {
    pub position: Placement,
    pub minarea: f64,
    pub minwidth: Option<f64>,
}

/// `try_place_part_on_sheet`'s result: three outcomes that used to all
/// collapse into a bare `None`, indistinguishable from one another - a
/// genuine "no valid, non-overlapping spot exists" (`NoRoom`) reads
/// completely differently from "a Clipper boolean op failed and this
/// attempt couldn't even be evaluated" (`GeometryError`), and only one of
/// those is worth ever surfacing as a diagnostic. Every current caller
/// treats both failure cases identically (via `.placed()` below), so this
/// changes nothing about behavior today - it just stops throwing the
/// distinction away at the one place that actually knows it.
#[derive(Clone, Copy, Debug)]
pub enum PlaceOnSheetOutcome {
    Placed(PlaceOnSheetResult),
    NoRoom,
    GeometryError,
}

impl PlaceOnSheetOutcome {
    /// Collapses `NoRoom`/`GeometryError` back into `None`, for callers
    /// that only ever wanted "did it fit" - matches this function's old
    /// `Option`-returning behavior exactly.
    pub fn placed(self) -> Option<PlaceOnSheetResult> {
        match self {
            PlaceOnSheetOutcome::Placed(result) => Some(result),
            PlaceOnSheetOutcome::NoRoom | PlaceOnSheetOutcome::GeometryError => None,
        }
    }
}

/// `TightFit`'s "neighborhood" against a given sheet/already-placed-obstacle
/// set - see `tight_fit_neighborhood`'s own doc comment. Depends only on
/// `sheet`/`placed`/`placement_type`, never on any candidate part's
/// rotation or position.
type TightFitNeighborhood = (Vec<(Bounds, Vec<Point>)>, Vec<(Bounds, Vec<Point>)>);

/// Builds `TightFit`'s "neighborhood", kept as two separate lists (not
/// merged) so `tight_fit_contact_area` can weight contact against an
/// already-placed part higher than contact against the empty sheet border -
/// see that function's own doc comment for why. Each polygon's bounding box
/// is precomputed alongside it so every candidate can cheaply cull down to
/// "only obstacles close enough to possibly touch" before paying for a real
/// Clipper call.
///
/// Depends only on `sheet`/`placed`/`placement_type` - never on a candidate
/// part's rotation or position - so a caller placing the same part at
/// several rotations in a row (`place_parts`'s 2nd+ part rotation search)
/// computes this once and reuses it across every rotation tried via
/// `try_place_part_on_sheet_with_neighborhood`, instead of paying for
/// `sheet_border_band`'s real `offset_bevel`/`difference_polygons` Clipper
/// call again on every single rotation - confirmed via code review to be a
/// real, avoidable cost on exactly the densely-packed-sheet jobs the
/// multi-rotation search itself targets.
fn tight_fit_neighborhood(sheet: &LayeredPolygon, placed: &[PlacedObstacle], placement_type: PlacementType) -> TightFitNeighborhood {
    if matches!(placement_type, PlacementType::TightFit | PlacementType::GravityTightFit | PlacementType::GravityCorrective) {
        let parts: Vec<(Bounds, Vec<Point>)> = placed
            .iter()
            .map(|o| shift_points(&o.polygon.points, o.placement.x, o.placement.y))
            .filter_map(|p| get_polygon_bounds(&p).map(|b| (b, p)))
            .collect();
        let border: Vec<(Bounds, Vec<Point>)> = sheet_border_band(sheet).into_iter().filter_map(|p| get_polygon_bounds(&p).map(|b| (b, p))).collect();
        (parts, border)
    } else {
        (Vec::new(), Vec::new())
    }
}

/// Port of `tryPlacePartOnSheet`. `place_parts` never calls this for a
/// sheet's first part (that stays the inline top-left-corner fast path,
/// same as the original) but `placed` being empty is otherwise handled
/// correctly here - `nesting::consolidation`'s cross-sheet relocation needs
/// that, since a relocation target isn't guaranteed to already have a part
/// on it.
///
/// Convenience wrapper computing its own `tight_fit_neighborhood` from
/// `sheet`/`placed`/`config.placement_type` - the right choice for any
/// caller placing at just one rotation (every test in this module,
/// `consolidation::refine_consolidation`'s single relocation attempt). A
/// caller trying several rotations of the *same* part/sheet/`placed` set in
/// a row should call `tight_fit_neighborhood` once and reuse
/// `try_place_part_on_sheet_with_neighborhood` directly instead - see that
/// function's own doc comment.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn try_place_part_on_sheet(
    part: &LayeredPolygon,
    part_source_id: usize,
    part_rotation: f64,
    sheet_nfp: &[Vec<Point>],
    sheet: &LayeredPolygon,
    placed: &[PlacedObstacle],
    config: &PlacementConfig,
    cache: &NfpCache,
    on_candidates: &(impl Fn(&[CandidateTrace]) + Sync),
) -> PlaceOnSheetOutcome {
    let neighborhood = tight_fit_neighborhood(sheet, placed, config.placement_type);
    try_place_part_on_sheet_with_neighborhood(part, part_source_id, part_rotation, sheet_nfp, sheet, placed, config, cache, on_candidates, &neighborhood)
}

/// Per-sheet memo of the accumulated no-fit region, keyed by the candidate
/// part's shape and rotation.
///
/// **This is where the engine's time goes.** Building `final_nfp` means
/// subtracting every already-placed obstacle's NFP from the sheet's - one
/// Clipper difference against up to a few hundred clip polygons. The scan
/// loop tries every *remaining* part against the same obstacle set, so on a
/// job with many copies of one shape (the common case: quantity > 1) that
/// identical clip is redone once per remaining part, per rotation. Measured
/// on the hat benchmark: 29,116 differences, 22.1s of thread time, ~95% of
/// the run.
///
/// `placed` only ever grows during a sheet's scan, and set difference is
/// associative, so a repeat call for the same (shape, rotation) can start
/// from the region it built last time and subtract only the obstacles added
/// since. Quadratic becomes linear.
///
/// Correctness rests on two invariants the caller must hold, which is why
/// this is `pub(crate)` and lives inside `place_parts`'s sheet loop rather
/// than being handed to arbitrary callers: `placed` is append-only for the
/// life of one accumulator, and one accumulator never spans two sheets.
#[derive(Default)]
pub(crate) struct NfpAccumulator {
    by_part: HashMap<(usize, i64), (Arc<Vec<Vec<Point>>>, usize)>,
    /// Contact score per candidate position, per shape/rotation.
    ///
    /// A candidate's `tight_fit_contact_area` can only change when a part is
    /// placed within `TIGHT_FIT_PROBE_DISTANCE` of it, and exactly one part
    /// is placed between two attempts on a sheet - so almost every candidate
    /// keeps its score. Measured on the hat benchmark before this existed:
    /// 90.3% of candidate positions recur between attempts and 86.0% are
    /// also untouched by the newly placed part, against a scoring path that
    /// was 89% of the whole run. Positions are keyed by exact bit pattern:
    /// unchanged parts of the no-fit region come back through Clipper's
    /// fixed-point grid deterministically, so an unchanged vertex is
    /// bit-identical, and a rounded key would risk reusing a score for a
    /// position that genuinely moved.
    contact: HashMap<(usize, i64), HashMap<(u64, u64), f64>>,
    /// Diagnostic only, populated when `profile::enabled()`: the candidate
    /// positions the previous attempt for each key produced, so the reuse
    /// rate of an incremental contact cache can be measured before one is
    /// built. Bit patterns, not rounded - unchanged parts of the region come
    /// back through Clipper's fixed-point grid deterministically, so an
    /// unchanged vertex should be bit-identical, and anything less exact
    /// would flatter the measurement.
    prev_candidates: HashMap<(usize, i64), std::collections::HashSet<(u64, u64)>>,
}

/// Recomputes a memoised contact score and panics if it moved, when
/// `NEST_VERIFY_CONTACT` is set. A no-op otherwise - checked through a cached
/// `OnceLock` rather than a `cfg`, so the same release binary can be run both
/// ways without a rebuild.
fn debug_assert_contact_unchanged(cached: f64, recompute: impl FnOnce() -> f64) {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*VERIFY.get_or_init(|| std::env::var("NEST_VERIFY_CONTACT").is_ok_and(|v| v != "0")) {
        return;
    }
    let fresh = recompute();
    assert!(
        (cached - fresh).abs() < 1e-9,
        "contact memo returned {cached} but recomputing gives {fresh} - a candidate's score changed without its position being invalidated"
    );
}

/// Counts how many of this attempt's candidate positions were already scored
/// last time for the same shape/rotation, and how many of those are also
/// untouched by the obstacles added since - i.e. how often an incremental
/// contact cache would hit. See `NfpAccumulator::prev_candidates`.
fn record_candidate_reuse(
    accumulator: &mut NfpAccumulator,
    key: (usize, i64),
    final_nfp: &[Vec<Point>],
    part: &LayeredPolygon,
    new_obstacles: &[PlacedObstacle],
) {
    let Some(part_bounds) = get_polygon_bounds(&part.points) else {
        return;
    };
    let origin = part.points[0];
    let new_bounds: Vec<Bounds> = new_obstacles
        .iter()
        .filter_map(|o| get_polygon_bounds(&shift_points(&o.polygon.points, o.placement.x, o.placement.y)))
        .collect();

    let mut positions = std::collections::HashSet::new();
    let previous = accumulator.prev_candidates.get(&key);
    let (mut total, mut repeated, mut unaffected) = (0u64, 0u64, 0u64);
    for region in final_nfp.iter() {
        for pt in region {
            total += 1;
            let bits = (pt.x.to_bits(), pt.y.to_bits());
            positions.insert(bits);
            if previous.is_some_and(|prev| prev.contains(&bits)) {
                repeated += 1;
                let candidate = Bounds { x: part_bounds.x + pt.x - origin.x, y: part_bounds.y + pt.y - origin.y, width: part_bounds.width, height: part_bounds.height };
                if !new_bounds.iter().any(|b| bounds_within_distance(&candidate, b, TIGHT_FIT_PROBE_DISTANCE)) {
                    unaffected += 1;
                }
            }
        }
    }
    crate::profile::CANDIDATES_TOTAL.add(total);
    crate::profile::CANDIDATES_REPEATED.add(repeated);
    crate::profile::CANDIDATES_UNAFFECTED.add(unaffected);
    accumulator.prev_candidates.insert(key, positions);
}

/// Same as `try_place_part_on_sheet`, but takes a precomputed
/// `TightFitNeighborhood` instead of building its own - see
/// `tight_fit_neighborhood`'s own doc comment for why/when a caller should
/// prefer this directly.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn try_place_part_on_sheet_with_neighborhood(
    part: &LayeredPolygon,
    part_source_id: usize,
    part_rotation: f64,
    sheet_nfp: &[Vec<Point>],
    sheet: &LayeredPolygon,
    placed: &[PlacedObstacle],
    config: &PlacementConfig,
    cache: &NfpCache,
    on_candidates: &(impl Fn(&[CandidateTrace]) + Sync),
    neighborhood: &TightFitNeighborhood,
) -> PlaceOnSheetOutcome {
    // A fresh accumulator: a one-off call has nothing to reuse, and this
    // keeps the public signature free of an optimisation detail that is only
    // sound under `place_parts`'s own append-only `placed` invariant.
    try_place_part_on_sheet_accumulated(part, part_source_id, part_rotation, sheet_nfp, sheet, placed, config, cache, on_candidates, neighborhood, &mut NfpAccumulator::default())
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub(crate) fn try_place_part_on_sheet_accumulated(
    part: &LayeredPolygon,
    part_source_id: usize,
    part_rotation: f64,
    sheet_nfp: &[Vec<Point>],
    sheet: &LayeredPolygon,
    placed: &[PlacedObstacle],
    config: &PlacementConfig,
    cache: &NfpCache,
    on_candidates: &(impl Fn(&[CandidateTrace]) + Sync),
    neighborhood: &TightFitNeighborhood,
    accumulator: &mut NfpAccumulator,
) -> PlaceOnSheetOutcome {
    let (tight_fit_parts_neighborhood, tight_fit_sheet_neighborhood) = neighborhood;
    let memo_key = (part_source_id, crate::cache_key::normalize_rotation(part_rotation));
    // Resume from the last region built for this shape/rotation, if it was
    // built against a prefix of the current obstacle set.
    let (mut final_nfp, already_subtracted): (Vec<Vec<Point>>, usize) = match accumulator.by_part.remove(&memo_key) {
        Some((regions, consumed)) if consumed <= placed.len() => ((*regions).clone(), consumed),
        _ => {
            // Starting over for this key - any cached contact scores were
            // measured against a different obstacle set.
            accumulator.contact.remove(&memo_key);
            (sheet_nfp.to_vec(), 0)
        }
    };
    let new_obstacles = &placed[already_subtracted..];

    // Obstacles with no holes just subtract from final_nfp - since set
    // difference commutes, consecutive holeless obstacles are batched into
    // one clipper call. Obstacles WITH holes still run one at a time
    // (difference, then union the hole-restore regions back in) so a later
    // obstacle can still cut into an earlier one's restored hole.
    let mut pending_clips: Vec<Vec<Point>> = Vec::new();
    let mut error = false;

    for obstacle in new_obstacles {
        let Some(cached) = crate::profile::OBSTACLE_NFP_LOOKUP
            .time(|| cached_obstacle_nfp(cache, &obstacle.polygon, obstacle.source_id, obstacle.rotation, part, part_source_id, part_rotation, config.curve_tolerance))
        else {
            error = true;
            break;
        };
        let CachedNfp::Outer { outer: nfp_outer, children: nfp_children } = &*cached else {
            error = true;
            break;
        };
        let outer = crate::profile::OBSTACLE_SHIFT.time(|| shift_points(nfp_outer, obstacle.placement.x, obstacle.placement.y));

        if nfp_children.is_empty() {
            pending_clips.push(outer);
            continue;
        }

        let children: Vec<Vec<Point>> = nfp_children.iter().map(|c| shift_points(c, obstacle.placement.x, obstacle.placement.y)).collect();

        if !flush_pending_clips(&mut final_nfp, &mut pending_clips) {
            error = true;
            break;
        }

        let after_diff = match crate::profile::CLIP_DIFFERENCE.time(|| difference_polygons(&final_nfp, std::slice::from_ref(&outer), FillRule::NonZero)) {
            Ok(r) => r,
            Err(_) => {
                error = true;
                break;
            }
        };

        final_nfp = match crate::profile::CLIP_UNION.time(|| union_polygons(&after_diff, &children, FillRule::NonZero)) {
            Ok(r) => r,
            Err(_) => {
                error = true;
                break;
            }
        };
    }

    if !error {
        error = !flush_pending_clips(&mut final_nfp, &mut pending_clips);
    }

    if error {
        return PlaceOnSheetOutcome::GeometryError;
    }
    if crate::profile::enabled() {
        record_candidate_reuse(accumulator, memo_key, &final_nfp, part, new_obstacles);
    }
    let final_nfp = Arc::new(final_nfp);
    accumulator.by_part.insert(memo_key, (Arc::clone(&final_nfp), placed.len()));
    if final_nfp.is_empty() {
        return PlaceOnSheetOutcome::NoRoom;
    }

    // choose the placement that results in the smallest bounding box/hull etc.
    let mut all_points: Vec<Point> = Vec::new();
    for obstacle in placed {
        for pt in &obstacle.polygon.points {
            all_points.push(Point::new(pt.x + obstacle.placement.x, pt.y + obstacle.placement.y));
        }
    }

    let all_bounds = get_polygon_bounds(&all_points);
    let part_bounds = get_polygon_bounds(&part.points);
    let placed_hull = if config.placement_type == PlacementType::ConvexHull && !all_points.is_empty() {
        Some(get_hull_or_fallback(&all_points))
    } else {
        None
    };

    // Once per call, not once per candidate - see `TightFitProbe`.
    let probe = TightFitProbe::new(part, sheet);

    // Drop the cached contact score of every candidate the newly placed
    // obstacles could have changed, then hand the rest of this scan a
    // mutable handle to what survived. `final_nfp` is an `Arc` handle rather
    // than a borrow precisely so this can be `&mut` while the scan runs.
    let contact_memo = accumulator.contact.entry(memo_key).or_default();
    if !new_obstacles.is_empty() && !contact_memo.is_empty() {
        let origin = part.points[0];
        let part_bbox = get_polygon_bounds(&part.points);
        let new_bounds: Vec<Bounds> = new_obstacles
            .iter()
            .filter_map(|o| get_polygon_bounds(&shift_points(&o.polygon.points, o.placement.x, o.placement.y)))
            .collect();
        match part_bbox {
            Some(part_bbox) => contact_memo.retain(|&(x_bits, y_bits), _| {
                let candidate = Bounds {
                    x: part_bbox.x + f64::from_bits(x_bits) - origin.x,
                    y: part_bbox.y + f64::from_bits(y_bits) - origin.y,
                    width: part_bbox.width,
                    height: part_bbox.height,
                };
                !new_bounds.iter().any(|b| bounds_within_distance(&candidate, b, TIGHT_FIT_PROBE_DISTANCE))
            }),
            // No bounds means no way to prove any entry still valid.
            None => contact_memo.clear(),
        }
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for region in final_nfp.iter() {
        for pt in region {
            let shiftvector = Placement {
                x: pt.x - part.points[0].x,
                y: pt.y - part.points[0].y,
            };

            // `GravityCorrective` isn't a real `CandidateScore` shape of its
            // own - it's `Gravity`'s scoring for the sheet's second part
            // (placed.len() <= 1) and `TightFit`'s for every part after
            // (the "correction" - see `PlacementType::GravityCorrective`'s
            // own doc comment). Mapped to whichever it means *here*, before
            // the score match below, so that match only ever needs to
            // handle the four real score shapes.
            let effective_score_type = match config.placement_type {
                PlacementType::GravityCorrective if placed.len() <= 1 => PlacementType::Gravity,
                PlacementType::GravityCorrective => PlacementType::TightFit,
                other => other,
            };

            let score = match effective_score_type {
                PlacementType::Gravity | PlacementType::Box | PlacementType::GravityTightFit => {
                    let part_bounds = part_bounds.expect("part always has points");
                    let candidate_part_corners = [
                        Point::new(part_bounds.x + shiftvector.x, part_bounds.y + shiftvector.y),
                        Point::new(part_bounds.x + part_bounds.width + shiftvector.x, part_bounds.y + shiftvector.y),
                        Point::new(
                            part_bounds.x + part_bounds.width + shiftvector.x,
                            part_bounds.y + part_bounds.height + shiftvector.y,
                        ),
                        Point::new(part_bounds.x + shiftvector.x, part_bounds.y + part_bounds.height + shiftvector.y),
                    ];
                    // `all_bounds` is `None` when nothing is placed yet (e.g.
                    // refineConsolidation relocating a part onto a sheet that
                    // - unlike place_parts's own first-part fast path, which
                    // never calls this function - could in principle be
                    // empty): there's no existing footprint to union with,
                    // so the candidate's own bounds are the whole answer.
                    // The original doesn't guard this at all (`allbounds.x`
                    // on a `null` `getPolygonBounds([])` would throw) - it
                    // just happens to never hit this path in practice, since
                    // every real caller keeps a target's placed list
                    // non-empty. Handling it here instead of relying on that
                    // same fragile guarantee is a deliberate improvement.
                    let rect_bounds = match all_bounds {
                        Some(all_bounds) => {
                            let rect_corners = [
                                Point::new(all_bounds.x, all_bounds.y),
                                Point::new(all_bounds.x + all_bounds.width, all_bounds.y),
                                Point::new(all_bounds.x + all_bounds.width, all_bounds.y + all_bounds.height),
                                Point::new(all_bounds.x, all_bounds.y + all_bounds.height),
                                candidate_part_corners[0],
                                candidate_part_corners[1],
                                candidate_part_corners[2],
                                candidate_part_corners[3],
                            ];
                            get_polygon_bounds(&rect_corners).expect("rect_corners always has exactly 8 points")
                        }
                        None => get_polygon_bounds(&candidate_part_corners).expect("candidate_part_corners always has exactly 4 points"),
                    };
                    if config.placement_type == PlacementType::Box {
                        CandidateScore::Box {
                            area: rect_bounds.width * rect_bounds.height,
                            width: rect_bounds.width,
                        }
                    } else {
                        // Gravity and GravityTightFit share this coarse
                        // score - GravityTightFit's own tie-break happens
                        // later, in find_best_hybrid_candidate, not here.
                        CandidateScore::Gravity {
                            area: rect_bounds.width * 5.0 + rect_bounds.height,
                            width: rect_bounds.width,
                        }
                    }
                }
                PlacementType::ConvexHull => {
                    let part_points: Vec<Point> = part.points.iter().map(|p| Point::new(p.x + shiftvector.x, p.y + shiftvector.y)).collect();
                    let combined_hull = match &placed_hull {
                        Some(h) => {
                            let mut merged = h.clone();
                            merged.extend(part_points);
                            get_hull_or_fallback(&merged)
                        }
                        None => get_hull_or_fallback(&part_points),
                    };
                    CandidateScore::ConvexHull { area: polygon_area(&combined_hull).abs() }
                }
                PlacementType::TightFit => {
                    let part_bounds = part_bounds.expect("part always has points");
                    let position = (pt.x.to_bits(), pt.y.to_bits());
                    let contact_area = match contact_memo.get(&position) {
                        Some(&cached) => {
                            // `NEST_VERIFY_CONTACT=1` recomputes every hit and
                            // compares. The memo's invalidation rule is meant
                            // to be exact, not approximate - "the benchmark
                            // still matches" would only prove it for one
                            // config, while this checks every candidate of
                            // every attempt. Off by default: it makes the
                            // memo a pure cost.
                            debug_assert_contact_unchanged(cached, || {
                                tight_fit_contact_area(&probe, shiftvector, part_bounds, tight_fit_parts_neighborhood, tight_fit_sheet_neighborhood)
                            });
                            cached
                        }
                        None => {
                            let computed = crate::profile::CANDIDATE_SCORING
                                .time(|| tight_fit_contact_area(&probe, shiftvector, part_bounds, tight_fit_parts_neighborhood, tight_fit_sheet_neighborhood));
                            contact_memo.insert(position, computed);
                            computed
                        }
                    };
                    CandidateScore::TightFit { area: -contact_area }
                }
                PlacementType::GravityCorrective => unreachable!("mapped to Gravity or TightFit above"),
            };

            candidates.push(Candidate { shiftvector, score });
        }
    }

    // Overlap check deferred until after the full scan finds the true
    // best-by-heuristic, instead of re-validating every transient champion -
    // retries against the next-best on a rare validation failure (NFP-derived
    // candidates can still overlap once checked against actual part geometry,
    // due to floating-point/Clipper-scaling artifacts near boundaries).
    let mut excluded: HashSet<usize> = HashSet::new();
    loop {
        let champion = if config.placement_type == PlacementType::GravityTightFit {
            find_best_hybrid_candidate(&candidates, &excluded, &probe, part_bounds.expect("part always has points"), tight_fit_parts_neighborhood, tight_fit_sheet_neighborhood)
        } else {
            find_best_candidate(&candidates, &excluded)
        };
        let champion_idx = match champion {
            Some(idx) => idx,
            // Every candidate has been tried and excluded (all of them
            // overlapped once checked against real geometry) - genuinely
            // nowhere left to place this part, not a computation failure.
            None => {
                on_candidates(&trace_candidates(&candidates, None, part_rotation));
                return PlaceOnSheetOutcome::NoRoom;
            }
        };
        let champion = &candidates[champion_idx];
        let shiftvector = champion.shiftvector;
        let test_shifted = shift_layered_polygon(part, shiftvector.x, shiftvector.y);

        let mut is_overlapping = has_material_outside_sheet(&test_shifted, sheet);
        if !is_overlapping {
            for obstacle in placed {
                let placed_shifted = shift_layered_polygon(&obstacle.polygon, obstacle.placement.x, obstacle.placement.y);
                if has_material_overlap(&test_shifted, &placed_shifted) {
                    is_overlapping = true;
                    break;
                }
            }
        }

        if !is_overlapping {
            on_candidates(&trace_candidates(&candidates, Some(champion_idx), part_rotation));
            return PlaceOnSheetOutcome::Placed(PlaceOnSheetResult {
                position: shiftvector,
                minarea: champion.score.area(),
                minwidth: champion.score.width(),
            });
        }

        excluded.insert(champion_idx);
    }
}

/// Flattens `try_place_part_on_sheet`'s internal `Candidate` list into the
/// public `CandidateTrace` shape, marking `accepted_idx` (if any) as the one
/// that won. Kept as its own function rather than inlined at both call
/// sites above, since a real overlap-retry means the champion the caller
/// cares about isn't necessarily `candidates`' first or only entry.
fn trace_candidates(candidates: &[Candidate], accepted_idx: Option<usize>, rotation: f64) -> Vec<CandidateTrace> {
    candidates
        .iter()
        .enumerate()
        .map(|(idx, c)| CandidateTrace { x: c.shiftvector.x, y: c.shiftvector.y, rotation, score: c.score.area(), accepted: Some(idx) == accepted_idx })
        .collect()
}

/// Port of `placeParts`: opens sheets once and never revisits them (a part
/// that doesn't fit the current sheet is deferred to a new one). Single
/// individual, no GA, no threads - Phase 3's first end-to-end milestone.
///
/// `cache` should be the *same* `NfpCache` across every individual and
/// generation of one nest run (not a fresh one per call) - that's what lets
/// the same (part id, part id, rotation, rotation) combination recurring
/// across the GA's many individuals actually hit instead of recomputing
/// every time. A single call still benefits too (repeated obstacle/sheet
/// pairs within one sheet's own placement pass).
///
/// `should_cancel` is checked once per part attempt (not just once per
/// whole call, the way `dispatch::run_generation` checks it between
/// individuals) - a single individual's full placement can itself take
/// seconds against real geometry, and a caller wanting Stop to actually
/// behave like a kill switch needs this call to bail out of its own
/// in-progress work quickly, not just be skipped before it starts. Returns
/// `None` if cancelled partway through - a partial placement (some parts
/// tried, the rest never even attempted) isn't a meaningful result to score
/// or compare against other individuals, so it's discarded entirely rather
/// than returned as if it were a genuine, fully-evaluated attempt.
///
/// `on_part_placed(sheet_index, &placed_part)` fires immediately after each
/// individual part is placed (both the first-part top-left-corner fast
/// path and the general `try_place_part_on_sheet` path below) - a step-by-
/// step observation hook for a caller that wants to watch placement happen
/// one part at a time (e.g. a visualization), not just receive the final
/// `PlaceResult`. Every non-visualization caller passes a no-op
/// (`&|_, _| {}`); this adds no behavior of its own.
///
/// `on_candidates(sheet_index, part_id, &candidates)` fires right alongside
/// `on_part_placed` (before it, on the same part attempt) with every
/// rotation/position `try_place_part_on_sheet` actually scored for that
/// part - not just the one that won. The sheet's first part (both the
/// plain top-left-corner fast path and the TightFit-family rotation search)
/// don't go through `try_place_part_on_sheet` at all; the former reports an
/// empty candidate list (it does no scoring, just picks the first valid NFP
/// vertex), the latter reports every rotation/position it actually
/// compared. Every non-visualization caller passes a no-op
/// (`&|_, _, _| {}`).
///
/// The `>=` boundary (not `>`) in the rotation-wraparound arithmetic used
/// throughout is a load-bearing quirk carried over from the original JS -
/// see CLAUDE.md's "Load-bearing quirks" list.
fn advance_rotation(current: f64, step: f64) -> f64 {
    let r = current + step;
    if r >= 360.0 {
        r % 360.0
    } else {
        r
    }
}

/// Would placing `part` (already rotated to its final angle) at `at` be a
/// legal placement on `sheet`, given everything else already on it?
///
/// The authority behind the UI's drag-a-part feedback. Deliberately answered
/// here rather than approximated in the frontend: this is the *same*
/// `has_material_overlap`/`has_material_outside_sheet` pair the placement
/// engine itself accepts or rejects candidates with, so a hand-placed part
/// is held to exactly the standard an engine-placed one is - including
/// margin/spacing, since the caller passes the same padded geometry the
/// engine works on.
#[must_use]
pub fn placement_is_valid(sheet: &LayeredPolygon, part: &LayeredPolygon, at: Placement, others: &[PlacedObstacle]) -> bool {
    let shifted = shift_layered_polygon(part, at.x, at.y);
    if has_material_outside_sheet(&shifted, sheet) {
        return false;
    }
    others.iter().all(|other| {
        let other_shifted = shift_layered_polygon(&other.polygon, other.placement.x, other.placement.y);
        !has_material_overlap(&shifted, &other_shifted)
    })
}

/// The sheet's own bounding box - what the band packer fills.
///
/// A band layout is rectangle arithmetic, so a non-rectangular sheet (a
/// remnant, say) is treated as its bounding box and the resulting placements
/// are simply worse, not wrong: the caller compares them against the greedy
/// pass and keeps whichever won, and on a non-rectangular sheet that will be
/// the greedy one.
fn sheet_usable_bounds(sheet: &LayeredPolygon) -> Bounds {
    get_polygon_bounds(&sheet.points).unwrap_or(Bounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 })
}

#[must_use]
pub fn place_parts(
    sheets: &[LayeredPolygon],
    parts: Vec<NestPart>,
    config: &PlacementConfig,
    cache: &NfpCache,
    should_cancel: &(impl Fn() -> bool + Sync),
    on_part_placed: &(impl Fn(usize, &PlacedPart) + Sync),
    on_candidates: &(impl Fn(usize, usize, &[CandidateTrace]) + Sync),
) -> Option<PlaceResult> {
    let mut parts: Vec<NestPart> = parts
        .into_iter()
        .map(|p| NestPart {
            id: p.id,
            source_id: p.source_id,
            polygon: crate::profile::ROTATE_PART.time(|| rotate_layered_polygon(&p.polygon, p.rotation)),
            rotation: p.rotation,
        })
        .collect();

    let mut total_sheet_area = 0.0;
    let mut total_usable_sheet_area = 0.0;
    let mut total_placed_area = 0.0;
    let mut fitness = 0.0;
    let mut all_placements: Vec<SheetPlacement> = Vec::new();

    // `PlacementType::GravityCorrective`'s rotation-reuse cache: once a
    // shape (source_id) has placed successfully at some rotation, a later
    // part sharing that source_id starts its own search there instead of
    // its own assigned starting rotation - see the cache-consult site
    // below for why this is safe against the rotation-angle grid. Empty
    // and unused for every other placement type.
    let mut rotation_by_source: HashMap<usize, f64> = HashMap::new();

    let mut cancelled_early = false;
    let mut sheet_idx = 0usize;
    while !parts.is_empty() {
        if sheet_idx >= sheets.len() {
            break;
        }
        if should_cancel() {
            cancelled_early = true;
            break;
        }
        let sheet = &sheets[sheet_idx];
        let sheet_src = sheet_source(sheet_idx);
        let sheet_area = polygon_area(&sheet.points).abs();
        let sheet_usable_area = polygon_material_area(sheet);
        total_sheet_area += sheet_area;
        total_usable_sheet_area += sheet_usable_area;
        fitness += sheet_area;

        let mut placed: Vec<PlacedObstacle> = Vec::new();
        // One accumulator per sheet: `placed` below is append-only for this
        // sheet's whole scan and starts empty for the next one, which is
        // exactly `NfpAccumulator`'s contract.
        let mut nfp_accumulator = NfpAccumulator::default();
        // Which slots of `parts` (indices, stable across this sheet's scan
        // since nothing removes elements mid-scan) got placed this pass -
        // NOT which ids: unlike the original's `parts.indexOf(placed[i])` +
        // `splice` (removal by object identity), keying removal off `.id`
        // would delete every part sharing an id with whatever got placed,
        // silently dropping untried duplicate-id siblings (quantity > 1 of
        // the same part is normal usage; nothing requires ids to be unique).
        let mut placed_indices: Vec<usize> = Vec::new();
        // The part list as it stands before the greedy pass mutates rotations
        // in place - `banded::pack_sheet` needs the same starting state to be
        // a fair comparison, and the greedy loop rewrites `parts[i]` as it
        // rotates trial candidates.
        let parts_before_sheet: Vec<NestPart> = if config.banded_pass { parts.clone() } else { Vec::new() };
        let mut placed_parts_out: Vec<PlacedPart> = Vec::new();
        let mut minwidth: Option<f64> = None;
        let mut minarea: Option<f64> = None;

        let mut i = 0;
        while i < parts.len() {
            if should_cancel() {
                cancelled_early = true;
                break;
            }

            // A first part on a sheet under TightFit/GravityTightFit/
            // GravityCorrective gets its own dedicated search: check every
            // configured rotation for which one hugs the sheet's own
            // corner/edges tightest, instead of the generic loop just below
            // (which stops at whichever rotation happens to fit *first*,
            // then takes that rotation's top-left-most point - ported as-is
            // from the original app, fine for Gravity/Box/ConvexHull's
            // aggregate bounding-box scores, but self-defeating for a
            // contact-based type's whole point on an irregular part:
            // confirmed against a real fixture where the first-fit rotation
            // left visibly more slack in the sheet corner than another
            // configured rotation would have). Every later part on the
            // sheet already gets this same contact-aware treatment via
            // `try_place_part_on_sheet`; this is only needed because *that*
            // function is never called for a sheet's first part - which
            // also means the rotation-reuse cache below never applies here
            // either, only from the second part onward (this search always
            // runs fresh, regardless of any cached rotation for the same
            // shape). Skipped entirely when there's only one configured
            // rotation - nothing to compare.
            //
            // Extending this to `GravityCorrective` (not just TightFit/
            // GravityTightFit) turned out to be the fix for a real
            // benchmark regression: on a real 170-part/~100-sheet job
            // (`tests/fixtures/FLAT.dxf`+`FLAT-struck.dxf`) averaging only
            // ~1.7 parts per sheet, most sheets never reach a 3rd part at
            // all, so the *first* part's placement quality dominates -
            // before this, GravityCorrective's first part used the plain
            // top-left fast path (no rotation comparison) and consistently
            // landed on 100 sheets/82.4% utilisation vs. plain TightFit's
            // 98/84.1%, while also running ~1.7x slower per generation.
            // With this fix it matches TightFit's 98-99 sheets/83-84%
            // (three repeat runs, all converged deterministically - this
            // job's search space isn't noisy enough for run-to-run luck to
            // explain the gap either way).
            if placed.is_empty() && config.rotations > 1 && matches!(config.placement_type, PlacementType::TightFit | PlacementType::GravityTightFit | PlacementType::GravityCorrective) {
                let border_neighborhood: Vec<(Bounds, Vec<Point>)> =
                    sheet_border_band(sheet).into_iter().filter_map(|p| get_polygon_bounds(&p).map(|b| (b, p))).collect();
                // Both are set from the first `rotation_steps` entry below,
                // whose delta is always 0 - i.e. they start exactly where the
                // part already is.
                let mut trial_rotation;
                let mut trial_polygon = parts[i].polygon.clone();
                // (contact_area, position, rotation, polygon) of the best
                // rotation/position seen so far - contact_area first so a
                // genuinely tighter rotation always wins; among near-ties
                // (within FIRST_PART_CONTACT_TOLERANCE of each other, not
                // just exactly equal), the same top-left-most tiebreak the
                // generic path below uses.
                //
                // Widened from an exact-equality tie ("almost_equal") to a
                // relative tolerance band: `sheet_border_band` treats all
                // four edges as equally attractive contact, so for an
                // irregular part the single rotation/corner that happens to
                // nestle marginally tighter than the rest wins outright,
                // anchoring the sheet's *entire* pack wherever that
                // happened to be - not necessarily anywhere near the
                // origin. Every part placed after this one only ever
                // extends the growing cluster (try_place_part_on_sheet's own
                // contact scoring, weighted 2x toward touching existing
                // parts over the sheet border - see TIGHT_FIT_PART_CONTACT_
                // WEIGHT), so a low-density job (a sheet with far more room
                // than the parts need) ends up as one tight blob parked in
                // whatever corner this first search preferred, leaving most
                // of the sheet empty - confirmed against a real 20-part/
                // 500x500 job that clustered entirely into x=[328,500]/
                // y=[304,500], nowhere near the origin. A genuinely
                // much-tighter-fitting corner elsewhere still wins outright
                // (this only changes *near-ties*), so this doesn't touch the
                // dense-packing case (a real 252-part/500x500 tessellation)
                // where the best corner is unambiguous either way.
                const FIRST_PART_CONTACT_TOLERANCE: f64 = 0.05; // 5%
                let mut best: Option<(f64, Placement, f64, LayeredPolygon)> = None;
                let mut candidate_traces: Vec<CandidateTrace> = Vec::new();
                let mut best_trace_idx: Option<usize> = None;
                for (angle, delta) in rotation_steps(config, parts[i].id, parts[i].rotation) {
                    if delta != 0.0 {
                        trial_polygon = rotate_layered_polygon(&trial_polygon, delta);
                    }
                    trial_rotation = angle;
                    // Same reasoning as the 2nd+ part rotation loop further down:
                    // each iteration is a real Clipper-backed inner-NFP lookup
                    // (plus a contact-area scan per returned vertex on a cache
                    // miss), so without this a Stop request could still have to
                    // wait out up to `config.rotations` of them before the
                    // caller sees it.
                    if should_cancel() {
                        cancelled_early = true;
                        break;
                    }
                    if let Some(nfp) = cached_inner_nfp(cache, sheet, sheet_src, &trial_polygon, parts[i].source_id, trial_rotation, config.curve_tolerance) {
                        if !nfp.is_empty() {
                            let trial_bounds = get_polygon_bounds(&trial_polygon.points).expect("part always has points");
                            // Once per rotation, not once per candidate vertex.
                            let trial_probe = TightFitProbe::new(&trial_polygon, sheet);
                            for region in &nfp {
                                for pt in region {
                                    let candidate = Placement { x: pt.x - trial_polygon.points[0].x, y: pt.y - trial_polygon.points[0].y };
                                    let shifted = shift_layered_polygon(&trial_polygon, candidate.x, candidate.y);
                                    if has_material_outside_sheet(&shifted, sheet) {
                                        continue;
                                    }
                                    let contact = tight_fit_contact_area(&trial_probe, candidate, trial_bounds, &[], &border_neighborhood);
                                    let better = match &best {
                                        None => true,
                                        Some((best_contact, best_pos, ..)) => {
                                            let tolerance = contact.max(*best_contact) * FIRST_PART_CONTACT_TOLERANCE;
                                            contact > *best_contact + tolerance
                                                || (contact >= *best_contact - tolerance
                                                    && (candidate.x < best_pos.x || (almost_equal(candidate.x, best_pos.x, None) && candidate.y < best_pos.y)))
                                        }
                                    };
                                    // Negated, same as `CandidateScore::TightFit` - keeps
                                    // "lower score wins" a universal convention across
                                    // every `CandidateTrace`, not just the ones that went
                                    // through `try_place_part_on_sheet`'s own scoring.
                                    candidate_traces.push(CandidateTrace { x: candidate.x, y: candidate.y, rotation: trial_rotation, score: -contact, accepted: false });
                                    if better {
                                        best = Some((contact, candidate, trial_rotation, trial_polygon.clone()));
                                        best_trace_idx = Some(candidate_traces.len() - 1);
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(idx) = best_trace_idx {
                    candidate_traces[idx].accepted = true;
                }
                on_candidates(sheet_idx, parts[i].id, &candidate_traces);

                let Some((_, position, rotation, polygon)) = best else {
                    i += 1;
                    continue;
                };

                placed_indices.push(i);
                let placed_part = PlacedPart { id: parts[i].id, placement: position, rotation };
                placed_parts_out.push(placed_part);
                on_part_placed(sheet_idx, &placed_part);
                let part_area = polygon_area(&polygon.points).abs();
                placed.push(PlacedObstacle { polygon: polygon.clone(), id: parts[i].id, source_id: parts[i].source_id, rotation, placement: position });
                parts[i] = NestPart { id: parts[i].id, source_id: parts[i].source_id, polygon, rotation };

                if part_area >= config.dominant_part_area_threshold * sheet_area {
                    break;
                }
                i += 1;
                continue;
            }

            // Rotation-reuse cache (GravityCorrective only, see
            // `rotation_by_source`'s own comment above): start the search
            // for this part from a rotation already known to place this
            // exact shape, instead of its own assigned starting rotation -
            // very often the first attempt below then fits immediately,
            // skipping the rest of the grid entirely. Safe to just
            // overwrite the starting rotation and let the loop below run
            // unchanged: that loop always completes a full cycle of
            // `config.rotations` grid steps on a miss regardless of where
            // it starts, so a fallback still tries every configured angle -
            // just in a different order, not a smaller set.
            if config.placement_type == PlacementType::GravityCorrective {
                if let Some(&cached_rotation) = rotation_by_source.get(&parts[i].source_id) {
                    if !almost_equal(cached_rotation, parts[i].rotation, None) {
                        let delta = cached_rotation - parts[i].rotation;
                        parts[i] = NestPart {
                            id: parts[i].id,
                            source_id: parts[i].source_id,
                            polygon: rotate_layered_polygon(&parts[i].polygon, delta),
                            rotation: cached_rotation,
                        };
                    }
                }
            }

            if placed.is_empty() {
                // Inner NFP, trying all configured rotations until the part
                // fits the sheet at all - there's nothing placed yet to
                // score contact/tightness against, so "first rotation that
                // fits" is as good as any other here (unlike the 2nd+ part
                // case below, where which rotation wins is the whole point).
                let mut sheet_nfp: Option<Vec<Vec<Point>>> = None;
                for (angle, delta) in rotation_steps(config, parts[i].id, parts[i].rotation) {
                    if delta != 0.0 {
                        parts[i] = NestPart {
                            id: parts[i].id,
                            source_id: parts[i].source_id,
                            polygon: rotate_layered_polygon(&parts[i].polygon, delta),
                            rotation: angle,
                        };
                    }
                    sheet_nfp = cached_inner_nfp(cache, sheet, sheet_src, &parts[i].polygon, parts[i].source_id, parts[i].rotation, config.curve_tolerance);
                    if sheet_nfp.as_ref().is_some_and(|n| !n.is_empty()) {
                        break;
                    }
                }

                let sheet_nfp = match sheet_nfp {
                    Some(n) if !n.is_empty() => n,
                    _ => {
                        i += 1;
                        continue;
                    }
                };

                // Borrowed, not cloned, until a placement is actually confirmed -
                // most evaluated parts on a busy sheet fail to place (no room,
                // overlap, wrong rotation), so cloning up front paid for a full
                // polygon copy (points + recursive hole children) on the common
                // reject path for nothing.
                let part = &parts[i].polygon;

                // first placement on this sheet: top-left corner
                let mut position: Option<Placement> = None;
                for region in &sheet_nfp {
                    for pt in region {
                        let candidate = Placement {
                            x: pt.x - part.points[0].x,
                            y: pt.y - part.points[0].y,
                        };
                        let shifted = shift_layered_polygon(part, candidate.x, candidate.y);
                        if has_material_outside_sheet(&shifted, sheet) {
                            continue;
                        }
                        let better = match position {
                            None => true,
                            Some(p) => candidate.x < p.x || (almost_equal(candidate.x, p.x, None) && candidate.y < p.y),
                        };
                        if better {
                            position = Some(candidate);
                        }
                    }
                }

                let Some(position) = position else {
                    i += 1;
                    continue;
                };

                placed_indices.push(i);
                let placed_part = PlacedPart { id: parts[i].id, placement: position, rotation: parts[i].rotation };
                placed_parts_out.push(placed_part);
                on_part_placed(sheet_idx, &placed_part);
                // No scoring happens on this fast path (first valid NFP
                // vertex wins outright) - nothing to report as candidates.
                on_candidates(sheet_idx, parts[i].id, &[]);
                let part_area = polygon_area(&part.points).abs();
                placed.push(PlacedObstacle {
                    polygon: parts[i].polygon.clone(),
                    id: parts[i].id,
                    source_id: parts[i].source_id,
                    rotation: parts[i].rotation,
                    placement: position,
                });
                if config.placement_type == PlacementType::GravityCorrective {
                    rotation_by_source.insert(parts[i].source_id, parts[i].rotation);
                }

                // This part alone already claims most of the sheet - close it now.
                if part_area >= config.dominant_part_area_threshold * sheet_area {
                    break;
                }
                i += 1;
                continue;
            }

            // 2nd+ part on this sheet: try every configured rotation, each
            // scored by try_place_part_on_sheet's real obstacle-aware
            // contact/area metric, and commit to whichever rotation+position
            // scores best - not just whichever rotation happens to fit the
            // sheet's bare remaining shape first, which is all a single
            // `try_place_part_on_sheet` call used to compare. This is the
            // same "which orientation actually fits best here" question the
            // dedicated first-part TightFit-family search above already
            // answers for a sheet's first part; every part after it used to
            // commit to one rotation before any position/score comparison
            // ever happened at all - no measurement of whether a different
            // orientation would sit tighter at this specific spot. Same NFP
            // cache as everywhere else in this file, so trying
            // `config.rotations` angles here is mostly cache hits after the
            // first few parts of any given shape have been tried.
            let mut trial_rotation;
            let mut trial_polygon = parts[i].polygon.clone();
            // (score, result, rotation, polygon) of the best rotation seen so
            // far - `result.minarea` is always `CandidateScore::area()`'s raw
            // number (lower wins), the same convention every other
            // comparison in this file already uses.
            //
            // Known, accepted gap for `GravityTightFit` specifically: within
            // one rotation's own candidates, `find_best_hybrid_candidate`
            // breaks near-ties by real contact area, not just `minarea`'s
            // coarse Gravity score - but that contact-area tiebreak never
            // carries across rotations here, since `minarea` (Gravity's
            // score) is all this cross-rotation comparison sees. Two
            // different rotations with near-identical Gravity scores (a
            // realistic outcome for a symmetric-ish part) get resolved by
            // whichever is infinitesimally smaller, not by which one
            // actually sits tighter. `TightFit`/`GravityCorrective` aren't
            // affected - their own `minarea` already *is* the real contact
            // score.
            let mut best: Option<(f64, PlaceOnSheetResult, f64, LayeredPolygon)> = None;
            // `Mutex`, not a plain `Vec`: `try_place_part_on_sheet` requires
            // its `on_candidates` hook to be `Sync` (it's called from
            // `dispatch`'s `par_iter()` across individuals, even though any
            // *one* `place_parts` call like this one is itself single-
            // threaded) - same pattern already used for exactly this reason
            // elsewhere (e.g. `commands.rs`'s `retrace_generation`).
            let rotation_traces: std::sync::Mutex<Vec<CandidateTrace>> = std::sync::Mutex::new(Vec::new());
            // Computed once, outside the rotation loop below - it depends
            // only on `sheet`/`placed`/`config.placement_type`, never on
            // which rotation is currently being tried, so recomputing it
            // per rotation (as calling the plain `try_place_part_on_sheet`
            // wrapper in a loop would) paid for `sheet_border_band`'s real
            // Clipper offset/difference call `config.rotations` times over
            // for no reason - exactly the densely-packed-sheet workload this
            // rotation search itself targets.
            let neighborhood = tight_fit_neighborhood(sheet, &placed, config.placement_type);

            for (angle, delta) in rotation_steps(config, parts[i].id, parts[i].rotation) {
                if delta != 0.0 {
                    trial_polygon = rotate_layered_polygon(&trial_polygon, delta);
                }
                trial_rotation = angle;
                // Checked every iteration, not just once per part: each
                // iteration is a real Clipper-backed placement attempt
                // (`try_place_part_on_sheet_with_neighborhood`), so without
                // this a Stop request could still have to wait out up to
                // `config.rotations` of them before the caller sees it -
                // same responsiveness contract as the per-part check above.
                if should_cancel() {
                    cancelled_early = true;
                    break;
                }
                if let Some(sheet_nfp) = cached_inner_nfp(cache, sheet, sheet_src, &trial_polygon, parts[i].source_id, trial_rotation, config.curve_tolerance) {
                    if !sheet_nfp.is_empty() {
                        let outcome = try_place_part_on_sheet_accumulated(
                            &trial_polygon,
                            parts[i].source_id,
                            trial_rotation,
                            &sheet_nfp,
                            sheet,
                            &placed,
                            config,
                            cache,
                            &|candidates| rotation_traces.lock().expect("single-threaded call, lock never contested").extend_from_slice(candidates),
                            &neighborhood,
                            &mut nfp_accumulator,
                        );
                        if let Some(result) = outcome.placed() {
                            // `total_cmp`, not a bare `<`: this codebase treats bare
                            // float `<` against a possibly-NaN value as a real gap
                            // elsewhere (see this module's own `Option<f64>` fitness
                            // handling) - NaN sorts as "never wins" here, not silently
                            // passed through as an unexamined tie.
                            let better = match &best {
                                None => true,
                                Some((best_score, ..)) => result.minarea.total_cmp(best_score).is_lt(),
                            };
                            if better {
                                best = Some((result.minarea, result, trial_rotation, trial_polygon.clone()));
                            }
                        }
                    }
                }
            }

            let mut rotation_traces = rotation_traces.into_inner().expect("single-threaded call, lock never poisoned");

            if let Some((_, result, rotation, polygon)) = best {
                // Only the overall winning rotation's champion candidate
                // should read as accepted - `try_place_part_on_sheet` marks
                // its own per-call champion for whichever single rotation it
                // was scoring at the time, which isn't necessarily this
                // loop's best-across-rotations winner.
                for trace in &mut rotation_traces {
                    trace.accepted =
                        almost_equal(trace.rotation, rotation, None) && almost_equal(trace.x, result.position.x, None) && almost_equal(trace.y, result.position.y, None);
                }
                on_candidates(sheet_idx, parts[i].id, &rotation_traces);

                placed_indices.push(i);
                let placed_part = PlacedPart { id: parts[i].id, placement: result.position, rotation };
                placed_parts_out.push(placed_part);
                on_part_placed(sheet_idx, &placed_part);
                placed.push(PlacedObstacle { polygon: polygon.clone(), id: parts[i].id, source_id: parts[i].source_id, rotation, placement: result.position });
                if config.placement_type == PlacementType::GravityCorrective {
                    rotation_by_source.insert(parts[i].source_id, rotation);
                }
                parts[i] = NestPart { id: parts[i].id, source_id: parts[i].source_id, polygon, rotation };
                minarea = Some(result.minarea);
                minwidth = result.minwidth;
            } else {
                on_candidates(sheet_idx, parts[i].id, &rotation_traces);
            }

            i += 1;
        }

        // Explicit decision (Phase 3 - see docs/PORT_STATUS.md's "NaN-fitness
        // gap" gotcha): minarea/minwidth are only ever set by the >=2nd-part
        // scoring branch above. The original's `(minwidth||0)/sheetarea +
        // (minarea||0)` leaned on JS's undefined-is-falsy coercion to avoid
        // NaN poisoning the running fitness total; `Option<f64>::unwrap_or`
        // makes the same zero-contribution choice explicit instead of
        // implicit for a sheet where 0-1 parts got placed.
        fitness += (minwidth.unwrap_or(0.0) / sheet_area) + minarea.unwrap_or(0.0);

        // Reward how much of THIS sheet actually got used, not just the
        // bounding-box shape of the last part placed on it - `minarea`/
        // `minwidth` above are a per-candidate positioning tiebreak (ported
        // from SVGnest as-is), not a measure of the sheet's overall packing
        // quality, so two same-sheet-count solutions that both place every
        // part could score almost identically regardless of how much slack
        // either one leaves behind - there was no gradient actually pushing
        // the GA toward denser packing once "does everyone fit" was
        // satisfied. Normalized by `sheet_area` (same convention
        // `minwidth/sheet_area` above already uses), so this stays a
        // same-sheet-count tiebreak, never a sheet-count override: even a
        // sheet left almost entirely empty contributes at most ~1.0, versus
        // `sheet_area` itself (into the hundreds of thousands for a real
        // sheet) charged once per *additional* sheet opened - opening one
        // more sheet can never pay for itself via a better leftover score
        // on this one.
        let mut sheet_placed_area: f64 = placed.iter().map(|p| polygon_material_area(&p.polygon)).sum();
        let leftover = (sheet_usable_area - sheet_placed_area).max(0.0);
        fitness += leftover / sheet_area;

        total_placed_area += sheet_placed_area;

        // Second opinion: a band/shelf layout of the same sheet from the same
        // remaining parts. The greedy pass above is contact-driven and cannot
        // represent "fill a band in one orientation, then switch the rest of
        // the sheet to the other" - a structure worth 88% where greedy
        // plateaus at 76% on rectangle-ish parts (see `crate::banded`). It is
        // equally true that band packing is hopeless on interlocking shapes,
        // which is why this is a comparison and not a replacement: whichever
        // pass put more true material on this sheet wins it.
        // Skipped when any part in play is *dominant* - a part large enough
        // to close its sheet on its own (`dominant_part_area_threshold`). That
        // rule is deliberate, documented behaviour the greedy pass implements
        // with an early break, and the band packer knows nothing about it: it
        // would happily fill the rest of the sheet and silently repeal a
        // decision the user configured.
        let has_dominant = parts_before_sheet.iter().any(|p| polygon_area(&p.polygon.points).abs() >= config.dominant_part_area_threshold * sheet_area);
        if config.banded_pass && !has_dominant {
            if let Some(banded) = crate::banded::pack_sheet(sheet_usable_bounds(sheet), &parts_before_sheet, config.curve_tolerance) {
                // Scored on the *same* measure the greedy pass reports -
                // `polygon_material_area`, holes subtracted - not on the band
                // packer's own padded-outline figure. Comparing those two
                // directly let the band pass win sheets it had actually lost,
                // because a padded outline is simply bigger.
                let banded_material: f64 = banded
                    .consumed
                    .iter()
                    .filter_map(|&i| parts_before_sheet.get(i))
                    .map(|p| polygon_material_area(&p.polygon))
                    .sum();
                if banded_material > sheet_placed_area {
                    placed_parts_out = banded.placed;
                    placed_indices = banded.consumed;
                    total_placed_area += banded_material - sheet_placed_area;
                    fitness -= (banded_material - sheet_placed_area) / sheet_area;
                    sheet_placed_area = banded_material;
                    placed = placed_parts_out
                        .iter()
                        .filter_map(|p| {
                            parts_before_sheet.iter().find(|q| q.id == p.id).map(|q| PlacedObstacle {
                                polygon: rotate_layered_polygon(&q.polygon, p.rotation),
                                id: q.id,
                                source_id: q.source_id,
                                rotation: p.rotation,
                                placement: p.placement,
                            })
                        })
                        .collect();
                }
            }
        }

        // Remove exactly the placed slots, by position - see the
        // `placed_indices` doc comment above for why this can't be `.id`-keyed.
        let placed_index_set: HashSet<usize> = placed_indices.iter().copied().collect();
        let mut kept: Vec<NestPart> = Vec::with_capacity(parts.len().saturating_sub(placed_index_set.len()));
        for (idx, part) in parts.into_iter().enumerate() {
            if !placed_index_set.contains(&idx) {
                kept.push(part);
            }
        }
        parts = kept;

        if placed.is_empty() {
            // Nothing fit on a freshly opened, empty sheet - something is
            // wrong (part(s) genuinely too big); stop rather than looping
            // forever opening empty sheets.
            break;
        }

        all_placements.push(SheetPlacement { sheet_index: sheet_idx, parts: placed_parts_out });

        sheet_idx += 1;

        if cancelled_early {
            break;
        }
    }

    if cancelled_early {
        return None;
    }

    // Parts that never fit any sheet get a massive area-scaled fitness
    // penalty so the GA (once wired up, Phase 4) strongly prefers solutions
    // where everything is placed, even at the cost of opening more sheets.
    // Guarded against total_sheet_area == 0.0 (place_parts called with no
    // sheets at all) - without it this silently produces `fitness ==
    // Infinity` instead of a large-but-defined value.
    for p in &parts {
        let area_ratio = if total_sheet_area > 0.0 {
            (polygon_area(&p.polygon.points).abs() * 100.0) / total_sheet_area
        } else {
            1.0
        };
        fitness += 100_000_000.0 * area_ratio;
    }

    let utilisation = if total_usable_sheet_area > 0.0 {
        (total_placed_area / total_usable_sheet_area) * 100.0
    } else {
        0.0
    };

    Some(PlaceResult {
        placements: all_placements,
        fitness,
        area: total_placed_area,
        total_area: total_usable_sheet_area,
        utilisation,
        unplaced_count: parts.len(),
        unplaced_ids: parts.iter().map(|p| p.id).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, size: f64) -> LayeredPolygon {
        rect(x, y, size, size)
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![Point::new(x, y), Point::new(x + w, y), Point::new(x + w, y + h), Point::new(x, y + h)],
            layer: "0".into(),
            is_circle: None,
            children: Vec::new(),
            texts: Vec::new(),
            real_boundary: None,
        }
    }

    fn square_with_hole(x: f64, y: f64, size: f64, hole_x: f64, hole_y: f64, hole_size: f64) -> LayeredPolygon {
        let mut poly = square(x, y, size);
        poly.children.push(square(hole_x, hole_y, hole_size));
        poly
    }

    fn config(placement_type: PlacementType) -> PlacementConfig {
        PlacementConfig {
            placement_type,
            rotations: 1,
            dominant_part_area_threshold: DEFAULT_DOMINANT_PART_AREA_THRESHOLD,
            curve_tolerance: 0.3,
            part_rules: Default::default(),
            banded_pass: true,
        }
    }

    fn separated(x0: f64, y0: f64, s0: f64, x1: f64, y1: f64, s1: f64) -> bool {
        x0 + s0 <= x1 + 1e-6 || x1 + s1 <= x0 + 1e-6 || y0 + s0 <= y1 + 1e-6 || y1 + s1 <= y0 + 1e-6
    }

    /// The milestone: one rectangle placed on one sheet, single individual,
    /// no GA, no threads - the earliest point the full placement stack
    /// (inner NFP -> top-left-corner fast path -> fitness) is provably
    /// correct end-to-end.
    #[test]
    fn one_rectangle_placed_on_one_sheet() {
        let sheet = square(0.0, 0.0, 100.0);
        let part = square(0.0, 0.0, 10.0);
        let parts = vec![NestPart { id: 0, source_id: 0, polygon: part, rotation: 0.0 }];

        let result = place_parts(&[sheet], parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0);
        assert_eq!(result.placements.len(), 1);
        assert_eq!(result.placements[0].parts.len(), 1);
        let placed = result.placements[0].parts[0];
        assert_eq!(placed.id, 0);
        assert_eq!(placed.rotation, 0.0);
        // top-left-corner fast path: the part's own (0,0) corner should land
        // at the sheet's (0,0) corner, the tightest valid position.
        assert!((placed.placement.x - 0.0).abs() < 1e-6, "x was {}", placed.placement.x);
        assert!((placed.placement.y - 0.0).abs() < 1e-6, "y was {}", placed.placement.y);
        assert!((result.area - 100.0).abs() < 1e-6, "area was {}", result.area);
        assert!(result.fitness.is_finite());
    }

    #[test]
    fn two_rectangles_placed_side_by_side_without_overlap() {
        let sheet = square(0.0, 0.0, 100.0);
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 30.0), rotation: 0.0 },
            NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 20.0), rotation: 0.0 },
        ];

        let result = place_parts(&[sheet], parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0);
        assert_eq!(result.placements.len(), 1);
        assert_eq!(result.placements[0].parts.len(), 2);
        assert!((result.area - (900.0 + 400.0)).abs() < 1e-6, "area was {}", result.area);

        // the two placed 30x30 / 20x20 squares must not overlap
        let placed: Vec<(f64, f64, f64)> = result.placements[0]
            .parts
            .iter()
            .map(|p| {
                let size = if p.id == 0 { 30.0 } else { 20.0 };
                (p.placement.x, p.placement.y, size)
            })
            .collect();
        let (x0, y0, s0) = placed[0];
        let (x1, y1, s1) = placed[1];
        assert!(separated(x0, y0, s0, x1, y1, s1), "parts overlap: ({x0},{y0},{s0}) vs ({x1},{y1},{s1})");
    }

    /// Guards the actual point of wiring `NfpCache` into `place_parts`: a
    /// passed-in cache must come out with real entries, not sit unused -
    /// this is the difference between "the parameter compiles" and "the
    /// caching this was built for actually happens."
    #[test]
    fn place_parts_populates_the_shared_nfp_cache() {
        let sheet = square(0.0, 0.0, 100.0);
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 30.0), rotation: 0.0 },
            NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 20.0), rotation: 0.0 },
        ];
        let cache = NfpCache::new();
        assert_eq!(cache.stats(), 0);

        let result = place_parts(&[sheet], parts, &config(PlacementType::Gravity), &cache, &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0);
        assert!(cache.stats() > 0, "placing 2 parts (at least one inner-NFP and one obstacle-NFP lookup) should populate the cache");
    }

    /// A second `place_parts` call against the exact same part/sheet
    /// identities and rotations must hit the cache rather than recompute -
    /// the whole reason `place_parts` takes a caller-supplied `NfpCache`
    /// instead of a fresh one per call. Asserted via entry count staying
    /// flat, not growing, on the repeat call.
    #[test]
    fn a_repeated_placement_reuses_cached_nfps_instead_of_growing_the_cache() {
        let sheet = square(0.0, 0.0, 100.0);
        let parts = || {
            vec![
                NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 30.0), rotation: 0.0 },
                NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 20.0), rotation: 0.0 },
            ]
        };
        let cache = NfpCache::new();

        let _ = place_parts(&[sheet.clone()], parts(), &config(PlacementType::Gravity), &cache, &|| false, &|_, _| {}, &|_, _, _| {});
        let entries_after_first = cache.stats();
        assert!(entries_after_first > 0);

        let _ = place_parts(&[sheet], parts(), &config(PlacementType::Gravity), &cache, &|| false, &|_, _| {}, &|_, _, _| {});
        assert_eq!(cache.stats(), entries_after_first, "an identical second placement should hit the cache, not add new entries");
    }

    /// The actual point of `source_id`: N parts that share one shape (same
    /// `source_id`, distinct `id`s - the "252 identical copies" scenario
    /// that motivated adding it) must produce measurably fewer cache
    /// entries than the same N parts with N distinct `source_id`s, since
    /// every pairwise NFP/obstacle-NFP lookup between two same-shape parts
    /// now shares one cache key instead of each `id` pair getting its own.
    #[test]
    fn parts_sharing_a_source_id_produce_fewer_cache_entries_than_distinct_shapes() {
        let sheet = square(0.0, 0.0, 100.0);
        let same_shape_parts = vec![
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 },
            NestPart { id: 1, source_id: 0, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 },
            NestPart { id: 2, source_id: 0, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 },
        ];
        let distinct_shape_parts = vec![
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 },
            NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 },
            NestPart { id: 2, source_id: 2, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 },
        ];

        let same_shape_cache = NfpCache::new();
        place_parts(&[sheet.clone()], same_shape_parts, &config(PlacementType::Gravity), &same_shape_cache, &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        let distinct_shape_cache = NfpCache::new();
        place_parts(&[sheet], distinct_shape_parts, &config(PlacementType::Gravity), &distinct_shape_cache, &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert!(
            same_shape_cache.stats() < distinct_shape_cache.stats(),
            "same-source_id parts should share cache entries: {} entries (shared) vs {} (distinct)",
            same_shape_cache.stats(),
            distinct_shape_cache.stats()
        );
    }

    /// Regression test for the leftover-area fitness term: two single-part,
    /// single-sheet placements with the *same* sheet count (so the dominant
    /// `sheet_area`-per-sheet term is identical between them) but very
    /// different packing density must not score as an near-tie the way the
    /// old last-part-only `minwidth`/`minarea` tiebreak could - the denser
    /// one should score a strictly lower (better) fitness. Both parts are
    /// under the 0.9 dominant-area threshold (81% and 1% of the sheet,
    /// respectively), so neither takes the dominant-part-closes-sheet
    /// shortcut - both go through the same first-part fast path and the
    /// same per-sheet leftover computation afterward.
    #[test]
    fn leftover_area_makes_a_denser_single_part_placement_score_a_better_fitness() {
        let sheet = square(0.0, 0.0, 100.0); // 10,000mm2
        let dense_parts = vec![NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 90.0), rotation: 0.0 }]; // 8,100mm2, 81%
        let sparse_parts = vec![NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 }]; // 100mm2, 1%

        let dense = place_parts(&[sheet.clone()], dense_parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();
        let sparse = place_parts(&[sheet], sparse_parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(dense.unplaced_count, 0);
        assert_eq!(sparse.unplaced_count, 0);
        assert_eq!(dense.placements.len(), 1, "sanity: both single-part jobs should use exactly one sheet");
        assert_eq!(sparse.placements.len(), 1);
        assert!(
            dense.fitness < sparse.fitness,
            "a sheet left mostly full (81%) should score a better (lower) fitness than one left mostly empty (1%): dense={}, sparse={}",
            dense.fitness,
            sparse.fitness
        );
        // Both share the identical `sheet_area` per-sheet term (same 100x100
        // sheet, same sheet count) - the gap between them must come from
        // somewhere else, i.e. actually be attributable to the leftover-area
        // term rather than incidental noise elsewhere in the formula.
        assert!(
            (sparse.fitness - dense.fitness - (0.99 - 0.19)).abs() < 1e-6,
            "expected the fitness gap to match the leftover-area term's own (leftover/sheet_area) computation exactly: dense={}, sparse={}",
            dense.fitness,
            sparse.fitness
        );
    }

    #[test]
    fn oversized_part_is_left_unplaced_with_a_fitness_penalty() {
        let sheet = square(0.0, 0.0, 10.0);
        let parts = vec![NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 20.0), rotation: 0.0 }];

        let result = place_parts(&[sheet], parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 1);
        assert!(result.placements.is_empty());
        // unplaced-part penalty dominates fitness (100,000,000 scale factor)
        assert!(result.fitness > 1_000_000.0, "fitness was {}", result.fitness);
    }

    #[test]
    fn dominant_part_closes_the_sheet_immediately() {
        // A part covering >=90% of the sheet area should close the sheet
        // right after being placed, leaving the second part for a new sheet.
        let sheet = square(0.0, 0.0, 100.0);
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 95.0), rotation: 0.0 },
            NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 5.0), rotation: 0.0 },
        ];

        let result = place_parts(&[sheet.clone(), sheet], parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0);
        assert_eq!(result.placements.len(), 2);
        assert_eq!(result.placements[0].parts.len(), 1);
        assert_eq!(result.placements[0].parts[0].id, 0);
        assert_eq!(result.placements[1].parts[0].id, 1);
    }

    #[test]
    fn box_and_convexhull_placement_types_also_place_without_overlap() {
        for placement_type in [PlacementType::Box, PlacementType::ConvexHull] {
            let sheet = square(0.0, 0.0, 100.0);
            let parts = vec![
                NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 30.0), rotation: 0.0 },
                NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 20.0), rotation: 0.0 },
            ];

            let result = place_parts(&[sheet], parts, &config(placement_type), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();
            assert_eq!(result.unplaced_count, 0, "placement_type {:?}", placement_type);
            assert_eq!(result.placements[0].parts.len(), 2, "placement_type {:?}", placement_type);
        }
    }

    /// The kill-switch guarantee: cancelling partway through - not just
    /// before the call starts - must stop `place_parts` before it finishes
    /// every part, and the result must be discarded (`None`), not returned
    /// as if it were a complete, honestly-evaluated placement.
    #[test]
    fn place_parts_bails_out_mid_computation_when_cancelled_partway_through() {
        let sheet = square(0.0, 0.0, 1000.0);
        // Enough parts that a naive "only checked before the call" flag
        // would place all of them before this test could tell the
        // difference - cancelling after the 3rd part-attempt proves the
        // check fires *inside* the per-part loop, not just once overall.
        let parts: Vec<NestPart> = (0..20).map(|id| NestPart { id, source_id: id, polygon: square(0.0, 0.0, 10.0), rotation: 0.0 }).collect();
        let cache = NfpCache::new();

        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let result =
            place_parts(&[sheet], parts, &config(PlacementType::Gravity), &cache, &|| attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 3, &|_, _| {}, &|_, _, _| {});

        assert!(result.is_none(), "a cancellation partway through must discard the whole attempt, not return a partial result");
    }

    /// `TightFit` must prefer positions with real local contact over ones
    /// with little or none, unlike `Gravity`/`Box`/`ConvexHull` (which only
    /// score the aggregate bounding shape of everything placed so far, not
    /// adjacency). Two obstacles form an L-shaped corner at (60,10) on an
    /// otherwise-empty 200x200 sheet. `Gravity` settles for a single-wall
    /// touch at (60,30) - that shrinks its tracked bounding measure just as
    /// well as the fuller L-corner does, so it never actually compares local
    /// contact at all. `TightFit` must land on a real high-contact position
    /// instead - either the L-corner itself or one of the sheet's own
    /// corners (both near the same contact-area ceiling; confirmed by
    /// direct measurement, not assumed) - and must not match Gravity's
    /// single-wall answer. `GravityTightFit` gets its own, more precise
    /// assertion: (60,30) and (60,10) are exactly tied by Gravity's cheap
    /// metric (neither grows the existing combined bounding box), so the
    /// hybrid's tie-break must land on the fuller L-corner specifically,
    /// not just "some high-contact spot" the way plain `TightFit`'s
    /// assertion allows.
    #[test]
    fn tight_fit_prefers_high_contact_positions_gravity_ignores() {
        let sheet = square(0.0, 0.0, 200.0);
        let obstacle_bottom = rect(60.0, 0.0, 40.0, 10.0);
        let obstacle_left = rect(50.0, 10.0, 10.0, 40.0);
        let part = square(0.0, 0.0, 20.0);
        let sheet_nfp = inner_nfp(&sheet, &part, 0.3).expect("part fits the empty sheet");
        let placed = vec![
            PlacedObstacle { polygon: obstacle_bottom, id: 0, source_id: 0, rotation: 0.0, placement: Placement { x: 0.0, y: 0.0 } },
            PlacedObstacle { polygon: obstacle_left, id: 1, source_id: 1, rotation: 0.0, placement: Placement { x: 0.0, y: 0.0 } },
        ];

        let gravity_outcome = try_place_part_on_sheet(&part, 2, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::Gravity), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(gravity) = gravity_outcome else { panic!("gravity should place: {gravity_outcome:?}") };
        assert_eq!((gravity.position.x, gravity.position.y), (60.0, 30.0), "test's own assumption about Gravity's answer changed");

        let tight_outcome = try_place_part_on_sheet(&part, 2, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::TightFit), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(tight) = tight_outcome else { panic!("tight fit should place: {tight_outcome:?}") };

        let high_contact_positions = [(0.0, 0.0), (0.0, 180.0), (180.0, 0.0), (180.0, 180.0), (60.0, 10.0)];
        assert!(
            high_contact_positions.contains(&(tight.position.x, tight.position.y)),
            "expected a high-contact corner, got ({}, {})",
            tight.position.x,
            tight.position.y
        );
        assert_ne!(
            (tight.position.x, tight.position.y),
            (gravity.position.x, gravity.position.y),
            "TightFit should not settle for Gravity's single-wall touch when a fuller-contact corner is reachable"
        );

        // GravityTightFit: Gravity's own bounding measure doesn't grow
        // whether the part sits at (60,30) (touching just the left wall) or
        // (60,10) (touching both walls) - both stay within the same
        // already-existing combined bounding box - so the two are tied by
        // Gravity's cheap metric, and the tie-break should pick the fuller
        // L-corner contact instead of Gravity's own plain x-position
        // tiebreak (which has no preference between x=60 candidates at
        // different y at all, since x doesn't differ).
        let hybrid_outcome = try_place_part_on_sheet(&part, 2, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::GravityTightFit), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(hybrid) = hybrid_outcome else { panic!("hybrid should place: {hybrid_outcome:?}") };
        assert_eq!(
            (hybrid.position.x, hybrid.position.y),
            (60.0, 10.0),
            "GravityTightFit should break Gravity's tie in favor of the fuller-contact L-corner, got ({}, {})",
            hybrid.position.x,
            hybrid.position.y
        );
    }

    /// `PlacementType::GravityCorrective`'s own doc comment: the sheet's
    /// second part (`placed.len() <= 1`) scores exactly like `Gravity`, not
    /// `TightFit`. Reuses `obstacle_left` alone (one obstacle - this is a
    /// "second part" scenario, not the full two-obstacle L-corner) - the
    /// test asserts Gravity and TightFit actually disagree here (otherwise
    /// it wouldn't prove anything), then checks GravityCorrective matches
    /// Gravity.
    #[test]
    fn gravity_corrective_places_the_second_part_like_gravity_not_tight_fit() {
        let sheet = square(0.0, 0.0, 200.0);
        let obstacle = rect(50.0, 10.0, 10.0, 40.0);
        let part = square(0.0, 0.0, 20.0);
        let sheet_nfp = inner_nfp(&sheet, &part, 0.3).expect("part fits the empty sheet");
        let placed = vec![PlacedObstacle { polygon: obstacle, id: 0, source_id: 0, rotation: 0.0, placement: Placement { x: 0.0, y: 0.0 } }];

        let gravity_outcome = try_place_part_on_sheet(&part, 1, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::Gravity), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(gravity) = gravity_outcome else { panic!("gravity should place: {gravity_outcome:?}") };

        let tight_outcome = try_place_part_on_sheet(&part, 1, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::TightFit), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(tight) = tight_outcome else { panic!("tight fit should place: {tight_outcome:?}") };

        assert_ne!(
            (gravity.position.x, gravity.position.y),
            (tight.position.x, tight.position.y),
            "test scenario must have Gravity and TightFit disagree for this test to prove anything"
        );

        let corrective_outcome =
            try_place_part_on_sheet(&part, 1, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::GravityCorrective), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(corrective) = corrective_outcome else { panic!("gravity-corrective should place: {corrective_outcome:?}") };

        assert_eq!(
            (corrective.position.x, corrective.position.y),
            (gravity.position.x, gravity.position.y),
            "the sheet's second part should match Gravity's answer, got ({}, {})",
            corrective.position.x,
            corrective.position.y
        );
    }

    /// `PlacementType::GravityCorrective`'s own doc comment: from the third
    /// part onward (`placed.len() >= 2`), scoring switches outright to
    /// `TightFit`'s real contact-area measure - reuses this file's own
    /// L-corner scenario (two obstacles already placed) verbatim, and
    /// asserts an exact match against pure `TightFit`'s own answer (not just
    /// "some high-contact corner") since the two use the byte-identical
    /// scoring formula here and must agree exactly.
    #[test]
    fn gravity_corrective_places_the_third_part_like_tight_fit_not_gravity() {
        let sheet = square(0.0, 0.0, 200.0);
        let obstacle_bottom = rect(60.0, 0.0, 40.0, 10.0);
        let obstacle_left = rect(50.0, 10.0, 10.0, 40.0);
        let part = square(0.0, 0.0, 20.0);
        let sheet_nfp = inner_nfp(&sheet, &part, 0.3).expect("part fits the empty sheet");
        let placed = vec![
            PlacedObstacle { polygon: obstacle_bottom, id: 0, source_id: 0, rotation: 0.0, placement: Placement { x: 0.0, y: 0.0 } },
            PlacedObstacle { polygon: obstacle_left, id: 1, source_id: 1, rotation: 0.0, placement: Placement { x: 0.0, y: 0.0 } },
        ];

        let tight_outcome = try_place_part_on_sheet(&part, 2, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::TightFit), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(tight) = tight_outcome else { panic!("tight fit should place: {tight_outcome:?}") };

        let corrective_outcome =
            try_place_part_on_sheet(&part, 2, 0.0, &sheet_nfp, &sheet, &placed, &config(PlacementType::GravityCorrective), &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(corrective) = corrective_outcome else { panic!("gravity-corrective should place: {corrective_outcome:?}") };

        assert_eq!(
            (corrective.position.x, corrective.position.y),
            (tight.position.x, tight.position.y),
            "the sheet's third part onward should match TightFit's contact score exactly, got ({}, {})",
            corrective.position.x,
            corrective.position.y
        );
    }

    /// `PlacementType::GravityCorrective`'s rotation-reuse cache. A 20x10
    /// rectangle only fits a 15x25 sheet once rotated to 10x20 - at the
    /// `rotations=4` grid, that's true at both 90 and 270 (a plain
    /// rectangle looks identical at 0/180 and at 90/270). Part 0 starts its
    /// own search at rotation 0 and lands on 90 (the first fit found
    /// stepping 0→90→180→270). Part 1 - the same shape (`source_id: 0`),
    /// deliberately given a *different* own starting rotation (180) - would,
    /// searching fresh from 180, step 180→270→0→90 and land on **270** (the
    /// first fit in *that* order), not 90. If it instead lands on 90 here,
    /// that's only explainable by the cache overriding its starting
    /// rotation to part 0's already-known-good 90 before the search ran.
    #[test]
    fn gravity_corrective_reuses_a_previously_successful_rotation_for_a_repeated_shape() {
        // A sheet's first part (under GravityCorrective, same as TightFit/
        // GravityTightFit) goes through its own dedicated full-rotation
        // search (`place_parts`'s `placed.is_empty()` branch) - that branch
        // never consults the rotation-reuse cache, it always searches fresh.
        // So both occurrences of the repeated shape need a `filler` placed
        // ahead of them, forcing each into the *generic* per-part path
        // (`try_place_part_on_sheet`'s caller) where the cache actually
        // applies - a single tall sheet holding filler + both occurrences
        // keeps this to one sheet instead of juggling "which sheet did the
        // second occurrence land on."
        let sheet = rect(0.0, 0.0, 15.0, 50.0);
        let filler = rect(0.0, 0.0, 15.0, 5.0); // spans the full width, distinct source_id
        let shape = rect(0.0, 0.0, 20.0, 10.0);
        let parts = vec![
            NestPart { id: 10, source_id: 10, polygon: filler, rotation: 0.0 },
            NestPart { id: 0, source_id: 0, polygon: shape.clone(), rotation: 0.0 },
            NestPart { id: 1, source_id: 0, polygon: shape, rotation: 180.0 },
        ];
        let mut cfg = config(PlacementType::GravityCorrective);
        cfg.rotations = 4;

        let result = place_parts(&[sheet], parts, &cfg, &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0, "filler plus both copies of the shape should all fit on the one generously-tall sheet");
        let mut rotation_by_id: HashMap<usize, f64> = HashMap::new();
        for sp in &result.placements {
            for p in &sp.parts {
                rotation_by_id.insert(p.id, p.rotation);
            }
        }
        assert!((rotation_by_id[&0] - 90.0).abs() < 1e-6, "part 0 should land on rotation 90, was {}", rotation_by_id[&0]);
        assert!(
            (rotation_by_id[&1] - 90.0).abs() < 1e-6,
            "part 1 should reuse part 0's winning rotation (90) instead of independently landing on 270 the way a fresh from-180 search would, was {}",
            rotation_by_id[&1]
        );
    }

    /// Regression test for a real bug report: the first part placed on a
    /// sheet used to always keep whatever rotation the generic first-fit
    /// loop above happened to land on - in practice always the part's
    /// starting rotation (0.0 here), since a large empty sheet accepts
    /// almost any rotation on the very first try - even under
    /// TightFit/GravityTightFit, where a different configured rotation can
    /// leave far less wasted space in the sheet's own corner for a concave
    /// part. This part is a 10x10 square with a 4x4 notch cut from what
    /// starts (at rotation 0) as its own bottom-left corner: the actual
    /// material there is 4 units away from that corner, well past
    /// `TIGHT_FIT_PROBE_DISTANCE` (1.0), so rotation 0 measures *zero*
    /// contact against the sheet's corner - while every other configured
    /// rotation (90/180/270) rotates a different, solid original corner
    /// into that same spot, measuring real contact. Deliberately asserts
    /// only "not 0" (not which of the three solid rotations wins) - the
    /// three are genuinely tied by this shape's symmetry, and picking among
    /// ties isn't what this test is checking.
    #[test]
    fn tight_fit_and_gravity_tight_fit_rotate_even_the_very_first_part_for_a_tighter_corner() {
        fn notched_square() -> LayeredPolygon {
            LayeredPolygon {
                points: vec![
                    Point::new(4.0, 0.0),
                    Point::new(10.0, 0.0),
                    Point::new(10.0, 10.0),
                    Point::new(0.0, 10.0),
                    Point::new(0.0, 4.0),
                    Point::new(4.0, 4.0),
                ],
                layer: "0".into(),
                is_circle: None,
                children: Vec::new(),
                texts: Vec::new(),
                real_boundary: None,
            }
        }

        for placement_type in [PlacementType::TightFit, PlacementType::GravityTightFit] {
            let sheet = square(0.0, 0.0, 100.0);
            let parts = vec![NestPart { id: 0, source_id: 0, polygon: notched_square(), rotation: 0.0 }];
            let mut cfg = config(placement_type);
            cfg.rotations = 4;

            let result = place_parts(&[sheet], parts, &cfg, &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

            assert_eq!(result.unplaced_count, 0, "{placement_type:?}");
            let placed = result.placements[0].parts[0];
            assert!(
                (placed.rotation - 0.0).abs() > 1e-6,
                "{placement_type:?}: expected the first part to rotate away from 0 degrees for a tighter corner, but it stayed at {}",
                placed.rotation
            );
        }
    }

    /// Regression test for the 2nd+ part rotation search: before it existed,
    /// `try_place_part_on_sheet` (as called for any part after a sheet's
    /// first) only ever saw one fixed rotation - whichever happened to fit
    /// the bare sheet region first - with nothing to compare it against.
    /// This checks the actual scoring signal the new per-rotation loop in
    /// `place_parts` relies on: a long, flat obstacle already on the sheet,
    /// and an asymmetric (non-square) candidate part scored at two
    /// different rotations against it. Lying flat (its long edge against
    /// the obstacle's long edge) must score strictly more contact than
    /// standing on its narrow edge (only its short edge available to touch)
    /// - if rotation genuinely didn't change the score, this new loop would
    /// have nothing real to select between.
    #[test]
    fn tight_fit_scores_more_contact_for_a_flush_long_edge_than_a_narrow_one() {
        let sheet = square(0.0, 0.0, 200.0);
        let obstacle = rect(0.0, 0.0, 50.0, 5.0); // long, flat obstacle along the bottom
        let placed = vec![PlacedObstacle { polygon: obstacle, id: 0, source_id: 0, rotation: 0.0, placement: Placement { x: 0.0, y: 0.0 } }];
        let cfg = config(PlacementType::TightFit);

        // Same rectangle, two rotations: lying flat (20 wide x 4 tall) can
        // rest its full 20mm-long edge against the obstacle's 50mm-long top
        // edge; standing up (4 wide x 20 tall, rotation_layered_polygon(_, 90))
        // only ever has its 4mm-wide edge available to touch it with.
        let flat = rect(0.0, 0.0, 20.0, 4.0);
        let standing = rotate_layered_polygon(&flat, 90.0);

        let sheet_nfp_flat = inner_nfp(&sheet, &flat, 0.3).expect("flat rectangle fits the empty sheet");
        let flat_outcome = try_place_part_on_sheet(&flat, 1, 0.0, &sheet_nfp_flat, &sheet, &placed, &cfg, &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(flat_result) = flat_outcome else { panic!("flat rectangle should place: {flat_outcome:?}") };

        let sheet_nfp_standing = inner_nfp(&sheet, &standing, 0.3).expect("standing rectangle fits the empty sheet");
        let standing_outcome = try_place_part_on_sheet(&standing, 1, 90.0, &sheet_nfp_standing, &sheet, &placed, &cfg, &NfpCache::new(), &|_| {});
        let PlaceOnSheetOutcome::Placed(standing_result) = standing_outcome else { panic!("standing rectangle should place: {standing_outcome:?}") };

        // TightFit's score is negated contact area (more contact = more
        // negative, see `CandidateScore::TightFit`'s own doc comment) - so
        // "more contact" means a strictly *smaller* (more negative) `minarea`.
        assert!(
            flat_result.minarea < standing_result.minarea,
            "lying flat against the long obstacle edge should score strictly more contact (smaller minarea) than standing on the narrow edge: flat={}, standing={}",
            flat_result.minarea,
            standing_result.minarea
        );
    }

    /// Regression test: `try_place_part_on_sheet` must not panic when
    /// `placed` is empty, under every placement type - a scenario
    /// `place_parts` itself only avoids for Gravity/Box/ConvexHull (whose
    /// first part on a sheet always takes the inline top-left-corner path
    /// instead) and for TightFit/GravityTightFit/GravityCorrective whenever
    /// `config.rotations <= 1` - but `nesting::consolidation`'s cross-sheet
    /// relocation can hit this directly (a relocation target isn't
    /// guaranteed to already have a part on it).
    #[test]
    fn try_place_part_on_sheet_handles_an_empty_target_sheet() {
        let sheet = square(0.0, 0.0, 100.0);
        let part = square(0.0, 0.0, 10.0);
        let sheet_nfp = inner_nfp(&sheet, &part, 0.3).expect("part fits the empty sheet");

        for placement_type in [PlacementType::Gravity, PlacementType::Box, PlacementType::ConvexHull, PlacementType::TightFit] {
            let result = try_place_part_on_sheet(&part, 0, 0.0, &sheet_nfp, &sheet, &[], &config(placement_type), &NfpCache::new(), &|_| {});
            assert!(matches!(result, PlaceOnSheetOutcome::Placed(_)), "placement_type {:?}", placement_type);
        }
    }

    /// Regression test for the id-based-removal bug (reviewer.md finding):
    /// two parts sharing an id, where the first one dominant-closes the
    /// sheet before the second is even attempted. The second must be
    /// deferred to the next sheet, not silently dropped.
    #[test]
    fn duplicate_id_parts_are_not_dropped_when_one_dominant_closes_a_sheet() {
        let sheet = square(0.0, 0.0, 30.0);
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 30.0), rotation: 0.0 },
            NestPart { id: 0, source_id: 0, polygon: square(0.0, 0.0, 30.0), rotation: 0.0 },
        ];

        let result = place_parts(&[sheet.clone(), sheet], parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0);
        assert_eq!(result.placements.len(), 2, "expected one part per sheet, got {:?}", result.placements);
        assert_eq!(result.placements[0].parts.len(), 1);
        assert_eq!(result.placements[1].parts.len(), 1);
    }

    /// Regression test for the "holed-obstacle path is untested" gap
    /// (reviewer.md finding): a part with a hole, a second part nested
    /// inside that hole, and a third part that must not be allowed to
    /// overlap the second - proving the restored-hole region is correctly
    /// narrowed by a later obstacle, not just correctly computed in isolation.
    #[test]
    fn a_part_placed_inside_another_parts_hole_blocks_a_later_part_from_overlapping_it() {
        // A: 30x30 square with a 10x10 hole in the middle (10,10)-(20,20).
        let a = square_with_hole(0.0, 0.0, 30.0, 10.0, 10.0, 10.0);
        // B and C: both 4x4, small enough to nest inside A's hole - only one can fit.
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: a, rotation: 0.0 },
            NestPart { id: 1, source_id: 1, polygon: square(0.0, 0.0, 4.0), rotation: 0.0 },
            NestPart { id: 2, source_id: 2, polygon: square(0.0, 0.0, 4.0), rotation: 0.0 },
        ];

        let result = place_parts(&[square(0.0, 0.0, 100.0)], parts, &config(PlacementType::Gravity), &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).unwrap();

        assert_eq!(result.unplaced_count, 0, "all 3 parts should fit on one 100x100 sheet: {:?}", result.placements);
        assert_eq!(result.placements.len(), 1);
        let placed = &result.placements[0].parts;
        assert_eq!(placed.len(), 3);

        let sizes = [30.0, 4.0, 4.0];
        for i in 0..placed.len() {
            for j in (i + 1)..placed.len() {
                let (pi, pj) = (placed[i], placed[j]);
                // A's own hole doesn't count as "material" for this simple
                // bbox-overlap check, so only compare the two 4x4 parts
                // against each other directly - A vs. either is fine even if
                // their bboxes touch, since the hole is inside A's bbox.
                if pi.id == 0 || pj.id == 0 {
                    continue;
                }
                assert!(
                    separated(pi.placement.x, pi.placement.y, sizes[pi.id], pj.placement.x, pj.placement.y, sizes[pj.id]),
                    "parts {} and {} overlap: {:?} vs {:?}",
                    pi.id,
                    pj.id,
                    pi.placement,
                    pj.placement
                );
            }
        }
    }

    // --- per-part constraints (grain direction / mirror override) -------

    fn rules(entries: &[(usize, &[f64], bool)]) -> PlacementConfig {
        let map: HashMap<usize, PartRule> =
            entries.iter().map(|(id, angles, mirror)| (*id, PartRule { angles: angles.to_vec(), mirror: *mirror })).collect();
        PlacementConfig { part_rules: std::sync::Arc::new(map), ..config(PlacementType::TightFit) }
    }

    /// The load-bearing one: an unconstrained part must walk exactly the
    /// angle sequence the old fixed `for _ in 0..rotations` loop walked, with
    /// exactly the same per-step rotation deltas. Everything else in this
    /// file is a regression test for the engine; this is the regression test
    /// for the refactor that made per-part rules possible at all.
    #[test]
    fn an_unconstrained_part_walks_the_untouched_rotation_grid() {
        for rotations in [1u32, 2, 3, 4, 7, 12] {
            let config = PlacementConfig { rotations, ..config(PlacementType::TightFit) };
            let step = 360.0 / rotations as f64;
            for from in [0.0, 90.0, 271.5] {
                let steps = rotation_steps(&config, 0, from);
                assert_eq!(steps.len(), rotations as usize, "rotations={rotations}");

                let mut expected_angle = from;
                for (i, (angle, delta)) in steps.iter().enumerate() {
                    assert_eq!(*angle, expected_angle, "rotations={rotations} from={from} step {i}");
                    // Deltas must stay a plain `step`, never a wrapped
                    // difference - rotating geometry by -270 instead of +90
                    // is the bug this shape exists to prevent.
                    assert_eq!(*delta, if i == 0 { 0.0 } else { step }, "rotations={rotations} from={from} step {i}");
                    expected_angle = advance_rotation(expected_angle, step);
                }
            }
        }
    }

    #[test]
    fn a_constrained_part_only_ever_offers_its_allowed_angles() {
        let config = rules(&[(7, &[0.0, 180.0], false)]);
        let steps = rotation_steps(&config, 7, 0.0);
        assert_eq!(steps.iter().map(|(a, _)| *a).collect::<Vec<_>>(), vec![0.0, 180.0]);

        // ...even though the global grid is much wider, and even after
        // stagnation widening has pushed `rotations` up.
        let widened = PlacementConfig { rotations: 32, ..config.clone() };
        let angles: Vec<f64> = rotation_steps(&widened, 7, 0.0).iter().map(|(a, _)| *a).collect();
        assert_eq!(angles, vec![0.0, 180.0], "a widened global grid must not leak into a constrained part");

        // A part with no rule of its own is unaffected by its neighbour's.
        assert_eq!(rotation_steps(&widened, 8, 0.0).len(), 32);
    }

    /// The allowed set *replaces* the grid rather than filtering it: 45 is
    /// not on a `rotations: 2` grid at all, and filtering would silently
    /// leave this part with nothing to try.
    #[test]
    fn allowed_angles_need_not_lie_on_the_global_grid() {
        let config = PlacementConfig { rotations: 2, ..rules(&[(0, &[0.0, 45.0], false)]) };
        let angles: Vec<f64> = rotation_steps(&config, 0, 0.0).iter().map(|(a, _)| *a).collect();
        assert_eq!(angles, vec![0.0, 45.0]);
    }

    #[test]
    fn a_constrained_part_starts_from_the_allowed_angle_nearest_where_it_is() {
        let config = rules(&[(0, &[0.0, 90.0, 180.0, 270.0], false)]);
        // Sitting at 200 degrees: 180 is the closest allowed angle, and the
        // rest follow cyclically.
        let angles: Vec<f64> = rotation_steps(&config, 0, 200.0).iter().map(|(a, _)| *a).collect();
        assert_eq!(angles, vec![180.0, 270.0, 0.0, 90.0]);

        // Deltas stay positive rotations across the 270 -> 0 wrap.
        let deltas: Vec<f64> = rotation_steps(&config, 0, 200.0).iter().map(|(_, d)| *d).collect();
        assert!(deltas.iter().all(|d| *d >= 0.0), "got {deltas:?}");
        assert_eq!(deltas[2], 90.0, "270 -> 0 is a +90 rotation, not -270");
    }

    /// A mirrored part id carries `MIRROR_ID_BIT`; its rule is authored
    /// against the un-flipped id, so lookups have to mask it off.
    #[test]
    fn a_mirrored_copy_obeys_its_own_parts_rule() {
        let config = rules(&[(3, &[0.0, 180.0], true)]);
        let angles: Vec<f64> = rotation_steps(&config, 3 ^ crate::dispatch::MIRROR_ID_BIT, 0.0).iter().map(|(a, _)| *a).collect();
        assert_eq!(angles, vec![0.0, 180.0]);
        assert!(part_may_mirror(&config.part_rules, 3 ^ crate::dispatch::MIRROR_ID_BIT, false));
    }

    #[test]
    fn a_part_rule_overrides_the_job_wide_mirror_switch_in_both_directions() {
        let deny = rules(&[(0, &[], false)]);
        assert!(!part_may_mirror(&deny.part_rules, 0, true), "a part may opt out of a mirroring job");

        let allow = rules(&[(0, &[], true)]);
        assert!(part_may_mirror(&allow.part_rules, 0, false), "and into a non-mirroring one");

        // A part with no rule follows the job.
        assert!(part_may_mirror(&allow.part_rules, 1, true));
        assert!(!part_may_mirror(&allow.part_rules, 1, false));
    }

    /// End to end through `place_parts`: whatever the search does, a
    /// grain-locked part may only come to rest at an allowed angle.
    #[test]
    fn place_parts_never_places_a_constrained_part_off_its_allowed_angles() {
        let config = PlacementConfig { rotations: 8, ..rules(&[(0, &[0.0, 180.0], false), (1, &[0.0, 180.0], false)]) };
        let parts = vec![
            NestPart { id: 0, source_id: 0, polygon: rect(0.0, 0.0, 60.0, 20.0), rotation: 45.0 },
            NestPart { id: 1, source_id: 0, polygon: rect(0.0, 0.0, 60.0, 20.0), rotation: 135.0 },
            // Unconstrained neighbour, free to use the whole grid.
            NestPart { id: 2, source_id: 1, polygon: rect(0.0, 0.0, 30.0, 30.0), rotation: 0.0 },
        ];
        let result = place_parts(&[square(0.0, 0.0, 200.0)], parts, &config, &NfpCache::new(), &|| false, &|_, _| {}, &|_, _, _| {}).expect("places");
        assert_eq!(result.unplaced_count, 0);

        for sheet in &result.placements {
            for placed in &sheet.parts {
                if placed.id == 0 || placed.id == 1 {
                    assert!(
                        placed.rotation == 0.0 || placed.rotation == 180.0,
                        "grain-locked part {} came to rest at {}",
                        placed.id,
                        placed.rotation
                    );
                }
            }
        }
    }
}
