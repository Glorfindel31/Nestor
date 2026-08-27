//! The UI's own data model.
//!
//! The web version kept role / quantity / allowed-angles / per-part mirror
//! **only in the DOM**, reading them back with `querySelector` at request
//! time. That's what forced its shapes table to be append-only, and what
//! caused the documented bug where switching language left already-created
//! rows' dropdown wording in the old language. Here they're plain fields on
//! `ShapeRow`, and egui rebuilds every row from them each frame - so the
//! table can be rebuilt freely, and that whole class of staleness is gone.

use crate::dto::{NestConfigDto, PlacementTypeDto, PolygonDto};

/// One imported (or hand-defined) shape, plus everything the user has
/// decided about it.
pub struct ShapeRow {
    /// Monotonic and never reused, so a row keeps its identity across
    /// removals. Not an index into `shapes`.
    pub ui_id: usize,
    /// Source filename (or the layer name, for a hand-added rectangle) -
    /// only ever displayed, never parsed.
    pub file: String,
    pub poly: PolygonDto,
    pub role: Role,
    pub qty: usize,
    pub rot: RotRule,
    pub mirror: MirrorRule,
    pub selected: bool,
    /// Cached shoelace area. Recomputed never - the geometry is immutable
    /// once imported, and the DOMINANT indicator re-reads this on every
    /// frame for every row.
    pub area: f64,
    /// The library entry this row came from, if any.
    ///
    /// Carried so a remnant can be marked consumed once it has actually been
    /// nested onto - without it the offcut shelf only ever grows, and the
    /// same physical piece of material gets offered again for every future
    /// job. Rows that were imported from a file have no store entry and stay
    /// `None`.
    pub from_store: Option<usize>,
}

impl ShapeRow {
    /// The single constructor. Two nearly-identical inlined literals existed
    /// before (one for import, one for the library) and adding a field meant
    /// remembering both; this makes forgetting one a compile error instead of
    /// a silently half-initialised row.
    pub fn new(ui_id: usize, file: String, poly: PolygonDto) -> Self {
        let area = poly.area();
        Self {
            ui_id,
            file,
            poly,
            role: Role::Part,
            qty: 1,
            rot: RotRule::Any,
            mirror: MirrorRule::Job,
            selected: false,
            area,
            from_store: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Part,
    Sheet,
    Skip,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Part, Role::Sheet, Role::Skip];

    pub fn key(self) -> &'static str {
        match self {
            Role::Part => "role_part",
            Role::Sheet => "role_sheet",
            Role::Skip => "role_skip",
        }
    }
}

/// Which angles a part is allowed to be placed at, overriding the job-wide
/// rotation grid. `Any` sends `None` - the backend only materialises a rule
/// for parts that actually constrain something.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RotRule {
    Any,
    Half,
    Quarter,
    Fixed,
    /// 90 degrees only. The mirror image of `Fixed`, and not reachable by any
    /// combination of the others: a part whose grain runs along its own Y
    /// axis has to be turned a quarter turn to lie along the material's
    /// grain, and must then stay there.
    Fixed90,
    /// 90 / 270 - the quarter-turn counterpart of `Half`, for a grained part
    /// that has been drawn across the grain rather than along it.
    HalfCross,
}

impl RotRule {
    pub const ALL: [RotRule; 6] = [RotRule::Any, RotRule::Half, RotRule::Quarter, RotRule::Fixed, RotRule::Fixed90, RotRule::HalfCross];

    pub fn key(self) -> &'static str {
        match self {
            RotRule::Any => "rot_any",
            RotRule::Half => "rot_0_180",
            RotRule::Quarter => "rot_quarter",
            RotRule::Fixed => "rot_fixed",
            RotRule::Fixed90 => "rot_fixed_90",
            RotRule::HalfCross => "rot_90_270",
        }
    }

    /// Reverse of `angles` - recovers the rule a saved part was stored with.
    ///
    /// The store keeps the *angles*, not the enum name, so an entry written
    /// by an older build still loads when a new variant is added. Anything
    /// that doesn't match a known rule falls back to `Any`: an unrecognised
    /// constraint must not silently become a different constraint.
    #[must_use]
    pub fn from_angles(angles: Option<&[f64]>) -> Self {
        let Some(angles) = angles else { return RotRule::Any };
        RotRule::ALL.into_iter().find(|r| r.angles().as_deref() == Some(angles)).unwrap_or(RotRule::Any)
    }

    pub fn angles(self) -> Option<Vec<f64>> {
        match self {
            RotRule::Any => None,
            RotRule::Half => Some(vec![0.0, 180.0]),
            RotRule::Quarter => Some(vec![0.0, 90.0, 180.0, 270.0]),
            RotRule::Fixed => Some(vec![0.0]),
            RotRule::Fixed90 => Some(vec![90.0]),
            RotRule::HalfCross => Some(vec![90.0, 270.0]),
        }
    }
}

