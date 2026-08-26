//! The saved shape library: parts worth keeping, and offcuts worth reusing.
//!
//! Two features sharing one store, because to the code they are the same
//! thing - a named polygon someone wants back later. The difference is only
//! what the user does with it: a saved **part** goes into a job as something
//! to cut, a saved **remnant** as something to cut it *from*.
//!
//! **Why the remnant half matters more than it looks.** Every other lever in
//! this app fights for another percent or two of packing density. Re-using an
//! offcut instead of opening a fresh sheet is a whole sheet, and a job that
//! leaves four sheets half-empty currently throws four usable pieces of
//! material away by simply not recording that they exist. The nesting engine
//! needs no changes at all to consume them - it already accepts an arbitrary
//! polygon with holes as a sheet - so the only thing that was ever missing is
//! somewhere to write them down.
//!
//! Remnants are offered oldest-first (`ShapeStore::available`). A shelf worked
//! newest-first silts up with old stock that is never chosen and eventually
//! gets binned, which is precisely the waste being addressed.

use egui::RichText;

use super::state::{bounds_of, Role, ShapeRow};
use super::{canvas, console, shell, theme, App};
use crate::dto::{RemnantRequest, StoredKind};

/// Timestamp for a new store entry.
///
/// ponytail: seconds since the epoch, zero-padded, not a formatted date -
/// the only thing this value has to do is sort correctly, and `chrono` is not
/// worth a dependency for a string nobody parses. Displayed as-is is fine
/// because it is displayed only as relative position in a FIFO list.
fn now_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs:012}")
}

/// The library panel, shown under IMPORT.
pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    let lang = app.prefs.lang;
    shell::panel_frame(ui, |ui| {
        ui.horizontal(|ui| {
            shell::heading(app, ui, "", "heading_library_text");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(if app.store_open { "v" } else { ">" }).on_hover_text(app.t("toggle_collapse_tooltip")).clicked() {
                    app.store_open = !app.store_open;
                }
                ui.label(
                    RichText::new(super::i18n::tv(
                        lang,
                        "library_summary",
                        &[
                            ("parts", &app.store.available(StoredKind::Part).len().to_string()),
                            ("remnants", &app.store.available(StoredKind::Remnant).len().to_string()),
                        ],
                    ))
                    .color(theme::DIM())
                    .small(),
                );
            });
        });
        if !app.store_open {
            return;
        }
        shell::heading_rule(ui);
        ui.label(RichText::new(app.t("library_hint")).color(theme::DIM()).small());
        ui.add_space(6.0);
        section(app, ui, StoredKind::Part);
        section(app, ui, StoredKind::Remnant);
    });
}

fn section(app: &mut App, ui: &mut egui::Ui, kind: StoredKind) {
    let lang = app.prefs.lang;
    let (heading, empty) = match kind {
        StoredKind::Part => ("library_parts_heading", "library_no_parts"),
        StoredKind::Remnant => ("library_remnants_heading", "library_no_remnants"),
    };
    ui.label(RichText::new(super::i18n::t(lang, heading)).color(theme::ACCENT()).small());

    // Ids and labels are collected first so the row loop doesn't hold a
    // borrow of `app.store` while the buttons want `&mut app`.
    #[allow(clippy::type_complexity)]
    let entries: Vec<(usize, String, crate::dto::PolygonDto, usize, Option<Vec<f64>>, Option<bool>)> = app
        .store
        .available(kind)
        .into_iter()
        .map(|s| (s.id, s.name.clone(), s.polygon.clone(), s.default_qty, s.allowed_rotations.clone(), s.mirror))
        .collect();

    if entries.is_empty() {
        ui.label(RichText::new(super::i18n::t(lang, empty)).color(theme::DIM()).small());
        ui.add_space(6.0);
        return;
    }

    let locked = app.controls_locked();
    #[allow(clippy::type_complexity)]
    let mut add: Option<(usize, crate::dto::PolygonDto, String, usize, Option<Vec<f64>>, Option<bool>)> = None;
    let mut delete: Option<usize> = None;

    egui::ScrollArea::horizontal().id_salt(heading).max_height(190.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (id, name, polygon, qty, angles, mirror) in &entries {
                let b = bounds_of(&polygon.points);
                ui.allocate_ui(egui::vec2(150.0, 170.0), |ui| {
                    ui.vertical(|ui| {
                        canvas::thumbnail(ui, polygon, 80.0, None);
                        ui.label(RichText::new(name).small());
                        ui.label(RichText::new(format!("{:.0} x {:.0} mm", b.w(), b.h())).color(theme::DIM()).small());
                        ui.horizontal(|ui| {
                            if ui.add_enabled(!locked, egui::Button::new(super::i18n::t(lang, "library_add"))).clicked() {
                                add = Some((*id, polygon.clone(), name.clone(), *qty, angles.clone(), *mirror));
                            }
                            if ui
                                .add_enabled(!locked, egui::Button::new(RichText::new(super::i18n::t(lang, "library_delete")).color(theme::ERROR())))
                                .on_hover_text(super::i18n::t(lang, "library_delete_tooltip"))
                                .clicked()
                            {
                                delete = Some(*id);
                            }
                        });
                    });
                });
            }
        });
    });
    ui.add_space(6.0);

    if let Some((id, polygon, name, qty, angles, mirror)) = add {
        // A remnant is stock to cut *from*; a saved part is a thing to cut.
        // Setting the role here rather than making the user do it is the
        // difference between the library saving a step and adding one.
        let role = if kind == StoredKind::Remnant { Role::Sheet } else { Role::Part };
        let mut row = ShapeRow::new(app.next_ui_id, name, polygon);
        row.role = role;
        row.qty = if role == Role::Sheet { 1 } else { qty };
        // A saved part keeps the grain rule it was saved with. Without this
        // the library silently hands back an unconstrained copy, and a
        // grained part comes back rotatable to any angle.
        row.rot = super::state::RotRule::from_angles(angles.as_deref());
        row.mirror = super::state::MirrorRule::from_option(mirror);
        // Provenance, so a remnant can be marked consumed once nested onto.
        row.from_store = Some(id);
        app.next_ui_id += 1;
        app.shapes.push(row);
    }
    if let Some(id) = delete {
        app.store.remove(id);
        let store = app.store.clone();
        app.worker.save_store(store);
    }
}

