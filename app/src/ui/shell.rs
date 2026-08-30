//! Chrome around the numbered panels: the header strip and its settings
//! menu, the CONFIGURE side panel, the floating RUN control, and the modal
//! dialogs (help, SVG units, and the three confirmations).

use egui::{Align, Layout, RichText};

use super::{config, prefs, theme, App};

/// Shared look for a panel heading: an accent-coloured step number followed
/// by the title. Pair it with `heading_rule` below.
///
/// An empty `number` draws the title alone, for the panels that are not steps
/// of the job. The library used to be "01b" - a lettered sub-step inside a
/// numbered sequence, which is the numbering admitting it does not fit. Like
/// CONFIGURE, it is optional: a shelf you visit when you have offcuts, not a
/// stage between importing and nesting.
pub fn heading(app: &App, ui: &mut egui::Ui, number: &str, key: &str) {
    ui.horizontal(|ui| {
        if !number.is_empty() {
            ui.label(RichText::new(number).color(theme::ACCENT()).strong().family(theme::heavy()));
        }
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
pub fn heading_rule(ui: &mut egui::Ui) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::ACCENT());
    ui.add_space(8.0);
}

/// A flat group box with one hairline border, applied to every panel. No
/// fill difference from its own contents and no bevel: the border is the
/// only thing saying where the panel ends.
pub fn panel_frame(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let response = egui::Frame::new().fill(theme::PANEL()).inner_margin(12.0).outer_margin(egui::Margin { top: 0, bottom: 8, left: 0, right: 0 }).show(ui, contents).response;
    theme::hairline(ui.painter(), response.rect, theme::LINE(), 1.0);
}

pub fn status_label(ui: &mut egui::Ui, status: &super::state::Status) {
    if !status.text.is_empty() {
        ui.label(RichText::new(&status.text).color(if status.error { theme::ERROR() } else { theme::DIM() }));
    }
}

pub fn header(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::top("header").frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(8.0)).show(ctx, |ui| {
        ui.horizontal(|ui| {
            // No inter-item spacing across these three: it is one word, split
            // only so the sigma can carry the accent colour on its own.
            //
            // Greek capital sigma for the S. The other five letters of NESTOR
            // have Greek forms too, but N/E/T/O/P are identical to their Latin
            // counterparts - they would cost legibility and buy nothing.
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (text, accented) in [("NE", false), ("Σ", true), ("TOR", false)] {
                    let mut mark = RichText::new(text).size(20.0).strong().family(theme::heavy());
                    if accented {
                        mark = mark.color(theme::ACCENT());
                    }
                    ui.label(mark);
                }
            });
            ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(theme::DIM()));
            // Only ever present when there really is a newer release, so it
            // costs nothing on an up-to-date install and needs no dismissal.
            if let Some(release) = app.update.clone() {
                let label = app.tv("update_available", &[("version", &release.version)]);
                if ui.button(RichText::new(label).color(theme::ACCENT())).on_hover_text(app.t("update_available_tooltip")).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(release.url));
                }
            }

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
                if ui.button(format!("{label} {}", app.t("settings_bar_text"))).on_hover_text(super::keys::hint(app.t("settings_bar_text"), "Ctrl+,")).clicked() {
                    app.settings_open = !app.settings_open;
                }
                // Same affordance on the other side: a collapsed side panel
                // leaves nothing behind to click.
                let label = if app.console_open { ">>" } else { "<<" };
                if ui.button(format!("{} {label}", app.t("console_title"))).on_hover_text(super::keys::hint(app.t("console_title"), "Ctrl+L")).clicked() {
                    app.console_open = !app.console_open;
                }
                if ui.button(RichText::new(app.t("btn_reset")).color(theme::ERROR())).on_hover_text(app.t("btn_reset_tooltip")).clicked() {
                    app.confirm_reset = true;
                }
            });
        });
    });

    if app.settings_menu_open {
        settings_menu(app, ctx);
    }
}

