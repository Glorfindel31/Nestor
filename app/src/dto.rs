//! Boundary types between the UI and the internal `geometry`/`nesting`
//! types, and the on-disk format for `config.json`/`best_result.json`/
//! `shapes.json`. Kept as a separate, explicit conversion layer
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

impl PolygonDto {
    /// The single constructor, mirroring `LayeredPolygon::new`:
    /// `children`/`texts`/`real_boundary` start empty.
    #[must_use]
    pub fn new(points: Vec<PointDto>, layer: String, is_circle: Option<CircleDto>) -> Self {
        PolygonDto { points, layer, is_circle, children: Vec::new(), texts: Vec::new(), real_boundary: None }
    }

    /// Unsigned shoelace area of the outline.
    #[must_use]
    pub fn area(&self) -> f64 {
        let n = self.points.len();
        if n < 3 {
            return 0.0;
        }
        let mut sum = 0.0;
        for i in 0..n {
            let j = (i + 1) % n;
            sum += self.points[i].x * self.points[j].y - self.points[j].x * self.points[i].y;
        }
        (sum / 2.0).abs()
    }

    /// Outline area minus its holes - the DTO-side twin of
    /// `geometry::polygon_material_area`, and the rule a commercial job
    /// report's Util column uses. Counting a drilled hole as material
    /// overstates every sheet holding a holed part, so a readout that used
    /// `area()` here could not be read beside the run's own utilisation.
    #[must_use]
    pub fn material_area(&self) -> f64 {
        (self.area() - self.children.iter().map(PolygonDto::area).sum::<f64>()).max(0.0)
    }
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
    /// The only angles (degrees) this part may be placed at, replacing the
    /// job-wide `rotations` grid for this part alone. `None` (the default)
    /// means unconstrained. The driving case is grain direction: material
    /// with a visible grain, a coating or a printed face often allows only
    /// 0/180. Deliberately a *replacement* rather than a filter over the
    /// grid - filtering would make 180 unreachable at `rotations: 3` and
    /// leave the part silently unplaceable.
    #[serde(default)]
    pub allowed_rotations: Option<Vec<f64>>,
    /// Overrides `NestConfigDto::mirror` for this part alone, so one job can
    /// mix flippable and non-flippable pieces. `None` (the default) follows
    /// the job-wide switch.
    #[serde(default)]
    pub mirror: Option<bool>,
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
/// What `expand_parts` produces: one entry per physical part copy, plus the
/// per-part rules those copies inherited from their definition. Was a
/// 3-tuple before per-part constraints existed; a named struct now that it
/// would otherwise be four positional values.
pub struct ExpandedParts {
    pub adam: Vec<usize>,
    pub parts_by_id: HashMap<usize, LayeredPolygon>,
    pub shape_ids: HashMap<usize, usize>,
    pub part_rules: nesting::placement::PartRules,
}

/// Normalises an authored angle list into what `PartRule` wants: degrees in
/// `[0, 360)`, sorted, deduped, and non-finite values dropped. An empty
/// result means unconstrained, which is also what a caller sending `[]`
/// should get - "no allowed angles at all" is never a useful request, and
/// treating it as a constraint would make the part unplaceable.
///
/// **Deduped at one degree, which is the resolution the NFP cache key
/// actually distinguishes** (`cache_key::normalize_rotation` truncates to an
/// integer, a preserved port behaviour). This used to dedupe at `1e-9`, nine
/// decades finer, so an authored list containing both `22.5` and `22.7` kept
/// two entries that then collided onto cache key `22` - whichever was
/// computed first served its NFP to the other, and the part was placed
/// against collision geometry for the wrong orientation. Angles under a
/// degree apart are not separable by this engine, so collapsing them is
/// honest where keeping both is silently wrong. Nothing the UI emits is
/// affected (`RotRule` only ever produces multiples of 90); a hand-edited
/// `config.json`/`shapes.json` is what reaches this.
fn normalize_angles(angles: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = angles.iter().filter(|a| a.is_finite()).map(|a| a.rem_euclid(360.0)).collect();
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    out
}

#[must_use]
pub fn expand_parts(parts: Vec<PartDto>, mirror: bool) -> ExpandedParts {
    let mut parts_by_id = HashMap::new();
    let mut shape_ids = HashMap::new();
    let mut rules: HashMap<usize, nesting::placement::PartRule> = HashMap::new();
    let mut adam = Vec::new();
    let mut next_id = 0usize;

    for (source_id, part) in parts.into_iter().enumerate() {
        let angles = part.allowed_rotations.as_deref().map(normalize_angles).unwrap_or_default();
        let may_mirror = part.mirror.unwrap_or(mirror);
        // Only parts that actually constrain something get an entry - an
        // empty map is the "everything is unconstrained" fast path every
        // lookup below already treats as free.
        let rule = (!angles.is_empty() || may_mirror != mirror).then_some(nesting::placement::PartRule { angles, mirror: may_mirror });

        let polygon: LayeredPolygon = part.polygon.into();
        let Some(last) = part.quantity.checked_sub(1) else { continue };
        // Clone for every copy but the last, where a move does instead -
        // `quantity` copies never need more than `quantity - 1` clones.
        for _ in 0..last {
            parts_by_id.insert(next_id, polygon.clone());
            shape_ids.insert(next_id, source_id);
            if let Some(rule) = &rule {
                rules.insert(next_id, rule.clone());
            }
            adam.push(next_id);
            next_id += 1;
        }
        parts_by_id.insert(next_id, polygon);
        shape_ids.insert(next_id, source_id);
        if let Some(rule) = rule {
            rules.insert(next_id, rule);
        }
        adam.push(next_id);
        next_id += 1;
    }

    // A part's *effective* mirror flag decides whether its flipped variant is
    // registered at all - so a mirror-denied part has no mirrored geometry
    // for a stray gene to reach, rather than being kept honest by a check
    // somewhere downstream.
    for (&id, poly) in parts_by_id.clone().iter() {
        if rules.get(&id).map_or(mirror, |r| r.mirror) {
            parts_by_id.insert(id ^ MIRROR_ID_BIT, geometry::dxf_import::mirror_layered_polygon(poly));
            shape_ids.insert(id ^ MIRROR_ID_BIT, shape_ids[&id] ^ MIRROR_ID_BIT);
        }
    }

    // Decorate-sort-undecorate: each id's area is computed once up front
    // instead of being recomputed on every comparison a sort makes
    // (O(n log n) recomputations otherwise, for a value that never changes
    // mid-sort).
    //
    // **Bounding-box area, not material area.** The seed order decides which
    // part a sheet is built around and which ones are left to fill in behind
    // it, so what it has to rank by is how much *sheet* a part eats, and a
    // concave part eats its box. `nestTest03` (280x150 with a bite, 32,202mm2)
    // sorts behind `nestTest02` (a plain 120x300, 36,000mm2) by material and
    // ahead of it by box - and it is `nestTest03` that should be spent as
    // filler first, because it nests at only 77.3% on its own against
    // `nestTest02`'s 89.6%. Filling with the part that packs *well* alone is
    // what wastes sheets. On the four-part mixed benchmark this one comparison
    // is worth a sheet: 32 -> 31, matching the commercial nester.
    let mut adam_with_area: Vec<(usize, f64)> = adam
        .into_iter()
        .map(|id| {
            let bounds = geometry::polygon::get_polygon_bounds(&parts_by_id[&id].points);
            (id, bounds.map_or(0.0, |b| b.width * b.height))
        })
        .collect();
    adam_with_area.sort_by(|&(_, area_a), &(_, area_b)| area_b.total_cmp(&area_a));
    let adam: Vec<usize> = adam_with_area.into_iter().map(|(id, _)| id).collect();

    ExpandedParts { adam, parts_by_id, shape_ids, part_rules: std::sync::Arc::new(rules) }
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
    ///
    /// This is what the user asks for; what the engine is *given* is
    /// `effective_spacing()`, which adds the kerf the cut itself will eat.
    #[serde(default)]
    pub spacing: f64,
    /// Cut width - how much material the tool destroys, centred on the path
    /// it follows. A machine property, not a nesting choice, which is why it
    /// is its own field: 0.15mm on a fibre laser, 1.5mm on plasma, ~0 on a
    /// waterjet finish pass. Defaults to 0.0, which is exactly the old
    /// behaviour.
    ///
    /// **Why it cannot just be folded into `spacing` by hand.** `spacing` is
    /// the web the user wants *left standing* between two finished parts, and
    /// the two cuts either side of that web each eat `kerf` of it - the path
    /// runs `kerf / 2` outside the outline and removes `kerf` of width, so
    /// the channel spans the outline outward by a full `kerf`. Two outlines
    /// `spacing` apart therefore leave `spacing - 2 * kerf` of material, and
    /// at plasma kerfs that is most of a 3mm web gone. Against the sheet edge
    /// only one cut is involved, and only its outer half leaves the part, so
    /// the margin owes `kerf / 2`.
    ///
    /// **This is the nesting half only.** Whether the exported geometry
    /// should be the drawn outline (and the CAM applies cutter compensation)
    /// or a path already offset outward by `kerf / 2` is a property of the
    /// user's machine and post-processor, and we do not know it - see
    /// `PLAN.md` 2.2. Export is unchanged: it still writes the drawn outline.
    #[serde(default)]
    pub kerf: f64,
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
    /// default of trying several escalating attempts is the deleted Electron
    /// frontend's own
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
    /// The clearance the engine is actually given between two parts:
    /// `spacing` plus the `kerf` each of the two cuts eats out of that web.
    #[must_use]
    pub fn effective_spacing(&self) -> f64 {
        self.spacing + 2.0 * self.kerf
    }

    /// The clearance the engine is actually given to the sheet edge: `margin`
    /// plus the half of the edge cut's kerf that falls outside the part.
    #[must_use]
    pub fn effective_margin(&self) -> f64 {
        self.margin + self.kerf / 2.0
    }

    pub fn placement_config(&self) -> PlacementConfig {
        PlacementConfig {
            placement_type: self.placement_type.into(),
            rotations: self.rotations,
            dominant_part_area_threshold: self.dominant_part_area_threshold,
            curve_tolerance: self.curve_tolerance,
            // Filled in by the caller from `expand_parts` - this struct has
            // no access to the parts list, only to the job-wide settings.
            part_rules: Default::default(),
            // **On.** The band packer (`nesting::banded`) finds the
            // banded layouts the greedy pass structurally cannot, and
            // `place_parts` only keeps its sheet when it holds more material
            // than the greedy one - so on interlocking shapes, where it is
            // bad, it simply never wins. Measured on the reference job
            // (`sheet_spread ref 3 4 0 6`): 18 sheets -> 15, best sheet
            // 76.5% -> 82.0%.
            //
            // It was off until its pairing searched the whole Pareto front of
            // pair boxes rather than the densest one - see
            // `banded::pareto_front`, and
            // `nesting/tests/banded_real_geometry.rs` for the overlap and
            // on-sheet invariants that gate it.
            // `NEST_NO_BANDED=1` turns it off, matching the same switch
            // `sheet_spread` honours - so a suspect result can be bisected
            // against the greedy path without a rebuild.
            banded_pass: !std::env::var("NEST_NO_BANDED").is_ok_and(|v| v != "0"),
        }
    }

    pub fn ga_config(&self) -> GaConfig {
        GaConfig { population_size: self.population_size, mutation_rate: self.mutation_rate, rotations: self.rotations, mirror: self.mirror, part_rules: Default::default() }
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
    /// Pinned by the user: a repack must leave this part exactly where it
    /// is and fit everything else around it. Lives on the placement rather
    /// than in a separate list so it cannot drift from the geometry it
    /// describes, and defaults to `false` so payloads that predate it (an
    /// older `best_result.json`) still load.
    #[serde(default)]
    pub locked: bool,
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
/// Wire form of `nesting::placement::PartRule` - the internal type lives in
/// an I/O-free crate, same reason every other type here has a DTO twin.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PartRuleDto {
    pub angles: Vec<f64>,
    pub mirror: bool,
}

impl From<&nesting::placement::PartRule> for PartRuleDto {
    fn from(rule: &nesting::placement::PartRule) -> Self {
        PartRuleDto { angles: rule.angles.clone(), mirror: rule.mirror }
    }
}

impl From<PartRuleDto> for nesting::placement::PartRule {
    fn from(dto: PartRuleDto) -> Self {
        nesting::placement::PartRule { angles: dto.angles, mirror: dto.mirror }
    }
}

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
    /// Per-part orientation constraints for the parts on this sheet, keyed
    /// by part id - the same map `RunNestResponse::part_rules` reports back.
    /// Without it a manual repack would happily re-rotate a grain-locked
    /// part into an orientation the main nest was forbidden from using.
    #[serde(default)]
    pub part_rules: HashMap<usize, PartRuleDto>,
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
    /// The per-part orientation constraints this run actually applied, keyed
    /// the same way `parts_by_id` is. Authoritative for the same reason it
    /// is: a later `repack_sheet_command` must be held to the same rules the
    /// nest was, and re-deriving them from a client-resent `parts` list is
    /// exactly the silent-corruption risk described above.
    pub part_rules: HashMap<usize, PartRuleDto>,
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
    /// Defaulted so a `best_result.json` written before per-part rules
    /// existed still loads - an old file simply has no constraints.
    #[serde(default)]
    pub part_rules: HashMap<usize, PartRuleDto>,
    /// The config that produced this result. Needed because a recovered
    /// result is a real, repackable/exportable result: without it the
    /// frontend had `lastNestRequest` with no `.config` at all, so REPACK on
    /// a recovered nest threw. Defaulted for the same backwards-compatibility
    /// reason as `part_rules`.
    #[serde(default)]
    pub config: Option<NestConfigDto>,
}

/// Input for `commands::validate_placement` - the result view asking whether
/// a hand-dragged part may rest where the pointer left it.
#[derive(Deserialize, Clone, Debug)]
pub struct ValidatePlacementRequest {
    pub sheet: PolygonDto,
    /// The sheet's current placement, including the dragged part's *old*
    /// position (which is ignored - a part is never an obstacle to itself).
    pub placement: SheetPlacementDto,
    pub parts_by_id: HashMap<usize, PolygonDto>,
    pub moved_id: usize,
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
    /// The run's own config - `margin`/`spacing` decide the answer, so this
    /// has to be the config the nest actually ran with, not defaults.
    pub config: NestConfigDto,
}

#[derive(Serialize, Clone, Copy, Debug)]
pub struct ValidatePlacementResponse {
    pub valid: bool,
}

/// What `commands::audit_nest` needs to check a whole result rather than one
/// dragged part.
///
/// Deliberately the same input shape as `ExportRequest`: the audit answers
/// "is what I am about to export cuttable?", so anything it could check that
/// export doesn't see would be checking a different nest than the one that
/// gets written. `sheets` are the true, unpadded shapes - the audit applies
/// `margin`/`spacing` itself, from `config`, exactly as `run_nest` did.
#[derive(Deserialize, Clone, Debug)]
pub struct AuditRequest {
    pub sheets: Vec<PolygonDto>,
    pub placements: Vec<SheetPlacementDto>,
    pub parts_by_id: HashMap<usize, PolygonDto>,
    pub config: NestConfigDto,
}

/// One problem found, flattened for display. `kind` is a stable string rather
/// than the engine enum so the UI (and any future report format) doesn't have
/// to be recompiled in step with `nesting::audit`.
#[derive(Serialize, Clone, Debug)]
pub struct AuditIssueDto {
    pub kind: String,
    /// Whether this means "do not cut this", as opposed to advisory.
    pub fatal: bool,
    pub sheet_index: usize,
    pub part_ids: Vec<usize>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct AuditReportDto {
    /// False if anything fatal was found. Warnings alone still pass - see
    /// `nesting::audit`'s module doc for why that separation matters.
    pub passed: bool,
    pub fatal_count: usize,
    pub warning_count: usize,
    pub issues: Vec<AuditIssueDto>,
}

impl From<&nesting::audit::AuditReport> for AuditReportDto {
    fn from(report: &nesting::audit::AuditReport) -> Self {
        use nesting::audit::IssueKind;
        AuditReportDto {
            passed: report.passed(),
            fatal_count: report.fatal_count(),
            warning_count: report.warning_count(),
            issues: report
                .issues
                .iter()
                .map(|i| AuditIssueDto {
                    kind: match i.kind {
                        IssueKind::Overlap => "overlap",
                        IssueKind::OutsideSheet => "outside_sheet",
                        IssueKind::BelowSpacing => "below_spacing",
                        IssueKind::OutsideMargin => "outside_margin",
                    }
                    .to_string(),
                    fatal: i.kind.is_fatal(),
                    sheet_index: i.sheet_index,
                    part_ids: i.part_ids.clone(),
                })
                .collect(),
        }
    }
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

/// One row of the PDF report's piece table. Names and quantities live only
/// in the frontend's own table, so they have to be sent - nothing on the
/// backend knows what the user called a shape.
#[derive(Deserialize, Clone, Debug)]
pub struct ReportPartDto {
    pub name: String,
    /// How many were ordered.
    pub quantity: usize,
    /// How many the result actually placed. Sent rather than derived: a
    /// `PlacedShape` carries no part identity by the time the report draws it,
    /// so only the UI - which knows which id block belongs to which row - can
    /// answer this. See `ui::result::report_part_list`.
    #[serde(default)]
    pub nested: usize,
    /// The piece itself, holes included. Size, area, contour count and cut
    /// length are all measured off this by the report rather than sent, so
    /// they cannot disagree with the shape it draws.
    pub polygon: PolygonDto,
}

/// Input for `commands::export_report`. Wraps `ExportRequest` rather than
/// adding fields to it: DXF/SVG callers have no business carrying
/// report-only metadata. Every number the report prints that *is* derivable
/// is computed from the drawn geometry instead of being sent, so the figures
/// and the picture can never disagree.
#[derive(Deserialize, Clone, Debug)]
pub struct ReportRequest {
    pub export: ExportRequest,
    pub config: NestConfigDto,
    #[serde(default)]
    pub parts: Vec<ReportPartDto>,
    /// Shown as the report's heading.
    #[serde(default)]
    pub title: Option<String>,
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

/// A shape the user saved to reuse later: either a part they cut regularly,
/// or an offcut sitting on a shelf.
///
/// One type for both, distinguished by `kind`, because everything that
/// handles them - store, list, pick, delete - treats them identically. The
/// difference is entirely in what the user does with it: a part goes into a
/// job as something to cut, a remnant as something to cut it *from*.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoredKind {
    Part,
    Remnant,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct StoredShape {
    /// Unique within the store and never reused - a picker holding an id
    /// across a delete must fail to find it rather than silently resolve to
    /// whatever took its place.
    pub id: usize,
    pub kind: StoredKind,
    /// What the user sees. Generated for a remnant (source job plus usable
    /// size); theirs to set for a part.
    pub name: String,
    pub polygon: PolygonDto,
    /// Quantity to pre-fill when a saved part is added to a job. Meaningless
    /// for a remnant, which is by definition a single physical object.
    #[serde(default = "one")]
    pub default_qty: usize,
    /// The grain rule the part was saved with, as *angles* rather than as the
    /// UI's enum name. Storing the angles means an entry written by an older
    /// build still loads after a new rule variant is added, and it is the
    /// same wire form `PartDto::allowed_rotations` already uses.
    #[serde(default)]
    pub allowed_rotations: Option<Vec<f64>>,
    /// The per-part mirror override, matching `PartDto::mirror`.
    #[serde(default)]
    pub mirror: Option<bool>,
    /// When it was saved, for the FIFO ordering a remnant shelf wants.
    #[serde(default)]
    pub created: String,
    /// Set once a remnant has been nested onto, so it stops being offered
    /// without being deleted - the record of what it became outlives the row.
    #[serde(default)]
    pub consumed: bool,
}

/// The on-disk parts library and remnant shelf.
///
/// `version` from the first release, deliberately: this file outlives
/// releases and users will hand-edit it. The format is additive - every
/// optional field carries `#[serde(default)]` and nothing is ever removed -
/// because losing a saved library is the one failure here nobody forgives.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ShapeStore {
    pub version: u32,
    #[serde(default)]
    pub shapes: Vec<StoredShape>,
    /// Highest id ever issued. Persisted rather than derived from `shapes`,
    /// so deleting the newest entry doesn't make the next save reuse its id.
    #[serde(default)]
    pub next_id: usize,
}

impl Default for ShapeStore {
    fn default() -> Self {
        Self { version: 1, shapes: Vec::new(), next_id: 1 }
    }
}

impl ShapeStore {
    /// Adds a shape, assigning it the next id. Returns that id.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        kind: StoredKind,
        name: String,
        polygon: PolygonDto,
        default_qty: usize,
        allowed_rotations: Option<Vec<f64>>,
        mirror: Option<bool>,
        created: String,
    ) -> usize {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.shapes.push(StoredShape { id, kind, name, polygon, default_qty, allowed_rotations, mirror, created, consumed: false });
        id
    }

