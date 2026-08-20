//! Palette and widget styling: brutalist, neo-futuristic, industrial.
//!
//! Five colours and nothing else: black, white, one blue-grey ground
//! (`#131c24`), one accent (`#f4833f`), one danger (`#f94a21`). Every surface
//! is flat and every edge is a drawn hairline - no bevels, no simulated light
//! source, no shadows, no corner radius, no animation. The tell of this look
//! is the hard 1px rule and a signal colour used sparingly enough to still
//! mean something.
//!
//! The accent is fixed. It used to be user-chosen from five swatches plus a
//! free hex field, which meant the app had no colour of its own - every
//! screenshot was somebody else's palette. One accent, always this one, is
//! the identity.

use egui::Color32;

/// The ground ramp, darkest to lightest, built from `PANEL` toward black
/// below it and toward white above. Four steps is deliberate: window ground,
/// panel, control face, hovered face. A fifth would only register as noise at
/// these values.
pub const BG: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x13);
pub const PANEL: Color32 = Color32::from_rgb(0x13, 0x1c, 0x24);
pub const FACE: Color32 = Color32::from_rgb(0x1c, 0x28, 0x33);
pub const FACE_HI: Color32 = Color32::from_rgb(0x26, 0x34, 0x3f);

/// Below the ramp, not on it: the ground behind *drawn* content - text
/// fields, the drop target, thumbnails, the sheet canvas. Pure black, so a
/// part outline reads at full contrast wherever it is drawn.
pub const WELL: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);

/// Structure. `LINE` is the default hairline around every control and panel;
/// `LINE_STRONG` is for edges that separate rather than merely contain
/// (window edges, open dropdowns).
pub const LINE: Color32 = Color32::from_rgb(0x33, 0x43, 0x4f);
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x4a, 0x5e, 0x6d);

pub const TEXT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const DIM: Color32 = Color32::from_rgb(0x8b, 0x98, 0xa3);

/// The accent, and the only one there is. Every "forward progress" control
/// (Browse -> Run -> Export, each step heading's number, the sigma in the
/// wordmark) is drawn in it.
pub const ACCENT: Color32 = Color32::from_rgb(0xf4, 0x83, 0x3f);

/// The halt/destructive signal - Stop, Remove Selected, unplaced parts.
///
/// Deliberately close to `ACCENT` in hue: this palette has no red, and a
/// borrowed one would be the sixth colour in a five-colour system. It
/// separates on saturation and darkness rather than hue, which is enough at
/// the sizes it is used - always on a short word, never on a large fill.
pub const ERROR: Color32 = Color32::from_rgb(0xf9, 0x4a, 0x21);

/// Success/confirmation. The accent, not a green: green is not in this
/// palette, and white is already ordinary text, so a confirmation drawn in it
/// would not read as a signal at all.
pub const OK: Color32 = ACCENT;

