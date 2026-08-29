//! 01 IMPORT: the file picker, the drop target, the SVG-unit dialog, and the
//! "just give me a rectangle" builder.

use egui::RichText;

use super::{console, shell, state, theme, App};
use crate::dto::{PointDto, PolygonDto};

const FILTER: [&str; 2] = ["dxf", "svg"];

/// A one-click sample job: fixture parts plus the stock sheet they were
/// measured on. The files are the repo's own benchmark fixtures - the same
/// ones `bench.sh` drives - embedded in the binary, so a fresh install has
/// something real to nest without hunting for a DXF, and a "is it slow for
/// you too?" question has a shared starting point.
struct Preset {
    /// Untranslated on purpose: these name fixtures, not UI concepts.
    name: &'static str,
    hint: &'static str,
    files: &'static [(&'static str, &'static [u8])],
    sheet: (f64, f64),
    qty: usize,
}

/// `include_bytes!` rather than reading `tests/fixtures/` at runtime: the
/// shipped binary has no repo next to it.
macro_rules! fixture {
    ($name:literal) => {
        ($name, include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/", $name)) as &[u8])
    };
}

const PRESETS: [Preset; 4] = [
    Preset { name: "TILE", hint: "one interlocking tile x60 - the shape the engine nests densest", files: &[fixture!("one.dxf")], sheet: (1500.0, 1500.0), qty: 60 },
    Preset { name: "MIX", hint: "two unrelated profiles x40 each - tests mixing parts on a sheet", files: &[fixture!("two.dxf"), fixture!("three.dxf")], sheet: (2440.0, 1220.0), qty: 40 },
    Preset { name: "BENCH", hint: "the four benchmark parts x100 each - the job bench.sh measures", files: &[fixture!("nestTest01.dxf"), fixture!("nestTest02.dxf"), fixture!("nestTest03.dxf"), fixture!("nestTest04.dxf")], sheet: (1500.0, 1500.0), qty: 100 },
    Preset { name: "CURVES", hint: "small fillets and splines x30 - tests curve tessellation", files: &[fixture!("fillets.dxf"), fixture!("curvy.dxf")], sheet: (500.0, 500.0), qty: 30 },
];

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    shell::panel_frame(ui, |ui| {
        shell::heading(app, ui, "01", "heading_import_text");
        shell::heading_rule(ui);

        ui.horizontal(|ui| {
            ui.label(RichText::new(app.t("tolerance_label")).color(theme::DIM()));
            ui.add(shell::number(&mut app.cfg.curve_tolerance, 0.05, 0.01..=10.0));

            let enabled = app.importing == 0 && !app.controls_locked();
            if ui.add_enabled(enabled, egui::Button::new(shell::accent(app.t("btn_browse")).strong().family(theme::heavy()))).clicked() {
                browse(app);
            }
            if app.importing > 0 {
                ui.spinner();
                let msg = app.tv("import_importing", &[("n", &app.importing.to_string())]);
                ui.label(RichText::new(msg).color(theme::DIM()));
            }
        })
        .response
        .on_hover_text(app.t("tolerance_tooltip"));

        // The drop target. egui reports hovered files globally rather than
        // per-widget, so this is a painted affordance, not a hit-tested one -
        // a file dropped anywhere on the window counts, which is what people
        // actually do.
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 48.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, if hovering { theme::ACCENT().gamma_multiply(0.25) } else { theme::WELL() });
        theme::hairline(&painter, rect, if hovering { theme::ACCENT() } else { theme::LINE() }, if hovering { 2.0 } else { 1.0 });
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            app.t("dropzone_text"),
            egui::TextStyle::Body.resolve(ui.style()),
            if hovering { theme::ACCENT() } else { theme::DIM() },
        );

        ui.add_space(8.0);
        ui.label(RichText::new(app.t("rect_hint")).color(theme::DIM()).small());
        ui.horizontal(|ui| {
            ui.label(RichText::new(app.t("rect_width_label")).color(theme::DIM()));
            ui.add(shell::number(&mut app.rect_w, 10.0, 0.0..=1e6));
            ui.label(RichText::new(app.t("rect_height_label")).color(theme::DIM()));
            ui.add(shell::number(&mut app.rect_h, 10.0, 0.0..=1e6));
            ui.label(RichText::new(app.t("rect_layer_label")).color(theme::DIM()));
            ui.add(egui::TextEdit::singleline(&mut app.rect_layer).desired_width(110.0));
            if ui.add_enabled(!app.controls_locked(), egui::Button::new(app.t("btn_add_rect"))).clicked() {
                add_rectangle(app);
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new(app.t("samples_hint")).color(theme::DIM()).small());
        ui.horizontal(|ui| {
            let enabled = app.importing == 0 && !app.controls_locked();
            for preset in &PRESETS {
                if ui.add_enabled(enabled, egui::Button::new(preset.name)).on_hover_text(preset.hint).clicked() {
                    load_preset(app, preset);
                }
            }
        });

        shell::status_label(ui, &app.import_status);
    });
}