/// The language picker, shared by the settings menu and the help dialog.
///
/// `horizontal_wrapped`, not `horizontal`: nine endonyms do not fit on one
/// line in either place, and egui's plain horizontal layout does not wrap -
/// it draws the overflow on top of what is already there.
fn lang_picker(app: &mut App, ctx: &egui::Context, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        for lang in super::i18n::Lang::ALL {
            if ui.selectable_label(app.prefs.lang == lang, lang.label()).clicked() {
                app.set_lang(ctx, lang);
            }
        }
    });
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
            // narrower than its own title bar, so the section labels wrap and
            // the panel reads as noise.
            ui.set_min_width(260.0);
            ui.label(RichText::new(app.t("lang_switch_label")).color(theme::DIM()));
            lang_picker(app, ctx, ui);

            ui.separator();
            ui.label(RichText::new(app.t("scale_switch_label")).color(theme::DIM()));
            ui.horizontal(|ui| {
                for scale in prefs::Scale::ALL {
                    if ui.selectable_label(app.prefs.scale == scale, app.t(scale.key())).clicked() {
                        app.prefs.scale = scale;
                        theme::apply(ctx, scale.factor(), true);
                    }
                }
            });

            ui.separator();
            ui.label(RichText::new(app.t("theme_switch_label")).color(theme::DIM()));
            // A column, not a row: six names do not fit across this menu at
            // the large TEXT SIZE, and a wrapped row puts the last two
            // somewhere the eye does not look for them.
            for theme in theme::Theme::ALL {
                if ui.selectable_label(app.prefs.theme == theme, theme.label()).clicked() {
                    app.set_theme(ctx, theme);
                }
            }

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
    egui::TopBottomPanel::bottom("bottom_bar").frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(8.0)).show(ctx, |ui| {
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
                ui.label(RichText::new(summary).color(theme::DIM())).on_hover_text(app.t("bottom_bar_summary_tooltip"));
            }
        });
    });
}

/// CONFIGURE, as a collapsible right-hand side panel: open it and the
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
    // A share of the window rather than a fixed 460. On a 1366-wide shop
    // laptop that fixed width took a third of the screen away from the canvas
    // it exists to configure; on a wide monitor it stops growing, because
    // nothing in here reads better past ~460.
    let width = (ctx.screen_rect().width() * 0.30).clamp(340.0, 460.0);
    egui::SidePanel::right("configure")
        .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(8.0))
        .default_width(width)
        .width_range(320.0..=760.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(app.t("settings_bar_text")).strong().family(theme::heavy()));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(">>").on_hover_text(app.t("settings_bar_text")).clicked() {
                        app.settings_open = false;
                    }
                });
            });
            heading_rule(ui);
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