pub fn apply(ctx: &egui::Context, text_scale: f32) {
    let mut style = (*ctx.style()).clone();
    style.animation_time = 0.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = WELL;
    v.faint_bg_color = PANEL;
    // Deliberately *not* `override_text_color`: that forces one colour on
    // every widget in every state, so a control can never darken its own
    // label against a light fill. Each state sets its own `fg_stroke`
    // instead, and explicit `RichText::color` calls still win over both.
    v.override_text_color = None;
    v.hyperlink_color = ACCENT;
    v.error_fg_color = ERROR;
    v.warn_fg_color = ACCENT;
    v.window_stroke = egui::Stroke::new(1.0_f32, LINE_STRONG);
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    // egui gives the frontmost window a brighter edge than the rest. Two
    // windows with different border colours is a light source by another
    // name; every edge here is the same weight regardless of stacking.
    v.window_highlight_topmost = false;
    // Selection: accent ground, window ground for the glyphs. The accent is
    // a light colour, so light-on-light has to be broken here rather than
    // hoped away.
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = egui::Stroke::new(1.0_f32, BG);

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::ZERO;
        // No grow-on-hover. A control that changes size under the pointer is
        // the opposite of this look, and it also shifts the hairline.
        w.expansion = 0.0;
    }

    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

    v.widgets.inactive.bg_fill = FACE;
    v.widgets.inactive.weak_bg_fill = FACE;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

    // Hover lights the border, not the fill: the accent outlines the target
    // rather than washing it.
    v.widgets.hovered.bg_fill = FACE_HI;
    v.widgets.hovered.weak_bg_fill = FACE_HI;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

    // Pressed: the accent floods the fill and its own hairline, at a value
    // dark enough to keep `TEXT` on top of it.
    //
    // `fg_stroke` here must stay `TEXT`, however tempting a black-on-accent
    // inversion looks: egui derives `Visuals::strong_text_color()` from
    // `widgets.active`, so anything set here also repaints every `.strong()`
    // label in the app - the wordmark and all four step headings included.
    // That is not a press state, it is a global text colour wearing one.
    v.widgets.active.bg_fill = ACCENT.gamma_multiply(0.55);
    v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.55);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

    v.widgets.open.bg_fill = FACE_HI;
    v.widgets.open.weak_bg_fill = FACE_HI;
    v.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, LINE_STRONG);
    v.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);

    // Everything is monospace, replacing the web UI's Courier New.
    style.text_styles.values_mut().for_each(|f| f.family = egui::FontFamily::Monospace);

    // Text size is set here, per style, rather than through
    // `ctx.set_zoom_factor` - zoom multiplies stroke widths and spacing too,
    // so the hairlines this whole look is built on thicken and the design
    // reads as a different design rather than the same one larger. The bases
    // below are egui's own defaults, so `text_scale == 1.0` reproduces the
    // original metrics exactly.
    for (style_name, base) in [
        (egui::TextStyle::Small, 9.0_f32),
        (egui::TextStyle::Body, 12.5),
        (egui::TextStyle::Button, 12.5),
        (egui::TextStyle::Monospace, 12.0),
        (egui::TextStyle::Heading, 18.0),
    ] {
        if let Some(font) = style.text_styles.get_mut(&style_name) {
            font.size = base * text_scale;
        }
    }

    // Buttons, 10% larger than egui's default box.
    //
    // The default padding is 4x1 around the label, so the box is almost
    // entirely text - scaling *that* by 1.1 would move each edge by well
    // under a pixel. The tenth has to come off the box height instead, split
    // between the two edges, and the same absolute amount goes on the sides.
    // Height is therefore exactly +10%; width grows by a fixed amount per
    // edge, which is more than a tenth on a short label and less on a long
    // one - unavoidable with padding-based sizing, and the right trade
    // (`RESET` and `REMOVE SELECTED` keep the same rhythm).
    //
    // Derived from the button text size rather than hardcoded so it tracks
    // the TEXT SIZE preference. Label centring needs no setting: a `Button`
    // shrink-wraps its label, so the padding below is symmetric by
    // construction.
    const BUTTON_GROWTH: f32 = 1.10;
    // Consolas' row height as a fraction of its nominal point size, measured
    // off the rendered UI (a 12.5pt button laid out an 18px line at TEXT
    // SIZE = 1.25). Not queried from `ctx.fonts()`, tempting as that is:
    // `apply` runs from `App::new`, before the first frame, and egui panics
    // with "No fonts available until first call to Context::run()" there.
    // Guessing 1.25 instead was wrong by 8% and turned a 10% bigger button
    // into a 20% bigger one, so the number has to be the real one.
    const ROW_HEIGHT_RATIO: f32 = 1.152;
    let line = 12.5 * text_scale * ROW_HEIGHT_RATIO;
    // egui's default box is the line plus 2 * button_padding.y, floored by
    // interact_size.y. Growing both by the same tenth grows the rendered
    // button by exactly a tenth whichever of the two is binding.
    let base = (line + 2.0).max(18.0);
    let grown = base * BUTTON_GROWTH;
    style.spacing.button_padding = egui::vec2(4.0 + (grown - base) / 2.0, (grown - line) / 2.0);
    style.spacing.interact_size.y = grown;

    ctx.set_style(style);
}

