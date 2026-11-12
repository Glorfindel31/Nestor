//! The NEST EDITOR window's view: a tray of loose parts above every sheet of
//! the finished nest, drawn in their own OS window (an immediate viewport, so
//! it can sit on a second monitor). Where the platform cannot open a second
//! window, the same body is drawn in an in-app `egui::Window`.
//!
//! No logic lives here. Every edit is a call into `editor` (which asks the
//! engine about each drop), and this file only draws, reads pointer and key
//! input, and turns that into at most one such call per frame.

use std::collections::HashMap;

use super::editor::{self, Held, Origin};
use super::result::SheetView;
use super::state::bounds_of;
use super::{canvas, theme, App};
use crate::dto::{PointDto, PolygonDto, SheetPlacementDto};


/// One user action, collected while drawing and applied after it.
enum Action {
    Pick(Origin, usize),
    Swap(usize, usize),
    Drop(usize, PointDto),
    ToTray,
}

pub(super) fn show(app: &mut App, ctx: &egui::Context) {
    let title = app.t("editor_title").to_string();
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("nest_editor"),
        egui::ViewportBuilder::default().with_title(title.clone()).with_inner_size([1200.0, 800.0]),
        |ctx, class| {
            if class == egui::ViewportClass::Embedded {
                let mut open = true;
                egui::Window::new(title.clone()).open(&mut open).default_size([1000.0, 700.0]).show(ctx, |ui| body(app, ui));
                if !open {
                    app.editor.open = false;
                }
            } else {
                // A real window has its own input: undo/redo and the rest of
                // the app's shortcuts have to be read here too, or Ctrl+Z only
                // works with the main window focused.
                super::keys::handle(app, ctx);
                egui::CentralPanel::default().show(ctx, |ui| body(app, ui));
                if ctx.input(|i| i.viewport().close_requested()) {
                    app.editor.open = false;
                }
            }
        },
    );
}

fn body(app: &mut App, ui: &mut egui::Ui) {
    if app.snapshot.is_none() {
        ui.label(app.t("editor_empty"));
        return;
    }

    // Keys only when no text field has focus, so typing elsewhere is safe.
    if ui.memory(|m| m.focused().is_none()) {
        let (esc, turn, shift, del) = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::R), i.modifiers.shift, i.key_pressed(egui::Key::Delete)));
        if esc {
            app.editor_cancel();
        }
        if turn {
            app.editor_turn(if shift { -1 } else { 1 });
        }
        if del {
            app.editor_to_tray();
        }
    }

    // Strings first: `app.t` borrows `app`, and the drawing below borrows its fields.
    let hint = app.t("editor_hint").to_string();
    let checking = app.t("editor_checking").to_string();
    let holding_word = app.t("editor_holding").to_string();
    let tray_word = app.t("editor_tray").to_string();
    let sheet_word = app.t("editor_sheet").to_string();

    let held = app.editor.held;
    let busy = app.editor.pending.is_some();
    let status = if busy {
        checking
    } else if let Some(h) = held {
        format!("{} #{} @ {:.0}°", holding_word, h.id, h.rotation)
    } else {
        String::new()
    };
    let tray_ids: Vec<usize> = app.snapshot.as_ref().map(|s| s.unplaced_ids.clone()).unwrap_or_default();
    let placements: Vec<SheetPlacementDto> = app.snapshot.as_ref().map(|s| s.placements.clone()).unwrap_or_default();
    let sheets: Vec<Option<PolygonDto>> = placements.iter().map(|p| app.result_sheets.get(p.sheet_index).or_else(|| app.result_sheets.first()).cloned()).collect();

    ui.label(egui::RichText::new(hint).small().color(theme::DIM()));
    ui.label(status);
    ui.separator();

    let mut action: Option<Action> = None;

    // TRAY. Thumbnails first, then the empty frame: a click on a thumbnail is
    // a pick, and only a click on the frame's bare area parks the held part.
    ui.label(egui::RichText::new(tray_word).strong());
    let parts_by_id = &app.parts_by_id;
    let frame = egui::Frame::default().fill(theme::WELL()).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
        ui.set_min_height(80.0);
        ui.horizontal_wrapped(|ui| {
            for &id in &tray_ids {
                let Some(poly) = parts_by_id.get(&id) else { continue };
                let colour = (held.map(|h| h.id) == Some(id)).then(theme::ACCENT);
                let resp = canvas::thumbnail(ui, poly, 64.0, colour);
                let clicked = ui.interact(resp.rect, egui::Id::new(("tray", id)), egui::Sense::click()).clicked();
                if clicked {
                    action = Some(Action::Pick(Origin::Tray, id));
                }
            }
        });
    });
    let thumb_clicked = action.is_some();
    if !thumb_clicked && !busy {
        if let Some(Held { from: Origin::Sheet(_), .. }) = held {
            let press = ui.input(|i| if i.pointer.primary_clicked() { i.pointer.interact_pos() } else { None });
            if press.is_some_and(|p| frame.response.rect.contains(p)) {
                action = Some(Action::ToTray);
            }
        }
    }

    // SHEETS, one per row, each as big as the window allows: the whole point
    // of a separate window is room, and a fixed small box made zoom the only
    // way to see a part - Ctrl+scroll still zooms past this.
    ui.separator();
    let budget = egui::vec2(ui.available_width() - 24.0, (ui.available_height() - 40.0).max(300.0));
    let views = &mut app.editor.views;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (i, (pl, sheet)) in placements.iter().zip(sheets.iter()).enumerate() {
            let Some(sheet) = sheet else { continue };
            ui.label(format!("{} {}", sheet_word, i + 1));
            sheet_canvas(ui, views, parts_by_id, i, pl, sheet, held, busy, budget, &mut action);
            ui.add_space(12.0);
        }
    });

    match action {
        Some(Action::Pick(from, id)) => app.editor_pick(from, id),
        Some(Action::Swap(i, id)) => app.editor_swap(i, id),
        Some(Action::Drop(i, centre)) => app.editor_drop(i, centre),
        Some(Action::ToTray) => app.editor_to_tray(),
        None => {}
    }
}