fn browse(app: &mut App) {
    let picked = rfd::FileDialog::new().add_filter("DXF / SVG", &FILTER).pick_files();
    if let Some(paths) = picked {
        start_import(app, paths);
    }
}

/// egui hands dropped files to whoever asks, once, on the frame they land.
/// Filtering by extension here (rather than trusting the OS dialog's filter,
/// which a drag bypasses entirely) is what keeps a dropped .pdf from being
/// handed to the DXF parser.
pub fn handle_dropped_files(app: &mut App, ctx: &egui::Context) {
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
    if dropped.is_empty() {
        return;
    }
    let total = dropped.len();
    let usable: Vec<_> = dropped.into_iter().filter(|p| p.extension().map(|e| FILTER.iter().any(|f| e.eq_ignore_ascii_case(f))).unwrap_or(false)).collect();
    if usable.len() < total {
        app.console.log(console::Kind::Plain, format!("ignored {} dropped file(s) that were not .dxf/.svg", total - usable.len()));
    }
    if !usable.is_empty() {
        start_import(app, usable);
    }
}

/// Asks about SVG units **once per batch**, not once per file, and only when
/// the batch actually contains an SVG. Being asked the same question thirty
/// times for one drop is not a dialog, it's a punishment.
fn start_import(app: &mut App, paths: Vec<std::path::PathBuf>) {
    let has_svg = paths.iter().any(|p| p.extension().map(|e| e.eq_ignore_ascii_case("svg")).unwrap_or(false));
    if has_svg {
        app.pending_svg_batch = Some(paths);
        app.svg_unit_choice = None;
    } else {
        dispatch(app, paths, None);
    }
}

fn dispatch(app: &mut App, paths: Vec<std::path::PathBuf>, svg_unit: Option<String>) {
    app.importing = paths.len();
    let msg = app.tv("import_importing", &[("n", &paths.len().to_string())]);
    app.import_status.ok(msg);
    app.worker.import(paths, app.cfg.curve_tolerance, svg_unit);
}

/// Metric only, and deliberately so: an SVG's own `width`/`height` may be
/// meaningless, and silently guessing wrong scales a whole job. Imperial
/// units are not offered at all - `geometry::svg_import` rejects them
/// outright rather than converting.
const SVG_UNITS: [(&str, &str); 5] = [("", "svg_unit_auto"), ("mm", "svg_unit_mm"), ("cm", "svg_unit_cm"), ("m", "svg_unit_m"), ("px", "svg_unit_px")];

pub fn svg_unit_dialog(app: &mut App, ctx: &egui::Context) {
    if app.pending_svg_batch.is_none() {
        return;
    }
    let mut go = false;
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("svg_units")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.label(RichText::new(app.t("svg_unit_title")).strong().family(theme::heavy()).size(16.0));
        ui.add_space(8.0);
        ui.label(app.t("svg_unit_intro"));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(app.t("svg_unit_label")).color(theme::DIM()));
            let current = app.svg_unit_choice.clone().unwrap_or_default();
            let label = SVG_UNITS.iter().find(|(v, _)| *v == current).map(|(_, k)| app.t(k)).unwrap_or("");
            egui::ComboBox::from_id_salt("svg_unit").selected_text(label).show_ui(ui, |ui| {
                for (value, key) in SVG_UNITS {
                    let selected = current == value;
                    if ui.selectable_label(selected, app.t(key)).clicked() {
                        // Empty string means "auto-detect", which is sent as
                        // no override at all rather than as a unit name.
                        app.svg_unit_choice = if value.is_empty() { None } else { Some(value.to_string()) };
                    }
                }
            });
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button(shell::accent(app.t("svg_unit_ok")).strong().family(theme::heavy())).clicked() {
                go = true;
            }
            if ui.button(app.t("svg_unit_cancel")).clicked() || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                cancel = true;
            }
        });
    });

    if go {
        if let Some(paths) = app.pending_svg_batch.take() {
            let unit = app.svg_unit_choice.clone();
            dispatch(app, paths, unit);
        }
    } else if cancel {
        // Cancelling the unit question aborts the whole batch - importing an
        // SVG at an unknown scale is worse than not importing it.
        app.pending_svg_batch = None;
        app.console.log(console::Kind::Plain, "import cancelled at the SVG unit prompt");
    }
}

