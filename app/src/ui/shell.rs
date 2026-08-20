//! Chrome around the four numbered panels: the header strip and its
//! settings menu, the bottom CONFIGURE drawer, the floating RUN control, and
//! the modal dialogs (help, SVG units, and the three confirmations).

use egui::{Align, Layout, RichText};

use super::{config, prefs, theme, App};

/// Shared look for a panel heading: an accent-coloured step number followed
/// by the title. Pair it with `heading_rule` below.
pub fn heading(app: &App, ui: &mut egui::Ui, number: &str, key: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).color(app.prefs.accent_color()).strong().family(theme::heavy()));
        ui.label(RichText::new(app.t(key)).strong().family(theme::heavy()));
    });
}

/// The heavy accent rule under a step heading - the one deliberately loud
/// piece of structure in the design, so the four numbered steps read as a
/// sequence from across the room.
///
/// Separate from `heading` rather than part of it because two of the three
/// panels put controls on the heading's own row (`shapes`'s ALL PART / ALL
/// SHEET / REMOVE SELECTED). A full-width rule allocated inside that row
/// consumes the width those buttons need and they end up drawn on top of
/// it. The rule belongs after the row closes, wherever that is.
pub fn heading_rule(app: &App, ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, app.prefs.accent_color());
    ui.add_space(8.0);
}

/// A flat group box with one hairline border, applied to every panel. No
/// fill difference from its own contents and no bevel: the border is the
/// only thing saying where the panel ends.
pub fn panel_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let response = egui::Frame::new().fill(theme::PANEL).inner_margin(12.0).outer_margin(egui::Margin { top: 0, bottom: 8, left: 0, right: 0 }).show(ui, contents).response;
    theme::hairline(ui.painter(), response.rect, theme::LINE, 1.0);
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
                ui.label(RichText::new("RUSTY").size(20.0).strong().family(theme::heavy()).color(app.prefs.accent_color()));
                ui.label(RichText::new("NESTING").size(20.0).strong().family(theme::heavy()));
            });
            ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(theme::DIM));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(app.t("btn_settings")).on_hover_text(app.t("app_settings_title")).clicked() {
                    app.settings_menu_open = !app.settings_menu_open;
                }
                if ui.button(app.t("btn_help")).on_hover_text(super::keys::hint(app.t("help_button_title"), "F1")).clicked() {
                    app.help_open = true;
                }
                // The CONFIGURE toggle lives up here rather than on the
                // panel it opens: once collapsed, a side panel leaves nothing
                // behind to click.
                let label = if app.settings_open { "<<" } else { ">>" };
                if ui.button(format!("{label} 03 {}", app.t("settings_bar_text"))).on_hover_text(super::keys::hint(app.t("settings_bar_text"), "Ctrl+,")).clicked() {
                    app.settings_open = !app.settings_open;
                }
                // Same affordance on the other side: a collapsed side panel
                // leaves nothing behind to click.
                let label = if app.console_open { ">>" } else { "<<" };
                if ui.button(format!("{} {label}", app.t("console_title"))).on_hover_text(super::keys::hint(app.t("console_title"), "Ctrl+L")).clicked() {
                    app.console_open = !app.console_open;
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
            // Without a floor the window shrinks to its widest row, which is
            // the five 22px swatches - narrower than its own title bar, so
            // the section labels wrap and the panel reads as noise.
            ui.set_min_width(260.0);
            ui.label(RichText::new(app.t("lang_switch_label")).color(theme::DIM));
            ui.horizontal(|ui| {
                for lang in super::i18n::Lang::ALL {
                    if ui.selectable_label(app.prefs.lang == lang, lang.label()).clicked() {
                        app.prefs.lang = lang;
                    }
                }
            });

            ui.separator();
            ui.label(RichText::new(app.t("scale_switch_label")).color(theme::DIM));
            ui.horizontal(|ui| {
                for scale in prefs::Scale::ALL {
                    if ui.selectable_label(app.prefs.scale == scale, app.t(scale.key())).clicked() {
                        app.prefs.scale = scale;
                        theme::apply(ctx, app.prefs.accent_color(), scale.factor());
                    }
                }
            });

            ui.separator();
            ui.label(RichText::new(app.t("accent_switch_label")).color(theme::DIM));
            ui.horizontal(|ui| {
                for swatch in theme::ACCENTS {
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
                    ui.painter().rect_filled(rect, 0.0, swatch);
                    // 2px in the text colour marks the chosen one; everything
                    // else gets the same containing hairline as any other box.
                    let selected = app.prefs.accent_color() == swatch;
                    let (edge, width) = if selected { (theme::TEXT, 2.0) } else { (theme::LINE, 1.0) };
                    theme::hairline(ui.painter(), rect, edge, width);
                    if response.clicked() {
                        app.prefs.accent = prefs::to_hex(swatch);
                        app.accent_hex = app.prefs.accent.clone();
                        theme::apply(ctx, swatch, app.prefs.scale.factor());
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
                        theme::apply(ctx, c, app.prefs.scale.factor());
                    }
                }
            })
            .response
            .on_hover_text(app.t("accent_hex_tooltip"));
        });
    app.settings_menu_open = open;
}

