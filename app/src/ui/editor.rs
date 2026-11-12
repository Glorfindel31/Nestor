//! The hand-editing window's logic: pick a part up (off a sheet or out of the
//! tray), turn it through the angles it is allowed, and put it down - on any
//! sheet, back in the tray, or in another part's place (a swap).
//!
//! RESULT's inline canvas can only move a part within its own sheet. Here a
//! part can leave its sheet, which is what makes it a separate window: the
//! sheets sit side by side with the tray, and nothing scrolls away mid-move.
//!
//! **Every drop on a sheet is asked of the engine** (`worker.validate` ->
//! `commands::validate_placement`), exactly like a drag, and an invalid one is
//! refused - the part stays where it was. Going *to* the tray needs no check:
//! the tray is not material. The view is `editor_view`; this file holds the
//! state and the edits, so they can be tested without a window.

use std::collections::HashMap;

use super::state::bounds_of;
use super::{canvas, App};
use crate::dto::{PlacedPartDto, PointDto, PolygonDto, SheetPlacementDto, ValidatePlacementRequest};

/// Where a held part came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Origin {
    /// An index into `Snapshot::placements` (not a sheet number).
    Sheet(usize),
    Tray,
}

/// The part the user picked up and has not put down yet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Held {
    pub id: usize,
    pub from: Origin,
    /// Absolute angle it will be put down at.
    pub rotation: f64,
}

/// A position a part is moving to: placement index, x, y, rotation.
type Spot = (usize, f64, f64, f64);

/// An edit out for validation. Only one at a time: the window ignores input
/// that would start a second one until this resolves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pending {
    Drop { held: Held, to: Spot },
    /// Two parts trading places, checked one after the other: `a` first,
    /// against the target sheet without `b`; then `b`, against its target
    /// with `a` already moved. Applied only if both pass.
    Swap { a: (Origin, usize), a_to: Spot, b: (usize, usize), b_to: Spot, second: bool },
}

#[derive(Default)]
pub struct EditorState {
    pub open: bool,
    pub held: Option<Held>,
    pub pending: Option<Pending>,
    /// Per-sheet zoom/pan, keyed by placement index - same as RESULT's.
    pub views: HashMap<usize, super::result::SheetView>,
}

/// The bounding-box centre of `poly` turned by `rotation` about the origin.
/// A part is put down by its centre, so it lands under the pointer whatever
/// its shape and angle.
pub fn centre_at_rotation(poly: &PolygonDto, rotation: f64) -> PointDto {
    let b = bounds_of(&canvas::rotated_translated(&poly.points, rotation, 0.0, 0.0));
    PointDto { x: (b.minx + b.maxx) / 2.0, y: (b.miny + b.maxy) / 2.0 }
}

/// The angles a part may take, in order: its rule's own list if it has one,
/// otherwise the nest's `360 / rotations` grid.
fn allowed_angles(rules: &HashMap<usize, crate::dto::PartRuleDto>, rotations: u32, id: usize) -> Vec<f64> {
    let base = nesting::fill::template_of(id & !nesting::dispatch::MIRROR_ID_BIT);
    if let Some(rule) = rules.get(&id).or_else(|| rules.get(&base)).filter(|r| !r.angles.is_empty()) {
        let mut angles = rule.angles.clone();
        angles.sort_by(f64::total_cmp);
        return angles;
    }
    let n = rotations.max(1);
    (0..n).map(|i| f64::from(i) * 360.0 / f64::from(n)).collect()
}

/// The next allowed angle after `current` in direction `dir` (+1 or -1). An
/// angle off the allowed list (a part placed before a rule changed) steps to
/// the nearest allowed one in that direction.
pub fn next_angle(rules: &HashMap<usize, crate::dto::PartRuleDto>, rotations: u32, id: usize, current: f64, dir: i32) -> f64 {
    let angles = allowed_angles(rules, rotations, id);
    let cur = current.rem_euclid(360.0);
    const EPS: f64 = 1e-6;
    let pick = if dir >= 0 {
        angles.iter().copied().find(|&a| a > cur + EPS).unwrap_or(angles[0])
    } else {
        angles.iter().rev().copied().find(|&a| a < cur - EPS).unwrap_or(angles[angles.len() - 1])
    };
    // Keep the turn short and in the asked direction, so `current + delta`
    // is what the caller applies.
    let mut delta = pick - cur;
    if dir >= 0 && delta <= 0.0 {
        delta += 360.0;
    }
    if dir < 0 && delta >= 0.0 {
        delta -= 360.0;
    }
    current + delta
}

