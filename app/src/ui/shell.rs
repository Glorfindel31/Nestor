//! Chrome around the four numbered panels: the header strip and its
//! settings menu, the bottom CONFIGURE drawer, the floating RUN control, and
//! the modal dialogs (help, SVG units, and the three confirmations).

use egui::{Align, Layout, RichText};

use super::{config, prefs, theme, App};

/// Shared look for a panel heading: an accent-coloured step number followed
/// by the title, in the two-colour system the rest of the UI uses.
pub fn heading(app: &App, ui: &mut egui::Ui, number: &str, key: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).color(app.prefs.accent_color()).strong());
        ui.label(RichText::new(app.t(key)).strong());
    });
}

/// A sunken, bevelled group box - the Win95 tell, applied to every panel.
pub fn panel_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let response = egui::Frame::new().fill(theme::PANEL).inner_margin(12.0).outer_margin(egui::Margin { top: 0, bottom: 8, left: 0, right: 0 }).show(ui, contents).response;
    theme::bevel(ui.painter(), response.rect, false);
}

pub fn status_label(ui: &mut egui::Ui, status: &super::state::Status) {
    if !status.text.is_empty() {
        ui.label(RichText::new(&status.text).color(if status.error { theme::ERROR } else { theme::DIM }));
    }
}

pub fn header(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::top("header").frame(egui::Frame::new().fill(theme::PANEL).inner_margin(8.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            // No inter-item spacing across these two: it is one wordmark
            // split by colour, not two words.
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(RichText::new("RUSTY").size(20.0).strong().color(app.prefs.accent_color()));
                ui.label(RichText::new("NESTING").size(20.0).strong());
            });
            ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(theme::DIM));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("*").on_hover_text(app.t("app_settings_title")).clicked() {
                    app.settings_menu_open = !app.settings_menu_open;
                }
                if ui.button("?").on_hover_text(app.t("help_button_title")).clicked() {
                    app.help_open = true;
                }
                if ui.button(RichText::new(app.t("btn_reset")).color(theme::ERROR)).on_hover_text(app.t("btn_reset_tooltip")).clicked() {
                    app.confirm_reset = true;
                }
            });
        });
    });

    if app.settings_menu_open {
        settings_menu(app, ctx);
    }
}

fn settings_menu(app: &mut App, ctx: &egui::Context) {
    let mut open = app.settings_menu_open;
    egui::Window::new(app.t("app_settings_title"))
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::RIGHT_TOP, [-12.0, 52.0])
        .show(ctx, |ui| {
            ui.label(RichText::new(app.t("lang_switch_label")).color(theme::DIM));
            ui.horizontal(|ui| {
                for lang in super::i18n::Lang::ALL {
                    if ui.selectable_label(app.prefs.lang == lang, lang.label()).clicked() {
                        app.prefs.lang = lang;
                    }
                }
            });

            ui.add_space(8.0);
            ui.label(RichText::new(app.t("scale_switch_label")).color(theme::DIM));
            ui.horizontal(|ui| {
                for scale in prefs::Scale::ALL {
                    if ui.selectable_label(app.prefs.scale == scale, app.t(scale.key())).clicked() {
                        app.prefs.scale = scale;
                        ctx.set_zoom_factor(scale.factor());
                    }
                }
            });

            ui.add_space(8.0);
            ui.label(RichText::new(app.t("accent_switch_label")).color(theme::DIM));
            ui.horizontal(|ui| {
                for swatch in theme::ACCENTS {
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 0.0, swatch);
                    theme::bevel(ui.painter(), rect, app.prefs.accent_color() != swatch);
                    if response.clicked() {
                        app.prefs.accent = prefs::to_hex(swatch);
                        app.accent_hex = app.prefs.accent.clone();
                        theme::apply(ctx, swatch);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label(app.t("accent_hex_label"));
                let response = ui.add(egui::TextEdit::singleline(&mut app.accent_hex).desired_width(80.0).char_limit(7));
                if response.changed() {
                    // Only a fully valid colour applies. A half-typed "#c8"
                    // must not snap the whole UI to black on the way past.
                    if let Some(c) = prefs::parse_hex(&app.accent_hex) {
                        app.prefs.accent = app.accent_hex.clone();
                        theme::apply(ctx, c);
                    }
                }
            })
            .response
            .on_hover_text(app.t("accent_hex_tooltip"));
        });
    app.settings_menu_open = open;
}

/// The bottom drawer: a permanently visible summary strip that expands
/// upward into the CONFIGURE panel, rather than pushing page content down.
pub fn bottom_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("bottom_bar").frame(egui::Frame::new().fill(theme::PANEL).inner_margin(8.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            let label = if app.settings_open { "v" } else { "^" };
            if ui.button(format!("{label} 03 {}", app.t("settings_bar_text"))).clicked() {
                app.settings_open = !app.settings_open;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(snap) = &app.snapshot {
                    let summary = super::i18n::tv(
                        app.prefs.lang,
                        "bottom_bar_summary",
                        &[
                            ("sheets", &snap.placements.len().to_string()),
                            ("unplaced", &snap.unplaced_count.to_string()),
                            ("util", &format!("{:.1}", snap.utilisation * 100.0)),
                        ],
                    );
                    ui.label(RichText::new(summary).color(theme::DIM)).on_hover_text(app.t("bottom_bar_summary_tooltip"));
                }
            });
        });

        if app.settings_open {
            ui.separator();
            egui::ScrollArea::vertical().max_height(ui.available_height() * 0.9).show(ui, |ui| config::panel(app, ui));
        }
    });
}