/// Per-part override of the job-wide mirror switch, so one job can mix
/// flippable and non-flippable pieces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MirrorRule {
    Job,
    Allow,
    Deny,
}

impl MirrorRule {
    pub const ALL: [MirrorRule; 3] = [MirrorRule::Job, MirrorRule::Allow, MirrorRule::Deny];

    pub fn key(self) -> &'static str {
        match self {
            MirrorRule::Job => "mirror_job",
            MirrorRule::Allow => "mirror_allow",
            MirrorRule::Deny => "mirror_deny",
        }
    }

    /// Reverse of `as_option`.
    #[must_use]
    pub fn from_option(value: Option<bool>) -> Self {
        match value {
            None => MirrorRule::Job,
            Some(true) => MirrorRule::Allow,
            Some(false) => MirrorRule::Deny,
        }
    }

    pub fn as_option(self) -> Option<bool> {
        match self {
            MirrorRule::Job => None,
            MirrorRule::Allow => Some(true),
            MirrorRule::Deny => Some(false),
        }
    }
}

/// Axis-aligned bounds of a point list. Explicitly a loop rather than
/// `iter().fold` over four separate min/max passes - this runs per row, per
/// frame, over a real CAD file's dense arc tessellation.
pub fn bounds_of(points: &[crate::dto::PointDto]) -> Bounds {
    let mut b = Bounds { minx: f64::INFINITY, miny: f64::INFINITY, maxx: f64::NEG_INFINITY, maxy: f64::NEG_INFINITY };
    for p in points {
        b.minx = b.minx.min(p.x);
        b.maxx = b.maxx.max(p.x);
        b.miny = b.miny.min(p.y);
        b.maxy = b.maxy.max(p.y);
    }
    if points.is_empty() {
        b = Bounds { minx: 0.0, miny: 0.0, maxx: 0.0, maxy: 0.0 };
    }
    b
}

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub minx: f64,
    pub miny: f64,
    pub maxx: f64,
    pub maxy: f64,
}

impl Bounds {
    pub fn w(&self) -> f64 {
        self.maxx - self.minx
    }
    pub fn h(&self) -> f64 {
        self.maxy - self.miny
    }
}

/// The config form's own state. Separate from `NestConfigDto` because the
/// form holds things the DTO doesn't (curve tolerance lives in the IMPORT
/// panel, and the cleanup threshold is a "blank means off" text field), and
/// because a half-typed number must be allowed to exist here without being
/// a valid config yet.
#[derive(Clone, PartialEq, Debug)]
pub struct ConfigForm {
    pub margin: f64,
    pub spacing: f64,
    /// Cut width. See `NestConfigDto::kerf` for why this is not just part of
    /// `spacing`.
    pub kerf: f64,
    pub runs: usize,
    /// Blank = the cleanup pass is off. Kept as text, not `Option<f64>`, so
    /// clearing the field doesn't fight the user by snapping to a value.
    pub cleanup_threshold: String,
    pub mirror: bool,
    pub placement_type: PlacementTypeDto,
    pub rotations: u32,
    pub population_size: usize,
    /// Percent, 0-100 - passed through to the engine as-is, matching what
    /// the web UI sent.
    pub mutation_rate: f64,
    pub generations: usize,
    pub dominant_threshold: f64,
    pub max_threads: usize,
    pub seed: u64,
    /// Lives in the IMPORT panel but is part of the nest config, exactly as
    /// in the web UI - it governs how finely arcs are tessellated on import
    /// *and* is sent along with the run.
    pub curve_tolerance: f64,
}