/// The RUN/STOP label, as a fraction of the size the button is built around.
/// At 1.0 the text ran edge to edge and the control read as a slab of letters;
/// the box is unchanged and the difference is interior margin.
const RUN_TEXT_SHRINK: f32 = 0.5;

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
        let full = egui::TextStyle::Button.resolve(ui.style()).size * RUN_BUTTON_SCALE;
        let text_size = full * RUN_TEXT_SHRINK;

        // The height the control had when the label filled it, captured
        // *before* the padding below changes: a smaller label must not shrink
        // the button, it must sit in more air inside the same box.
        let min_size = egui::vec2(0.0, full + ui.spacing().button_padding.y * 2.0);
        // Width can't be pinned the same way - it shrink-wraps the label, and
        // the label's width is whatever the current language makes it. Handing
        // the padding roughly what the glyphs gave back keeps the footprint
        // close to the old one without hardcoding a width per translation.
        ui.spacing_mut().button_padding.x += full;

        let big = |text: RichText| egui::Button::new(text.size(text_size).strong().family(theme::heavy())).min_size(min_size);

        // A plain shrink-wrapping row, status first so the button still ends
        // up on the right. Deliberately *not* `Layout::right_to_left`, which
        // looks like the natural choice for a right-anchored control and is a
        // trap: it needs to know where its right edge is, so it claims the
        // full available width, and an auto-sized window then inflates to
        // half the screen with the button marooned in an empty panel.
        ui.horizontal(|ui| {
            let accent = theme::ACCENT();
            status_label(ui, &app.run_status);
            if app.running {
                ui.spinner();
                if ui.add(big(RichText::new(app.t("btn_stop")).color(theme::ERROR()))).on_hover_text(super::keys::hint(app.t("btn_stop"), "Ctrl+R")).clicked() {
                    app.worker.cancel.cancel();
                    app.run_status.ok(app.t("run_status_stopped"));
                }
            } else {
                // **What this search is about to cost, before the wait rather
                // than after it.** `runs` raises rotations, population and
                // generations together, so the basic panel's friendliest knob
                // is also the one that quietly turns a 47-second job into a
                // ten-minute one for the same answer - see
                // `ConfigForm::search_cost_multiple`. Shown only once the
                // settings are actually above the defaults, so the normal case
                // stays uncluttered, and coloured once it is expensive enough
                // to be worth a second look rather than at any increase.
                let cost = app.cfg.search_cost_multiple();
                if cost > 1.05 {
                    let text = super::i18n::tv(app.prefs.lang, "search_cost", &[("n", &format!("{cost:.0}"))]);
                    let colour = if cost >= 4.0 { theme::ERROR() } else { theme::DIM() };
                    ui.label(RichText::new(text).color(colour).small()).on_hover_text(app.t("search_cost_tooltip"));
                }
                if ui
                    .add_enabled(!app.shapes.is_empty(), big(RichText::new(app.t("btn_run")).color(accent)))
                    .on_hover_text(super::keys::hint(app.t("btn_run_tooltip"), "Ctrl+R"))
                    .clicked()
                {
                    app.start_run();
                }
            }
        });

        if app.running {
            // Square, like every other edge in this theme - egui's default is
            // a fully rounded pill, which is the one shape the design has no
            // place for.
            //
            // **The one place this design moves.** Everything else is static
            // on purpose (`animation_time` is zero and controls do not even
            // grow under the pointer), but a nest can run for minutes with
            // the operator away from the screen, and a bar that only advances
            // when a generation lands is indistinguishable from one that has
            // stopped. The travelling highlight is what says "still working"
            // in between, and the percentage is what says how much longer.
            ui.add(
                egui::ProgressBar::new(app.progress)
                    .desired_width(240.0 * RUN_BUTTON_SCALE)
                    .corner_radius(egui::CornerRadius::ZERO)
                    .fill(theme::ACCENT())
                    .animate(true)
                    .text(RichText::new(format!("{:.0}%", app.progress * 100.0)).color(theme::TEXT()).family(theme::heavy())),
            );
            // egui repaints on input or on request only. Without this the
            // highlight advances just once per progress message - exactly the
            // stutter it exists to cover. 30fps is plenty for it and leaves
            // the GA the cores it is actually using.
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        // Deliberately outside the `if app.running` above: this is the one
        // control that is *more* useful mid-run than before it, because the
        // engine reads the flag on every part it places. Turning it off part
        // way through a long job stops the cost immediately without
        // restarting anything, and turning it on shows the next part to land.
        let mut live = app.prefs.live_view;
        if ui.checkbox(&mut live, RichText::new(app.t("live_view")).color(theme::TEXT())).on_hover_text(app.t("live_view_hint")).changed() {
            app.set_live_view(live);
        }
        // Same reasoning as the live-view checkbox above for living outside
        // the `if app.running` block: this is read when the run *ends*, so
        // changing it mid-run still takes effect for the run in progress.
        let mut chime = app.prefs.sound_on_finish;
        if ui.checkbox(&mut chime, RichText::new(app.t("sound_on_finish")).color(theme::TEXT())).on_hover_text(app.t("sound_on_finish_hint")).changed() {
            app.prefs.sound_on_finish = chime;
        }
        if app.cfg.mirror {
            ui.label(RichText::new(app.t("mirror_run_warning")).color(theme::ERROR()).small());
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
        ui.label(RichText::new(app.t("help_title")).strong().family(theme::heavy()).size(18.0).color(theme::ACCENT()));
        ui.add_space(6.0);
        // Its own row rather than right-aligned beside the title: nine
        // endonyms are wider than the title row has left over, and the
        // overflow silently drew them on top of each other.
        lang_picker(app, ctx, ui);
        ui.add_space(8.0);
        ui.label(app.t("help_intro"));
        ui.add_space(8.0);
        for key in ["help_step_import", "help_step_roles", "help_step_configure", "help_step_run"] {
            ui.label(app.t(key));
        }
        ui.add_space(12.0);
        ui.label(RichText::new(app.t("help_keys_title")).color(theme::ACCENT()).strong().family(theme::heavy()));
        ui.add_space(4.0);
        egui::Grid::new("help_keys").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            for binding in super::keys::BINDINGS {
                ui.label(RichText::new(binding.keys).strong().family(theme::heavy()));
                ui.label(RichText::new(app.t(binding.description_key)).color(theme::DIM()));
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new(app.t("help_tip")).color(theme::DIM()));
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
        ui.add_space(10.0);
        // Not an i18n key: a name, a year and a domain are the same in every
        // language, and routing them through the dictionary would only create
        // two copies to keep in sync.
        ui.label(RichText::new("dev by Cedric Florentin 2026").color(theme::DIM()).small());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new("By and For Upset Climbing").color(theme::DIM()).small());
            ui.hyperlink_to(RichText::new("upsetclimbing.com").small(), "https://upsetclimbing.com");
        });
    });
    if close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.help_open = false;
    }
}