/// The RESULT panel's offcut controls: scan the finished nest, then keep what
/// is worth keeping.
///
/// Two steps rather than one, deliberately. Harvesting automatically after
/// every run would fill the shelf with offcuts from arrangements the user was
/// only trying out, and a shelf full of material that was never actually cut
/// is worse than no shelf - it makes the whole list untrustworthy.
pub fn offcut_controls(app: &mut App, ui: &mut egui::Ui) {
    if app.snapshot.is_none() {
        return;
    }
    let lang = app.prefs.lang;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let can_scan = app.result_config.is_some() && !app.controls_locked() && !app.harvesting;
        if ui.add_enabled(can_scan, egui::Button::new(app.t("offcut_scan"))).on_hover_text(app.t("offcut_scan_tooltip")).clicked() {
            scan(app);
        }
        if app.harvesting {
            ui.label(RichText::new(app.t("offcut_scanning")).color(theme::DIM()).small());
        }
        if !app.remnants.is_empty() {
            let total: f64 = app.remnants.iter().map(|r| r.area).sum();
            ui.label(
                RichText::new(super::i18n::tv(
                    lang,
                    "offcut_found",
                    &[("n", &app.remnants.len().to_string()), ("area", &format!("{:.2}", total / 1_000_000.0))],
                ))
                .color(theme::DIM())
                .small(),
            );
            if ui.button(app.t("offcut_keep")).on_hover_text(app.t("offcut_keep_tooltip")).clicked() {
                keep_all(app);
            }
        }
    });
}

fn scan(app: &mut App) {
    let Some(snapshot) = &app.snapshot else { return };
    let Some(config) = app.result_config.clone() else { return };
    app.harvesting = true;
    app.worker.compute_remnants(RemnantRequest {
        sheets: app.result_sheets.clone(),
        placements: snapshot.placements.clone(),
        parts_by_id: app.parts_by_id.clone(),
        config,
    });
}

fn keep_all(app: &mut App) {
    let stamp = now_stamp();
    let remnants = std::mem::take(&mut app.remnants);
    let count = remnants.len();
    for (i, remnant) in remnants.into_iter().enumerate() {
        // The name carries the usable rectangle, not the true outline's
        // bounding box: it is what someone reads off the shelf months later,
        // and the usable size is the number they can actually plan against.
        let name = super::i18n::tv(
            app.prefs.lang,
            "offcut_name",
            &[
                ("sheet", &(remnant.sheet_index + 1).to_string()),
                ("w", &format!("{:.0}", remnant.usable_width)),
                ("h", &format!("{:.0}", remnant.usable_height)),
            ],
        );
        // Distinct stamps so the FIFO order within one harvest is stable
        // rather than decided by whatever the sort happens to do with ties.
        app.store.add(StoredKind::Remnant, name, remnant.polygon, 1, None, None, format!("{stamp}-{i:03}"));
    }
    let store = app.store.clone();
    app.worker.save_store(store);
    app.console.log(console::Kind::Plain, format!("offcuts: kept {count}"));
}

/// Saves the ticked rows into the parts library.
///
/// Sheets are skipped rather than saved as parts: a sheet already in the job
/// is stock, not a repeat part, and saving it under the parts heading would
/// put it in the list the user picks *things to cut* from.
pub fn save_selected_parts(app: &mut App) {
    let stamp = now_stamp();
    #[allow(clippy::type_complexity)]
    let saved: Vec<(String, crate::dto::PolygonDto, usize, Option<Vec<f64>>, Option<bool>)> = app
        .shapes
        .iter()
        .filter(|s| s.selected && s.role == Role::Part)
        .map(|s| (s.file.clone(), s.poly.clone(), s.qty.max(1), s.rot.angles(), s.mirror.as_option()))
        .collect();
    if saved.is_empty() {
        app.run_status.err(app.t("library_nothing_to_save"));
        return;
    }
    let count = saved.len();
    for (i, (name, polygon, qty, angles, mirror)) in saved.into_iter().enumerate() {
        app.store.add(StoredKind::Part, name, polygon, qty, angles, mirror, format!("{stamp}-{i:03}"));
    }
    let store = app.store.clone();
    app.worker.save_store(store);
    let msg = super::i18n::tv(app.prefs.lang, "library_saved", &[("n", &count.to_string())]);
    app.run_status.ok(msg);
}