/// One sheet: its parts, a ghost of the held part under the pointer, and the
/// click targets. At most one action is recorded per frame.
#[allow(clippy::too_many_arguments)]
fn sheet_canvas(
    ui: &mut egui::Ui,
    views: &mut HashMap<usize, SheetView>,
    parts_by_id: &HashMap<usize, PolygonDto>,
    i: usize,
    pl: &SheetPlacementDto,
    sheet: &PolygonDto,
    held: Option<Held>,
    busy: bool,
    budget: egui::Vec2,
    action: &mut Option<Action>,
) {
    let sheet_bounds = bounds_of(&sheet.points);
    let (w, h) = (sheet_bounds.w() as f32, sheet_bounds.h() as f32);
    let scale = if w > 0.0 && h > 0.0 { (budget.x / w).min(budget.y / h) } else { 1.0 };
    let size = egui::vec2(w * scale, h * scale).max(egui::vec2(40.0, 40.0));

    // Allocated before the parts, so a part added later sits on top of the
    // canvas and takes its clicks (same ordering as RESULT's sheet).
    let (rect, background) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let vs = super::result::pan_zoom(views, ui, i, rect, &background);
    let view = canvas::View::fit(sheet_bounds, rect).zoomed(rect.center(), vs.zoom, vs.pan);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::WELL());
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, theme::LINE()), egui::StrokeKind::Inside);

    // Off-screen sheets cost nothing beyond their allocated space.
    if !ui.is_rect_visible(rect) {
        return;
    }

    let map = |p: PointDto| view.model_to_screen(p);
    canvas::draw_shape(&painter, sheet, &map, true, Some(theme::LINE()));

    let shift = ui.input(|i| i.modifiers.shift);
    let pointer = ui.input(|i| i.pointer.interact_pos());

    for part in &pl.parts {
        // The held part has left this sheet; do not draw it here.
        if held.is_some_and(|h| h.id == part.id && h.from == Origin::Sheet(i)) {
            continue;
        }
        let Some(poly) = parts_by_id.get(&part.id) else { continue };
        let (rot, x, y) = (part.rotation, part.x, part.y);
        let b = bounds_of(&canvas::rotated_translated(&poly.points, rot, x, y));
        let part_rect = egui::Rect::from_two_pos(view.model_to_screen(PointDto { x: b.minx, y: b.miny }), view.model_to_screen(PointDto { x: b.maxx, y: b.maxy }));

        let pmap = |p: PointDto| view.model_to_screen(canvas::rotated_translated(std::slice::from_ref(&p), rot, x, y)[0]);
        canvas::draw_shape(&painter, poly, &pmap, true, None);

        let response = ui.interact(part_rect, egui::Id::new(("editor_part", i, part.id)), egui::Sense::click());
        if response.clicked() && !busy && action.is_none() {
            *action = match (held, pointer) {
                (None, _) => Some(Action::Pick(Origin::Sheet(i), part.id)),
                (Some(_), _) if shift => Some(Action::Swap(i, part.id)),
                (Some(_), Some(pos)) => Some(Action::Drop(i, view.screen_to_model(pos))),
                (Some(_), None) => None,
            };
        }
    }

    // The held part, under the pointer, centred the way it will be put down.
    if let Some(h) = held {
        if let (Some(poly), Some(pos)) = (parts_by_id.get(&h.id), pointer.filter(|p| rect.contains(*p))) {
            let m = view.screen_to_model(pos);
            let c = editor::centre_at_rotation(poly, h.rotation);
            let (dx, dy) = (m.x - c.x, m.y - c.y);
            let gmap = |p: PointDto| view.model_to_screen(canvas::rotated_translated(std::slice::from_ref(&p), h.rotation, dx, dy)[0]);
            canvas::draw_shape(&painter, poly, &gmap, true, Some(theme::ACCENT()));
        }
    }

    // Bare sheet: a click puts the held part down here. A click on a part is
    // caught by the part's own target above, so this only sees empty space.
    if background.clicked() && !busy && action.is_none() {
        if let (Some(_), Some(pos)) = (held, pointer) {
            *action = Some(Action::Drop(i, view.screen_to_model(pos)));
        }
    }
}
