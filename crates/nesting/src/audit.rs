//! Whole-result manufacturability check: is this nest actually cuttable?
//!
//! Everything else in this crate *produces* placements. This one only ever
//! reads them back and asks whether they are legal, because nothing else did.
//! `placement::place_parts` validates each part as it places it, but a result
//! can be edited after the fact - `repack`, and the UI's drag - and neither
//! re-checks the sheet as a whole afterwards. So the state a user finally
//! exports is not, in general, a state anything has ever validated end to
//! end. That gap is the reason this module exists: it is the difference
//! between "the engine believes this is fine" and "this was checked".
//!
//! **The distinction that makes the report worth reading.** Parts arrive here
//! already padded outward by `spacing / 2` each (`geometry::clearance::
//! prepare_part`), so two padded outlines that just touch have exactly
//! `spacing` between their true ones. That gives an exact, two-level answer
//! rather than one blurry one:
//!
//! - two *true* outlines sharing material is an `Overlap` - the cut is wrong,
//!   parts are destroyed, hard fail;
//! - two *padded* outlines sharing material while the true ones don't is
//!   `BelowSpacing` - the parts are fine but sit closer than asked, which
//!   matters for heat and for tabs, and is a warning.
//!
//! Reporting those two identically is how an audit gets ignored: a check that
//! cries wolf about a 0.1mm clearance shortfall in the same voice it uses for
//! destroyed parts trains the operator to skip it. Same reasoning for the
//! sheet pair, `OutsideSheet` (off the material entirely) versus
//! `OutsideMargin` (on the material, inside the edge keep-out).
//!
//! **The AABB prefilter is not an optimisation.** Naively this is O(n^2)
//! Clipper calls; a 200-part sheet is ~20k of them at single-digit
//! milliseconds each, which is minutes, which means nobody runs it. The cull
//! is `placement::bounds_within_distance`, which is documented there as exact
//! rather than approximate - if it says two boxes are apart, no Clipper call
//! could have found them touching. So the prefilter costs nothing in
//! accuracy and takes a real nest back to near-linear.

use geometry::dxf_import::{shift_layered_polygon, LayeredPolygon};
use geometry::polygon::{get_polygon_bounds, Bounds};

use crate::placement::{bounds_within_distance, has_material_outside_sheet, has_material_overlap, PlacedObstacle};

/// What went wrong. Ordered worst-first, and `is_fatal` is the only thing
/// that decides pass/fail - a warning is information, not a veto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IssueKind {
    /// Two parts share real material. The cut is wrong.
    Overlap,
    /// A part extends past the sheet's true outer boundary, or into one of
    /// the sheet's own holes.
    OutsideSheet,
    /// Two parts are closer than the configured `spacing`, without actually
    /// overlapping.
    BelowSpacing,
    /// A part is on the material but inside the sheet-edge keep-out.
    OutsideMargin,
}

impl IssueKind {
    /// Whether this issue means "do not cut this". Only the two geometry
    /// failures qualify; the clearance ones are advisory.
    #[must_use]
    pub fn is_fatal(self) -> bool {
        matches!(self, IssueKind::Overlap | IssueKind::OutsideSheet)
    }
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub kind: IssueKind,
    /// The sheet this was found on, indexing the placements that were passed
    /// in.
    pub sheet_index: usize,
    /// One id for a sheet-boundary issue, two for a part-versus-part one.
    pub part_ids: Vec<usize>,
}

/// One part as the audit needs it: both outlines, already positioned.
///
/// Carrying *both* the true and the padded outline is the whole trick - it is
/// what lets one pass separate a real overlap from a clearance shortfall
/// without guessing at an epsilon. The caller already has both, because
/// producing the padded one is what it did before nesting.
#[derive(Clone, Debug)]
pub struct AuditPart {
    pub id: usize,
    /// The part as it will actually be cut, at its placed position.
    pub outline: LayeredPolygon,
    /// The same part grown by `spacing / 2`, at the same position.
    pub padded: LayeredPolygon,
}

