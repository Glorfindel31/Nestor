//! 02 ASSIGN ROLES: the shapes table.
//!
//! Every row's role / quantity / allowed angles / mirror override is a field
//! on `ShapeRow`, not a DOM node - so unlike the web version this table is
//! rebuilt from state each frame. That removes the append-only constraint,
//! the renumbering pass that existed to patch it up, and the bug where a
//! language switch left already-created rows labelled in the old language.

use egui::RichText;

use super::state::{bounds_of, MirrorRule, Role, RotRule};
use super::{canvas, shell, theme, App};

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    if app.shapes.is_empty() {
        return;
    }
    shell::panel_frame(ui, |ui| {
        ui.horizontal(|ui| {
            shell::heading(app, ui, "02", "heading_roles_text");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(if app.shapes_collapsed { ">" } else { "v" }).on_hover_text(app.t("toggle_collapse_tooltip")).clicked() {
                    app.shapes_collapsed = !app.shapes_collapsed;
                }
                let any_selected = app.shapes.iter().any(|s| s.selected);
                if ui
                    .add_enabled(any_selected && !app.controls_locked(), egui::Button::new(RichText::new(app.t("btn_remove_selected")).color(theme::ERROR)))
                    .on_hover_text(app.t("btn_remove_selected_tooltip"))
                    .clicked()
                {
                    app.confirm_remove = true;
                }
                if ui.add_enabled(!app.controls_locked(), egui::Button::new(app.t("btn_mark_all_sheets"))).on_hover_text(app.t("btn_mark_all_sheets_tooltip")).clicked() {
                    app.shapes.iter_mut().for_each(|s| s.role = Role::Sheet);
                }
                if ui.add_enabled(!app.controls_locked(), egui::Button::new(app.t("btn_mark_all_parts"))).on_hover_text(app.t("btn_mark_all_parts_tooltip")).clicked() {
                    app.shapes.iter_mut().for_each(|s| s.role = Role::Part);
                }
            });
        });
        shell::heading_rule(app, ui);
        ui.label(RichText::new(app.t("roles_hint")).color(theme::DIM).small());

        if app.shapes_collapsed {
            return;
        }
        ui.add_space(6.0);
        table(app, ui);
    });
}

fn table(app: &mut App, ui: &mut egui::Ui) {
    let locked = app.controls_locked();
    let lang = app.prefs.lang;
    let accent = app.prefs.accent_color();
    // Which parts count as "closes the sheet" - computed once for the whole
    // table rather than per row, since the reference sheet is job-wide.
    let sheet_area = app.largest_sheet_area();
    let threshold = app.cfg.dominant_threshold;

    egui::ScrollArea::vertical().max_height(560.0).auto_shrink([false, true]).show(ui, |ui| {
        egui::Grid::new("shapes").striped(true).num_columns(10).spacing([8.0, 4.0]).show(ui, |ui| {
            let mut select_all = app.select_all;
            if ui.checkbox(&mut select_all, "").on_hover_text(super::i18n::t(lang, "select_all_tooltip")).changed() {
                app.select_all = select_all;
                app.shapes.iter_mut().for_each(|s| s.selected = select_all);
            }
            for key in ["th_index", "th_name", "th_bbox", "th_preview", "th_role", "th_qty"] {
                ui.label(RichText::new(super::i18n::t(lang, key)).color(theme::DIM).small());
            }
            ui.label(RichText::new(super::i18n::t(lang, "th_grain")).color(theme::DIM).small()).on_hover_text(super::i18n::t(lang, "th_grain_tooltip"));
            ui.label(RichText::new(super::i18n::t(lang, "th_part_mirror")).color(theme::DIM).small()).on_hover_text(super::i18n::t(lang, "th_part_mirror_tooltip"));
            ui.label(RichText::new(super::i18n::t(lang, "th_dominant")).color(theme::DIM).small()).on_hover_text(super::i18n::t(lang, "th_dominant_tooltip"));
            ui.end_row();

            for (index, row) in app.shapes.iter_mut().enumerate() {
                ui.add_enabled_ui(!locked, |ui| {
                    ui.checkbox(&mut row.selected, "");
                });
                ui.label(RichText::new((index + 1).to_string()).color(theme::DIM));
                ui.label(format!("{}-{}", row.file, index + 1));
                let b = bounds_of(&row.poly.points);
                ui.label(RichText::new(format!("{:.1} x {:.1}", b.w(), b.h())).color(theme::DIM));
                canvas::thumbnail(ui, &row.poly, 88.0, None);

                ui.add_enabled_ui(!locked, |ui| {
                    shell::choice(ui, &format!("role{}", row.ui_id), &mut row.role, &Role::ALL, |r| super::i18n::t(lang, r.key()).to_string());
                });
                ui.add_enabled_ui(!locked, |ui| {
                    ui.add(egui::DragValue::new(&mut row.qty).speed(0.2).range(0..=100_000));
                });
                ui.add_enabled_ui(!locked && row.role == Role::Part, |ui| {
                    shell::choice(ui, &format!("rot{}", row.ui_id), &mut row.rot, &RotRule::ALL, |r| super::i18n::t(lang, r.key()).to_string());
                });
                ui.add_enabled_ui(!locked && row.role == Role::Part, |ui| {
                    shell::choice(ui, &format!("mir{}", row.ui_id), &mut row.mirror, &MirrorRule::ALL, |m| super::i18n::t(lang, m.key()).to_string());
                });

                let dominant = row.role == Role::Part && sheet_area > 0.0 && row.area >= threshold * sheet_area;
                if dominant {
                    ui.label(RichText::new(super::i18n::t(lang, "dominant_closes_sheet")).color(accent).small());
                } else {
                    ui.label("");
                }
                ui.end_row();
            }
        });
    });
}