/// Measure for a help bubble, in points before `text_scale`. Wide enough
/// that a 400-character explanation is four or five lines rather than a
/// column, narrow enough to stay inside a readable line length - egui's
/// plain `on_hover_text` gives neither, since it sizes itself to whatever
/// the longest unbroken run of text happens to be.
const HELP_WIDTH: f32 = 380.0;

/// The hover bubble every config option gets: the option's own name as a
/// heading, a rule, then the explanation.
///
/// **Attach this to real widget responses, never to a `ui.horizontal(..)
/// .response`.** Every config row used to hang its `on_hover_text` on the
/// row container, and not one of those tooltips ever appeared - verified
/// against the running app, hovering both the label and the control. A
/// container response does not win the hover its children are sitting on, so
/// the explanation was unreachable however long you rested on the row. Union
/// the label's response with the control's instead (`number_row` below), so
/// the whole row is live.
///
/// The heading earns its place because the bubble can cover the label it
/// describes, and the fixed measure keeps 200-400 characters of real operator
/// guidance ("set 0 if your CAM already accounts for it") readable rather than
/// dumping it as one unstructured blob at whatever width egui picks.
pub fn help_bubble(response: egui::Response, title: &str, body: &str) -> egui::Response {
    response.on_hover_ui(|ui| {
        ui.set_max_width(HELP_WIDTH);
        ui.label(RichText::new(title).color(theme::ACCENT()).family(theme::heavy()));
        ui.separator();
        ui.label(RichText::new(body).color(theme::TEXT()));
    })
}

/// Width of a CONFIGURE row's label column, shared by `number_row`,
/// `scale_row` and the three rows in `ui::config` that build themselves (the
/// cleanup text field, the placement dropdown, the dominant-area slider).
///
/// Wide enough for the longest English label at Normal text scale
/// (`STARTING GENERATIONS`). A longer translation, or Large scale, overflows
/// it and pushes that row's controls right - preferable to truncating a label
/// the operator has to read.
pub const LABEL_W: f32 = 226.0;

