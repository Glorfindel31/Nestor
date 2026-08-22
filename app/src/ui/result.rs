//! 04 RESULT: stats, the unplaced list, the per-sheet canvases (with drag and
//! pin editing), and export.

use std::collections::HashMap;

use egui::{Color32, RichText};

use super::state::{bounds_of, polygon_area, Bounds};
use super::{canvas, console, shell, theme, App, Snapshot};
use crate::dto::{ExportRequest, PlacedPartDto, PointDto, PolygonDto, ReportPartDto, ReportRequest, RepackSheetRequest, SheetPlacementDto, ValidatePlacementRequest};
use crate::worker::ExportFormat;

/// A part being dragged on the result canvas.
pub struct Drag {
    pub sheet: usize,
    pub part_id: usize,
    /// Model-space offset applied so far, relative to where the part started.
    pub dx: f64,
    pub dy: f64,
    /// Turn applied so far, in degrees, relative to the angle the nest gave
    /// the part. Folded into `dx`/`dy` as it is applied so the part turns
    /// about its own centre rather than swinging around the model origin -
    /// see `turn`.
    pub drot: f64,
    /// Whether the pointer has moved far enough to count as a drag rather
    /// than a click. A click pins/unpins; a drag relocates.
    pub moved: bool,
    /// What the live hint currently says. Approximate - see `clear_of_others`.
    pub clear: bool,
    /// True once the drop has been sent for validation, so the part isn't
    /// snapped back before the answer arrives.
    pub awaiting: bool,
}

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    if app.snapshot.is_none() {
        return;
    }
    shell::panel_frame(ui, |ui| {
        shell::heading(app, ui, "04", "heading_result_text");
        shell::heading_rule(ui);
        history_selector(app, ui);
        stats(app, ui);
        unplaced(app, ui);
        ui.add_space(8.0);
        ui.label(RichText::new(app.t("drag_hint")).color(theme::DIM).small());
        sheets(app, ui);
        super::library::offcut_controls(app, ui);
        ui.add_space(8.0);
        export_controls(app, ui);
    });
}

fn history_selector(app: &mut App, ui: &mut egui::Ui) {
    if app.history.len() <= 1 {
        return;
    }
    // Clicking a point on the chart selects that attempt, exactly as the
    // combo below does - so both routes go through `show_attempt`.
    if let Some(clicked) = super::history_chart::chart(app, ui) {
        show_attempt(app, clicked);
    }
    ui.add_space(4.0);
    let lang = app.prefs.lang;
    let last = app.history.len() - 1;
    let label_for = |i: usize, h: &crate::dto::NestSnapshotDto| {
        super::i18n::tv(
            lang,
            "history_option",
            &[
                ("i", &(i + 1).to_string()),
                ("gen", &h.generation.to_string()),
                ("best", if i == last { super::i18n::t(lang, "history_best_suffix") } else { "" }),
                ("fitness", &format!("{:.1}", h.fitness)),
                ("unplaced", &h.unplaced_count.to_string()),
            ],
        )
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(app.t("view_attempt_label")).color(theme::DIM));
        let mut chosen = app.history_index;
        egui::ComboBox::from_id_salt("history").width(400.0).selected_text(label_for(chosen, &app.history[chosen])).show_ui(ui, |ui| {
            for (i, h) in app.history.iter().enumerate() {
                ui.selectable_value(&mut chosen, i, label_for(i, h));
            }
        });
        show_attempt(app, chosen);
    })
    .response
    .on_hover_text(app.t("view_attempt_tooltip"));
}

/// Switches the displayed attempt. Shared by the chart and the combo box so
/// the two can't drift - in particular so both rebuild the `Snapshot` rather
/// than only moving the index.
fn show_attempt(app: &mut App, index: usize) {
    if index == app.history_index || index >= app.history.len() {
        return;
    }
    app.history_index = index;
    // A fresh Snapshot, so pins from the previously viewed attempt don't leak
    // onto a different set of placements.
    app.snapshot = Some(Snapshot::from_history(&app.history[index]));
    // A different attempt is a different arrangement - the badge must not
    // carry the previous one's verdict across.
    app.request_audit();
}

fn stats(app: &App, ui: &mut egui::Ui) {
    let Some(snap) = &app.snapshot else { return };
    ui.horizontal(|ui| {
        let mut stat = |key: &str, value: String| {
            ui.vertical(|ui| {
                ui.label(RichText::new(app.t(key)).color(theme::DIM).small());
                ui.label(RichText::new(value).strong().family(theme::heavy()));
            });
            ui.add_space(20.0);
        };
        stat("stat_fitness", format!("{:.1}", snap.fitness));
        stat("stat_utilisation", format!("{:.1}%", snap.utilisation));
        stat("stat_unplaced", snap.unplaced_count.to_string());
        stat("stat_sheets_used", snap.placements.len().to_string());
        audit_badge(app, ui);
    });
}