impl App {
    pub(super) fn editor_turn_angle(&self, id: usize, current: f64, dir: i32) -> f64 {
        next_angle(&self.part_rules, self.result_config.as_ref().map_or(4, |c| c.rotations), id, current, dir)
    }

    /// Picks a part up. Ignored while an edit is out for validation.
    pub(super) fn editor_pick(&mut self, from: Origin, id: usize) {
        if self.editor.pending.is_some() {
            return;
        }
        let rotation = match from {
            Origin::Sheet(i) => self.snapshot.as_ref().and_then(|s| s.placements.get(i)).and_then(|p| p.parts.iter().find(|q| q.id == id)).map_or(0.0, |p| p.rotation),
            Origin::Tray => 0.0,
        };
        self.editor.held = Some(Held { id, from, rotation });
    }

    pub(super) fn editor_turn(&mut self, dir: i32) {
        if let Some(held) = self.editor.held {
            let rotation = self.editor_turn_angle(held.id, held.rotation, dir);
            self.editor.held = Some(Held { rotation, ..held });
        }
    }

    pub(super) fn editor_cancel(&mut self) {
        if self.editor.pending.is_none() {
            self.editor.held = None;
        }
    }

    /// Puts the held part down on sheet `target` (a placement index) with its
    /// centre at `centre` (model coordinates). Sent for validation; applied
    /// when the engine agrees.
    pub(super) fn editor_drop(&mut self, target: usize, centre: PointDto) {
        let (Some(held), None) = (self.editor.held, self.editor.pending) else { return };
        let Some(poly) = self.parts_by_id.get(&held.id) else { return };
        let c = centre_at_rotation(poly, held.rotation);
        let to = (target, centre.x - c.x, centre.y - c.y, held.rotation);
        if self.send_validation(held.id, to, &[]) {
            self.editor.pending = Some(Pending::Drop { held, to });
        }
    }

    /// The held part goes to the tray. No check needed: the tray is not
    /// material, and a part there is exported as unplaced.
    pub(super) fn editor_to_tray(&mut self) {
        let (Some(held), None) = (self.editor.held, self.editor.pending) else { return };
        self.editor.held = None;
        let Origin::Sheet(from) = held.from else { return };
        self.push_undo();
        if let Some(snap) = &mut self.snapshot {
            if let Some(pl) = snap.placements.get_mut(from) {
                pl.parts.retain(|p| p.id != held.id);
            }
            snap.locked.remove(&held.id);
            snap.unplaced_ids.push(held.id);
        }
        self.after_edit("a part to the tray");
    }

    /// The held part trades places with `other` on sheet `other_sheet`: each
    /// goes where the other's centre was, keeping its own angle.
    pub(super) fn editor_swap(&mut self, other_sheet: usize, other: usize) {
        let (Some(held), None) = (self.editor.held, self.editor.pending) else { return };
        if held.id == other {
            return;
        }
        let Some(snap) = &self.snapshot else { return };
        let Some(b) = snap.placements.get(other_sheet).and_then(|p| p.parts.iter().find(|q| q.id == other)).copied() else { return };
        let (Some(a_poly), Some(b_poly)) = (self.parts_by_id.get(&held.id), self.parts_by_id.get(&other)) else { return };
        let b_centre = add(centre_at_rotation(b_poly, b.rotation), b.x, b.y);
        let a_c = centre_at_rotation(a_poly, held.rotation);
        let a_to = (other_sheet, b_centre.x - a_c.x, b_centre.y - a_c.y, held.rotation);
        // From the tray, the other part goes to the tray.
        let b_to = match held.from {
            Origin::Sheet(from) => {
                let Some(a) = snap.placements.get(from).and_then(|p| p.parts.iter().find(|q| q.id == held.id)).copied() else { return };
                let a_centre = add(centre_at_rotation(a_poly, a.rotation), a.x, a.y);
                let b_c = centre_at_rotation(b_poly, b.rotation);
                (from, a_centre.x - b_c.x, a_centre.y - b_c.y, b.rotation)
            }
            Origin::Tray => (usize::MAX, 0.0, 0.0, 0.0),
        };
        if self.send_validation(held.id, a_to, &[other]) {
            self.editor.pending = Some(Pending::Swap { a: (held.from, held.id), a_to, b: (other_sheet, other), b_to, second: false });
        }
    }