impl Default for ConfigForm {
    fn default() -> Self {
        // Not `NestConfigDto`'s serde defaults - those exist to keep old saved
        // configs behaving unchanged and must not move.
        //
        // **Measured, not inherited.** These used to mirror the old web UI's
        // The deleted Electron frontend's field defaults (`runs: 6, rotations: 2,
        // population_size: 6, generations: 5, curve_tolerance: 0.1`), and that
        // combination is unusable on a real job. `runs` escalates every
        // attempt by +1 rotation, +4 population and +5 generations, so six
        // runs is 105 generations at up to 26 individuals and 7 angles. On the
        // four-part 800-piece benchmark (1500x1500, spacing 5), at that base:
        // one run takes 46.9s, two take 205.6s, three take over ten minutes,
        // and six never finished. Every attempt after the first returned the
        // *identical* 31-sheet answer.
        //
        // What each knob is actually worth on that job, measured on an idle
        // machine:
        //
        // - `runs`      6 -> 1:   4.4x per extra attempt, zero sheets gained
        // - `rotations` 2 -> 4:   the only knob that changes the answer (at 1
        //                         the job needs 32 sheets, at 2 or 4 it needs
        //                         31 - but 2 is job-specific luck, and 4 is
        //                         what holds across the whole benchmark board)
        // - `population` 6 -> 4:  free either way
        // - `generations` 5 -> 2: free either way (1 generation at population
        //                         2 finds the same 31 sheets as 3 at 5)
        // - `curve_tolerance` 0.1 -> 0.3: 2.7x faster with identical sheet
        //                         counts and marginally better utilisation;
        //                         the Minkowski sums the band packer runs per
        //                         shape scale with the tessellated point count
        //
        // Result: 31 sheets in 32.6s, from a default that previously did not
        // finish. Raising these is still there for anyone who wants it - see
        // `ui::config`'s cost estimate, which exists so the price is visible
        // before the wait rather than after it.
        Self {
            margin: 0.0,
            spacing: 0.0,
            kerf: 0.0,
            runs: 1,
            cleanup_threshold: String::new(),
            mirror: false,
            placement_type: PlacementTypeDto::TightFit,
            rotations: 4,
            population_size: 4,
            mutation_rate: 10.0,
            generations: 2,
            dominant_threshold: 0.9,
            max_threads: 0,
            seed: 0,
            curve_tolerance: 0.3,
        }
    }
}

impl ConfigForm {
    /// How expensive this search is against the shipped defaults, as a plain
    /// multiple.
    ///
    /// **The trap this exists to make visible.** `runs` is the one knob the
    /// basic panel exposes, and every extra attempt raises rotations *and*
    /// population *and* generations at once
    /// (`commands::escalated_run_config`), so its cost compounds - measured on
    /// the 800-part benchmark, one run took 46.9s, two took 205.6s and three
    /// took over ten minutes, all returning the identical answer. Nothing in
    /// the UI said so until the operator had already waited.
    ///
    /// A count of placements, not a time: an individual's cost is dominated by
    /// how many angles it has to try, and a generation evaluates a whole
    /// population, so `rotations x population x generations` summed over the
    /// escalation tracks the real work closely enough to warn with. It is
    /// deliberately not shown as seconds - that depends on the parts, and a
    /// wrong number in seconds is worse than an honest relative one.
    pub fn search_cost_multiple(&self) -> f64 {
        fn cost(runs: usize, rotations: u32, population: usize, generations: usize) -> f64 {
            (0..runs)
                .map(|i| {
                    let rotations = f64::from(rotations + i as u32);
                    let population = (population + i * crate::commands::RUN_POPULATION_STEP) as f64;
                    let generations = (generations + i * crate::commands::RUN_GENERATIONS_STEP) as f64;
                    rotations * population * generations
                })
                .sum()
        }
        let d = Self::default();
        let baseline = cost(d.runs, d.rotations, d.population_size, d.generations);
        if baseline <= 0.0 {
            return 1.0;
        }
        cost(self.runs, self.rotations, self.population_size, self.generations) / baseline
    }