impl AuditPart {
    /// Builds one from the geometry and placement the nest recorded.
    ///
    /// `outline`/`padded` are the unshifted shapes; the placement is applied
    /// here so every caller does it the same way.
    #[must_use]
    pub fn placed(id: usize, outline: &LayeredPolygon, padded: &LayeredPolygon, x: f64, y: f64) -> Self {
        Self { id, outline: shift_layered_polygon(outline, x, y), padded: shift_layered_polygon(padded, x, y) }
    }

    fn from_obstacle(o: &PlacedObstacle, padded: &LayeredPolygon) -> Self {
        Self::placed(o.id, &o.polygon, padded, o.placement.x, o.placement.y)
    }
}

/// One sheet plus everything placed on it.
pub struct AuditSheet {
    /// The sheet as cut - its true outer boundary.
    pub outline: LayeredPolygon,
    /// The sheet inset by `margin` (i.e. what a part must stay inside of).
    /// Pass a clone of `outline` when there is no margin.
    pub usable: LayeredPolygon,
    pub parts: Vec<AuditPart>,
}

#[derive(Clone, Debug, Default)]
pub struct AuditReport {
    pub issues: Vec<Issue>,
}

impl AuditReport {
    /// True only if nothing fatal was found. Warnings do not fail the audit -
    /// see the module doc for why that separation is load-bearing.
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.issues.iter().any(|i| i.kind.is_fatal())
    }

    #[must_use]
    pub fn fatal_count(&self) -> usize {
        self.issues.iter().filter(|i| i.kind.is_fatal()).count()
    }

    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.issues.len() - self.fatal_count()
    }
}

/// Bounding box of a positioned part's true outline, for the prefilter.
///
/// The *padded* box would be the conservative choice, but `audit_pair` culls
/// on the true boxes plus a distance, and the padding is at most `spacing / 2`
/// per side - so passing `spacing` as that distance covers the padded extent
/// exactly. Keeping one box per part rather than two also halves the
/// bookkeeping.
fn true_bounds(part: &AuditPart) -> Option<Bounds> {
    get_polygon_bounds(&part.outline.points)
}

/// Audits every sheet. Sheets are independent, so this is just a fold - the
/// interesting logic is all in `audit_sheet`.
#[must_use]
pub fn audit(sheets: &[AuditSheet]) -> AuditReport {
    let mut report = AuditReport::default();
    for (index, sheet) in sheets.iter().enumerate() {
        audit_sheet(sheet, index, &mut report);
    }
    // Worst first, so the UI's "N ISSUES" list opens on what matters. Stable
    // within a kind, so the order is reproducible between runs.
    report.issues.sort_by_key(|i| match i.kind {
        IssueKind::Overlap => 0,
        IssueKind::OutsideSheet => 1,
        IssueKind::BelowSpacing => 2,
        IssueKind::OutsideMargin => 3,
    });
    report
}

fn audit_sheet(sheet: &AuditSheet, sheet_index: usize, report: &mut AuditReport) {
    let mut issue = |kind: IssueKind, part_ids: Vec<usize>| report.issues.push(Issue { kind, sheet_index, part_ids });

    for part in &sheet.parts {
        // Checked against the true sheet first: a part hanging off the
        // material is a different (and worse) statement than one merely
        // inside the edge keep-out, and reporting both for the same part
        // would just be noise.
        if has_material_outside_sheet(&part.outline, &sheet.outline) {
            issue(IssueKind::OutsideSheet, vec![part.id]);
        } else if has_material_outside_sheet(&part.outline, &sheet.usable) {
            issue(IssueKind::OutsideMargin, vec![part.id]);
        }
    }

    // Precomputed once: `get_polygon_bounds` walks every point, and doing it
    // inside the pair loop would make the prefilter itself quadratic in point
    // count - which is the cost the prefilter exists to avoid.
    let bounds: Vec<Option<Bounds>> = sheet.parts.iter().map(true_bounds).collect();

    for i in 0..sheet.parts.len() {
        for j in (i + 1)..sheet.parts.len() {
            if let Some(kind) = audit_pair(&sheet.parts[i], &sheet.parts[j], bounds[i].as_ref(), bounds[j].as_ref()) {
                issue(kind, vec![sheet.parts[i].id, sheet.parts[j].id]);
            }
        }
    }
}

