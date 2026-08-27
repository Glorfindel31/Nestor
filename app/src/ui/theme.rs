//! Six visual worlds: palette, type, edges and motion.
//!
//! **OXIDE is the default and the product's identity** - brutalist,
//! neo-futuristic, industrial. Five colours and nothing else: black, white,
//! one blue-grey ground, one accent, one danger. Every surface flat, every
//! edge a drawn hairline, no simulated light source, no animation. The tell
//! of that look is the hard 1px rule and a signal colour used sparingly
//! enough to still mean something.
//!
//! The other five - MATRIX, TERMINATOR, KAWAII, FALLOUT, CYBERPUNK - are
//! complete worlds rather than tints. Each brings its own typeface, its own
//! edge treatment (`FrameStyle`), its own motion (`ui::effects`) and its own
//! way of colouring parts on the sheet (`CanvasStyle`).
//!
//! **What does not change is the structure.** All six fill the identical set
//! of colour roles, so no theme can quietly become less legible than
//! another, and none of them moves a single widget - this module supplies
//! colours, metrics and edge shapes, never layout.
//!
//! The accent within a world is still fixed. It used to be user-chosen from
//! five swatches plus a free hex field, which meant the app had no colour of
//! its own - every screenshot was somebody else's palette. Choosing a whole
//! coherent world is a different thing from tinting one control.
//!
//! Type is embedded, never read off the system, and every face carries the
//! full Vietnamese repertoire - see `install_fonts` for why that requirement
//! ruled out most of the obvious genre faces.

use super::i18n::Lang;
use egui::Color32;
use std::sync::atomic::{AtomicUsize, Ordering};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Which visual world the app is wearing.
///
/// OXIDE is the original and stays the default - it is the identity every
/// screenshot and the wordmark were built around. The other five are complete
/// worlds, not tints: each brings its own type, edge treatment and motion, so
/// switching is a change of character rather than a change of hue.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Theme {
    #[default]
    Oxide,
    Matrix,
    Terminator,
    Kawaii,
    Fallout,
    Cyberpunk,
}

impl Theme {
    /// Every theme, in picker order.
    pub const ALL: [Theme; 6] = [Theme::Oxide, Theme::Matrix, Theme::Terminator, Theme::Kawaii, Theme::Fallout, Theme::Cyberpunk];

    /// Shown in the picker. Proper nouns, deliberately untranslated.
    ///
    /// The variant is still `Oxide` while the label reads NESTOR - the product's
    /// own name, for the world that is its own identity. The mismatch is
    /// deliberate: serde writes the *variant* name into the saved
    /// preferences, so renaming it would make every existing user's stored
    /// theme unreadable and silently reset them. The label is what people
    /// see; the variant is a storage key.
    ///
    /// KAWAII is the pastel world. It is *not* named after the character
    /// franchise that inspired it, and ships none of that franchise's marks,
    /// artwork or likeness - a borrowed trademark is a legal problem, and the
    /// aesthetic itself (soft pink, cream, rounded, bouncy) is nobody's
    /// property.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Theme::Oxide => "NESTOR",

            Theme::Matrix => "MATRIX",
            Theme::Terminator => "TERMINATOR",
            Theme::Kawaii => "KAWAII",
            Theme::Fallout => "FALLOUT",
            Theme::Cyberpunk => "CYBERPUNK",
        }
    }
}

/// How an edge is drawn. `hairline` reads this, so no call site changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameStyle {
    /// One hard 1px rule. The original.
    Hard,
    /// Corner ticks only - a HUD bracket rather than a box.
    Bracket,
    /// Rounded and heavier. The only style with a corner radius.
    Soft,
    /// A second rule inset inside the first, reading as a CRT bezel.
    Bezel,
    /// One corner cut away.
    Chamfer,
}

