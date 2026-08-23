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
//!
//! The type is JetBrains Mono, embedded. See `install_fonts` for why it
//! replaced a Consolas read off `C:\Windows\Fonts`.

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

/// Applies the palette, widget styling and text metrics.
///
/// `fonts_ready` is false only for the call from `App::new`, which happens
/// before egui has a font atlas - see the button-metrics block below.
pub fn apply(ctx: &egui::Context, text_scale: f32, fonts_ready: bool) {
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
    // The button label's real laid-out row height, asked of the font rather
    // than assumed. This used to be a hardcoded ratio measured off the
    // rendered UI, which was correct only for the face it was measured on -
    // and guessing it instead was once wrong by 8%, turning a 10% bigger
    // button into a 20% bigger one.
    //
    // `apply` also runs from `App::new`, before the first frame, where egui
    // has no atlas yet and `ctx.fonts` panics with "No fonts available until
    // first call to Context::run()". `fonts_ready` is that distinction; the
    // fallback is JetBrains Mono's own (ascent - descent) / units_per_em, and
    // `App::update` re-applies on its first frame with the measured value
    // before anything is drawn.
    const JETBRAINS_ROW_RATIO: f32 = 1.320;
    let button_font = egui::FontId::new(12.5 * text_scale, egui::FontFamily::Monospace);
    let line = if fonts_ready { ctx.fonts(|f| f.row_height(&button_font)) } else { 12.5 * text_scale * JETBRAINS_ROW_RATIO };
    // egui's default box is the line plus 2 * button_padding.y, floored by
    // interact_size.y. Growing both by the same tenth grows the rendered
    // button by exactly a tenth whichever of the two is binding.
    let base = (line + 2.0).max(18.0);
    let grown = base * BUTTON_GROWTH;
    style.spacing.button_padding = egui::vec2(4.0 + (grown - base) / 2.0, (grown - line) / 2.0);
    style.spacing.interact_size.y = grown;

    ctx.set_style(style);
}

/// Installs JetBrains Mono, embedded in the binary, as the whole UI's type.
///
/// **Why embedded rather than read off the system.** This used to load
/// Consolas from `C:\Windows\Fonts\consola.ttf`, because egui's built-in
/// monospace (Hack) has no Vietnamese coverage and every diacritic in the
/// `vi` dictionary rendered as a fallback box. That worked on Windows and
/// quietly failed everywhere else: the mac and linux binaries fell through to
/// Hack, so they shipped a different typeface *and* an unreadable second
/// language. Since Vietnamese is a product requirement, not a locale demo,
/// the face has to be in the binary. ~540KB for both weights.
///
/// **Why this face.** Its vertical metrics are drawn around its own
/// Latin-Extended coverage, which is exactly what Consolas' are not:
///
/// | | units (em = 1000) |
/// |---|---|
/// | tallest Vietnamese cap stack (`Ậ Ộ Ề`) | 1020 |
/// | hhea ascent | 1020 |
/// | deepest descender / dot-below (`Ậ Ợ g y`) | -213 |
/// | hhea descent | -300 |
/// | cap height | 730, leaving 290 above and 300 below |
///
/// Two things fall out of that table. The line box already clears every mark
/// Vietnamese can stack on a capital, and caps already sit optically centred
/// in it - so the `FontTweak { y_offset_factor: 0.192 }` that used to nudge
/// Consolas' glyphs down is gone rather than retuned. That tweak was derived
/// on the premise that "every label in this UI is uppercase, so no glyph ever
/// reaches the descender", which `Ậ`'s dot below falsifies; keeping it and
/// picking a smaller number would have preserved a correction the new face
/// does not need.
///
/// Call once at startup, not from `apply`: `ctx.set_fonts` throws away and
/// rebuilds the whole font atlas, while `apply` reruns on every TEXT SIZE
/// change. Nothing here depends on the text scale.
pub fn install_fonts(ctx: &egui::Context) {
    const REGULAR: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
    const BOLD: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf");

    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [("jetbrains", REGULAR), ("jetbrains_bold", BOLD)] {
        fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
    }
    // Front of both families: monospace is what the UI actually uses, and
    // proportional is what egui falls back to for anything that slips
    // through. The built-ins stay behind it for glyphs this face lacks.
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts.families.entry(family).or_default().insert(0, "jetbrains".to_owned());
    }

    // A real bold face, as its own family.
    //
    // `RichText::strong()` only swaps the *colour* in egui - it does not
    // reach for a heavier weight, because a weight has to be a separate
    // loaded font and nothing here loaded one. So every "strong" label in
    // this UI - the wordmark, the step numbers, RUN NEST - would be the same
    // stroke thickness as body text. `heavy()` below is the family that
    // actually is bold.
    fonts.families.insert(heavy(), vec!["jetbrains_bold".to_owned(), "jetbrains".to_owned()]);

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
