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
        ui.add_space(6.0);
        history_selector(app, ui);
        stats(app, ui);
        unplaced(app, ui);
        ui.add_space(8.0);
        ui.label(RichText::new(app.t("drag_hint")).color(theme::DIM).small());
        sheets(app, ui);
        ui.add_space(8.0);
        export_controls(app, ui);
    });
}

fn history_selector(app: &mut App, ui: &mut egui::Ui) {
    if app.history.len() <= 1 {
        return;
    }
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
        if chosen != app.history_index {
            app.history_index = chosen;
            // A fresh Snapshot, so pins from the previously viewed attempt
            // don't leak onto a different set of placements.
            app.snapshot = Some(Snapshot::from_history(&app.history[chosen]));
        }
    })
    .response
    .on_hover_text(app.t("view_attempt_tooltip"));
}

fn stats(app: &App, ui: &mut egui::Ui) {
    let Some(snap) = &app.snapshot else { return };
    ui.horizontal(|ui| {
        let mut stat = |key: &str, value: String| {
            ui.vertical(|ui| {
                ui.label(RichText::new(app.t(key)).color(theme::DIM).small());
                ui.label(RichText::new(value).strong());
            });
            ui.add_space(20.0);
        };
        stat("stat_fitness", format!("{:.1}", snap.fitness));
        stat("stat_utilisation", format!("{:.1}%", snap.utilisation * 100.0));
        stat("stat_unplaced", snap.unplaced_count.to_string());
        stat("stat_sheets_used", snap.placements.len().to_string());
    });
}