    pub fn to_dto(&self) -> NestConfigDto {
        NestConfigDto {
            placement_type: self.placement_type,
            rotations: self.rotations,
            population_size: self.population_size,
            mutation_rate: self.mutation_rate,
            dominant_part_area_threshold: self.dominant_threshold,
            curve_tolerance: self.curve_tolerance,
            generations: self.generations,
            margin: self.margin,
            spacing: self.spacing,
            kerf: self.kerf,
            max_threads: self.max_threads,
            seed: self.seed,
            runs: self.runs,
            cleanup_threshold_percent: self.cleanup_threshold.trim().parse().ok(),
            mirror: self.mirror,
        }
    }

    pub fn from_dto(&mut self, d: &NestConfigDto) {
        self.placement_type = d.placement_type;
        self.rotations = d.rotations;
        self.population_size = d.population_size;
        self.mutation_rate = d.mutation_rate;
        self.dominant_threshold = d.dominant_part_area_threshold;
        self.curve_tolerance = d.curve_tolerance;
        self.generations = d.generations;
        self.margin = d.margin;
        self.spacing = d.spacing;
        self.kerf = d.kerf;
        self.max_threads = d.max_threads;
        self.seed = d.seed;
        self.runs = d.runs;
        self.cleanup_threshold = d.cleanup_threshold_percent.map(|v| v.to_string()).unwrap_or_default();
        // `mirror` is deliberately NOT restored - it always starts off.
        // A flip setting that quietly survives into a session where the
        // material *does* have a side (grain, coating, printed face)
        // produces scrap, and the cost of re-ticking a box is nothing
        // against that.
    }

    /// The NaN sweep the web UI ran before dispatching, naming the offending
    /// field. Without it an empty numeric input becomes a `NaN` that the
    /// engine rejects with a far less actionable message.
    pub fn first_nan_field(&self) -> Option<&'static str> {
        let checks: [(&'static str, f64); 9] = [
            ("cfg_margin", self.margin),
            ("cfg_spacing", self.spacing),
            ("cfg_kerf", self.kerf),
            ("cfg_mutation", self.mutation_rate),
            ("cfg_dominant", self.dominant_threshold),
            ("tolerance_label", self.curve_tolerance),
            ("cfg_rotations", self.rotations as f64),
            ("cfg_population", self.population_size as f64),
            ("cfg_generations", self.generations as f64),
        ];
        checks.into_iter().find(|(_, v)| v.is_nan()).map(|(k, _)| k)
    }
}

/// A one-line status message under a panel. `error` drives the colour, and
/// (for the run status specifically) whether it gets routed to the console
/// instead of shown inline.
#[derive(Default, Clone)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

impl Status {
    pub fn ok(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.error = false;
    }

    pub fn err(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.error = true;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.error = false;
    }
}

#[cfg(test)]
mod rule_round_trip_tests {
    use super::*;

    /// A saved library part must come back with the grain rule it went in
    /// with. Without the reverse mapping the library silently hands back an
    /// unconstrained copy, and a part that may only be cut along the grain
    /// becomes free to rotate - which is not a UI nicety, it is scrap.
    #[test]
    fn every_rotation_rule_survives_a_store_round_trip() {
        for rule in RotRule::ALL {
            let stored = rule.angles();
            assert_eq!(RotRule::from_angles(stored.as_deref()), rule, "{rule:?} did not survive the round trip");
        }
    }

    #[test]
    fn every_mirror_rule_survives_a_store_round_trip() {
        for rule in MirrorRule::ALL {
            assert_eq!(MirrorRule::from_option(rule.as_option()), rule, "{rule:?} did not survive the round trip");
        }
    }