/// RUN / STOP, its progress bar and status line - floating bottom-left so
/// it's reachable without scrolling however long the shapes table gets.
pub fn run_float(app: &mut App, ctx: &egui::Context) {
    egui::Window::new("run").title_bar(false).resizable(false).anchor(egui::Align2::LEFT_BOTTOM, [12.0, -56.0]).show(ctx, |ui| {
        ui.horizontal(|ui| {
            let accent = app.prefs.accent_color();
            if app.running {
                if ui.button(RichText::new(app.t("btn_stop")).color(theme::ERROR).strong()).clicked() {
                    app.worker.cancel.cancel();
                    app.run_status.ok(app.t("run_status_stopped"));
                }
                ui.spinner();
            } else if ui.add_enabled(!app.shapes.is_empty(), egui::Button::new(RichText::new(app.t("btn_run")).color(accent).strong())).clicked() {
                app.start_run();
            }
            status_label(ui, &app.run_status);
        });

        if app.running {
            ui.add(egui::ProgressBar::new(app.progress).desired_width(240.0).fill(app.prefs.accent_color()));
        }
        if app.cfg.mirror {
            ui.label(RichText::new(app.t("mirror_run_warning")).color(theme::ERROR).small());
        }
    });
}

pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    help(app, ctx);
    super::import::svg_unit_dialog(app, ctx);

    if app.confirm_reset {
        match confirm(app, ctx, "confirm_reset_title", "confirm_reset_message", &[]) {
            Some(true) => {
                app.confirm_reset = false;
                app.reset();
            }
            Some(false) => app.confirm_reset = false,
            None => {}
        }
    }

    if app.confirm_remove {
        let n = app.shapes.iter().filter(|s| s.selected).count().to_string();
        match confirm(app, ctx, "confirm_remove_title", "confirm_remove_message", &[("n", &n)]) {
            Some(true) => {
                app.confirm_remove = false;
                app.shapes.retain(|s| !s.selected);
                app.select_all = false;
            }
            Some(false) => app.confirm_remove = false,
            None => {}
        }
    }

    // "Recover last session's best result, or start fresh?" Declining clears
    // the saved file, so the answer sticks instead of being asked again on
    // every launch.
    if app.recover_prompt.is_some() && !app.help_open {
        let (sheets, util) = app
            .recover_prompt
            .as_ref()
            .map(|b| (b.placements.len().to_string(), format!("{:.1}", b.utilisation * 100.0)))
            .unwrap_or_default();
        match confirm(app, ctx, "recover_title", "recover_message", &[("sheets", &sheets), ("util", &util)]) {
            Some(true) => {
                if let Some(best) = app.recover_prompt.take() {
                    app.recover(*best);
                }
            }
            Some(false) => {
                app.recover_prompt = None;
                app.worker.clear_best_result();
            }
            None => {}
        }
    }
}

/// A yes/no modal. Returns `Some(true)`/`Some(false)` on the frame the user
/// answers, `None` while it's still open.
fn confirm(app: &App, ctx: &egui::Context, title_key: &str, message_key: &str, vars: &[(&str, &str)]) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::new(title_key)).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.label(RichText::new(app.t(title_key)).strong().size(16.0));
        ui.add_space(8.0);
        ui.label(super::i18n::tv(app.prefs.lang, message_key, vars));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("OK").clicked() {
                answer = Some(true);
            }
            // `consume_key`, not `key_pressed`: several dialogs can be
            // stacked (help over the recovery prompt on first launch), and a
            // plain read lets one Escape press dismiss all of them at once -
            // silently answering a question the user never saw.
            if ui.button("CANCEL").clicked() || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

fn help(app: &mut App, ctx: &egui::Context) {
    if !app.help_open {
        return;
    }
    let mut close = false;
    egui::Modal::new(egui::Id::new("help")).show(ctx, |ui| {
        ui.set_max_width(560.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(app.t("help_title")).strong().size(18.0).color(app.prefs.accent_color()));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for lang in super::i18n::Lang::ALL {
                    if ui.selectable_label(app.prefs.lang == lang, lang.label()).clicked() {
                        app.prefs.lang = lang;
                    }
                }
            });
        });
        ui.add_space(8.0);
        ui.label(app.t("help_intro"));
        ui.add_space(8.0);
        for key in ["help_step_import", "help_step_roles", "help_step_configure", "help_step_run"] {
            ui.label(app.t(key));
        }
        ui.add_space(8.0);
        ui.label(RichText::new(app.t("help_tip")).color(theme::DIM));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            // Persisted only when the dialog is actually closed, so ticking
            // the box and then killing the window doesn't half-apply.
            let mut dismissed = app.prefs.help_dismissed;
            if ui.checkbox(&mut dismissed, app.t("help_dont_show")).changed() {
                app.prefs.help_dismissed = dismissed;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(app.t("help_close")).clicked() {
                    close = true;
                }
            });
        });
    });
    if close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.help_open = false;
    }
}

/// A labelled numeric field, the shape almost every config row takes.
pub fn number_row<T: egui::emath::Numeric>(ui: &mut egui::Ui, label: &str, tooltip: &str, value: &mut T, speed: f64, range: std::ops::RangeInclusive<T>) {
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 20.0], egui::Label::new(RichText::new(label).color(theme::DIM)));
        ui.add(egui::DragValue::new(value).speed(speed).range(range));
    })
    .response
    .on_hover_text(tooltip);
}

/// A dropdown over a fixed set of variants, each labelled through `t()`.
pub fn choice<T: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, current: &mut T, options: &[T], label_of: impl Fn(T) -> String) {
    egui::ComboBox::from_id_salt(id).selected_text(label_of(*current)).show_ui(ui, |ui| {
        for &option in options {
            ui.selectable_value(current, option, label_of(option));
        }
    });
}

/// Text drawn in the accent colour, for the one-off places that need it.
pub fn accent(app: &App, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(app.prefs.accent_color())
}