/// The per-pair check, prefiltered.
///
/// Split out so the cull and the two-level test can be tested directly, and
/// so the nested loop above stays readable.
fn audit_pair(a: &AuditPart, b: &AuditPart, a_bounds: Option<&Bounds>, b_bounds: Option<&Bounds>) -> Option<IssueKind> {
    if let (Some(ab), Some(bb)) = (a_bounds, b_bounds) {
        // The padded outlines extend at most `spacing / 2` beyond the true
        // ones, so a gap wider than the padding of both cannot hide a
        // clearance issue either. Derived from the shapes themselves rather
        // than from a passed-in `spacing`, so the two can't disagree.
        let slack = padding_extent(a) + padding_extent(b);
        if !bounds_within_distance(ab, bb, slack) {
            return None;
        }
    }
    if has_material_overlap(&a.outline, &b.outline) {
        return Some(IssueKind::Overlap);
    }
    has_material_overlap(&a.padded, &b.padded).then_some(IssueKind::BelowSpacing)
}

/// How far this part's padded outline reaches beyond its true one, from the
/// two bounding boxes.
///
/// Measured rather than assumed: it keeps the cull correct even for a part
/// whose padding isn't uniform, and means the audit needs no `spacing`
/// parameter that could be passed inconsistently with the geometry it is
/// checking. Falls back to infinity - i.e. never cull - if either box is
/// degenerate, because a wrong cull silently hides a real overlap.
fn padding_extent(part: &AuditPart) -> f64 {
    match (get_polygon_bounds(&part.outline.points), get_polygon_bounds(&part.padded.points)) {
        (Some(t), Some(p)) => ((t.x - p.x).max(t.y - p.y)).max(((p.x + p.width) - (t.x + t.width)).max((p.y + p.height) - (t.y + t.height))).max(0.0),
        _ => f64::INFINITY,
    }
}

