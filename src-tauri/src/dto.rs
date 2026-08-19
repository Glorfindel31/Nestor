//! Serialization boundary between the Tauri IPC surface and the internal
//! `geometry`/`nesting` types. Kept as a separate, explicit conversion layer
//! rather than deriving `Serialize`/`Deserialize` directly on
//! `geometry::point::Point`/`LayeredPolygon` etc. - those crates are
//! deliberately I/O-free (`geometry`'s own module doc: "Zero I/O, zero
//! threading"), and serialization is exactly the kind of boundary concern
//! that belongs at the edge, not baked into core geometry types.

use std::collections::HashMap;

use geometry::dxf_import::{LayeredPolygon, RealVertex, TextAnnotation};
use geometry::point::Point;
use nesting::dispatch::MIRROR_ID_BIT;
use nesting::ga::GaConfig;
use nesting::placement::{PlacementConfig, PlacementType};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct PointDto {
    pub x: f64,
    pub y: f64,
}

impl From<&Point> for PointDto {
    fn from(p: &Point) -> Self {
        PointDto { x: p.x, y: p.y }
    }
}

impl From<PointDto> for Point {
    fn from(p: PointDto) -> Self {
        Point::new(p.x, p.y)
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct CircleDto {
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
}

/// Matches `geometry::dxf_import::RealVertex` field-for-field - see that
/// type's doc comment for why it exists (letting `dxf_export` write a real
/// arc back out on export instead of a tessellated approximation).
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
pub struct RealVertexDto {
    pub point: PointDto,
    pub bulge: f64,
}

impl From<&RealVertex> for RealVertexDto {
    fn from(v: &RealVertex) -> Self {
        RealVertexDto { point: PointDto::from(&v.point), bulge: v.bulge }
    }
}

impl From<RealVertexDto> for RealVertex {
    fn from(dto: RealVertexDto) -> Self {
        RealVertex { point: Point::from(dto.point), bulge: dto.bulge }
    }
}

/// A `TEXT`/`MTEXT` label attached to a part/sheet, matching
/// `geometry::dxf_import::TextAnnotation` field-for-field - see that type's
/// doc comment for why this exists (DXF text has no closed boundary, so it
/// rides along attached to whichever profile contains it instead of being a
/// `PolygonDto` of its own).
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct TextDto {
    pub position: PointDto,
    pub rotation_deg: f64,
    pub height: f64,
    pub value: String,
    pub is_multiline: bool,
}

impl From<&TextAnnotation> for TextDto {
    fn from(text: &TextAnnotation) -> Self {
        TextDto {
            position: PointDto::from(&text.position),
            rotation_deg: text.rotation_deg,
            height: text.height,
            value: text.value.clone(),
            is_multiline: text.is_multiline,
        }
    }
}

impl From<TextDto> for TextAnnotation {
    fn from(dto: TextDto) -> Self {
        TextAnnotation {
            position: Point::from(dto.position),
            rotation_deg: dto.rotation_deg,
            height: dto.height,
            value: dto.value,
            is_multiline: dto.is_multiline,
        }
    }
}

/// A polygon plus its holes, matching `geometry::dxf_import::LayeredPolygon`
/// field-for-field. Deserializable (a `run_nest` request builds these from
/// whatever the frontend already has) and serializable (`import_dxf`'s
/// response is these).
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PolygonDto {
    pub points: Vec<PointDto>,
    pub layer: String,
    #[serde(default)]
    pub is_circle: Option<CircleDto>,
    #[serde(default)]
    pub children: Vec<PolygonDto>,
    #[serde(default)]
    pub texts: Vec<TextDto>,
    #[serde(default)]
    pub real_boundary: Option<Vec<RealVertexDto>>,
}

impl From<&LayeredPolygon> for PolygonDto {
    fn from(poly: &LayeredPolygon) -> Self {
        PolygonDto {
            points: poly.points.iter().map(PointDto::from).collect(),
            layer: poly.layer.clone(),
            is_circle: poly.is_circle.map(|c| CircleDto { cx: c.cx, cy: c.cy, r: c.r }),
            children: poly.children.iter().map(PolygonDto::from).collect(),
            texts: poly.texts.iter().map(TextDto::from).collect(),
            real_boundary: poly.real_boundary.as_ref().map(|verts| verts.iter().map(RealVertexDto::from).collect()),
        }
    }
}

impl From<PolygonDto> for LayeredPolygon {
    fn from(dto: PolygonDto) -> Self {
        LayeredPolygon {
            points: dto.points.into_iter().map(Point::from).collect(),
            layer: dto.layer,
            is_circle: dto.is_circle.map(|c| geometry::circular_nfp::Circle { cx: c.cx, cy: c.cy, r: c.r }),
            children: dto.children.into_iter().map(LayeredPolygon::from).collect(),
            texts: dto.texts.into_iter().map(TextAnnotation::from).collect(),
            real_boundary: dto.real_boundary.map(|verts| verts.into_iter().map(RealVertex::from).collect()),
        }
    }
}

/// One part definition from the frontend: a shape plus how many copies to
/// nest. Expanded into individually-id'd `NestPart`s by `expand_parts`
/// below - `nesting::dispatch`'s `parts_by_id: HashMap<usize, _>` needs one
/// entry per physical copy, not per shape (matches the original's
/// `launchWorkers` building `adam` the same way: one polygon clone with a
/// fresh id per `parts[i].quantity`).
#[derive(Deserialize, Clone, Debug)]
pub struct PartDto {
    pub polygon: PolygonDto,
    #[serde(default = "one")]
    pub quantity: usize,
}

fn one() -> usize {
    1
}

/// Expands `parts` (shape + quantity) into `(adam, parts_by_id)`: `adam` is
/// every physical copy's id, area-sorted decreasing (same seed order
/// `launchWorkers` uses for the GA's `population[0]`); `parts_by_id` maps
/// each id to its geometry. A part with `quantity: 0` contributes zero
/// copies - matches the original's plain `for (j=0; j<quantity; j++)` loop
/// for parts (`launchWorkers`'s non-sheet branch). There's no
/// fallback-to-1 here: that convention exists only for *sheet* quantity
/// (`Number(quantity) || totalPartInstances || 1`, "0 means unlimited"), a
/// different code path with different semantics that doesn't apply to
/// parts.
/// Also returns `shape_ids` (instance id -> source id): every quantity-copy
/// of the same `PartDto` shares one source id (this loop's own index over
/// the input `Vec<PartDto>`, before per-quantity expansion) - lets the NFP
/// cache dedupe by shape instead of by per-instance id, restoring parity
/// with the original app's `.source`-keyed cache (see
/// `nesting::placement::NestPart::source_id`'s doc comment for where this
/// actually gets used). A
/// definition-order identity, not a content-hash one - two separate
/// `PartDto` entries with byte-identical polygons still get different
/// source ids; fine for "one imported shape, quantity N", not "the same
/// shape imported twice as separate parts".
///
/// `mirror` registers a second, mirrored variant of every copy under
/// `id ^ nesting::dispatch::MIRROR_ID_BIT` (with its own source id, so it
/// gets its own NFP cache entries - a mirrored shape's NFP against anything
/// is genuinely different). Those extra ids are deliberately **not** in
/// `adam`: they're alternatives the GA's rotation genes can select, not
/// extra parts to place.
#[must_use]
pub fn expand_parts(parts: Vec<PartDto>, mirror: bool) -> (Vec<usize>, HashMap<usize, LayeredPolygon>, HashMap<usize, usize>) {
    let mut parts_by_id = HashMap::new();
    let mut shape_ids = HashMap::new();
    let mut adam = Vec::new();
    let mut next_id = 0usize;

    for (source_id, part) in parts.into_iter().enumerate() {
        let polygon: LayeredPolygon = part.polygon.into();
        let Some(last) = part.quantity.checked_sub(1) else { continue };
        // Clone for every copy but the last, where a move does instead -
        // `quantity` copies never need more than `quantity - 1` clones.
        for _ in 0..last {
            parts_by_id.insert(next_id, polygon.clone());
            shape_ids.insert(next_id, source_id);
            adam.push(next_id);
            next_id += 1;
        }
        parts_by_id.insert(next_id, polygon);
        shape_ids.insert(next_id, source_id);
        adam.push(next_id);
        next_id += 1;
    }

    if mirror {
        for (&id, poly) in parts_by_id.clone().iter() {
            parts_by_id.insert(id ^ MIRROR_ID_BIT, geometry::dxf_import::mirror_layered_polygon(poly));
            shape_ids.insert(id ^ MIRROR_ID_BIT, shape_ids[&id] ^ MIRROR_ID_BIT);
        }
    }

    // Decorate-sort-undecorate: each id's area is computed once up front
    // instead of being recomputed on every comparison a sort makes
    // (O(n log n) recomputations otherwise, for a value that never changes
    // mid-sort).
    let mut adam_with_area: Vec<(usize, f64)> = adam.into_iter().map(|id| (id, geometry::polygon::polygon_area(&parts_by_id[&id].points).abs())).collect();
    adam_with_area.sort_by(|&(_, area_a), &(_, area_b)| area_b.total_cmp(&area_a));
    let adam: Vec<usize> = adam_with_area.into_iter().map(|(id, _)| id).collect();

    (adam, parts_by_id, shape_ids)
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlacementTypeDto {
    Gravity,
    Box,
    #[serde(rename = "convexhull")]
    ConvexHull,
    #[serde(rename = "tightfit")]
    TightFit,
    #[serde(rename = "gravitytightfit")]
    GravityTightFit,
    #[serde(rename = "gravitycorrective")]
    GravityCorrective,
}

impl From<PlacementTypeDto> for PlacementType {
    fn from(dto: PlacementTypeDto) -> Self {
        match dto {
            PlacementTypeDto::Gravity => PlacementType::Gravity,
            PlacementTypeDto::Box => PlacementType::Box,
            PlacementTypeDto::ConvexHull => PlacementType::ConvexHull,
            PlacementTypeDto::TightFit => PlacementType::TightFit,
            PlacementTypeDto::GravityTightFit => PlacementType::GravityTightFit,
            PlacementTypeDto::GravityCorrective => PlacementType::GravityCorrective,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct NestConfigDto {
    pub placement_type: PlacementTypeDto,
    pub rotations: u32,
    pub population_size: usize,
    pub mutation_rate: f64,
    #[serde(default = "default_dominant_part_area_threshold")]
    pub dominant_part_area_threshold: f64,
    #[serde(default = "default_curve_tolerance")]
    pub curve_tolerance: f64,
    pub generations: usize,
    /// Minimum clearance between a part and the sheet's true edge. Applied
    /// via `geometry::clearance::prepare_sheet` - see that module's doc
    /// comment for why this needs to be independent of `spacing`, not just
    /// "half the spacing" like the original app's single-parameter model.
    /// Defaults to 0.0 (no edge clearance requirement) - a laser job with
    /// no margin/spacing at all must be a true no-op, not a degenerate case.
    #[serde(default)]
    pub margin: f64,
    /// Minimum clearance between two parts' true outlines. Applied via
    /// `geometry::clearance::prepare_part`. Defaults to 0.0.
    #[serde(default)]
    pub spacing: f64,
    /// Caps how many CPU threads a single `run_nest` call's rayon-parallel
    /// generation evaluation may use (`dispatch::run_generation`'s
    /// `par_iter()`). `0` (the default) means "no cap" - rayon's own global
    /// pool, sized to all available cores. Scoped to this one call via a
    /// fresh `rayon::ThreadPoolBuilder` rather than touching rayon's global
    /// pool, which can only ever be configured once per process.
    #[serde(default)]
    pub max_threads: usize,
    /// Seed for `nesting::ga::GeneticAlgorithm`'s RNG - the same seed with
    /// the same everything else always reproduces the exact same run
    /// (initial population, every mutation/crossover/selection roll across
    /// every generation). Defaults to 0 for old saved configs that predate
    /// this field. See `GeneticAlgorithm::new`'s own doc comment for why
    /// this replaced `rand::thread_rng()` - comparing placement strategies
    /// needs to isolate "did this change actually help" from "did this run
    /// just get a luckier starting population."
    #[serde(default)]
    pub seed: u64,
    /// How many increasingly thorough attempts to run automatically - each
    /// one tries one more rotation angle than the last (`rotations` above is
    /// this escalation's *starting* value, not a fixed setting - see
    /// `commands::run_nest_with_progress`'s run loop) plus a proportionally
    /// larger population/generation budget to actually search that wider
    /// grid, keeping whichever attempt actually nests best. This is the one
    /// knob the simple/default UI exposes; `rotations`/`population_size`/
    /// `generations` are tucked under Advanced Settings as this escalation's
    /// starting point, for anyone who wants to override where it begins.
    /// Defaults to 1 (exactly the given settings, no escalation) for old
    /// saved configs/API callers that predate this field - the friction-free
    /// default of trying several escalating attempts is index.html's own
    /// field default, not this one, so a pre-existing saved config's
    /// behavior never silently changes underneath it.
    #[serde(default = "default_runs")]
    pub runs: usize,
    /// Percent (0-100). After the main run, any sheet whose own utilisation
    /// ends up below this gets repacked in place - same technique/config as
    /// the main run, that sheet's current parts only (see
    /// `nesting::repack::repack_sheet`; never pulls parts from other
    /// sheets - that's `refine_consolidation`'s job, not this one). `None`
    /// (the default) turns the pass off, so old saved configs keep today's
    /// behavior unchanged.
    #[serde(default)]
    pub cleanup_threshold_percent: Option<f64>,
    /// Let the nest also try each part flipped over (mirrored), not just
    /// rotated. Off by default, and deliberately loud in the UI: a mirrored
    /// part is only the same part if the material has no side (no grain, no
    /// coating, no printed face) and no asymmetric feature has to stay on
    /// one face - flipping a part that *does* have a side silently produces
    /// scrap. See `nesting::dispatch::MIRROR_ID_BIT` for how a mirrored part
    /// is carried through the run.
    #[serde(default)]
    pub mirror: bool,
}

fn default_runs() -> usize {
    1
}

fn default_dominant_part_area_threshold() -> f64 {
    nesting::placement::DEFAULT_DOMINANT_PART_AREA_THRESHOLD
}

fn default_curve_tolerance() -> f64 {
    0.1
}

impl NestConfigDto {
    pub fn placement_config(&self) -> PlacementConfig {
        PlacementConfig {
            placement_type: self.placement_type.into(),
            rotations: self.rotations,
            dominant_part_area_threshold: self.dominant_part_area_threshold,
            curve_tolerance: self.curve_tolerance,
        }
    }

    pub fn ga_config(&self) -> GaConfig {
        GaConfig { population_size: self.population_size, mutation_rate: self.mutation_rate, rotations: self.rotations, mirror: self.mirror }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct RunNestRequest {
    pub sheets: Vec<PolygonDto>,
    pub parts: Vec<PartDto>,
    pub config: NestConfigDto,
}

// Deserialize too (not just Serialize): run_nest_command returns these to
// the frontend, but export_dxf_command needs to accept the very same
// placements back - the frontend already has them from the run_nest
// response and shouldn't need the engine to recompute anything just to
// export what it already showed on screen.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct PlacedPartDto {
    pub id: usize,
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SheetPlacementDto {
    pub sheet_index: usize,
    pub parts: Vec<PlacedPartDto>,
}

/// Request for the manual per-sheet "REPACK" trigger (`commands::repack_sheet`) -
/// the click-a-sheet counterpart to the automatic
/// `NestConfigDto::cleanup_threshold_percent` pass, both backed by the same
/// `nesting::repack::repack_sheet`.
#[derive(Deserialize, Clone, Debug)]
pub struct RepackSheetRequest {
    pub sheet: PolygonDto,
    pub placement: SheetPlacementDto,
    /// True, unpadded geometry for every id in `placement.parts` - just
    /// this sheet's subset (the frontend already has all of it from
    /// `RunNestResponse::parts_by_id`).
    pub parts_by_id: HashMap<usize, PolygonDto>,
    /// The same config used for the main run, reused verbatim - not a
    /// separate "repack settings" (same rights/techniques as the first nest).
    pub config: NestConfigDto,
}

#[derive(Serialize, Clone, Debug)]
pub struct RepackSheetResponse {
    pub placement: SheetPlacementDto,
    /// `false` means `placement` is unchanged from the request - the
    /// frontend uses this to show "no improvement found" vs "improved".
    pub improved: bool,
    pub utilisation: f64,
}

#[derive(Serialize, Clone, Debug)]
pub struct RunNestResponse {
    pub placements: Vec<SheetPlacementDto>,
    pub fitness: f64,
    pub utilisation: f64,
    pub unplaced_count: usize,
    /// Ids of the parts that never fit any sheet, so the frontend can show
    /// *which* parts are missing (highlighted distinctly) instead of just
    /// the count.
    pub unplaced_ids: Vec<usize>,
    /// The authoritative id -> shape mapping `expand_parts` built for this
    /// run (true, unpadded geometry) - the frontend should hand this back
    /// to `export_dxf_command` verbatim rather than resending its own
    /// `parts`/quantities for `export_dxf` to re-run `expand_parts` on a
    /// second time. Re-deriving ids from client-resent input was a real
    /// silent-corruption risk: if that resent list ever differed in order,
    /// count, or content from what actually produced `placements`' ids (a
    /// stale cached array, a reorder, anything), `export_dxf` would resolve
    /// a placement's id to the *wrong* part's geometry with no error at
    /// all - just the wrong outline silently written at that position.
    pub parts_by_id: HashMap<usize, PolygonDto>,
    /// Every genuinely-better nest found during the run, in the order
    /// found (chronological, not sorted by fitness) - the top-level
    /// `placements`/`fitness`/etc. above are just `history`'s last entry,
    /// duplicated for callers that only want the winner and don't care
    /// about the rest. Lets the frontend show "the other nests it tried",
    /// not just the one that ended up best.
    pub history: Vec<NestSnapshotDto>,
    /// True if a `cancel_nest_command` call cut the run short before
    /// `generations` completed - `placements`/`fitness`/etc. above are still
    /// the best found up to that point, not an error, since a user-requested
    /// stop is a normal outcome, not a failure.
    pub cancelled: bool,
}

/// One candidate nest result kept in `RunNestResponse::history` - the same
/// shape as `RunNestResponse`'s own placement/fitness fields, plus which
/// generation produced it.
#[derive(Serialize, Clone, Debug)]
pub struct NestSnapshotDto {
    pub generation: usize,
    pub placements: Vec<SheetPlacementDto>,
    pub fitness: f64,
    pub utilisation: f64,
    pub unplaced_count: usize,
    pub unplaced_ids: Vec<usize>,
}

/// The best nest result across every run this app has ever completed,
/// persisted to disk (`commands::best_result_file_path`) so a later session
/// can offer to recover it instead of starting blank. Deliberately a
/// separate, smaller type from `RunNestResponse` - no `history` (a past
/// run's intermediate attempts aren't meaningful once you're just restoring
/// the winner) and no `cancelled` (irrelevant to a persisted snapshot) - and
/// deliberately carries its own `sheets`, which `RunNestResponse` doesn't:
/// a live session already has the request's sheets in hand to render
/// against, but a result recovered fresh in a *new* session has nothing
/// else to render it with.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BestResultDto {
    pub placements: Vec<SheetPlacementDto>,
    pub fitness: f64,
    pub utilisation: f64,
    pub unplaced_count: usize,
    pub unplaced_ids: Vec<usize>,
    pub parts_by_id: HashMap<usize, PolygonDto>,
    pub sheets: Vec<PolygonDto>,
}

/// What `export_dxf_command`/`export_svg_command` need to write a nest
/// result back out to either format - shared by both (same input shape,
/// only the on-disk format written differs) - exactly what the frontend
/// already has after a `run_nest_command` call: the original request's
/// `sheets` (true, unpadded geometry - the same ones `run_nest` was given,
/// not the padded shapes it built internally), `RunNestResponse::
/// parts_by_id` (the authoritative id -> shape mapping that call already
/// built - see its own doc comment for why this must be the same mapping,
/// not re-derived from a resent `parts`/quantity list), and that call's own
/// `placements` response.
#[derive(Deserialize, Clone, Debug)]
pub struct ExportRequest {
    pub sheets: Vec<PolygonDto>,
    /// Every part copy from the run, placed or not - `RunNestResponse::
    /// parts_by_id` covers both (see its own doc comment: it's `expand_parts`'
    /// full output, built before placement ever runs). `commands::
    /// build_export_layouts` consumes this by removing each id `placements`
    /// references; whatever's left over afterward *is* the unplaced set -
    /// see `include_unplaced` below, which is why this request needs no
    /// separate `unplaced_ids` field of its own.
    pub parts_by_id: HashMap<usize, PolygonDto>,
    pub placements: Vec<SheetPlacementDto>,
    /// Gap, in the same units as the geometry (mm), kept between
    /// consecutive sheets when laying them out left-to-right in one drawing
    /// space - neither DXF nor SVG has a notion of separate "sheets", so
    /// without this every sheet's parts would land in the same place and
    /// overlap. Also used as the spacing between rows/parts when
    /// `include_unplaced` packs the leftover parts (see below).
    pub sheet_spacing: f64,
    /// Whether to also write each used sheet's own outline (on the sheet's
    /// original layer), or omit it and write only the parts.
    pub include_sheet_outline: bool,
    /// Whether to also write every part that never got placed on any sheet,
    /// laid out in a simple non-overlapping grid after the last real sheet
    /// (`geometry::dxf_export::pack_unplaced_parts` - not a nesting pass,
    /// just keeps them from overlapping each other for visibility/manual
    /// handling). Defaults to `false` (today's behavior: export only what
    /// was actually placed) for old saved requests/API callers that predate
    /// this field.
    #[serde(default)]
    pub include_unplaced: bool,
}

/// Payload for the `"nest-progress"` event `run_nest_command` emits once per
/// completed generation, so the frontend can show a live console instead of
/// blocking silently until the whole run finishes - see
/// `commands::run_nest_with_progress`.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct NestProgressDto {
    pub generation: usize,
    pub generations: usize,
    pub best_fitness: f64,
    pub sheets_used: usize,
    pub unplaced_count: usize,
    pub utilisation: f64,
}

/// Payload for the `"nest-tick"` event, emitted from inside a single
/// generation as individuals are placed (not just once the whole generation
/// finishes, like `"nest-progress"` above) - see
/// `nesting::dispatch::run_generation`'s `on_individual_placed` doc comment
/// for why: a single individual can be real, tens-of-seconds work against
/// non-trivial geometry, and without a signal at this granularity a slow
/// generation looks indistinguishable from a hung one.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct NestTickDto {
    pub generation: usize,
    pub individuals_done: usize,
    pub individuals_total: usize,
}

/// Payload for the `"nest-run-start"` event - fired once right before each
/// escalating "Run"'s own generation loop starts (see `NestConfigDto::runs`'s
/// own doc comment for the escalation this narrates), so the console can say
/// what's about to be tried instead of only ever reporting after the fact.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct NestRunStartDto {
    /// 1-based - the Nth attempt out of `total_runs`.
    pub run: usize,
    pub total_runs: usize,
    pub rotations: u32,
    pub population_size: usize,
    pub generations: usize,
}

/// Payload for the `"nest-run-complete"` event, fired once a "Run" finishes -
/// except a run that never placed a single individual (`generations: 0` for
/// that run, or a cancel landing before the first individual finished),
/// which has no `run_best` to report and so emits no event at all; the
/// frontend only ever sees a `"nest-run-start"` for that attempt with no
/// matching completion. `improved` is true only if this run's result
/// actually beat every run before it in the same escalation (via
/// `nesting::ga::is_better_nest`), not just this run's own internal best -
/// the frontend uses this to color-code the console line (a new overall
/// best vs. a run that didn't pan out).
#[derive(Serialize, Clone, Copy, Debug)]
pub struct NestRunCompleteDto {
    pub run: usize,
    pub total_runs: usize,
    pub rotations: u32,
    pub population_size: usize,
    pub generations: usize,
    pub sheets_used: usize,
    pub unplaced_count: usize,
    pub utilisation: f64,
    pub improved: bool,
}


