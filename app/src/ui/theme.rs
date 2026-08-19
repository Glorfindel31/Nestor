//! Palette and widget styling: rusty dark-orange, brutal industrial, Win95
//! revisited. Square corners, chiselled bevels, no shadows, no animation.

use egui::Color32;

pub const BG: Color32 = Color32::from_rgb(0x14, 0x11, 0x10);
pub const PANEL: Color32 = Color32::from_rgb(0x1e, 0x1a, 0x17);
pub const FACE: Color32 = Color32::from_rgb(0x2b, 0x25, 0x21);
pub const BEVEL_HI: Color32 = Color32::from_rgb(0x5a, 0x4a, 0x40);
pub const BEVEL_LO: Color32 = Color32::from_rgb(0x0a, 0x08, 0x07);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe2, 0xdc);
pub const DIM: Color32 = Color32::from_rgb(0x9a, 0x8b, 0x80);
/// Default accent. Every "forward progress" control (Browse -> Run ->
/// Export, and each step heading's number) is drawn in whatever accent is
/// currently chosen; `ERROR` below marks the destructive/halt ones (Stop,
/// Remove Selected, unplaced parts). A deliberate two-colour system rather
/// than one accent on a single button.
pub const ACCENT: Color32 = Color32::from_rgb(0xc8, 0x5a, 0x1b);

/// Quick-pick swatches for the accent picker - shortcuts, not the only
/// allowed values: the hex field accepts any valid colour. All within the
/// oxide/rust family the rest of the palette is built around.
pub const ACCENTS: [Color32; 5] = [
    Color32::from_rgb(0xc8, 0x5a, 0x1b),
    Color32::from_rgb(0xe8, 0x7a, 0x2e),
    Color32::from_rgb(0xb7, 0x41, 0x0e),
    Color32::from_rgb(0xd9, 0x90, 0x58),
    Color32::from_rgb(0x8c, 0x3a, 0x10),
];
pub const ERROR: Color32 = Color32::from_rgb(0xd8, 0x34, 0x2a);

pub fn apply(ctx: &egui::Context, accent: Color32) {
    let mut style = (*ctx.style()).clone();
    style.animation_time = 0.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = BEVEL_LO;
    v.faint_bg_color = PANEL;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = accent;
    v.error_fg_color = ERROR;
    v.warn_fg_color = accent;
    v.window_stroke = egui::Stroke::new(1.0_f32, BEVEL_HI);
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    // Selection (selectable labels, highlighted text) defaults to egui's
    // blue, which is the one colour this palette has no place for.
    v.selection.bg_fill = accent.gamma_multiply(0.6);
    v.selection.stroke = egui::Stroke::new(1.0_f32, TEXT);

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::ZERO;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BEVEL_HI);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, DIM);
    v.widgets.inactive.bg_fill = FACE;
    v.widgets.inactive.weak_bg_fill = FACE;
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.bg_fill = accent.gamma_multiply(0.35);
    v.widgets.hovered.weak_bg_fill = accent.gamma_multiply(0.35);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, accent);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    v.widgets.active.bg_fill = accent.gamma_multiply(0.55);
    v.widgets.active.weak_bg_fill = accent.gamma_multiply(0.55);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, accent);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, TEXT);
    v.widgets.open.bg_fill = FACE;
    v.widgets.open.weak_bg_fill = FACE;

    // Everything is monospace, replacing the web UI's Courier New.
    style.text_styles.values_mut().for_each(|f| f.family = egui::FontFamily::Monospace);

    ctx.set_style(style);
    install_fonts(ctx);
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
fn install_fonts(ctx: &egui::Context) {
    const CONSOLAS: &str = r"C:\Windows\Fonts\consola.ttf";
    let Ok(bytes) = std::fs::read(CONSOLAS) else { return };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("consolas".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    // Front of both families: monospace is what the UI actually uses, and
    // proportional is what egui falls back to for anything that slips
    // through. The built-ins stay behind it as a further fallback for
    // glyphs Consolas itself lacks.
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        fonts.families.entry(family).or_default().insert(0, "consolas".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Win95's one unmistakable tell: a 2px chiselled edge, light on the
/// top/left and dark on the bottom/right for a raised face, swapped for an
/// inset (pressed button, text field, sunken panel).
///
/// Two L-shaped polylines rather than four separate segments so the corner
/// pixels join cleanly at any zoom factor.
pub fn bevel(painter: &egui::Painter, rect: egui::Rect, raised: bool) {
    let (hi, lo) = if raised { (BEVEL_HI, BEVEL_LO) } else { (BEVEL_LO, BEVEL_HI) };
    let stroke = |c| egui::Stroke::new(2.0_f32, c);
    painter.add(egui::Shape::line(vec![rect.left_bottom(), rect.left_top(), rect.right_top()], stroke(hi)));
    painter.add(egui::Shape::line(vec![rect.right_top(), rect.right_bottom(), rect.left_bottom()], stroke(lo)));
}