/// Convenience for callers that already hold `PlacedObstacle`s (the engine's
/// own representation) plus a padded outline per part.
#[must_use]
pub fn parts_from_obstacles(obstacles: &[PlacedObstacle], padded_of: impl Fn(usize) -> Option<LayeredPolygon>) -> Vec<AuditPart> {
    obstacles.iter().filter_map(|o| padded_of(o.id).map(|padded| AuditPart::from_obstacle(o, &padded))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geometry::point::Point;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> LayeredPolygon {
        LayeredPolygon {
            points: vec![Point::new(x, y), Point::new(x + w, y), Point::new(x + w, y + h), Point::new(x, y + h)],
            layer: "cut".into(),
            children: Vec::new(),
            texts: Vec::new(),
            is_circle: None,
            real_boundary: None,
        }
    }

    /// A part plus its padding, both anchored at the origin and placed at
    /// `(x, y)` - mirroring what the real caller does.
    fn part(id: usize, x: f64, y: f64, w: f64, h: f64, pad: f64) -> AuditPart {
        AuditPart::placed(id, &rect(0.0, 0.0, w, h), &rect(-pad, -pad, w + 2.0 * pad, h + 2.0 * pad), x, y)
    }

    fn sheet(parts: Vec<AuditPart>, margin: f64) -> AuditSheet {
        AuditSheet { outline: rect(0.0, 0.0, 100.0, 100.0), usable: rect(margin, margin, 100.0 - 2.0 * margin, 100.0 - 2.0 * margin), parts }
    }

    #[test]
    fn a_clean_sheet_reports_nothing() {
        let report = audit(&[sheet(vec![part(1, 10.0, 10.0, 20.0, 20.0, 1.0), part(2, 50.0, 50.0, 20.0, 20.0, 1.0)], 0.0)]);
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert!(report.passed());
    }

    /// The feature, in one test: a part moved on top of another must be
    /// caught. This is exactly what a manual drag can produce.
    #[test]
    fn two_overlapping_parts_are_a_fatal_overlap() {
        let report = audit(&[sheet(vec![part(1, 10.0, 10.0, 20.0, 20.0, 1.0), part(2, 20.0, 20.0, 20.0, 20.0, 1.0)], 0.0)]);
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].kind, IssueKind::Overlap);
        assert_eq!(report.issues[0].part_ids, vec![1, 2]);
        assert!(!report.passed());
        assert_eq!(report.fatal_count(), 1);
    }

    /// The distinction the whole module is built around: parts that do not
    /// touch, but sit closer than the spacing they were nested with, must
    /// report as a warning and must NOT fail the audit.
    #[test]
    fn parts_closer_than_spacing_warn_without_failing() {
        // 2mm padding each side means a 4mm minimum gap; these sit 1mm apart.
        let report = audit(&[sheet(vec![part(1, 10.0, 10.0, 20.0, 20.0, 2.0), part(2, 31.0, 10.0, 20.0, 20.0, 2.0)], 0.0)]);
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].kind, IssueKind::BelowSpacing);
        assert!(report.passed(), "a clearance shortfall must not fail the audit");
        assert_eq!(report.warning_count(), 1);
    }

    #[test]
    fn a_part_hanging_off_the_sheet_is_fatal() {
        let report = audit(&[sheet(vec![part(1, 90.0, 10.0, 20.0, 20.0, 1.0)], 0.0)]);
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].kind, IssueKind::OutsideSheet);
        assert!(!report.passed());
    }

    /// On the material but inside the edge keep-out: a warning, and
    /// specifically not also reported as OutsideSheet.
    #[test]
    fn a_part_inside_the_margin_warns_once() {
        let report = audit(&[sheet(vec![part(1, 2.0, 20.0, 20.0, 20.0, 0.0)], 5.0)]);
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].kind, IssueKind::OutsideMargin);
        assert!(report.passed());
    }

    /// The prefilter must never change an answer, only how long it takes. Two
    /// parts far apart are culled; the same two overlapping are not.
    #[test]
    fn the_prefilter_culls_distant_pairs_without_changing_close_ones() {
        let (near_a, near_b) = (part(1, 0.0, 0.0, 10.0, 10.0, 1.0), part(2, 5.0, 5.0, 10.0, 10.0, 1.0));
        let far = part(3, 900.0, 900.0, 10.0, 10.0, 1.0);
        let b = |p: &AuditPart| true_bounds(p);
        assert_eq!(audit_pair(&near_a, &near_b, b(&near_a).as_ref(), b(&near_b).as_ref()), Some(IssueKind::Overlap));
        assert_eq!(audit_pair(&near_a, &far, b(&near_a).as_ref(), b(&far).as_ref()), None);
        // ...and culling is never the reason a close pair is missed: the same
        // pair with no boxes at all must give the same answer.
        assert_eq!(audit_pair(&near_a, &near_b, None, None), Some(IssueKind::Overlap));
    }

    #[test]
    fn issues_are_reported_worst_first() {
        let parts = vec![
            // 1+2 sit close (warning), 3 hangs off the edge (fatal).
            part(1, 10.0, 10.0, 20.0, 20.0, 2.0),
            part(2, 31.0, 10.0, 20.0, 20.0, 2.0),
            part(3, 95.0, 60.0, 20.0, 20.0, 2.0),
        ];
        let report = audit(&[sheet(parts, 0.0)]);
        assert!(report.issues.len() >= 2, "{:?}", report.issues);
        assert!(report.issues[0].kind.is_fatal(), "fatal issues must sort first: {:?}", report.issues);
        assert!(!report.passed());
    }

    #[test]
    fn every_sheet_is_audited_and_its_index_recorded() {
        let clean = sheet(vec![part(1, 10.0, 10.0, 10.0, 10.0, 0.0)], 0.0);
        let broken = sheet(vec![part(2, 10.0, 10.0, 20.0, 20.0, 0.0), part(3, 15.0, 15.0, 20.0, 20.0, 0.0)], 0.0);
        let report = audit(&[clean, broken]);
        assert_eq!(report.issues.len(), 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].sheet_index, 1);
    }
}