/// The bottom strip: sheets/unplaced/utilisation for the current best
/// result, always visible. It used to also be the CONFIGURE drawer, but a
/// panel that expands upward from the bottom edge fights this layout - it
/// covers the result it is meant to be tuning, and its own controls end up
/// under the floating RUN control. CONFIGURE is a right-hand side panel
/// now (`config_panel` below); this strip is only a readout.
pub fn bottom_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("bottom_bar").frame(egui::Frame::new().fill(theme::PANEL).inner_margin(8.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            if let Some(snap) = &app.snapshot {
                let summary = super::i18n::tv(
                    app.prefs.lang,
                    "bottom_bar_summary",
                    &[
                        ("sheets", &snap.placements.len().to_string()),
                        ("unplaced", &snap.unplaced_count.to_string()),
                        ("util", &format!("{:.1}", snap.utilisation)),
                    ],
                );
                ui.label(RichText::new(summary).color(theme::DIM)).on_hover_text(app.t("bottom_bar_summary_tooltip"));
            }
        });
    });
}

/// 03 CONFIGURE, as a collapsible right-hand side panel: open it and the
/// central column narrows rather than being covered, so a setting can be
/// changed while the sheet it affects stays on screen.
///
/// Collapsed is a plain early return rather than a zero-width panel - egui
/// side panels do not collapse natively, and the header's own CONFIGURE
/// button is the affordance that brings it back.
pub fn config_panel(app: &mut App, ctx: &egui::Context) {
    if !app.settings_open {
        return;
    }
    egui::SidePanel::right("configure")
        .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(8.0))
        .default_width(460.0)
        .width_range(360.0..=760.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("03").color(app.prefs.accent_color()).strong().family(theme::heavy()));
                ui.label(RichText::new(app.t("settings_bar_text")).strong().family(theme::heavy()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(">>").on_hover_text(app.t("settings_bar_text")).clicked() {
                        app.settings_open = false;
                    }
                });
            });
            heading_rule(app, ui);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| config::panel(app, ui));
        });
}

/// RUN / STOP, its progress bar and status line - floating bottom-left so
/// it's reachable without scrolling however long the shapes table gets.
/// How much bigger RUN/STOP are than an ordinary button. It is the one
/// control the whole screen leads to, and at the shared button size it read
/// as just another item in the row. Applied to both the label and the box:
/// scaling the text alone would leave the same padding around a bigger word.
const RUN_BUTTON_SCALE: f32 = 2.0;

pub fn run_float(app: &mut App, ctx: &egui::Context) {
    // Anchored to the *central column's* right edge, not the window's - this
    // runs after `config_panel`/`console::panel`, so `available_rect` is
    // already what those two left behind. Anchoring to the screen instead
    // parks RUN underneath CONFIGURATION the moment that panel opens.
    //
    // `RIGHT_BOTTOM` takes its x offset leftward from the screen edge, so the
    // panel's own width has to come off it as well as the 12px gutter.
    let inset = ctx.available_rect().right() - ctx.screen_rect().right() - 12.0;
    egui::Window::new("run").title_bar(false).resizable(false).anchor(egui::Align2::RIGHT_BOTTOM, [inset, -56.0]).show(ctx, |ui| {
        // Derived from the live button size rather than hardcoded, so this
        // keeps tracking the TEXT SIZE preference like everything else.
        let text_size = egui::TextStyle::Button.resolve(ui.style()).size * RUN_BUTTON_SCALE;
        let min_size = egui::vec2(0.0, (text_size + ui.spacing().button_padding.y * 2.0) * RUN_BUTTON_SCALE / 2.0);
        let big = |text: RichText| egui::Button::new(text.size(text_size).strong().family(theme::heavy())).min_size(min_size);

        // A plain shrink-wrapping row, status first so the button still ends
        // up on the right. Deliberately *not* `Layout::right_to_left`, which
        // looks like the natural choice for a right-anchored control and is a
        // trap: it needs to know where its right edge is, so it claims the
        // full available width, and an auto-sized window then inflates to
        // half the screen with the button marooned in an empty panel.
        ui.horizontal(|ui| {
            let accent = app.prefs.accent_color();
            status_label(ui, &app.run_status);
            if app.running {
                ui.spinner();
                if ui.add(big(RichText::new(app.t("btn_stop")).color(theme::ERROR))).on_hover_text(super::keys::hint(app.t("btn_stop"), "Ctrl+R")).clicked() {
                    app.worker.cancel.cancel();
                    app.run_status.ok(app.t("run_status_stopped"));
                }
            } else if ui
                .add_enabled(!app.shapes.is_empty(), big(RichText::new(app.t("btn_run")).color(accent)))
                .on_hover_text(super::keys::hint(app.t("btn_run_tooltip"), "Ctrl+R"))
                .clicked()
            {
                app.start_run();
            }
        });

        if app.running {
            // Square, like every other edge in this theme - egui's default is
            // a fully rounded pill, which is the one shape the design has no
            // place for.
            ui.add(
                egui::ProgressBar::new(app.progress)
                    .desired_width(240.0 * RUN_BUTTON_SCALE)
                    .corner_radius(egui::CornerRadius::ZERO)
                    .fill(app.prefs.accent_color()),
            );
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
            .map(|b| (b.placements.len().to_string(), format!("{:.1}", b.utilisation)))
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
        ui.label(RichText::new(app.t(title_key)).strong().family(theme::heavy()).size(16.0));
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
            ui.label(RichText::new(app.t("help_title")).strong().family(theme::heavy()).size(18.0).color(app.prefs.accent_color()));
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
        ui.add_space(12.0);
        ui.label(RichText::new(app.t("help_keys_title")).color(app.prefs.accent_color()).strong().family(theme::heavy()));
        ui.add_space(4.0);
        egui::Grid::new("help_keys").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            for binding in super::keys::BINDINGS {
                ui.label(RichText::new(binding.keys).strong().family(theme::heavy()));
                ui.label(RichText::new(app.t(binding.description_key)).color(theme::DIM));
                ui.end_row();
            }
        });
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