/// The manufacturability verdict, as one word in the stats row.
///
/// Four states, deliberately - "not checked" is not folded into "passed".
/// The whole value of the badge is that it distinguishes an arrangement
/// something verified from one nobody did, and defaulting the unknown case
/// to green would destroy exactly that.
fn audit_badge(app: &App, ui: &mut egui::Ui) {
    let lang = app.prefs.lang;
    let (key, color, detail) = match (&app.audit, app.auditing) {
        (_, true) => ("audit_checking", theme::DIM, "audit_checking_tooltip"),
        (None, false) => ("audit_unknown", theme::DIM, "audit_unknown_tooltip"),
        (Some(r), false) if !r.passed => ("audit_failed", theme::ERROR, "audit_failed_tooltip"),
        (Some(r), false) if r.warning_count > 0 => ("audit_warned", theme::ACCENT, "audit_warned_tooltip"),
        (Some(_), false) => ("audit_passed", theme::OK, "audit_passed_tooltip"),
    };
    ui.vertical(|ui| {
        ui.label(RichText::new(super::i18n::t(lang, "stat_audit")).color(theme::DIM).small());
        let text = match &app.audit {
            // The counts are the actionable part - "3 ISSUES" sends someone
            // looking, "FAILED" only makes them wonder.
            Some(r) if !app.auditing && !r.passed => super::i18n::tv(lang, "audit_failed_count", &[("n", &r.fatal_count.to_string())]),
            Some(r) if !app.auditing && r.warning_count > 0 => super::i18n::tv(lang, "audit_warned_count", &[("n", &r.warning_count.to_string())]),
            _ => super::i18n::t(lang, key).to_string(),
        };
        ui.label(RichText::new(text).color(color).strong().family(theme::heavy())).on_hover_text(audit_detail(app, super::i18n::t(lang, detail)));
    });
    ui.add_space(20.0);
}

/// Tooltip: the generic explanation, plus the specific offenders when there
/// are any. Capped, because a badly broken nest can produce hundreds and a
/// tooltip taller than the window helps nobody.
fn audit_detail(app: &App, base: &str) -> String {
    const MAX_LISTED: usize = 8;
    let Some(report) = &app.audit else { return base.to_string() };
    if report.issues.is_empty() {
        return base.to_string();
    }
    let lang = app.prefs.lang;
    let mut out = base.to_string();
    for issue in report.issues.iter().take(MAX_LISTED) {
        let ids = issue.part_ids.iter().map(|id| format!("#{id}")).collect::<Vec<_>>().join(" + ");
        out.push_str(&format!("
{} - sheet {}, {}", super::i18n::t(lang, audit_kind_key(&issue.kind)), issue.sheet_index + 1, ids));
    }
    if report.issues.len() > MAX_LISTED {
        out.push_str(&super::i18n::tv(lang, "audit_more", &[("n", &(report.issues.len() - MAX_LISTED).to_string())]));
    }
    out
}

/// Maps the wire-format issue kind onto its translated label. An unknown
/// string renders as itself rather than being dropped - a report from a newer
/// engine should still be readable, not silently shortened.
fn audit_kind_key(kind: &str) -> &str {
    match kind {
        "overlap" => "audit_kind_overlap",
        "outside_sheet" => "audit_kind_outside_sheet",
        "below_spacing" => "audit_kind_below_spacing",
        "outside_margin" => "audit_kind_outside_margin",
        other => other,
    }
}

fn unplaced(app: &App, ui: &mut egui::Ui) {
    let Some(snap) = &app.snapshot else { return };
    if snap.unplaced_ids.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(RichText::new(app.t("unplaced_hint")).color(theme::ERROR));
    egui::ScrollArea::horizontal().id_salt("unplaced").max_height(190.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            for id in &snap.unplaced_ids {
                let Some(poly) = app.parts_by_id.get(id) else { continue };
                let too_large = !fits_any_sheet(poly, &app.result_sheets);
                let (label, detail) = if too_large { ("unplaced_label_too_large", "unplaced_detail_too_large") } else { ("unplaced_label_no_room", "unplaced_detail_no_room") };
                ui.allocate_ui(egui::vec2(136.0, 124.0), |ui| {
                    ui.vertical(|ui| {
                        canvas::thumbnail(ui, poly, 80.0, Some(canvas::UNPLACED));
                        ui.label(RichText::new(format!("#{id}")).color(theme::DIM).small());
                        ui.label(RichText::new(app.t(label)).color(theme::ERROR).small());
                    });
                })
                .response
                .on_hover_text(app.t(detail));
            }
        });
    });
}

/// Best-effort reason for a part being unplaced: does its own bounding box
/// fit *any* available sheet's bounding box at all, in either orientation?
///
/// ponytail: bounding boxes only, because the engine returns no structured
/// reason - it reports what it could not place, not why. That means the
/// answer is "too large" only when it is unambiguously too large; everything
/// else reads as "no room", which may or may not be improvable by more
/// generations. Good enough to tell a hopeless job from a tight one.
fn fits_any_sheet(part: &PolygonDto, sheets: &[PolygonDto]) -> bool {
    let p = bounds_of(&part.points);
    sheets.iter().any(|s| {
        let b = bounds_of(&s.points);
        (p.w() <= b.w() && p.h() <= b.h()) || (p.h() <= b.w() && p.w() <= b.h())
    })
}

fn sheets(app: &mut App, ui: &mut egui::Ui) {
    let Some(snap) = &app.snapshot else { return };
    let count = snap.placements.len();
    for index in 0..count {
        sheet_card(app, ui, index);
    }
}