    /// Everything of one kind that is still available, oldest first.
    ///
    /// FIFO rather than newest-first, specifically for remnants: an offcut
    /// shelf worked newest-first accumulates old stock that never gets used
    /// and is eventually thrown away - the exact waste the feature exists to
    /// prevent.
    #[must_use]
    pub fn available(&self, kind: StoredKind) -> Vec<&StoredShape> {
        let mut out: Vec<&StoredShape> = self.shapes.iter().filter(|s| s.kind == kind && !s.consumed).collect();
        out.sort_by(|a, b| a.created.cmp(&b.created));
        out
    }

    /// Marks a remnant used up. Returns false if the id isn't in the store.
    pub fn consume(&mut self, id: usize) -> bool {
        match self.shapes.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                s.consumed = true;
                true
            }
            None => false,
        }
    }

    pub fn remove(&mut self, id: usize) -> bool {
        let before = self.shapes.len();
        self.shapes.retain(|s| s.id != id);
        self.shapes.len() != before
    }
}

/// What `commands::compute_remnants` needs: the result to harvest offcuts
/// from. Same shape as `AuditRequest` for the same reason - offcuts are
/// computed from exactly what would be exported.
#[derive(Deserialize, Clone, Debug)]
pub struct RemnantRequest {
    pub sheets: Vec<PolygonDto>,
    pub placements: Vec<SheetPlacementDto>,
    pub parts_by_id: HashMap<usize, PolygonDto>,
    pub config: NestConfigDto,
}