/// How parts are coloured on the sheet.
///
/// Two strategies, because the six worlds split cleanly in two. A palette
/// built from a single phosphor cannot host three unrelated hues without one
/// of them being a foreign object on the screen; a palette that already spans
/// the wheel can.
#[derive(Clone, Copy, Debug)]
pub enum CanvasStyle {
    /// cut/drill/etch as three hues, everything else hashed into a hue that
    /// never lands in `reserved` - the bands the chrome itself owns, so a
    /// part outline can never accidentally say "press this" or "this is
    /// wrong".
    Hue { reserved: &'static [(f32, f32)], sat: f32, val: f32 },
    /// cut/drill/etch as three brightnesses of one colour. Unrecognised
    /// layers hash into the dimmer steps below those.
    Mono { base: Color32 },
}

/// One complete visual world.
///
/// The colour roles are identical across all six by design: every one has
/// exactly one accent, one danger signal and one four-step ground ramp, so no
/// theme can quietly become less legible than another. What changes is which
/// colours fill those roles, the type, the edges and the motion.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub face: Color32,
    pub face_hi: Color32,
    pub well: Color32,
    pub line: Color32,
    pub line_strong: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub error: Color32,
    pub ok: Color32,
    /// Drives `Visuals::dark_mode`, which egui uses to derive a handful of
    /// colours this struct does not name. KAWAII is the one light world.
    pub dark: bool,
    /// Family keys registered by `install_fonts`. Where a family ships only
    /// one weight the two are the same name - see `install_fonts`.
    pub font: &'static str,
    pub font_bold: &'static str,
    /// This face's own (ascent - descent) / units_per_em, for the single
    /// `apply` call that happens before egui has a font atlas.
    ///
    /// Per-theme because it is a property of the face: the JetBrains number
    /// is simply wrong for any other, and guessing this ratio was once off by
    /// 8%, which turned a 10%-bigger button into a 20%-bigger one.
    pub row_ratio: f32,
    pub frame: FrameStyle,
    /// `Style::animation_time`. Zero everywhere the world is meant to be
    /// static; only KAWAII genuinely wants easing on its controls.
    pub animation: f32,
    pub canvas: CanvasStyle,
}

/// The chrome's own hue band in OXIDE: `ACCENT` sits at 22.5 degrees and
/// `ERROR` at 11.4. Each theme names its own - see `CanvasStyle::Hue`.
const OXIDE_RESERVED: &[(f32, f32)] = &[(4.0, 40.0)];
/// CYBERPUNK is the one palette with two signal hues far apart: the yellow
/// primary at ~57 degrees and the magenta-red danger at ~346.
/// Wide on the yellow side: measured on screen, a part outline generated at
/// 70 degrees still reads as *the accent* next to a 57-degree chrome, because
/// the eye names both "yellow" long before it separates them.
const CYBERPUNK_RESERVED: &[(f32, f32)] = &[(40.0, 88.0), (338.0, 355.0)];

/// KAWAII puts bubblegum at ~336 degrees.
const KAWAII_RESERVED: &[(f32, f32)] = &[(325.0, 350.0)];