fn sheet_card(app: &mut App, ui: &mut egui::Ui, index: usize) {
    let Some(snap) = &app.snapshot else { return };
    let Some(placement) = snap.placements.get(index) else { return };
    let Some(sheet) = app.result_sheets.get(placement.sheet_index).or_else(|| app.result_sheets.first()) else { return };

    let sheet_area = polygon_area(&sheet.points);
    let used: f64 = placement.parts.iter().filter_map(|p| app.parts_by_id.get(&p.id)).map(|poly| polygon_area(&poly.points)).sum();
    let util = if sheet_area > 0.0 { used / sheet_area * 100.0 } else { 0.0 };
    // ponytail: raw polygon area, not margin/spacing-net "usable" area, and
    // the bands are untuned - they exist to make a bad sheet obvious at a
    // glance, not to be a number anyone quotes.
    let band = if util >= 75.0 {
        theme::OK
    } else if util >= 45.0 {
        theme::ACCENT
    } else {
        theme::ERROR
    };

    let caption = super::i18n::tv(
        app.prefs.lang,
        "sheet_caption",
        &[("n", &(index + 1).to_string()), ("parts", &placement.parts.len().to_string()), ("util", &format!("{util:.1}"))],
    );
    let can_edit = app.result_config.is_some() && !app.controls_locked();

    ui.horizontal(|ui| {
        ui.label(RichText::new(caption).color(band));
        if ui.add_enabled(can_edit && app.repacking.is_none(), egui::Button::new(app.t("repack_button"))).on_hover_text(app.t("repack_tooltip")).clicked() {
            start_repack(app, index);
        }
        if !can_edit && app.result_config.is_none() {
            ui.label(RichText::new(app.t("repack_needs_config")).color(theme::DIM).small());
        }
        // Only once this sheet is actually zoomed: a FIT button on a fitted
        // sheet is a button that does nothing, on every card, forever. It
        // doubles as the only place the zoom level is written down.
        if let Some(vs) = app.sheet_views.get(&index).copied() {
            if ui.button(super::i18n::tv(app.prefs.lang, "sheet_fit_button", &[("zoom", &format!("{:.0}", vs.zoom * 100.0))])).on_hover_text(app.t("sheet_fit_tooltip")).clicked() {
                app.sheet_views.remove(&index);
            }
        }
    });

    draw_sheet(app, ui, index, band);
    ui.add_space(10.0);
}

/// The canvas for one sheet, plus its click-to-pin / drag-to-move editing.
///
/// Hit-testing is per-part bounding box, which is not an approximation
/// introduced here: the web version set `pointer-events="bounding-box"` on
/// each part group for exactly the same reason (a `fill:none` outline has
/// nothing to hit), so this is the same behaviour, not a simplification of it.
fn draw_sheet(app: &mut App, ui: &mut egui::Ui, index: usize, band: Color32) {
    let Some(snap) = &app.snapshot else { return };
    let Some(placement) = snap.placements.get(index) else { return };
    let Some(sheet) = app.result_sheets.get(placement.sheet_index).or_else(|| app.result_sheets.first()) else { return };

    let sheet_bounds = bounds_of(&sheet.points);
    // Fit-to-box, matching the web UI's own 700x500 budget, then zoomed and
    // panned on top. A 3000x1500 sheet fitted into 700x500 draws its parts a
    // few pixels across, which is neither readable nor grabbable.
    let (w, h) = (sheet_bounds.w() as f32, sheet_bounds.h() as f32);
    let avail = ui.available_width().min(700.0);
    let scale = if w > 0.0 && h > 0.0 { (avail / w).min(500.0 / h) } else { 1.0 };
    let size = egui::vec2(w * scale, h * scale).max(egui::vec2(40.0, 40.0));
    // `click_and_drag`, and allocated *before* the parts below: egui gives an
    // overlapping interaction to whichever widget was added last, so a drag
    // that starts on a part still moves the part, and only a drag starting on
    // bare canvas pans the view.
    let (rect, background) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let vs = pan_zoom(&mut app.sheet_views, ui, index, rect, &background);
    let view = canvas::View::fit(sheet_bounds, rect).zoomed(rect.center(), vs.zoom, vs.pan);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::WELL);
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, band), egui::StrokeKind::Inside);

    let editable = app.result_config.is_some() && !app.controls_locked();
    let mut click_to_toggle = None;
    let mut hovered_part = None;
    let mut drag_started = None;
    let mut drag_delta = egui::Vec2::ZERO;
    let mut drag_released = false;

    for part in &placement.parts {
        let Some(poly) = app.parts_by_id.get(&part.id) else { continue };
        // The offset a drag in progress is applying to this part, so it
        // follows the pointer live rather than jumping on release.
        let (odx, ody, odrot) = match &app.drag {
            Some(d) if d.sheet == index && d.part_id == part.id => (d.dx, d.dy, d.drot),
            _ => (0.0, 0.0, 0.0),
        };
        let pts = canvas::rotated_translated(&poly.points, part.rotation + odrot, part.x + odx, part.y + ody);
        let b = bounds_of(&pts);
        let part_rect = egui::Rect::from_two_pos(view.model_to_screen(PointDto { x: b.minx, y: b.miny }), view.model_to_screen(PointDto { x: b.maxx, y: b.maxy }));

        let pinned = snap.locked.contains(&part.id);
        let mut color = None;
        if let Some(d) = &app.drag {
            if d.sheet == index && d.part_id == part.id {
                color = Some(if d.clear { theme::OK } else { theme::ERROR });
            }
        }

        let map = |p: PointDto| {
            let r = canvas::rotated_translated(std::slice::from_ref(&p), part.rotation + odrot, part.x + odx, part.y + ody);
            view.model_to_screen(r[0])
        };
        canvas::draw_shape(&painter, poly, &map, true, color);
        if pinned {
            // Pinned parts get a visible frame - the state has to be
            // readable without hovering, or "why won't this move" is a
            // guessing game.
            painter.rect_stroke(part_rect, 0.0, egui::Stroke::new(1.0_f32, theme::ACCENT), egui::StrokeKind::Outside);
        }

        if editable {
            let response = ui.interact(part_rect, egui::Id::new(("part", index, part.id)), egui::Sense::click_and_drag());
            if response.hovered() {
                hovered_part = Some(part.id);
            }
            if response.drag_started() {
                drag_started = Some(part.id);
            }
            if response.dragged() {
                drag_delta = response.drag_delta();
            }
            if response.drag_stopped() {
                drag_released = true;
            }
            if response.clicked() {
                click_to_toggle = Some(part.id);
            }
        }
    }

    if let Some(id) = click_to_toggle {
        toggle_pin(app, id);
    }
    if let Some(part_id) = drag_started {
        app.drag = Some(Drag { sheet: index, part_id, dx: 0.0, dy: 0.0, drot: 0.0, moved: false, clear: true, awaiting: false });
    }
    if drag_delta != egui::Vec2::ZERO {
        let (mdx, mdy) = view.model_delta(drag_delta);
        if let Some(d) = &mut app.drag {
            d.dx += mdx;
            d.dy += mdy;
            // Anything past this is a drag, not a click - matching the web
            // UI's own 0.01-model-unit threshold.
            if d.dx.abs() > 0.01 || d.dy.abs() > 0.01 {
                d.moved = true;
            }
        }
        update_drag_hint(app, index);
    }
    if drag_released {
        commit_drag(app, index);
    }
    if editable {
        keyboard_edit(app, ui, index, hovered_part, background.contains_pointer());
    }
}