    /// The engine's answer to the edit in `editor.pending`.
    pub(super) fn editor_validated(&mut self, result: Result<crate::dto::ValidatePlacementResponse, String>) {
        let Some(pending) = self.editor.pending.take() else { return };
        let valid = match result {
            Ok(r) => r.valid,
            Err(e) => {
                self.console.error(format!("placement check failed: {e}"));
                false
            }
        };
        if !valid {
            // Refused: nothing moves, and the part stays in hand to try again.
            self.run_status.err(self.t("drag_rejected"));
            return;
        }
        match pending {
            Pending::Drop { held, to } => {
                self.push_undo();
                self.move_part(held.id, held.from, Some(to));
                self.editor.held = None;
                self.after_edit("a hand edit");
            }
            Pending::Swap { a, a_to, b, b_to, second: false } => {
                if b_to.0 == usize::MAX {
                    self.push_undo();
                    self.move_part(b.1, Origin::Sheet(b.0), None);
                    self.move_part(a.1, a.0, Some(a_to));
                    self.editor.held = None;
                    self.after_edit("a swap");
                    return;
                }
                // `a` fits where `b` was. Now `b`, against its new sheet with
                // `a` gone from it - and with `a` in its new place, if both
                // are on the same sheet.
                let moved_a = PlacedPartDto { id: a.1, x: a_to.1, y: a_to.2, rotation: a_to.3, locked: true };
                let extra = (a_to.0 == b_to.0).then_some(moved_a);
                if self.send_validation_with(b.1, b_to, &[a.1], extra) {
                    self.editor.pending = Some(Pending::Swap { a, a_to, b, b_to, second: true });
                }
            }
            Pending::Swap { a, a_to, b, b_to, second: true } => {
                self.push_undo();
                self.move_part(b.1, Origin::Sheet(b.0), None);
                self.move_part(a.1, a.0, Some(a_to));
                self.move_part_from_nowhere(b.1, b_to);
                self.editor.held = None;
                self.after_edit("a swap");
            }
        }
    }

    fn send_validation(&mut self, id: usize, to: Spot, also_absent: &[usize]) -> bool {
        self.send_validation_with(id, to, also_absent, None)
    }

    /// Asks the engine whether part `id` may sit at `to`, against the target
    /// sheet's parts minus `also_absent`, plus `extra` if given.
    fn send_validation_with(&mut self, id: usize, to: Spot, also_absent: &[usize], extra: Option<PlacedPartDto>) -> bool {
        let Some(config) = self.result_config.clone() else {
            self.run_status.err(self.t("repack_needs_config"));
            return false;
        };
        let Some(snap) = &self.snapshot else { return false };
        let Some(target) = snap.placements.get(to.0) else { return false };
        let Some(sheet) = self.result_sheets.get(target.sheet_index).or_else(|| self.result_sheets.first()).cloned() else { return false };
        let mut parts: Vec<PlacedPartDto> = target.parts.iter().filter(|p| p.id != id && !also_absent.contains(&p.id)).copied().collect();
        parts.extend(extra);
        let parts_by_id: HashMap<usize, PolygonDto> = parts.iter().map(|p| p.id).chain(std::iter::once(id)).filter_map(|i| self.parts_by_id.get(&i).map(|g| (i, g.clone()))).collect();
        self.worker.validate(ValidatePlacementRequest {
            sheet,
            placement: SheetPlacementDto { sheet_index: target.sheet_index, parts },
            parts_by_id,
            moved_id: id,
            x: to.1,
            y: to.2,
            rotation: to.3,
            config,
        });
        true
    }

    /// Takes `id` out of `from` and, unless `to` is `None`, puts it at `to`,
    /// pinned like a dragged part.
    fn move_part(&mut self, id: usize, from: Origin, to: Option<Spot>) {
        let Some(snap) = &mut self.snapshot else { return };
        match from {
            Origin::Sheet(i) => {
                if let Some(pl) = snap.placements.get_mut(i) {
                    pl.parts.retain(|p| p.id != id);
                }
            }
            Origin::Tray => snap.unplaced_ids.retain(|&u| u != id),
        }
        if to.is_none() {
            snap.locked.remove(&id);
            snap.unplaced_ids.push(id);
        }
        if let Some(spot) = to {
            self.move_part_from_nowhere(id, spot);
        }
    }