/// The six worlds, in `Theme::ALL` order.
pub const PALETTES: [Palette; 6] = [
    // OXIDE - unchanged. Brutalist, neo-futuristic, industrial: five colours,
    // every surface flat, every edge a drawn hairline.
    Palette {
        bg: rgb(0x0a, 0x0e, 0x13),
        panel: rgb(0x13, 0x1c, 0x24),
        face: rgb(0x1c, 0x28, 0x33),
        face_hi: rgb(0x26, 0x34, 0x3f),
        well: rgb(0x00, 0x00, 0x00),
        line: rgb(0x33, 0x43, 0x4f),
        line_strong: rgb(0x4a, 0x5e, 0x6d),
        text: rgb(0xff, 0xff, 0xff),
        dim: rgb(0x8b, 0x98, 0xa3),
        accent: rgb(0xf4, 0x83, 0x3f),
        // No red in this palette, so danger separates from the accent on
        // saturation and darkness rather than hue. Enough at the sizes it is
        // used - always a short word, never a large fill.
        error: rgb(0xf9, 0x4a, 0x21),
        ok: rgb(0xf4, 0x83, 0x3f),
        dark: true,
        font: "jetbrains",
        font_bold: "jetbrains_bold",
        row_ratio: 1.320,
        frame: FrameStyle::Hard,
        animation: 0.0,
        canvas: CanvasStyle::Hue { reserved: OXIDE_RESERVED, sat: 0.78, val: 1.0 },
    },
    // MATRIX - one green phosphor on absolute black. The whole table is a
    // ramp of a single hue; there is no second colour except the failure red,
    // which the films themselves use for it.
    Palette {
        bg: rgb(0x00, 0x00, 0x00),
        panel: rgb(0x02, 0x0a, 0x05),
        face: rgb(0x06, 0x17, 0x0b),
        face_hi: rgb(0x0b, 0x27, 0x13),
        well: rgb(0x00, 0x00, 0x00),
        line: rgb(0x0e, 0x3d, 0x1d),
        line_strong: rgb(0x1c, 0x6d, 0x33),
        // Not white: a phosphor tube has one colour, and text is the dimmest
        // thing it shows rather than a different substance.
        text: rgb(0xb6, 0xff, 0xc6),
        dim: rgb(0x3f, 0x8f, 0x55),
        accent: rgb(0x00, 0xff, 0x41),
        error: rgb(0xff, 0x1f, 0x3d),
        ok: rgb(0x00, 0xff, 0x41),
        dark: true,
        font: "plex",
        font_bold: "plex_bold",
        row_ratio: 1.300,
        frame: FrameStyle::Hard,
        animation: 0.0,
        canvas: CanvasStyle::Mono { base: rgb(0x00, 0xff, 0x41) },
    },
    // TERMINATOR - machine vision. Red HUD on near-black, and the one palette
    // that cannot use red for danger, because red is already the whole world;
    // warnings go HUD yellow instead.
    Palette {
        bg: rgb(0x06, 0x01, 0x01),
        panel: rgb(0x13, 0x04, 0x04),
        face: rgb(0x20, 0x07, 0x07),
        face_hi: rgb(0x2e, 0x0b, 0x0b),
        well: rgb(0x00, 0x00, 0x00),
        line: rgb(0x4c, 0x10, 0x10),
        line_strong: rgb(0x7c, 0x1d, 0x1d),
        text: rgb(0xff, 0x9d, 0x8a),
        dim: rgb(0x8c, 0x40, 0x38),
        accent: rgb(0xff, 0x2d, 0x16),
        error: rgb(0xff, 0xd4, 0x00),
        ok: rgb(0xff, 0x6b, 0x3d),
        dark: true,
        font: "chakra",
        font_bold: "chakra_bold",
        row_ratio: 1.340,
        frame: FrameStyle::Bracket,
        animation: 0.0,
        canvas: CanvasStyle::Mono { base: rgb(0xff, 0x2d, 0x16) },
    },
    // KAWAII - the one light world, and the only one with motion in its
    // controls. Milk ground, bubblegum accent, mint confirm.
    Palette {
        bg: rgb(0xff, 0xf4, 0xf8),
        panel: rgb(0xff, 0xff, 0xff),
        face: rgb(0xff, 0xe6, 0xee),
        face_hi: rgb(0xff, 0xd5, 0xe3),
        // Near-white, not black: on a light ground a black canvas would be
        // the heaviest object on screen, and the theme would read as two
        // designs stapled together.
        well: rgb(0xff, 0xfb, 0xfd),
        line: rgb(0xff, 0xc0, 0xd3),
        line_strong: rgb(0xff, 0x8d, 0xb2),
        text: rgb(0x4a, 0x2b, 0x38),
        dim: rgb(0xa8, 0x78, 0x8a),
        accent: rgb(0xff, 0x5c, 0x9e),
        error: rgb(0xff, 0x2f, 0x68),
        ok: rgb(0x4f, 0xd0, 0xbb),
        dark: false,
        font: "nunito",
        font_bold: "nunito",
        row_ratio: 1.364,
        frame: FrameStyle::Soft,
        animation: 0.12,
        // Darker and more saturated than any dark world, because these sit on
        // a near-white sheet: a value of 0.80 that reads as vivid on black is
        // a pastel smudge on milk, and a cut line the operator has to lean in
        // to see is a failure of the one job this canvas has.
        canvas: CanvasStyle::Hue { reserved: KAWAII_RESERVED, sat: 0.75, val: 0.62 },

    },
    // FALLOUT - the amber Pip-Boy, not the green one. Amber is a selectable
    // in-game variant, so it is canon, and it keeps this world from reading
    // as a warm MATRIX at a glance.
    Palette {
        bg: rgb(0x0d, 0x0b, 0x06),
        panel: rgb(0x17, 0x11, 0x0a),
        face: rgb(0x23, 0x1a, 0x0d),
        face_hi: rgb(0x32, 0x25, 0x12),
        well: rgb(0x05, 0x04, 0x00),
        line: rgb(0x4c, 0x3a, 0x14),
        line_strong: rgb(0x7d, 0x60, 0x1e),
        text: rgb(0xff, 0xb6, 0x42),
        dim: rgb(0x9c, 0x73, 0x28),
        accent: rgb(0xff, 0xc9, 0x4a),
        error: rgb(0xff, 0x5a, 0x1f),
        ok: rgb(0xff, 0xc9, 0x4a),
        dark: true,
        font: "overpass",
        font_bold: "overpass",
        row_ratio: 1.320,
        frame: FrameStyle::Bezel,
        animation: 0.0,
        canvas: CanvasStyle::Mono { base: rgb(0xff, 0xc9, 0x4a) },
    },
    // CYBERPUNK - the only three-signal palette: yellow primary, cyan
    // confirm, magenta danger. That is the world's own grammar rather than
    // decoration, so the usual one-accent rule is spent here deliberately.
    Palette {
        bg: rgb(0x07, 0x06, 0x0f),
        panel: rgb(0x10, 0x0d, 0x1d),
        face: rgb(0x18, 0x14, 0x33),
        face_hi: rgb(0x23, 0x1d, 0x48),
        well: rgb(0x00, 0x00, 0x00),
        line: rgb(0x2e, 0x24, 0x72),
        line_strong: rgb(0x4c, 0x3c, 0xaa),
        text: rgb(0xf0, 0xf0, 0xff),
        dim: rgb(0x8b, 0x86, 0xc0),
        accent: rgb(0xfc, 0xee, 0x0a),
        error: rgb(0xff, 0x00, 0x3c),
        ok: rgb(0x00, 0xf0, 0xff),
        dark: true,
        font: "saira",
        font_bold: "saira_bold",
        row_ratio: 1.290,
        frame: FrameStyle::Chamfer,
        animation: 0.0,
        canvas: CanvasStyle::Hue { reserved: CYBERPUNK_RESERVED, sat: 0.85, val: 1.0 },
    },
];