/// Arrow keys nudge, R turns - on the part being dragged, or failing that the
/// one under the pointer.
///
/// **No selection state, deliberately.** "Which part do the keys apply to" is
/// answered by where the pointer already is, the same way the drag and the pin
/// toggle answer it, so there is no third notion of a current part to keep in
/// sync with the other two, and nothing extra to draw to explain it.
///
/// Both edits go down the drag path rather than editing the placement
/// directly: that is the one route that asks the engine whether the result is
/// legal, pins what lands and pushes an undo entry. A keyboard nudge that
/// skipped it would be the one way to put a part somewhere the audit later
/// rejects.
fn keyboard_edit(app: &mut App, ui: &egui::Ui, index: usize, hovered_part: Option<usize>, canvas_hovered: bool) {
    // A focused text field owns the arrow keys; taking them from under a
    // half-typed number in the config panel would be its own bug.
    if ui.memory(|m| m.focused().is_some()) {
        return;
    }
    let dragging = app.drag.as_ref().filter(|d| d.sheet == index).map(|d| d.part_id);
    if dragging.is_none() && !canvas_hovered {
        return;
    }
    let Some(part_id) = dragging.or(hovered_part) else { return };

    // A nudge is worth exactly one clearance: the smallest move that can
    // change whether two parts are legal neighbours. With no spacing set
    // there is no such distance, so fall back to a millimetre.
    let step = match app.result_config.as_ref().map(crate::dto::NestConfigDto::effective_spacing) {
        Some(s) if s > 0.0 => s,
        _ => 1.0,
    };
    // The nest's own rotation grid - a hand-turned part landing off it would
    // sit at an angle the engine would never have chosen for itself.
    let turn_step = match app.result_config.as_ref().map(|c| c.rotations) {
        Some(r) if r >= 2 => 360.0 / f64::from(r),
        _ => 90.0,
    };

    let (mut dx, mut dy, mut drot) = (0.0, 0.0, 0.0);
    ui.input(|i| {
        for (key, x, y) in [
            (egui::Key::ArrowLeft, -1.0, 0.0),
            (egui::Key::ArrowRight, 1.0, 0.0),
            // Model Y is up, screen Y is down: "up" on the keyboard has to
            // mean +y here, or the part goes the way the key does not point.
            (egui::Key::ArrowUp, 0.0, 1.0),
            (egui::Key::ArrowDown, 0.0, -1.0),
        ] {
            if i.key_pressed(key) {
                dx += x * step;
                dy += y * step;
            }
        }
        if i.key_pressed(egui::Key::R) {
            drot += if i.modifiers.shift { -turn_step } else { turn_step };
        }
    });
    if dx == 0.0 && dy == 0.0 && drot == 0.0 {
        return;
    }

    let existing = app.drag.take().filter(|d| d.sheet == index && d.part_id == part_id);
    let mut drag = existing.unwrap_or(Drag { sheet: index, part_id, dx: 0.0, dy: 0.0, drot: 0.0, moved: false, clear: true, awaiting: false });
    if drag.awaiting {
        // A drop is already out for validation; a second edit on top of it
        // would be applied to a position the engine has not agreed to yet.
        app.drag = Some(drag);
        return;
    }
    if drot != 0.0 {
        let (cdx, cdy) = turn(app, index, &drag, drot);
        drag.dx += cdx;
        drag.dy += cdy;
        drag.drot += drot;
    }
    drag.dx += dx;
    drag.dy += dy;
    drag.moved = true;
    let live_drag = ui.input(|i| i.pointer.any_down());
    app.drag = Some(drag);
    update_drag_hint(app, index);
    // Mid-drag the pointer is still down and the drop will validate on
    // release; a keyboard-only edit has no release, so it commits now.
    if !live_drag {
        commit_drag(app, index);
    }
}