/// egui's built-in monospace (Hack) has no Vietnamese coverage: every
/// diacritic in the `vi` dictionary renders as a fallback box, which makes
/// the whole second language unreadable rather than merely ugly.
///
/// Consolas ships with every Windows install, is monospace, and does cover
/// Vietnamese - so read it off disk rather than embedding a ~700KB face into
/// the binary for one script. If it is ever missing, fall back silently to
/// the built-in font: an app that starts with slightly wrong glyphs beats an
/// app that refuses to start.
///
/// Call once at startup, not from `apply`: `ctx.set_fonts` throws away and
/// rebuilds the whole font atlas, while `apply` reruns on every TEXT SIZE
/// change. Nothing here depends on the text scale anyway - `CAP_CENTRING` is
/// a *factor*, so it tracks the size on its own.
pub fn install_fonts(ctx: &egui::Context) {
    const CONSOLAS: &str = r"C:\Windows\Fonts\consola.ttf";
    const CONSOLAS_BOLD: &str = r"C:\Windows\Fonts\consolab.ttf";
    let consolas_bytes = std::fs::read(CONSOLAS);

    // Nudge the glyphs down inside their line box.
    //
    // Every label in this UI is uppercase, so no glyph ever reaches the
    // descender - but the line box still reserves room for one. Measured on
    // a 22px button: 2px of space above the caps and 8px below, which reads
    // as text stuck to the top of the button however symmetric the padding
    // is. Half that 6px difference, as a fraction of the font size
    // (3 / 15.625), re-centres the caps optically.
    //
    // `y_offset_factor` rather than `y_offset` so it tracks the TEXT SIZE
    // preference: the gap it corrects is proportional to the font size.
    // Visual only - it does not move the layout, so nothing reflows.
    const CAP_CENTRING: f32 = 0.192;

    let mut fonts = egui::FontDefinitions::default();
    // `heavy()` is referenced all over the UI and epaint panics outright on a
    // family bound to no fonts, so bind it to the built-in monospace stack
    // before the missing-Consolas bail-out below can return.
    let builtin_mono = fonts.families[&egui::FontFamily::Monospace].clone();
    fonts.families.insert(heavy(), builtin_mono);

    let Ok(bytes) = consolas_bytes else {
        ctx.set_fonts(fonts);
        return;
    };
    let consolas = egui::FontData::from_owned(bytes).tweak(egui::FontTweak { y_offset_factor: CAP_CENTRING, ..Default::default() });
    fonts.font_data.insert("consolas".to_owned(), std::sync::Arc::new(consolas));
    // Front of both families: monospace is what the UI actually uses, and
    // proportional is what egui falls back to for anything that slips
    // through. The built-ins stay behind it as a further fallback for glyphs
    // Consolas itself lacks.
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts.families.entry(family).or_default().insert(0, "consolas".to_owned());
    }

    // A real bold face, as its own family.
    //
    // `RichText::strong()` only swaps the *colour* in egui - it does not
    // reach for a heavier weight, because a weight has to be a separate
    // loaded font and nothing here loaded one. So every "strong" label in
    // this UI - the wordmark, the step numbers, RUN NEST - has been the same
    // stroke thickness as body text all along, which is not what an accent
    // is for. `heavy()` below is the family that actually is bold.
    //
    // Falls back to the regular face if `consolab.ttf` is missing, so the
    // family always resolves to something: mildly wrong weight beats
    // tofu boxes.
    let regular_first = vec!["consolas".to_owned()];
    let bold_stack = match std::fs::read(CONSOLAS_BOLD) {
        Ok(bold) => {
            fonts.font_data.insert("consolas_bold".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bold).tweak(egui::FontTweak { y_offset_factor: CAP_CENTRING, ..Default::default() })));
            vec!["consolas_bold".to_owned(), "consolas".to_owned()]
        }
        Err(_) => regular_first,
    };
    fonts.families.insert(heavy(), bold_stack);

    ctx.set_fonts(fonts);
}

/// The bold family - see `install_fonts`. Use it wherever `.strong()` was
/// meant to make something *look* heavier rather than merely brighter.
#[must_use]
pub fn heavy() -> egui::FontFamily {
    egui::FontFamily::Name("heavy".into())
}


/// One hard border, drawn inside the rect so it never overlaps a neighbour.
///
/// This replaces the old `bevel()`: an edge here is a line someone drew, not
/// a light source someone simulated. Width is a parameter because the step
/// from "this box contains things" (1px) to "this thing is selected" (2px)
/// is the only weight hierarchy the look has.
pub fn hairline(painter: &egui::Painter, rect: egui::Rect, color: Color32, width: f32) {
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(width, color), egui::StrokeKind::Inside);
}