/// The active theme, as an index into `PALETTES`.
///
/// An atomic rather than anything behind a lock: every colour accessor below
/// is called many times a frame from the UI thread, and `Palette` is `Copy`
/// with no allocation anywhere in it, so a relaxed load plus an array index
/// is the whole cost of making the palette swappable.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// The live palette. Everything else in this module reads through it.
#[must_use]
pub fn palette() -> &'static Palette {
    &PALETTES[ACTIVE.load(Ordering::Relaxed) % PALETTES.len()]
}

#[must_use]
pub fn active() -> Theme {
    Theme::ALL[ACTIVE.load(Ordering::Relaxed) % Theme::ALL.len()]
}

/// Serialises the tests that switch theme.
///
/// The active palette is process-global, and `cargo test` runs a module's
/// tests in parallel on one process - so without this, one test setting
/// CYBERPUNK is read by another that had just set MATRIX, and both fail for
/// reasons neither is testing. Any test that calls `set` must hold this.
///
/// Poisoning is deliberately ignored: a panic in one theme test must not
/// cascade into every other one reporting a poisoned lock instead of its own
/// result.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Switches theme. Call `apply` and `install_fonts` afterwards - this only
/// moves the pointer, it does not restyle a live `Context`.
pub fn set(theme: Theme) {
    let index = Theme::ALL.iter().position(|t| *t == theme).unwrap_or(0);
    ACTIVE.store(index, Ordering::Relaxed);
}