/// The translation that keeps a part's bounding-box centre still while it
/// turns by `extra` degrees.
///
/// A placement is "rotate about the model origin, then translate", so turning
/// a part in place means undoing the arc its centre would otherwise travel.
/// Without this, turning a part sitting two metres from the origin throws it
/// clean off the sheet, which reads as the feature being broken rather than
/// as the convention it is.
fn turn(app: &App, index: usize, drag: &Drag, extra: f64) -> (f64, f64) {
    let Some(snap) = &app.snapshot else { return (0.0, 0.0) };
    let Some(part) = snap.placements.get(index).and_then(|pl| pl.parts.iter().find(|p| p.id == drag.part_id)) else { return (0.0, 0.0) };
    let Some(poly) = app.parts_by_id.get(&drag.part_id) else { return (0.0, 0.0) };
    let b = bounds_of(&poly.points);
    let centre = PointDto { x: (b.minx + b.maxx) / 2.0, y: (b.miny + b.maxy) / 2.0 };
    let before = canvas::rotated_translated(std::slice::from_ref(&centre), part.rotation + drag.drot, 0.0, 0.0)[0];
    let after = canvas::rotated_translated(std::slice::from_ref(&centre), part.rotation + drag.drot + extra, 0.0, 0.0)[0];
    (before.x - after.x, before.y - after.y)
}

/// One sheet card's view transform. `Default` is the fitted view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SheetView {
    pub zoom: f32,
    pub pan: egui::Vec2,
}

impl Default for SheetView {
    fn default() -> Self {
        Self { zoom: 1.0, pan: egui::Vec2::ZERO }
    }
}

impl SheetView {
    pub fn is_fitted(&self) -> bool {
        *self == Self::default()
    }
}

/// Zoom is never below the fitted view - "smaller than the whole sheet" is
/// not a useful place to be - and 40x is a millimetre or two across on a
/// 3m sheet.
const MAX_ZOOM: f32 = 40.0;

/// Applies this frame's zoom/pan input for one sheet and returns the view
/// transform to draw with.
///
/// Zoom is on ctrl+scroll (egui's own `zoom_delta`, so a trackpad pinch works
/// too) rather than plain scroll, because the whole RESULT panel is inside a
/// scroll area and stealing the wheel from it would make a long result
/// impossible to scroll past.
fn pan_zoom(views: &mut HashMap<usize, SheetView>, ui: &egui::Ui, index: usize, rect: egui::Rect, background: &egui::Response) -> SheetView {
    let mut vs = views.get(&index).copied().unwrap_or_default();
    // `contains_pointer`, not `hovered`: a part sitting on top of the canvas
    // takes the hover, and the pointer is over a part almost all of the time
    // on a well-packed sheet - so `hovered` made zoom work only in the gaps.
    if background.contains_pointer() {
        let zoom_delta = ui.input(|i| i.zoom_delta());
        if zoom_delta != 1.0 {
            let factor = (vs.zoom * zoom_delta).clamp(1.0, MAX_ZOOM) / vs.zoom;
            // Keep whatever is under the pointer under the pointer. Derived
            // from the composed origin rather than guessed: with
            // `origin = centre + (fit_origin - centre) * zoom + pan`, holding
            // point `p` fixed across a scale by `factor` gives exactly this.
            let pointer = ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
            vs.pan = (pointer - rect.center()) * (1.0 - factor) + vs.pan * factor;
            vs.zoom *= factor;
        }
    }
    // Middle-drag pans from anywhere, read straight off the pointer rather
    // than from a `Response`: once zoomed in, the parts' own bounding boxes
    // cover the whole canvas, so there is no background left to grab and a
    // primary drag belongs to the part under it anyway. Primary-drag on bare
    // canvas still pans, for an empty or sparsely filled sheet.
    if background.contains_pointer() && ui.input(|i| i.pointer.button_down(egui::PointerButton::Middle)) {
        vs.pan += ui.input(|i| i.pointer.delta());
    }
    if background.dragged() {
        vs.pan += background.drag_delta();
    }
    if background.double_clicked() {
        vs = SheetView::default();
    }

    // Keep the sheet covering the viewport, so a pan can never lose it
    // off-screen and leave the user staring at an empty well. At zoom 1 this
    // pins the pan to zero, which is why panning a fitted sheet does nothing.
    let slack = rect.size() * (vs.zoom - 1.0) / 2.0;
    vs.pan.x = vs.pan.x.clamp(-slack.x, slack.x);
    vs.pan.y = vs.pan.y.clamp(-slack.y, slack.y);

    if vs.is_fitted() {
        views.remove(&index);
    } else {
        views.insert(index, vs);
    }
    vs
}

fn toggle_pin(app: &mut App, id: usize) {
    let lang = app.prefs.lang;
    let Some(snap) = &mut app.snapshot else { return };
    let now_pinned = if snap.locked.contains(&id) {
        snap.locked.remove(&id);
        false
    } else {
        snap.locked.insert(id);
        true
    };
    let msg = super::i18n::tv(lang, if now_pinned { "pin_locked" } else { "pin_unlocked" }, &[("id", &id.to_string())]);
    app.run_status.ok(msg);
}