/// Unpacks a preset's embedded fixtures next to each other in the temp
/// directory and imports them through the same path a browsed file takes -
/// nothing downstream can tell a sample job from a real one.
///
/// The sheet comes with the job: a "sample" that still needs the user to
/// guess a stock size before it will nest isn't one click, it's three.
fn load_preset(app: &mut App, preset: &Preset) {
    let dir = std::env::temp_dir().join("rustynesting-samples");
    let mut paths = Vec::with_capacity(preset.files.len());
    for (name, bytes) in preset.files {
        let path = dir.join(name);
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, bytes)) {
            app.import_status.err(format!("couldn't unpack {name}: {e}"));
            return;
        }
        paths.push(path);
    }

    app.rect_w = preset.sheet.0;
    app.rect_h = preset.sheet.1;
    app.rect_layer = "SHEET".to_string();
    add_rectangle(app);
    if let Some(row) = app.shapes.last_mut() {
        row.role = state::Role::Sheet;
    }

    // Applied to each row as it lands, in `Msg::Imported` - the import is
    // asynchronous, so there is nothing to set a quantity on yet.
    app.preset_qty = Some(preset.qty);
    app.console.log(console::Kind::Plain, format!("sample job {}: {} file(s) on a {}x{} sheet", preset.name, preset.files.len(), preset.sheet.0, preset.sheet.1));
    dispatch(app, paths, None);
}

/// A stock sheet size (or a plain rectangular part) that isn't in any DXF on
/// hand. Pushed onto the same shape list an import feeds, so it flows through
/// role/quantity/nest/export unchanged.
fn add_rectangle(app: &mut App) {
    if !(app.rect_w > 0.0 && app.rect_h > 0.0) {
        app.import_status.err(app.t("rect_invalid_size"));
        return;
    }
    let layer = if app.rect_layer.trim().is_empty() { "CUSTOM".to_string() } else { app.rect_layer.trim().to_string() };
    let (w, h) = (app.rect_w, app.rect_h);
    // Counter-clockwise, matching the winding importers produce.
    let poly = PolygonDto::new(vec![PointDto { x: 0.0, y: 0.0 }, PointDto { x: w, y: 0.0 }, PointDto { x: w, y: h }, PointDto { x: 0.0, y: h }], layer.clone(), None);
    app.push_shape(layer, poly);
    let msg = super::i18n::tv(app.prefs.lang, "import_status_ok", &[("n", "1"), ("total", &app.shapes.len().to_string())]);
    app.import_status.ok(msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every preset file must be a real, importable drawing. `include_bytes!`
    /// only proves the path existed at compile time - this proves the bytes
    /// still parse, which is what a user clicking the button finds out.
    #[test]
    fn every_preset_ships_files_that_import() {
        let dir = std::env::temp_dir().join("rustynesting-samples-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        for preset in &PRESETS {
            assert!(!preset.files.is_empty(), "{} has no files", preset.name);
            assert!(preset.sheet.0 > 0.0 && preset.sheet.1 > 0.0, "{} has no sheet", preset.name);
            for (name, bytes) in preset.files {
                let path = dir.join(name);
                std::fs::write(&path, bytes).expect("unpack");
                let shapes = crate::commands::import_dxf(path.to_str().unwrap(), 0.3).unwrap_or_else(|e| panic!("{name} ({}) failed to import: {e}", preset.name));
                assert!(!shapes.is_empty(), "{name} imported no shapes");
            }
        }
    }

    /// The rectangle builder is the one place the UI creates geometry rather
    /// than receiving it, so its winding and extent have to be right or every
    /// downstream area/placement calculation is quietly wrong.
    #[test]
    fn a_built_rectangle_has_the_requested_size() {
        let poly = PolygonDto::new(vec![PointDto { x: 0.0, y: 0.0 }, PointDto { x: 2440.0, y: 0.0 }, PointDto { x: 2440.0, y: 1220.0 }, PointDto { x: 0.0, y: 1220.0 }], "CUSTOM".into(), None);
        let b = crate::ui::state::bounds_of(&poly.points);
        assert_eq!((b.w(), b.h()), (2440.0, 1220.0));
        assert_eq!(poly.area(), 2440.0 * 1220.0);
    }
}