// The colour roles, as functions over the live palette.
//
// Deliberately still SHOUTING_CASE. These were consts referenced by name in
// ~130 places across ten files, and every one of those call sites reads the
// same as it did before with a `()` on the end. Renaming them to snake_case
// would have been the same behavioural change wearing a diff several times
// the size, for no gain at any call site.
#[allow(non_snake_case)]
mod roles {
    use super::{palette, Color32};
    macro_rules! role {
        ($($name:ident => $field:ident),* $(,)?) => {
            $(
                #[inline]
                #[must_use]
                pub fn $name() -> Color32 {
                    palette().$field
                }
            )*
        };
    }
    role! {
        BG => bg,
        PANEL => panel,


        WELL => well,
        LINE => line,
        LINE_STRONG => line_strong,
        TEXT => text,
        DIM => dim,
        ACCENT => accent,
        ERROR => error,
        OK => ok,
    }
}
pub use roles::{ACCENT, BG, DIM, ERROR, LINE, LINE_STRONG, OK, PANEL, TEXT, WELL};
/// Applies the active palette, widget styling and text metrics.
///
/// `fonts_ready` is false only for the call from `App::new`, which happens
/// before egui has a font atlas - see the button-metrics block below.
///
/// Re-run this after `set` to restyle a live `Context`; it is cheap (it
/// clones and stores a `Style`), unlike `install_fonts`, which rebuilds the
/// glyph atlas and so is called only when the *face* changes.
pub fn apply(ctx: &egui::Context, text_scale: f32, fonts_ready: bool) {
    let p = palette();
    let mut style = (*ctx.style()).clone();
    style.animation_time = p.animation;

    let v = &mut style.visuals;
    v.dark_mode = p.dark;
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.well;
    v.faint_bg_color = p.panel;
    // Deliberately *not* `override_text_color`: that forces one colour on
    // every widget in every state, so a control can never darken its own
    // label against a light fill. Each state sets its own `fg_stroke`
    // instead, and explicit `RichText::color` calls still win over both.
    v.override_text_color = None;
    v.hyperlink_color = p.accent;
    v.error_fg_color = p.error;
    v.warn_fg_color = p.accent;
    v.window_stroke = egui::Stroke::new(1.0_f32, p.line_strong);
    // KAWAII is the only world with a light source. Everywhere else a shadow
    // would be the one simulated depth cue in a design made entirely of drawn
    // edges.
    let shadow = if p.frame == FrameStyle::Soft {
        egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: Color32::from_black_alpha(28) }
    } else {
        egui::epaint::Shadow::NONE
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    let radius = corner_radius();
    v.window_corner_radius = radius;
    v.menu_corner_radius = radius;
    // egui gives the frontmost window a brighter edge than the rest. Two
    // windows with different border colours is a light source by another
    // name; every edge here is the same weight regardless of stacking.
    v.window_highlight_topmost = false;
    // Selection: accent ground, window ground for the glyphs. On most of
    // these palettes the accent is a light colour, so light-on-light has to
    // be broken here rather than hoped away.
    v.selection.bg_fill = p.accent;
    v.selection.stroke = egui::Stroke::new(1.0_f32, p.bg);

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
        // No grow-on-hover. A control that changes size under the pointer is
        // the opposite of this look, and it also shifts the hairline.
        w.expansion = 0.0;
    }

    v.widgets.noninteractive.bg_fill = p.panel;
    v.widgets.noninteractive.weak_bg_fill = p.panel;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(stroke_width(), p.line);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, p.text);

    v.widgets.inactive.bg_fill = p.face;
    v.widgets.inactive.weak_bg_fill = p.face;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(stroke_width(), p.line);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, p.text);

    // Hover lights the border, not the fill: the accent outlines the target
    // rather than washing it.
    v.widgets.hovered.bg_fill = p.face_hi;
    v.widgets.hovered.weak_bg_fill = p.face_hi;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(stroke_width(), p.accent);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, p.text);

    // Pressed: the accent floods the fill and its own hairline, at a value
    // that keeps `text` legible on top of it.
    //
    // `fg_stroke` here must stay `p.text`, however tempting a black-on-accent
    // inversion looks: egui derives `Visuals::strong_text_color()` from
    // `widgets.active`, so anything set here also repaints every `.strong()`
    // label in the app - the wordmark and all four step headings included.
    // That is not a press state, it is a global text colour wearing one.
    //
    // On a light palette the accent has to go *darker* under the pointer, not
    // more transparent: `gamma_multiply` on a pale pink lifts it toward the
    // white ground and the pressed state disappears.
    let pressed = if p.dark { p.accent.gamma_multiply(0.55) } else { p.accent.gamma_multiply(0.82) };
    v.widgets.active.bg_fill = pressed;
    v.widgets.active.weak_bg_fill = pressed;
    v.widgets.active.bg_stroke = egui::Stroke::new(stroke_width(), p.accent);
    // `p.text`, and nothing cleverer. The comment above is not decoration:
    // setting this to white for the light palette - which looked like the
    // obvious contrast fix against a saturated pink fill - turned every
    // `.strong()` label in KAWAII white-on-white, so the wordmark, all four
    // step headings and every stat value vanished from the window. It is a
    // global text colour wearing a press state, exactly as documented.
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, p.text);


    v.widgets.open.bg_fill = p.face_hi;
    v.widgets.open.weak_bg_fill = p.face_hi;
    v.widgets.open.bg_stroke = egui::Stroke::new(stroke_width(), p.line_strong);
    v.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, p.text);

    // One family for the whole UI, whichever face this world uses. Monospace
    // is what every text style resolves to - the layout was built on
    // fixed-width columns and a proportional face would reflow it - so the
    // proportional faces below (KAWAII, TERMINATOR, CYBERPUNK) are registered
    // under the monospace family too rather than being a second style.
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
    // button into a 20% bigger one. `Palette::row_ratio` is the per-face
    // fallback for the one call that happens before there is an atlas to ask.
    let button_font = egui::FontId::new(12.5 * text_scale, egui::FontFamily::Monospace);
    let line = if fonts_ready { ctx.fonts(|f| f.row_height(&button_font)) } else { 12.5 * text_scale * p.row_ratio };
    // egui's default box is the line plus 2 * button_padding.y, floored by
    // interact_size.y. Growing both by the same tenth grows the rendered
    // button by exactly a tenth whichever of the two is binding.
    let base = (line + 2.0).max(18.0);
    let grown = base * BUTTON_GROWTH;
    style.spacing.button_padding = egui::vec2(4.0 + (grown - base) / 2.0, (grown - line) / 2.0);
    style.spacing.interact_size.y = grown;

    ctx.set_style(style);
}