/// The live green/red hint shown while dragging.
///
/// ponytail: a bounding-box test, not the real one - it has to run on every
/// pointer move, and the authoritative check is a full engine call. The real
/// check runs once on drop and it wins, so two concave pieces can read
/// "green" here and still be refused. If that ever gets annoying, debounce a
/// real `validate_placement` call rather than making this test smarter -
/// a second, cleverer approximation would just disagree with the engine
/// somewhere else.
fn update_drag_hint(app: &mut App, sheet_index: usize) {
    let Some(drag) = &app.drag else { return };
    let (part_id, dx, dy, drot) = (drag.part_id, drag.dx, drag.dy, drag.drot);
    let Some(snap) = &app.snapshot else { return };
    let Some(placement) = snap.placements.get(sheet_index) else { return };
    let Some(sheet) = app.result_sheets.get(placement.sheet_index).or_else(|| app.result_sheets.first()) else { return };
    let sheet_b = bounds_of(&sheet.points);

    let Some(moved) = placement.parts.iter().find(|p| p.id == part_id) else { return };
    let Some(moved_bounds) = part_bounds(app, moved, dx, dy, drot) else { return };

    const EPS: f64 = 1e-9;
    let inside = moved_bounds.minx >= sheet_b.minx - EPS && moved_bounds.maxx <= sheet_b.maxx + EPS && moved_bounds.miny >= sheet_b.miny - EPS && moved_bounds.maxy <= sheet_b.maxy + EPS;
    let overlaps = placement.parts.iter().filter(|p| p.id != part_id).filter_map(|p| part_bounds(app, p, 0.0, 0.0, 0.0)).any(|b| {
        moved_bounds.minx < b.maxx - EPS && moved_bounds.maxx > b.minx + EPS && moved_bounds.miny < b.maxy - EPS && moved_bounds.maxy > b.miny + EPS
    });

    if let Some(d) = &mut app.drag {
        d.clear = inside && !overlaps;
    }
}

fn part_bounds(app: &App, part: &PlacedPartDto, dx: f64, dy: f64, drot: f64) -> Option<Bounds> {
    let poly = app.parts_by_id.get(&part.id)?;
    Some(bounds_of(&canvas::rotated_translated(&poly.points, part.rotation + drot, part.x + dx, part.y + dy)))
}

/// On release, ask the engine whether the new position is actually legal.
/// The UI never decides this itself: an approximation here would disagree
/// with the engine exactly where it matters, and would have to be written
/// twice.
fn commit_drag(app: &mut App, sheet_index: usize) {
    let Some(drag) = &app.drag else { return };
    if !drag.moved {
        // A click, already handled by the pin toggle.
        app.drag = None;
        return;
    }
    let (part_id, dx, dy, drot) = (drag.part_id, drag.dx, drag.dy, drag.drot);
    let Some(config) = app.result_config.clone() else {
        app.run_status.err(app.t("repack_needs_config"));
        app.drag = None;
        return;
    };
    let Some(snap) = &app.snapshot else { return };
    let Some(placement) = snap.placements.get(sheet_index).cloned() else { return };
    let Some(sheet) = app.result_sheets.get(placement.sheet_index).or_else(|| app.result_sheets.first()).cloned() else { return };
    let Some(part) = placement.parts.iter().find(|p| p.id == part_id).copied() else { return };

    if let Some(d) = &mut app.drag {
        d.awaiting = true;
    }
    app.worker.validate(drag_request(sheet, placement, sheet_parts(app, sheet_index), part, dx, dy, drot, config));
}

/// Builds the question put to the engine on drop: "this part, at its current
/// position plus the drag offset - legal?"
///
/// Extracted from `commit_drag` purely so it can be tested. The failure this
/// guards against is quiet and expensive: sending `dx` instead of
/// `part.x + dx`, or leaving the offset in screen units, produces a UI that
/// refuses every legal move (or accepts illegal ones) while looking
/// completely normal.
fn drag_request(
    sheet: PolygonDto,
    placement: SheetPlacementDto,
    parts_by_id: HashMap<usize, PolygonDto>,
    part: PlacedPartDto,
    dx: f64,
    dy: f64,
    drot: f64,
    config: crate::dto::NestConfigDto,
) -> ValidatePlacementRequest {
    ValidatePlacementRequest {
        sheet,
        placement,
        parts_by_id,
        moved_id: part.id,
        // Absolute model coordinates, not the delta - the engine has no idea
        // where the part started.
        x: part.x + dx,
        y: part.y + dy,
        // A drag on its own never rotates, but R during one does, and so does
        // R with the pointer over a part - `drot` is that turn.
        rotation: part.rotation + drot,
        config,
    }
}

impl App {
    /// Applies (or rejects) the engine's verdict on a dropped part. A part
    /// that lands successfully is pinned automatically - moving it by hand is
    /// the whole point, and having the next repack undo it immediately would
    /// make the feature useless.
    pub(super) fn finish_drag(&mut self, result: Result<crate::dto::ValidatePlacementResponse, String>) {
        let Some(drag) = self.drag.take() else { return };
        match result {
            Ok(response) if response.valid => {
                let id = drag.part_id;
                self.push_undo();
                if let Some(snap) = &mut self.snapshot {
                    if let Some(p) = snap.placements.get_mut(drag.sheet).and_then(|pl| pl.parts.iter_mut().find(|p| p.id == id)) {
                        p.x += drag.dx;
                        p.y += drag.dy;
                        p.rotation += drag.drot;
                        p.locked = true;
                    }
                    snap.locked.insert(id);
                }
                // The drag was checked against this one part; the audit is
                // what confirms the sheet as a whole is still sound.
                self.request_audit();
                let msg = super::i18n::tv(self.prefs.lang, "drag_placed", &[("id", &id.to_string())]);
                self.run_status.ok(msg);
            }
            Ok(_) => self.run_status.err(self.t("drag_rejected")),
            Err(e) => {
                self.console.error(format!("placement check failed: {e}"));
                let msg = super::i18n::tv(self.prefs.lang, "drag_failed", &[("err", &e)]);
                self.run_status.err(msg);
            }
        }
    }
}

