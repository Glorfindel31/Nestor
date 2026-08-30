//! CONFIGURE: the nest settings, basic and advanced.
//!
//! Every field keeps the plain-language tooltip the web UI gave it - these
//! are job parameters a shop-floor operator sets, not developer knobs, and
//! several of them (dominant threshold, rotations, runs) do something
//! non-obvious enough that the tooltip is the only explanation there is.
//!
//! **Four options carry a `+` box and an on/off switch**: rotations,
//! population, mutation and generations. Those are the search's own budget,
//! and the `runs` loop spends more of it on each successive attempt - the box
//! is how much more, the switch is whether that option joins in at all. The
//! escalation used to be hardcoded at +1 / +4 / +5 with mutation flat, and the
//! defaults still are exactly that. Nothing else in the panel gets one: see
//! `dto::RunScales` for why growing a *job* parameter like margin or kerf
//! between attempts would mean comparing two different jobs.

use egui::RichText;

use super::{shell, theme, App};
use crate::dto::PlacementTypeDto;

const PLACEMENT_TYPES: [(PlacementTypeDto, &str); 6] = [
    (PlacementTypeDto::TightFit, "placement_opt_tightfit"),
    (PlacementTypeDto::GravityCorrective, "placement_opt_gravitycorrective"),
    (PlacementTypeDto::GravityTightFit, "placement_opt_gravitytightfit"),
    (PlacementTypeDto::Gravity, "placement_opt_gravity"),
    (PlacementTypeDto::Box, "placement_opt_box"),
    (PlacementTypeDto::ConvexHull, "placement_opt_convexhull"),
];

pub fn panel(app: &mut App, ui: &mut egui::Ui) {
    let lang = app.prefs.lang;
    fn tr(lang: super::i18n::Lang, k: &str) -> &str {
        super::i18n::t(lang, k)
    }
    let t = |k: &'static str| tr(lang, k);
    ui.add_enabled_ui(!app.controls_locked(), |ui| {
        shell::number_row(ui, t("margin_label"), t("margin_tooltip"), &mut app.cfg.margin, 0.1, 0.0..=1000.0);
        shell::number_row(ui, t("spacing_label"), t("spacing_tooltip"), &mut app.cfg.spacing, 0.1, 0.0..=1000.0);
        shell::number_row(ui, t("kerf_label"), t("kerf_tooltip"), &mut app.cfg.kerf, 0.05, 0.0..=100.0);
        shell::number_row(ui, t("runs_label"), t("runs_tooltip"), &mut app.cfg.runs, 0.1, 1..=100);

        // `.inner`, not `.response` - a row container never wins the hover its
        // own children are under, so a tooltip hung on it can't be reached.
        // See `shell::help_bubble`.
        let cleanup_row = ui
            .horizontal(|ui| {
                let name = ui.add_sized([shell::LABEL_W, 20.0], egui::Label::new(RichText::new(t("cleanup_label")).color(theme::DIM())));
                let field = ui.add(egui::TextEdit::singleline(&mut app.cfg.cleanup_threshold).desired_width(70.0).hint_text(t("cleanup_placeholder")));
                name.union(field)
            })
            .inner;
        shell::help_bubble(cleanup_row, t("cleanup_label"), t("cleanup_tooltip"));
        ui.label(RichText::new(t("cleanup_hint")).color(theme::DIM()).small());

        ui.add_space(6.0);
        // Mirroring is deliberately loud. A flipped part is only the same
        // part if the material has no side - no grain, no coating, no printed
        // face - and no asymmetric feature has to stay on one face. Flipping
        // one that does have a side silently produces scrap.
        ui.horizontal(|ui| {
            shell::help_bubble(ui.checkbox(&mut app.cfg.mirror, t("mirror_label")), t("mirror_label"), t("mirror_tooltip"));
            if app.cfg.mirror {
                ui.label(RichText::new(t("mirror_on_badge")).color(theme::ERROR()).strong().family(theme::heavy()));
            }
        });
        if app.cfg.mirror {
            ui.label(RichText::new(t("mirror_hint")).color(theme::ERROR()).small());
        }

        ui.add_space(8.0);
        if ui.button(t(if app.advanced_open { "btn_advanced_expanded" } else { "btn_advanced_collapsed" })).clicked() {
            app.advanced_open = !app.advanced_open;
        }
        if app.advanced_open {
            advanced(app, ui);
        }
    });
}