/// The active world's corner radius. Only KAWAII has one.
#[must_use]
pub fn corner_radius() -> egui::CornerRadius {
    match palette().frame {
        FrameStyle::Soft => egui::CornerRadius::same(8),
        _ => egui::CornerRadius::ZERO,
    }
}

/// The active world's default edge weight. KAWAII draws heavier, because a
/// 1px pink hairline on a near-white ground is not an edge, it is a smudge.
#[must_use]
pub fn stroke_width() -> f32 {
    match palette().frame {
        FrameStyle::Soft => 2.0,
        _ => 1.0,
    }
}

/// Installs JetBrains Mono, embedded in the binary, as the whole UI's type.
///
/// Installs every theme's face, with the active world's at the front.
///
/// **Why embedded rather than read off the system.** This used to load
/// Consolas from `C:\Windows\Fonts\consola.ttf`, because egui's built-in
/// monospace (Hack) has no Vietnamese coverage and every diacritic in the
/// `vi` dictionary rendered as a fallback box. That worked on Windows and
/// quietly failed everywhere else: the mac and linux binaries fell through to
/// Hack, so they shipped a different typeface *and* an unreadable second
/// language. Since Vietnamese is a product requirement, not a locale demo,
/// every face has to be in the binary. ~1.7MB for all six worlds.
///
/// **Every face here carries the whole Vietnamese repertoire**, checked
/// against each file's `cmap` rather than assumed, and that requirement is
/// what picked them. The obvious genre faces all failed it and are *not*
/// here: Share Tech Mono, Rajdhani, Oxanium and Orbitron each miss ~100 of
/// the 128 codepoints a `vi` UI needs, which is the whole language, not an
/// edge case. IBM Plex Mono and Saira Condensed are the covering faces
/// closest to what MATRIX and CYBERPUNK wanted.
///
/// JetBrains Mono sits behind every family as the fallback, so a glyph no
/// theme face has still renders as a glyph.
///
/// **KAWAII and FALLOUT ship one weight**, because Nunito and Overpass Mono
/// are distributed only as variable fonts and egui renders a variable font at
/// its default instance. Their `heavy()` family resolves to the same file, so
/// headings in those two worlds separate by colour and size rather than
/// weight - which `RichText::strong()` alone already did before `heavy()`
/// existed.
///
/// **No bundled face carries CJK glyphs**, so Japanese, Korean and Chinese
/// get a face borrowed from the operating system, appended as the last
/// fallback (see `cjk_face`). Embedding Noto CJK instead would have been
/// self-contained, but it is roughly 20MB per script on every platform
/// binary for three languages, on a release that is otherwise about 10MB.
/// Returns `false` when `lang` itself needs CJK and no such font could be
/// found - the caller says so out loud, because the alternative is a screen
/// of empty boxes with no explanation.
///
/// Call on startup, on a theme change, and on a language change:
/// `ctx.set_fonts` throws away and rebuilds the whole glyph atlas, so it
/// must *not* be folded into `apply`, which reruns on every TEXT SIZE
/// change.
#[must_use]
pub fn install_fonts(ctx: &egui::Context, lang: Lang) -> bool {
    const FACES: [(&str, &[u8]); 10] = [
        ("jetbrains", include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf")),
        ("jetbrains_bold", include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf")),
        ("plex", include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")),
        ("plex_bold", include_bytes!("../../assets/fonts/IBMPlexMono-Bold.ttf")),
        ("chakra", include_bytes!("../../assets/fonts/ChakraPetch-Regular.ttf")),
        ("chakra_bold", include_bytes!("../../assets/fonts/ChakraPetch-Bold.ttf")),
        ("nunito", include_bytes!("../../assets/fonts/Nunito.ttf")),
        ("overpass", include_bytes!("../../assets/fonts/OverpassMono.ttf")),
        ("saira", include_bytes!("../../assets/fonts/SairaCondensed-Regular.ttf")),
        ("saira_bold", include_bytes!("../../assets/fonts/SairaCondensed-Bold.ttf")),
    ];

    let p = palette();
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in FACES {
        fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
    }

    // Front of both families: monospace is what the UI actually uses, and
    // proportional is what egui falls back to for anything that slips
    // through. JetBrains sits behind the theme face as the glyph fallback,
    // and egui's own built-ins behind that.
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        let entry = fonts.families.entry(family).or_default();
        entry.insert(0, "jetbrains".to_owned());
        if p.font != "jetbrains" {
            entry.insert(0, p.font.to_owned());
        }
    }

    // A real bold face, as its own family.
    //
    // `RichText::strong()` only swaps the *colour* in egui - it does not
    // reach for a heavier weight, because a weight has to be a separate
    // loaded font and nothing here loaded one. So every "strong" label in
    // this UI - the wordmark, the step numbers, RUN NEST - would be the same
    // stroke thickness as body text. `heavy()` below is the family that
    // actually is bold.
    fonts.families.insert(heavy(), vec![p.font_bold.to_owned(), p.font.to_owned(), "jetbrains_bold".to_owned(), "jetbrains".to_owned()]);

    // Last in every family, so they only ever supply glyphs no bundled face
    // has. The theme's own type still draws all the Latin, which is what
    // keeps a CJK language looking like the same app.
    //
    // **All three are loaded, not just the current language's.** The language
    // picker spells every language in its own script, so the one thing a
    // Japanese speaker must be able to read is the word 日本語 while the app
    // is still in English - loading on demand would show them a box and
    // nothing to click. `lang`'s own face goes first because Japanese and
    // Chinese draw some shared characters differently, and whichever face
    // comes first wins the codepoint.
    //
    // ponytail: reads up to ~46MB of system fonts at startup on Windows.
    // If that ever shows up as launch latency, the upgrade is to load the
    // picker's handful of glyphs from a subset and the rest on demand.
    let mut missing = false;
    for face in [lang, Lang::Ja, Lang::Ko, Lang::Zh] {
        let name = match face {
            Lang::Ja => "cjk_ja",
            Lang::Ko => "cjk_ko",
            Lang::Zh => "cjk_zh",
            // `lang` on its first pass through, when it is not a CJK language.
            _ => continue,
        };
        if fonts.font_data.contains_key(name) {
            continue;
        }
        match cjk_paths(face).iter().find_map(|path| std::fs::read(path).ok()) {
            Some(bytes) => {
                fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
                for list in fonts.families.values_mut() {
                    list.push(name.to_owned());
                }
            }
            // Only the language actually being displayed is worth complaining
            // about; an unreadable entry in the picker is self-explanatory.
            None => missing |= face == lang,
        }
    }

    ctx.set_fonts(fonts);
    !missing
}

/// Where to look for a system font carrying `lang`'s script, best first.
///
/// One list per language rather than one font for all three: no regional
/// font has kana *and* hangul *and* hanzi, so listing them separately is what
/// correctness costs. Ordered newest-first, since a machine with the modern
/// face usually also has the legacy one and the modern face looks better.
///
/// `.ttc` collections are read at index 0, which is the regular weight in
/// every collection listed here.
fn cjk_paths(lang: Lang) -> &'static [&'static str] {
    // `cfg!` rather than `#[cfg]`: the other platforms' paths are just
    // strings, and one expression reads better than three copies of the
    // function.
    if cfg!(target_os = "windows") {
        match lang {
            Lang::Ja => &[r"C:\Windows\Fonts\YuGothM.ttc", r"C:\Windows\Fonts\meiryo.ttc", r"C:\Windows\Fonts\msgothic.ttc"],
            Lang::Ko => &[r"C:\Windows\Fonts\malgun.ttf", r"C:\Windows\Fonts\gulim.ttc", r"C:\Windows\Fonts\batang.ttc"],
            _ => &[r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\simhei.ttf", r"C:\Windows\Fonts\simsun.ttc"],
        }
    } else if cfg!(target_os = "macos") {
        match lang {
            Lang::Ja => &["/System/Library/Fonts/Hiragino Sans GB.ttc", "/Library/Fonts/Arial Unicode.ttf"],
            Lang::Ko => &["/System/Library/Fonts/AppleSDGothicNeo.ttc", "/System/Library/Fonts/Supplemental/AppleGothic.ttf", "/Library/Fonts/Arial Unicode.ttf"],
            _ => &["/System/Library/Fonts/PingFang.ttc", "/System/Library/Fonts/Hiragino Sans GB.ttc", "/Library/Fonts/Arial Unicode.ttf"],
        }
    } else {
        // Distributions disagree about where Noto CJK lives, and it is not
        // installed by default everywhere - hence the caller's warning. The
        // one file covers all three scripts, so the lists are identical.
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
        ]
    }
}

/// The bold family - see `install_fonts`. Use it wherever `.strong()` was
/// meant to make something *look* heavier rather than merely brighter.
#[must_use]
pub fn heavy() -> egui::FontFamily {
    egui::FontFamily::Name("heavy".into())
}
/// One border, drawn inside the rect so it never overlaps a neighbour, in
/// whatever shape the active world draws its edges.
///
/// This replaces the old `bevel()`: an edge here is a line someone drew, not
/// a light source someone simulated. Width is a parameter because the step
/// from "this box contains things" (1px) to "this thing is selected" (2px) is
/// the only weight hierarchy the look has - the theme scales that, it does
/// not replace it, so a selected thing stays heavier than a plain one in
/// every world.
pub fn hairline(painter: &egui::Painter, rect: egui::Rect, color: Color32, width: f32) {
    let stroke = egui::Stroke::new(width * stroke_width(), color);
    match palette().frame {
        FrameStyle::Hard => {
            painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
        }
        FrameStyle::Soft => {
            painter.rect_stroke(rect, corner_radius(), stroke, egui::StrokeKind::Inside);
        }
        // A HUD does not box a target, it brackets it. The ticks are a sixth
        // of the shorter side so they stay corner marks on a wide panel
        // instead of growing back into a full rectangle.
        FrameStyle::Bracket => {
            let t = (rect.width().min(rect.height()) / 6.0).clamp(6.0, 24.0);
            let (l, r, top, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
            for (corner, dx, dy) in [
                (egui::pos2(l, top), 1.0, 1.0),
                (egui::pos2(r, top), -1.0, 1.0),
                (egui::pos2(l, b), 1.0, -1.0),
                (egui::pos2(r, b), -1.0, -1.0),
            ] {
                painter.line_segment([corner, corner + egui::vec2(t * dx, 0.0)], stroke);
                painter.line_segment([corner, corner + egui::vec2(0.0, t * dy)], stroke);
            }
        }
        // Two rules with a gap: the glass, then the tube behind it. The inner
        // one is dimmed rather than a second colour, so the palette still has
        // the same number of colours in it.
        FrameStyle::Bezel => {
            painter.rect_stroke(rect, 0.0, stroke, egui::StrokeKind::Inside);
            let inner = rect.shrink(3.0);
            if inner.width() > 0.0 && inner.height() > 0.0 {
                painter.rect_stroke(inner, 0.0, egui::Stroke::new(stroke.width, color.gamma_multiply(0.45)), egui::StrokeKind::Inside);
            }
        }
        // One corner cut away. Drawn as a closed path rather than a rect plus
        // a patch, so the chamfer is part of the outline instead of a line
        // lying across it.
        FrameStyle::Chamfer => {
            let c = (rect.width().min(rect.height()) / 8.0).clamp(4.0, 14.0);
            let (l, r, top, b) = (rect.left(), rect.right(), rect.top(), rect.bottom());
            let points = vec![
                egui::pos2(l, top),
                egui::pos2(r - c, top),
                egui::pos2(r, top + c),
                egui::pos2(r, b),
                egui::pos2(l, b),
            ];
            painter.add(egui::Shape::closed_line(points, stroke));
        }
    }
}