/// Just this sheet's parts. Repack and validation only ever reason about one
/// sheet, and sending the whole job's map would be a lot of geometry across
/// a call that doesn't look at it.
fn sheet_parts(app: &App, sheet_index: usize) -> HashMap<usize, PolygonDto> {
    let Some(snap) = &app.snapshot else { return Default::default() };
    let Some(placement) = snap.placements.get(sheet_index) else { return Default::default() };
    placement.parts.iter().filter_map(|p| app.parts_by_id.get(&p.id).map(|poly| (p.id, poly.clone()))).collect()
}

fn start_repack(app: &mut App, index: usize) {
    let Some(config) = app.result_config.clone() else {
        app.run_status.err(app.t("repack_needs_config"));
        return;
    };
    let Some(snap) = &app.snapshot else { return };
    let Some(mut placement) = snap.placements.get(index).cloned() else { return };
    let Some(sheet) = app.result_sheets.get(placement.sheet_index).or_else(|| app.result_sheets.first()).cloned() else { return };
    // Pins are carried onto the placement itself - that's how the engine is
    // told which parts to pack *around* rather than move.
    for part in &mut placement.parts {
        part.locked = snap.locked.contains(&part.id);
    }
    let parts_by_id = sheet_parts(app, index);

    app.repacking = Some(index);
    app.run_status.ok(app.t("repack_status_running"));
    app.worker.repack(RepackSheetRequest { sheet, placement, parts_by_id, config, part_rules: app.part_rules.clone() });
}

fn export_controls(app: &mut App, ui: &mut egui::Ui) {
    ui.separator();
    ui.label(RichText::new(app.t("export_hint")).color(theme::DIM).small());
    let lang = app.prefs.lang;
    ui.horizontal(|ui| {
        ui.label(RichText::new(app.t("export_format_label")).color(theme::DIM));
        shell::choice(ui, "export_format", &mut app.export_format, &ExportFormat::ALL, |f| match f {
            ExportFormat::Pdf => super::i18n::t(lang, "export_format_pdf").to_string(),
            other => other.label().to_string(),
        });
        ui.label(RichText::new(app.t("export_spacing_label")).color(theme::DIM));
        ui.add(egui::DragValue::new(&mut app.export_spacing).speed(1.0).range(0.0..=10_000.0)).on_hover_text(app.t("export_spacing_tooltip"));
        // Labels resolved before the `&mut` borrows, or the checkbox's
        // mutable field borrow and `app.t`'s shared one collide.
        let (outline_label, outline_tip) = (app.t("export_outline_label"), app.t("export_outline_tooltip"));
        let (unplaced_label, unplaced_tip) = (app.t("export_unplaced_label"), app.t("export_unplaced_tooltip"));
        ui.checkbox(&mut app.export_outline, outline_label).on_hover_text(outline_tip);
        ui.checkbox(&mut app.export_unplaced, unplaced_label).on_hover_text(unplaced_tip);
        if ui.add_enabled(!app.controls_locked(), egui::Button::new(shell::accent(app.t("btn_export")).strong().family(theme::heavy()))).clicked() {
            do_export(app, app.export_format);
        }
        // Its own button rather than "pick PDF, then press EXPORT": the report
        // is what gets printed and signed, and it is asked for far more often
        // than the format picker's other entries put together.
        let report_tip = app.t("report_tooltip");
        if ui
            .add_enabled(!app.controls_locked(), egui::Button::new(shell::accent(app.t("btn_report")).strong().family(theme::heavy())))
            .on_hover_text(report_tip)
            .clicked()
        {
            do_export(app, ExportFormat::Pdf);
        }
    });
    shell::status_label(ui, &app.export_status);
}

fn do_export(app: &mut App, format: ExportFormat) {
    if !(app.export_spacing >= 0.0) {
        app.export_status.err(app.t("export_invalid_spacing"));
        return;
    }
    let Some(snap) = &app.snapshot else { return };
    let Some(path) = rfd::FileDialog::new().set_file_name(export_file_name(format.ext())).add_filter(format.label(), &[format.ext()]).save_file() else {
        return;
    };

    let export = ExportRequest {
        sheets: app.result_sheets.clone(),
        // The authoritative map from the run itself, not a re-derived one -
        // `build_export_layouts` works out the never-placed set from
        // whatever is left in it after resolving every referenced id, so it
        // has to be the complete map, placed parts and all.
        parts_by_id: app.parts_by_id.clone(),
        placements: snap.placements.clone(),
        sheet_spacing: app.export_spacing,
        include_sheet_outline: app.export_outline,
        include_unplaced: app.export_unplaced,
    };
    let report = (format == ExportFormat::Pdf).then(|| ReportRequest {
        export: export.clone(),
        config: app.result_config.clone().unwrap_or_else(|| app.cfg.to_dto()),
        parts: report_part_list(app),
        title: None,
    });

    app.export_status.ok(app.t("export_status_running"));
    app.console.log(console::Kind::Plain, format!("exporting {}", format.label()));
    app.worker.export(format, path, export, report);
}

/// Every exported file is named `NESThh-mm_YYYY-MM-DD`, local time.
///
/// A fixed default meant every export landed on `nest.dxf` and quietly
/// overwrote the last one, which is exactly wrong for a workflow that produces
/// several attempts of the same job in a sitting. The user can still rename in
/// the dialog; this only decides what it opens with.
fn export_file_name(ext: &str) -> String {
    format!("{}.{ext}", chrono::Local::now().format("NEST%H-%M_%Y-%m-%d"))
}