/// Width of a value field. Fixed rather than fitted to its digits, so the
/// scale controls beside the four escalating options land in a column instead
/// of following `0.00` and `2` to different x on every row.
pub const VALUE_W: f32 = 66.0;

/// A labelled numeric field, the shape almost every config row takes.
pub fn number_row<T: egui::emath::Numeric>(ui: &mut egui::Ui, label: &str, tooltip: &str, value: &mut T, speed: f64, range: std::ops::RangeInclusive<T>) {
    let row = ui
        .horizontal(|ui| {
            let name = ui.add_sized([LABEL_W, 20.0], egui::Label::new(RichText::new(label).color(theme::DIM())));
            let field = ui.add_sized([VALUE_W, 20.0], number(value, speed, range));
            name.union(field)
        })
        .inner;
    help_bubble(row, label, tooltip);
}

/// A sliding on/off switch.
///
/// egui ships a checkbox and nothing else, and a tick beside a tick (the
/// mirror row already has one) reads as two of the same control doing
/// unrelated things. A switch says "this mode is on" where a tick says "I
/// selected this", which is what the four scaling rows actually mean.
pub fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(32.0, 16.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        // Animated, so the knob visibly travels: the state changed *because
        // you clicked it* is worth half a frame of motion, and a switch that
        // teleports reads as a redraw glitch.
        let how = ui.ctx().animate_bool_responsive(response.id, *on);
        let radius = rect.height() / 2.0;
        let track = if *on { theme::ACCENT() } else { theme::DIM().gamma_multiply(0.4) };
        ui.painter().rect_filled(rect, radius, track);
        let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how);
        ui.painter().circle_filled(egui::pos2(cx, rect.center().y), radius - 2.5, theme::TEXT());
    }
    response
}

/// A `number_row` for one of the four options the run escalation may grow:
/// the value, how much it gains per run, and the switch that turns that
/// growth on. See `dto::RunScales` for why only four options have this.
#[allow(clippy::too_many_arguments)]
pub fn scale_row<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    tooltip: &str,
    step_tooltip: &str,
    switch_tooltip: &str,
    value: &mut T,
    speed: f64,
    range: std::ops::RangeInclusive<T>,
    scale: &mut crate::dto::RunScale,
) {
    let row = ui
        .horizontal(|ui| {
            let name = ui.add_sized([LABEL_W, 20.0], egui::Label::new(RichText::new(label).color(theme::DIM())));
            let field = ui.add_sized([VALUE_W, 20.0], number(value, speed, range));

            // Greyed rather than hidden while the switch is off: the number is
            // still there and comes straight back, so switching off is not the
            // same as losing what you dialled in.
            ui.add_enabled_ui(scale.on, |ui| {
                // A `u32`, so the widget cannot offer a fraction in the first
                // place: three of the four knobs this grows are counts, and
                // "+1.5 rotations" is not a thing.
                ui.add_sized([54.0, 20.0], egui::DragValue::new(&mut scale.step).speed(0.1).range(0..=100).prefix("+"))
            })
            .inner
            .on_hover_text(step_tooltip);
            toggle_switch(ui, &mut scale.on).on_hover_text(switch_tooltip);

            name.union(field)
        })
        .inner;
    help_bubble(row, label, tooltip);
}

/// A dropdown over a fixed set of variants, each labelled through `t()`.
///
/// Returns the closed combo's own response so a caller can hang a
/// `help_bubble` on it - see that function for why the surrounding row's
/// response cannot carry one.
pub fn choice<T: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, current: &mut T, options: &[T], label_of: impl Fn(T) -> String) -> egui::Response {
    egui::ComboBox::from_id_salt(id)
        .selected_text(label_of(*current))
        .show_ui(ui, |ui| {
            for &option in options {
                ui.selectable_value(current, option, label_of(option));
            }
        })
        .response
}

/// Text drawn in the accent colour, for the one-off places that need it.
pub fn accent(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(theme::ACCENT())
}