fn unplaced(app: &App, ui: &mut egui::Ui) {
    let Some(snap) = &app.snapshot else { return };
    if snap.unplaced_ids.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(RichText::new(app.t("unplaced_hint")).color(theme::ERROR));
    egui::ScrollArea::horizontal().id_salt("unplaced").max_height(110.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            for id in &snap.unplaced_ids {
                let Some(poly) = app.parts_by_id.get(id) else { continue };
                let too_large = !fits_any_sheet(poly, &app.result_sheets);
                let (label, detail) = if too_large { ("unplaced_label_too_large", "unplaced_detail_too_large") } else { ("unplaced_label_no_room", "unplaced_detail_no_room") };
                ui.allocate_ui(egui::vec2(96.0, 74.0), |ui| {
                    ui.vertical(|ui| {
                        canvas::thumbnail(ui, poly, 40.0, Some(canvas::UNPLACED));
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
        Color32::from_rgb(0x4f, 0xd1, 0x5c)
    } else if util >= 45.0 {
        app.prefs.accent_color()
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
    // Fit-to-box, matching the web UI's own 700x500 budget. No pan, no zoom -
    // deliberately the same as before; the mapping is `canvas::View`, so
    // adding them later is a change in one place.
    let (w, h) = (sheet_bounds.w() as f32, sheet_bounds.h() as f32);
    let avail = ui.available_width().min(700.0);
    let scale = if w > 0.0 && h > 0.0 { (avail / w).min(500.0 / h) } else { 1.0 };
    let size = egui::vec2(w * scale, h * scale).max(egui::vec2(40.0, 40.0));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let view = canvas::View::fit(sheet_bounds, rect);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(0x0d, 0x0d, 0x0d));
    theme::bevel(&painter, rect, false);
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, band), egui::StrokeKind::Inside);

    let editable = app.result_config.is_some() && !app.controls_locked();
    let mut click_to_toggle = None;
    let mut drag_started = None;
    let mut drag_delta = egui::Vec2::ZERO;
    let mut drag_released = false;

    for part in &placement.parts {
        let Some(poly) = app.parts_by_id.get(&part.id) else { continue };
        // The offset a drag in progress is applying to this part, so it
        // follows the pointer live rather than jumping on release.
        let (odx, ody) = match &app.drag {
            Some(d) if d.sheet == index && d.part_id == part.id => (d.dx, d.dy),
            _ => (0.0, 0.0),
        };
        let pts = canvas::rotated_translated(&poly.points, part.rotation, part.x + odx, part.y + ody);
        let b = bounds_of(&pts);
        let part_rect = egui::Rect::from_two_pos(view.model_to_screen(PointDto { x: b.minx, y: b.miny }), view.model_to_screen(PointDto { x: b.maxx, y: b.maxy }));

        let pinned = snap.locked.contains(&part.id);
        let mut color = None;
        if let Some(d) = &app.drag {
            if d.sheet == index && d.part_id == part.id {
                color = Some(if d.clear { Color32::from_rgb(0x4f, 0xd1, 0x5c) } else { theme::ERROR });
            }
        }

        let map = |p: PointDto| {
            let r = canvas::rotated_translated(std::slice::from_ref(&p), part.rotation, part.x + odx, part.y + ody);
            view.model_to_screen(r[0])
        };
        canvas::draw_shape(&painter, poly, &map, true, color);
        if pinned {
            // Pinned parts get a visible frame - the state has to be
            // readable without hovering, or "why won't this move" is a
            // guessing game.
            painter.rect_stroke(part_rect, 0.0, egui::Stroke::new(1.0_f32, app.prefs.accent_color()), egui::StrokeKind::Outside);
        }

        if editable {
            let response = ui.interact(part_rect, egui::Id::new(("part", index, part.id)), egui::Sense::click_and_drag());
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
        app.drag = Some(Drag { sheet: index, part_id, dx: 0.0, dy: 0.0, moved: false, clear: true, awaiting: false });
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
    let (part_id, dx, dy) = (drag.part_id, drag.dx, drag.dy);
    let Some(snap) = &app.snapshot else { return };
    let Some(placement) = snap.placements.get(sheet_index) else { return };
    let Some(sheet) = app.result_sheets.get(placement.sheet_index).or_else(|| app.result_sheets.first()) else { return };
    let sheet_b = bounds_of(&sheet.points);

    let Some(moved) = placement.parts.iter().find(|p| p.id == part_id) else { return };
    let Some(moved_bounds) = part_bounds(app, moved, dx, dy) else { return };

    const EPS: f64 = 1e-9;
    let inside = moved_bounds.minx >= sheet_b.minx - EPS && moved_bounds.maxx <= sheet_b.maxx + EPS && moved_bounds.miny >= sheet_b.miny - EPS && moved_bounds.maxy <= sheet_b.maxy + EPS;
    let overlaps = placement.parts.iter().filter(|p| p.id != part_id).filter_map(|p| part_bounds(app, p, 0.0, 0.0)).any(|b| {
        moved_bounds.minx < b.maxx - EPS && moved_bounds.maxx > b.minx + EPS && moved_bounds.miny < b.maxy - EPS && moved_bounds.maxy > b.miny + EPS
    });

    if let Some(d) = &mut app.drag {
        d.clear = inside && !overlaps;
    }
}

fn part_bounds(app: &App, part: &PlacedPartDto, dx: f64, dy: f64) -> Option<Bounds> {
    let poly = app.parts_by_id.get(&part.id)?;
    Some(bounds_of(&canvas::rotated_translated(&poly.points, part.rotation, part.x + dx, part.y + dy)))
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
    let (part_id, dx, dy) = (drag.part_id, drag.dx, drag.dy);
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
    app.worker.validate(drag_request(sheet, placement, sheet_parts(app, sheet_index), part, dx, dy, config));
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
        // A drag never rotates; the part keeps whatever angle the nest gave it.
        rotation: part.rotation,
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
                if let Some(snap) = &mut self.snapshot {
                    if let Some(p) = snap.placements.get_mut(drag.sheet).and_then(|pl| pl.parts.iter_mut().find(|p| p.id == id)) {
                        p.x += drag.dx;
                        p.y += drag.dy;
                        p.locked = true;
                    }
                    snap.locked.insert(id);
                }
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
        if ui.add_enabled(!app.controls_locked(), egui::Button::new(shell::accent(app, app.t("btn_export")).strong())).clicked() {
            do_export(app);
        }
    });
    shell::status_label(ui, &app.export_status);
}

fn do_export(app: &mut App) {
    if !(app.export_spacing >= 0.0) {
        app.export_status.err(app.t("export_invalid_spacing"));
        return;
    }
    let Some(snap) = &app.snapshot else { return };
    let format = app.export_format;
    let Some(path) = rfd::FileDialog::new().set_file_name(format!("nest.{}", format.ext())).add_filter(format.label(), &[format.ext()]).save_file() else {
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

/// The part list the PDF report prints - source shapes and their quantities,
/// as the user defined them, not the expanded per-copy ids.
fn report_part_list(app: &App) -> Vec<ReportPartDto> {
    app.shapes
        .iter()
        .filter(|s| s.role == super::state::Role::Part && s.qty > 0)
        .enumerate()
        .map(|(i, s)| ReportPartDto { name: format!("{}-{}", s.file, i + 1), quantity: s.qty })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let request = drag_request(square(100.0), placement.clone(), parts.clone(), moved, dx, dy, config.clone());
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