    fn move_part_from_nowhere(&mut self, id: usize, (sheet, x, y, rotation): Spot) {
        let Some(snap) = &mut self.snapshot else { return };
        // A part sent to the tray by the first half of a swap comes back out.
        snap.unplaced_ids.retain(|&u| u != id);
        if let Some(pl) = snap.placements.get_mut(sheet) {
            pl.parts.retain(|p| p.id != id);
            pl.parts.push(PlacedPartDto { id, x, y, rotation, locked: true });
        }
        snap.locked.insert(id);
    }

    /// The bookkeeping every applied edit needs: the totals, then the audit.
    ///
    /// Utilisation is rescaled rather than recomputed: the engine measures it
    /// against the *usable* sheet (margin taken off), which the window does not
    /// have, so starting over from gross areas would make the figure jump on
    /// an edit that changed nothing. Scaling the engine's own number by how
    /// much the placed material and the used sheets changed keeps its basis.
    fn after_edit(&mut self, reason: &'static str) {
        let parts_by_id = &self.parts_by_id;
        let sheets = &self.result_sheets;
        let totals = |placements: &[SheetPlacementDto]| -> (f64, f64) {
            let used: f64 = placements.iter().flat_map(|p| &p.parts).filter_map(|p| parts_by_id.get(&p.id)).map(PolygonDto::material_area).sum();
            let area: f64 = placements.iter().filter(|p| !p.parts.is_empty()).filter_map(|p| sheets.get(p.sheet_index)).map(PolygonDto::material_area).sum();
            (used, area)
        };
        let before = self.undo_stack.last().map(|s| (totals(&s.placements), s.utilisation));
        if let Some(snap) = &mut self.snapshot {
            let (used, area) = totals(&snap.placements);
            if let Some(((used0, area0), util0)) = before {
                if used0 > 0.0 && area > 0.0 {
                    snap.utilisation = util0 * (used / used0) * (area0 / area);
                }
            }
            snap.unplaced_count = snap.unplaced_ids.len();
        }
        self.request_audit(reason);
    }
}

fn add(p: PointDto, x: f64, y: f64) -> PointDto {
    PointDto { x: p.x + x, y: p.y + y }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::PartRuleDto;

    #[test]
    fn turning_walks_the_part_rule_and_wraps() {
        let rules = HashMap::from([(5, PartRuleDto { angles: vec![0.0, 180.0], mirror: false, no_hole_nesting: false })]);
        assert_eq!(next_angle(&rules, 4, 5, 0.0, 1), 180.0);
        assert_eq!(next_angle(&rules, 4, 5, 180.0, 1), 360.0, "wraps forward, never back through 90");
        assert_eq!(next_angle(&rules, 4, 5, 0.0, -1), -180.0);
        assert_eq!(next_angle(&rules, 4, 5, 90.0, 1), 180.0, "an off-list angle steps to the next allowed one");
    }

    #[test]
    fn without_a_rule_turning_follows_the_nest_grid() {
        let rules = HashMap::new();
        assert_eq!(next_angle(&rules, 4, 1, 0.0, 1), 90.0);
        assert_eq!(next_angle(&rules, 4, 1, 270.0, 1), 360.0);
        assert_eq!(next_angle(&rules, 2, 1, 0.0, -1), -180.0);
    }

    /// A filler copy has no rule of its own under its own id in older
    /// responses - it must still follow its template's.
    #[test]
    fn a_filler_copy_follows_its_templates_rule() {
        let rules = HashMap::from([(3, PartRuleDto { angles: vec![0.0, 90.0], mirror: false, no_hole_nesting: false })]);
        let copy = nesting::fill::copy_id(3, 7);
        assert_eq!(next_angle(&rules, 4, copy, 0.0, 1), 90.0);
        assert_eq!(next_angle(&rules, 4, copy, 90.0, 1), 360.0);
    }

    #[test]
    fn the_centre_follows_the_turn() {
        let rect = PolygonDto::new(vec![PointDto { x: 0.0, y: 0.0 }, PointDto { x: 40.0, y: 0.0 }, PointDto { x: 40.0, y: 10.0 }, PointDto { x: 0.0, y: 10.0 }], "0".into(), None);
        let c0 = centre_at_rotation(&rect, 0.0);
        assert!((c0.x - 20.0).abs() < 1e-9 && (c0.y - 5.0).abs() < 1e-9);
        let c90 = centre_at_rotation(&rect, 90.0);
        assert!((c90.x + 5.0).abs() < 1e-9 && (c90.y - 20.0).abs() < 1e-9, "{c90:?}");
    }
}