/// Every numeric field in the app, so that typing `120*3` or `1200/8` into
/// one works everywhere rather than in whichever one got the treatment.
///
/// `custom_parser` replaces egui's own parse outright, so `eval` has to
/// accept a bare number too - it does.
pub fn number<'a, T: egui::emath::Numeric>(value: &'a mut T, speed: f64, range: std::ops::RangeInclusive<T>) -> egui::DragValue<'a> {
    egui::DragValue::new(value).speed(speed).range(range).custom_parser(eval)
}

/// `+ - * /`, parentheses and unary minus, left-to-right with the usual
/// precedence. Returns `None` on anything it does not understand, which is
/// what `DragValue` wants to hear to keep the old value.
///
// ponytail: recursive descent over chars, no tokenizer struct. Add one if
// this ever needs functions or units.
pub fn eval(text: &str) -> Option<f64> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut at = 0;
    let value = expr(&chars, &mut at)?;
    (at == chars.len() && value.is_finite()).then_some(value)
}

/// Whitespace is skipped between tokens but never inside one, so `2 3` is
/// two numbers with no operator (rejected) rather than twenty-three.
fn skip_ws(c: &[char], at: &mut usize) {
    while matches!(c.get(*at), Some(ch) if ch.is_whitespace()) {
        *at += 1;
    }
}

fn expr(c: &[char], at: &mut usize) -> Option<f64> {
    let mut left = term(c, at)?;
    skip_ws(c, at);
    while let Some(&op @ ('+' | '-')) = c.get(*at) {
        *at += 1;
        let right = term(c, at)?;
        skip_ws(c, at);
        left = if op == '+' { left + right } else { left - right };
    }
    Some(left)
}

fn term(c: &[char], at: &mut usize) -> Option<f64> {
    let mut left = atom(c, at)?;
    skip_ws(c, at);
    while let Some(&op @ ('*' | '/' | 'x')) = c.get(*at) {
        *at += 1;
        let right = atom(c, at)?;
        skip_ws(c, at);
        left = if op == '*' || op == 'x' { left * right } else { left / right };
    }
    Some(left)
}

fn atom(c: &[char], at: &mut usize) -> Option<f64> {
    skip_ws(c, at);
    match c.get(*at)? {
        '-' => {
            *at += 1;
            Some(-atom(c, at)?)
        }
        '+' => {
            *at += 1;
            atom(c, at)
        }
        '(' => {
            *at += 1;
            let inner = expr(c, at)?;
            skip_ws(c, at);
            (c.get(*at) == Some(&')')).then(|| *at += 1)?;
            Some(inner)
        }
        _ => {
            let start = *at;
            while matches!(c.get(*at), Some(d) if d.is_ascii_digit() || *d == '.' || *d == ',') {
                *at += 1;
            }
            (*at > start).then_some(())?;
            // A comma is a decimal separator in half the languages this app
            // ships in, and a thousands separator in the other half. Only the
            // former can be meant here - `1,5` has to be 1.5, not 15.
            c[start..*at].iter().collect::<String>().replace(',', ".").parse().ok()
        }
    }
}

#[cfg(test)]
mod eval_tests {
    use super::eval;

    #[test]
    fn arithmetic_and_rejections() {
        assert_eq!(eval("12"), Some(12.0));
        assert_eq!(eval("1,5"), Some(1.5));
        assert_eq!(eval("120 * 3"), Some(360.0));
        assert_eq!(eval("1200/8"), Some(150.0));
        assert_eq!(eval("2+3*4"), Some(14.0));
        assert_eq!(eval("(2+3)*4"), Some(20.0));
        assert_eq!(eval("-4+10"), Some(6.0));
        assert_eq!(eval("10x3"), Some(30.0));
        assert_eq!(eval("1/0"), None);
        assert_eq!(eval("2+"), None);
        assert_eq!(eval("2 3"), None);
        assert_eq!(eval("(2+3"), None);
        assert_eq!(eval("abc"), None);
    }
}