    /// An angle set written by some future build must not silently become a
    /// *different* constraint - falling back to unconstrained is the only
    /// honest answer for a rule this build cannot express.
    #[test]
    fn an_unknown_angle_set_falls_back_to_unconstrained() {
        assert_eq!(RotRule::from_angles(Some(&[17.0, 191.0])), RotRule::Any);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::PointDto;

    fn poly(points: Vec<PointDto>) -> PolygonDto {
        PolygonDto { points, layer: "0".into(), is_circle: None, children: Vec::new(), texts: Vec::new(), real_boundary: None }
    }

    fn rect(w: f64, h: f64) -> Vec<PointDto> {
        vec![PointDto { x: 0.0, y: 0.0 }, PointDto { x: w, y: 0.0 }, PointDto { x: w, y: h }, PointDto { x: 0.0, y: h }]
    }

    #[test]
    fn area_and_bounds_agree_with_a_known_rectangle() {
        let r = rect(3.0, 4.0);
        assert_eq!(poly(r.clone()).area(), 12.0);
        let b = bounds_of(&r);
        assert_eq!((b.w(), b.h()), (3.0, 4.0));
        // Winding must not change the magnitude - imported DXF and SVG
        // profiles do not agree on direction.
        let mut reversed = r.clone();
        reversed.reverse();
        assert_eq!(poly(reversed).area(), 12.0);
    }

    #[test]
    fn degenerate_point_lists_do_not_produce_infinities() {
        let b = bounds_of(&[]);
        assert_eq!((b.minx, b.miny, b.w(), b.h()), (0.0, 0.0, 0.0, 0.0));
        assert_eq!(poly(Vec::new()).area(), 0.0);
        assert_eq!(poly(rect(1.0, 1.0)[..2].to_vec()).area(), 0.0);
    }

    #[test]
    fn a_blank_cleanup_threshold_means_off() {
        let mut f = ConfigForm::default();
        assert_eq!(f.to_dto().cleanup_threshold_percent, None);
        f.cleanup_threshold = "  ".into();
        assert_eq!(f.to_dto().cleanup_threshold_percent, None);
        f.cleanup_threshold = "40".into();
        assert_eq!(f.to_dto().cleanup_threshold_percent, Some(40.0));
        // Garbage is "off", not a panic and not a zero.
        f.cleanup_threshold = "forty".into();
        assert_eq!(f.to_dto().cleanup_threshold_percent, None);
    }

    #[test]
    fn loading_a_saved_config_never_restores_the_mirror_switch() {
        let mut f = ConfigForm::default();
        let mut saved = f.to_dto();
        saved.mirror = true;
        saved.spacing = 6.5;
        f.from_dto(&saved);
        assert_eq!(f.spacing, 6.5, "ordinary fields must round-trip");
        assert!(!f.mirror, "mirror must always start off, however it was saved");
    }

    #[test]
    fn the_nan_sweep_names_the_offending_field() {
        let mut f = ConfigForm::default();
        assert_eq!(f.first_nan_field(), None);
        f.spacing = f64::NAN;
        assert_eq!(f.first_nan_field(), Some("cfg_spacing"));
    }
    /// **The estimate has to be 1x at the defaults, or it cries wolf on every
    /// job.** It is the only thing telling an operator what a setting costs
    /// before they wait for it, so a baseline that drifts off 1.0 as the
    /// defaults change would make the warning meaningless.
    #[test]
    fn the_cost_estimate_reads_one_at_the_shipped_defaults() {
        assert!((ConfigForm::default().search_cost_multiple() - 1.0).abs() < 1e-9);
    }

    /// The old defaults, which is the case this whole estimate exists for:
    /// six escalating runs from `rotations: 2, population_size: 6,
    /// generations: 5` measured 46.9s for one run and over ten minutes for
    /// three on the 800-piece benchmark, all returning the same answer.
    #[test]
    fn the_cost_estimate_flags_the_old_defaults_as_expensive() {
        let old = ConfigForm { runs: 6, rotations: 2, population_size: 6, generations: 5, ..ConfigForm::default() };
        let cost = old.search_cost_multiple();
        assert!(cost >= 4.0, "the old defaults must land in the warning band, got {cost:.1}x");
    }

    /// Every knob it prices has to move it, or a user turning one up sees no
    /// change and learns to ignore the number.
    #[test]
    fn the_cost_estimate_rises_with_every_knob_it_prices() {
        let base = ConfigForm::default();
        let baseline = base.search_cost_multiple();
        for (name, raised) in [
            ("runs", ConfigForm { runs: base.runs + 1, ..ConfigForm::default() }),
            ("rotations", ConfigForm { rotations: base.rotations + 1, ..ConfigForm::default() }),
            ("population", ConfigForm { population_size: base.population_size + 1, ..ConfigForm::default() }),
            ("generations", ConfigForm { generations: base.generations + 1, ..ConfigForm::default() }),
        ] {
            assert!(raised.search_cost_multiple() > baseline, "raising {name} must raise the estimate");
        }
    }
}