fn advanced(app: &mut App, ui: &mut egui::Ui) {
    let lang = app.prefs.lang;
    fn tr(lang: super::i18n::Lang, k: &str) -> &str {
        super::i18n::t(lang, k)
    }
    let t = |k: &'static str| tr(lang, k);
    let step_tip = t("scale_step_tooltip");
    let switch_tip = t("scale_switch_tooltip");
    ui.separator();

    let placement_row = ui
        .horizontal(|ui| {
            let name = ui.add_sized([shell::LABEL_W, 20.0], egui::Label::new(RichText::new(t("placement_label")).color(theme::DIM())));
            let options: Vec<PlacementTypeDto> = PLACEMENT_TYPES.iter().map(|(v, _)| *v).collect();
            let combo = shell::choice(ui, "placement", &mut app.cfg.placement_type, &options, |v| {
                PLACEMENT_TYPES.iter().find(|(o, _)| *o == v).map(|(_, k)| t(k).to_string()).unwrap_or_default()
            });
            name.union(combo)
        })
        .inner;
    shell::help_bubble(placement_row, t("placement_label"), t("placement_tooltip"));
    ui.label(RichText::new(t("placement_hint")).color(theme::DIM()).small());

    // The four the `runs` escalation is allowed to grow, and the only rows in
    // the panel with a `+` box and a switch.
    //
    // The rotation grid is left exactly as the engine takes it, including the
    // known quirk that `rotations = 6` produces poor angles for rectangular
    // parts (confirmed by a 60-run sweep). It stays a user-facing setting
    // rather than being silently corrected - a shop that has tuned around it
    // must keep getting the same result.
    shell::scale_row(ui, t("rotations_label"), t("rotations_tooltip"), step_tip, switch_tip, &mut app.cfg.rotations, 0.1, 1..=64, &mut app.cfg.scales.rotations);
    shell::scale_row(ui, t("population_label"), t("population_tooltip"), step_tip, switch_tip, &mut app.cfg.population_size, 0.1, 2..=1000, &mut app.cfg.scales.population);
    shell::scale_row(ui, t("mutation_label"), t("mutation_tooltip"), step_tip, switch_tip, &mut app.cfg.mutation_rate, 0.5, 0.0..=100.0, &mut app.cfg.scales.mutation);
    shell::scale_row(ui, t("generations_label"), t("generations_tooltip"), step_tip, switch_tip, &mut app.cfg.generations, 0.2, 1..=10_000, &mut app.cfg.scales.generations);
    // Said once under the group rather than in four tooltips nobody hovers:
    // with RUNS at 1 there is no second attempt for any of this to apply to.
    ui.label(
        RichText::new(if app.cfg.runs > 1 { t("scale_hint") } else { t("scale_hint_single_run") })
            .color(if app.cfg.runs > 1 { theme::DIM() } else { theme::ACCENT() })
            .small(),
    );
    ui.label(RichText::new(t("mutation_hint")).color(theme::DIM()).small());

    let dominant_row = ui
        .horizontal(|ui| {
            let name = ui.add_sized([shell::LABEL_W, 20.0], egui::Label::new(RichText::new(t("dominant_label")).color(theme::DIM())));
            let slider = ui.add(egui::Slider::new(&mut app.cfg.dominant_threshold, 0.01..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
            name.union(slider)
        })
        .inner;
    shell::help_bubble(dominant_row, t("dominant_label"), t("dominant_tooltip"));
    ui.label(RichText::new(t("dominant_hint")).color(theme::DIM()).small());

    // Capped at what this machine actually has, so the knob is monotonic:
    // more threads is never slower, and the top of the range means "all of
    // them". Above the real count the setting only ever costs time - see
    // `commands::effective_threads`, which clamps the same way for a config
    // saved before this cap existed.
    let cores = std::thread::available_parallelism().map_or(256, std::num::NonZeroUsize::get);
    shell::number_row(ui, t("max_threads_label"), t("max_threads_tooltip"), &mut app.cfg.max_threads, 0.1, 0..=cores);
    ui.label(RichText::new(super::i18n::tv(lang, "max_threads_hint", &[("cores", &cores.to_string())])).color(theme::DIM()).small());
    shell::number_row(ui, t("seed_label"), t("seed_tooltip"), &mut app.cfg.seed, 1.0, 0..=u64::MAX);
}