/// One computed offcut on its way to the UI.
#[derive(Serialize, Clone, Debug)]
pub struct RemnantDto {
    /// Which sheet of the result it came off.
    pub sheet_index: usize,
    pub polygon: PolygonDto,
    pub area: f64,
    /// Largest rectangle that fits inside it - the size to write on the label.
    pub usable_width: f64,
    pub usable_height: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f64) -> Vec<PointDto> {
        vec![
            PointDto { x: 0.0, y: 0.0 },
            PointDto { x: size, y: 0.0 },
            PointDto { x: size, y: size },
            PointDto { x: 0.0, y: size },
        ]
    }

    /// `cache_key::normalize_rotation` truncates a rotation to an integer, so
    /// two authored angles inside the same degree are one cache entry. Keeping
    /// both meant the second was placed against the first's NFP.
    #[test]
    fn authored_angles_are_deduped_at_the_cache_keys_own_resolution() {
        use nesting::cache_key::normalize_rotation;

        let kept = normalize_angles(&[22.5, 22.7, 45.0]);
        assert_eq!(kept, vec![22.5, 45.0], "22.7 is not separable from 22.5");

        let mut keys: Vec<i64> = kept.iter().copied().map(normalize_rotation).collect();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "every surviving angle must own a distinct cache key");
    }

    /// Angles a full degree apart are separable and must all survive - the
    /// dedupe must not swallow a real 90-degree grain rule.
    #[test]
    fn angles_a_degree_or_more_apart_all_survive() {
        assert_eq!(normalize_angles(&[0.0, 90.0, 180.0, 270.0]).len(), 4);
        assert_eq!(normalize_angles(&[0.0, 1.0, 2.0]).len(), 3);
        // out of range, negative, duplicate and non-finite all handled
        assert_eq!(normalize_angles(&[360.0, -90.0, 0.0, f64::NAN]), vec![0.0, 270.0]);
    }

    /// The GUI's per-sheet readout and the headless CLI's per-sheet column
    /// both go through `material_area`. Counting a drilled hole as material
    /// overstates every sheet holding a holed part, which is what made the
    /// two disagree with each other and with the run's own utilisation
    /// (`nesting::consolidation::recompute_totals`, which has always
    /// subtracted holes).
    #[test]
    fn material_area_subtracts_holes_while_area_does_not() {
        let mut poly = PolygonDto::new(square(10.0), "cut".into(), None);
        poly.children.push(PolygonDto::new(square(3.0), "drill".into(), None));

        assert_eq!(poly.area(), 100.0);
        assert_eq!(poly.material_area(), 91.0);
    }

    /// Winding must not change the magnitude - imported DXF and SVG profiles
    /// do not agree on direction - and a degenerate ring contributes nothing
    /// rather than panicking.
    #[test]
    fn area_is_unsigned_and_survives_degenerate_rings() {
        let mut reversed = square(10.0);
        reversed.reverse();
        assert_eq!(PolygonDto::new(reversed, "cut".into(), None).area(), 100.0);
        assert_eq!(PolygonDto::new(Vec::new(), "cut".into(), None).area(), 0.0);
        assert_eq!(PolygonDto::new(square(1.0)[..2].to_vec(), "cut".into(), None).material_area(), 0.0);
    }
}