/// The part list the PDF report prints - source shapes as the user defined
/// them, not the expanded per-copy ids, plus how many of each the result
/// actually placed.
///
/// **How `nested` is worked out.** `dto::expand_parts` hands out instance ids
/// in one contiguous block of `quantity` per part, walking the same list this
/// builds - `ui::mod`'s run request filters `app.shapes` by exactly this
/// predicate, in this order. So a placed id's block is its source row, and the
/// cumulative quantities are the block boundaries. Mirrored copies carry
/// `MIRROR_ID_BIT`, which is masked off first or every flipped piece would land
/// past the end of the table.
fn report_part_list(app: &App) -> Vec<ReportPartDto> {
    let rows: Vec<&super::state::ShapeRow> = app.shapes.iter().filter(|s| s.role == super::state::Role::Part && s.qty > 0).collect();

    let mut block_end = Vec::with_capacity(rows.len());
    let mut running = 0usize;
    for row in &rows {
        running += row.qty;
        block_end.push(running);
    }

    let mut nested = vec![0usize; rows.len()];
    if let Some(snap) = &app.snapshot {
        for placement in &snap.placements {
            for part in &placement.parts {
                let id = part.id & !nesting::dispatch::MIRROR_ID_BIT;
                let at = block_end.partition_point(|&end| end <= id);
                if let Some(count) = nested.get_mut(at) {
                    *count += 1;
                }
            }
        }
    }

    rows.iter()
        .enumerate()
        .map(|(i, s)| ReportPartDto { name: format!("{}-{}", s.file, i + 1), quantity: s.qty, nested: nested[i], polygon: s.poly.clone() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user asked for exactly this shape, and it is load-bearing: a fixed
    /// default silently overwrote the previous export.
    #[test]
    fn every_export_is_named_for_the_moment_it_was_made() {
        let name = export_file_name("pdf");
        let stamp: String = name.chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect();
        assert_eq!(stamp, "NEST00-00_0000-00-00.pdf", "got {name}");
    }

    fn poly(w: f64, h: f64) -> PolygonDto {
        PolygonDto {
            points: vec![PointDto { x: 0.0, y: 0.0 }, PointDto { x: w, y: 0.0 }, PointDto { x: w, y: h }, PointDto { x: 0.0, y: h }],
            layer: "cut".into(),
            is_circle: None,
            children: vec![],
            texts: vec![],
            real_boundary: None,
        }
    }

    fn square(size: f64) -> PolygonDto {
        poly(size, size)
    }

    /// End-to-end through the real engine: the coordinates `commit_drag`
    /// sends must be absolute model-space positions. A part sitting at
    /// x=50 dragged 20mm left has to be asked about at x=30 - asking about
    /// x=-20 (the raw delta) rejects every legal move, and the UI would look
    /// entirely normal while doing it.
    #[test]
    fn a_dropped_part_is_asked_about_at_its_new_absolute_position() {
        let placement = SheetPlacementDto {
            sheet_index: 0,
            parts: vec![
                PlacedPartDto { id: 0, x: 0.0, y: 0.0, rotation: 0.0, locked: false },
                PlacedPartDto { id: 1, x: 50.0, y: 50.0, rotation: 0.0, locked: false },
            ],
        };
        let parts: HashMap<usize, PolygonDto> = HashMap::from([(0, square(20.0)), (1, square(20.0))]);
        let moved = placement.parts[1];
        let config = crate::ui::state::ConfigForm::default().to_dto();

        let ask = |dx: f64, dy: f64| {
            let request = drag_request(square(100.0), placement.clone(), parts.clone(), moved, dx, dy, 0.0, config.clone());
            (request.x, request.y, crate::commands::validate_placement(request).unwrap().valid)
        };

        // Nudged 20mm left and 10mm down, still clear of part 0 and inside
        // the sheet.
        assert_eq!(ask(-20.0, -10.0).0, 30.0);
        assert_eq!(ask(-20.0, -10.0).1, 40.0);
        assert!(ask(-20.0, -10.0).2, "a legal move must be accepted");

        // Dragged onto part 0.
        assert!(!ask(-50.0, -50.0).2, "a drop on top of another part must be refused");

        // Dragged off the right-hand edge.
        assert!(!ask(45.0, 0.0).2, "a drop hanging off the sheet must be refused");

        // A turn is asked about as an absolute angle, on top of whatever the
        // nest gave the part - sending the delta would leave a part that was
        // already at 90 degrees being checked at 45.
        let turned = drag_request(square(100.0), placement.clone(), parts.clone(), PlacedPartDto { rotation: 90.0, ..moved }, 0.0, 0.0, 45.0, config.clone());
        assert_eq!(turned.rotation, 135.0);
    }

    /// The "too large" label must only appear when the part genuinely cannot
    /// fit any sheet in either orientation - claiming a placeable part is
    /// too big sends someone off to redraw a file that was fine.
    #[test]
    fn the_too_large_reason_accounts_for_rotation() {
        let sheets = vec![poly(100.0, 50.0)];
        assert!(fits_any_sheet(&poly(90.0, 40.0), &sheets), "fits outright");
        assert!(fits_any_sheet(&poly(40.0, 90.0), &sheets), "fits once turned 90 degrees");
        assert!(!fits_any_sheet(&poly(120.0, 40.0), &sheets), "genuinely too long");
        assert!(!fits_any_sheet(&poly(10.0, 10.0), &[]), "no sheets at all");
    }
}
