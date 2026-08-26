//! Drawing a shape (or a placed part) with `egui::Painter`, plus the
//! model <-> screen mapping the result canvas's drag editing depends on.
//!
//! A port of the web UI's `render.js`, and smaller than it was: no string
//! building, no DOM, no `vector-effect` workaround - a painter stroke is
//! already in screen pixels.
//!
//! The one thing that must not be got wrong here is the Y flip. DXF (and
//! every geometry in this workspace) is Y-up; screen coordinates are Y-down.
//! Dropping that flip mirrors every shape vertically *and* reverses its
//! apparent winding - which an area or bounds check will not catch, because
//! neither changes. This project has already been bitten by exactly that
//! class of bug once, in `geometry::svg_import`. Hence `model_to_screen` and
//! `screen_to_model` being a matched pair with a round-trip test, rather
//! than two places that each do the arithmetic by hand.

use egui::{Color32, Pos2, Rect, Stroke};

use super::state::{bounds_of, Bounds};
use super::theme;
use crate::dto::{PointDto, PolygonDto};

/// Unplaced parts are drawn entirely in the error colour regardless of their
/// real layer, so they read as "this one's a problem" at a glance.
/// A function rather than a const because the palette is now chosen at
/// runtime - the danger colour is not the same in all six worlds.
#[must_use]
pub fn unplaced() -> Color32 {
    theme::ERROR()
}


/// Maps model coordinates (Y-up, millimetres) onto a screen rectangle
/// (Y-down, points), preserving aspect ratio and centring the content.
#[derive(Clone, Copy, Debug)]
pub struct View {
    scale: f32,
    /// Screen position of the model-space origin corner (`bounds.minx`,
    /// `bounds.maxy` - i.e. the *top* left in screen terms, because Y flips).
    origin: Pos2,
    bounds: Bounds,
}

impl View {
    /// Fits `bounds` inside `rect`. A zero-extent model (a single point, an
    /// empty sheet) gets a scale of 1 rather than an infinity.
    pub fn fit(bounds: Bounds, rect: Rect) -> Self {
        let (w, h) = (bounds.w() as f32, bounds.h() as f32);
        let scale = if w > 0.0 && h > 0.0 { (rect.width() / w).min(rect.height() / h) } else { 1.0 };
        let drawn = egui::vec2(w * scale, h * scale);
        let origin = rect.center() - drawn / 2.0;
        Self { scale, origin, bounds }
    }

    /// The same fit, zoomed by `zoom` about `rect`'s centre and then shifted
    /// by `pan` screen pixels.
    ///
    /// Expressed as a transform *of the fit* rather than as its own
    /// scale/origin pair so that a resized window still re-fits: the zoom is
    /// a factor on whatever the sheet's fitted size currently is, not a
    /// remembered absolute scale that would leave the sheet the wrong size in
    /// a different-sized panel.
    pub fn zoomed(self, center: Pos2, zoom: f32, pan: egui::Vec2) -> Self {
        Self { scale: self.scale * zoom, origin: center + (self.origin - center) * zoom + pan, bounds: self.bounds }
    }

    pub fn model_to_screen(&self, p: PointDto) -> Pos2 {
        Pos2::new(
            self.origin.x + (p.x - self.bounds.minx) as f32 * self.scale,
            // The flip: model Y grows upward, screen Y grows downward.
            self.origin.y + (self.bounds.maxy - p.y) as f32 * self.scale,
        )
    }

    pub fn screen_to_model(&self, p: Pos2) -> PointDto {
        PointDto {
            x: self.bounds.minx + ((p.x - self.origin.x) / self.scale) as f64,
            y: self.bounds.maxy - ((p.y - self.origin.y) / self.scale) as f64,
        }
    }

    /// A screen-space drag delta in model units. Deliberately expressed as
    /// the difference between two `screen_to_model` calls rather than as its
    /// own bit of division: that way there is exactly one place that knows
    /// about the scale and the Y flip, and the sign of the Y component
    /// follows from the inverse instead of being re-derived (and re-gotten
    /// wrong) here.
    pub fn model_delta(&self, screen_delta: egui::Vec2) -> (f64, f64) {
        let from = self.screen_to_model(Pos2::ZERO);
        let to = self.screen_to_model(Pos2::ZERO + screen_delta);
        (to.x - from.x, to.y - from.y)
    }
}

/// Rotate about the origin, then translate. The order matters and matches
/// what the engine's own placement means by `(rotation, x, y)`.
pub fn rotated_translated(points: &[PointDto], rotation_deg: f64, dx: f64, dy: f64) -> Vec<PointDto> {
    let rad = rotation_deg.to_radians();
    let (sin, cos) = rad.sin_cos();
    points.iter().map(|p| PointDto { x: p.x * cos - p.y * sin + dx, y: p.x * sin + p.y * cos + dy }).collect()
}

/// The three operations this product names, as (matching substrings, hue,
/// saturation, value) for a `Hue` world and as a brightness rank for a
/// `Mono` one.
///
/// Substring rather than equality: real files ship `CUT`, `Cut Layer`,
/// `OUTER_CUT`, `DRILLING`. First match wins, so the order matters only where
/// one name contains another.
const KNOWN: [(&[&str], f32, f32, f32); 3] = [
    // The profile - the edge the machine actually follows, so it is the
    // brightest and coolest thing on the sheet.
    (&["cut", "profile", "outline", "contour"], 193.0, 0.62, 1.0),
    // Holes: their own hue, because an operator scanning for them is usually
    // counting them.
    (&["drill", "hole", "bore"], 142.0, 0.60, 0.95),
    // Surface work - never a through-cut, so it is deliberately the quietest
    // of the three.
    (&["etch", "engrave", "mark", "score", "raster"], 272.0, 0.52, 0.98),
];

/// Where a layer name ranks among the known operations, or `None` for one
/// this product does not name.
fn known_index(layer: &str) -> Option<usize> {
    let name = layer.trim().to_ascii_lowercase();
    KNOWN.iter().position(|(names, ..)| names.iter().any(|n| name.contains(n)))
}

/// A stable hash of the name, so the same layer draws the same colour in
/// every file, thumbnail and sheet, with no legend and no per-job setting.
fn name_hash(layer: &str) -> u32 {
    let mut hash: u32 = 0;
    for b in layer.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(u32::from(b));
    }
    hash
}

/// Colour per layer name: the operations this product names get a fixed one,
/// everything else gets a stable generated one.
///
/// This used to hash every name to a hue, on the reasoning that DXF layer
/// names are arbitrary. They are arbitrary in *spelling*, not in meaning -
/// `PRODUCT.md` names cut / etch / drill as different machine operations, and
/// hashing meant the same physical operation drew in a different colour in
/// two files, while nothing stopped a hash landing on the accent.
///
/// **The strategy is the theme's**, because the six palettes do not all have
/// room for three unrelated hues. MATRIX, TERMINATOR and FALLOUT are built
/// from a single phosphor, and a cyan profile line on an amber CRT is a
/// foreign object rather than a colour code; those worlds separate the same
/// three operations by *brightness* instead. Nothing is lost by it: the
/// operations stay distinguishable, consistently, which is the whole
/// requirement.
pub fn color_for_layer(layer: &str) -> Color32 {
    match theme::palette().canvas {
        theme::CanvasStyle::Hue { reserved, sat, val } => {
            if let Some(i) = known_index(layer) {
                let (_, h, s, v) = KNOWN[i];
                // The theme scales the pinned operations rather than
                // overriding them, so a light-ground world darkens all three
                // together and they keep their relationship to each other.
                return egui::ecolor::Hsva::new(h / 360.0, s * (sat / 0.78), v * val, 1.0).into();
            }
            let hue = spread_outside(name_hash(layer), reserved);
            egui::ecolor::Hsva::new(hue / 360.0, sat, val, 1.0).into()
        }
        theme::CanvasStyle::Mono { base } => {
            // Three steps for the three named operations, in the same order
            // of prominence the hues carry: profile brightest, then holes,
            // then surface work.
            const STEPS: [f32; 3] = [1.0, 0.72, 0.45];
            // Unrecognised names land between and below those, so they never
            // impersonate a named operation but stay visible.
            const OTHER: [f32; 4] = [0.86, 0.60, 0.34, 0.24];
            let factor = match known_index(layer) {
                Some(i) => STEPS[i],
                None => OTHER[(name_hash(layer) % OTHER.len() as u32) as usize],
            };
            base.gamma_multiply(factor)
        }
    }
}

/// Spreads a hash over the hue circle, skipping every band the chrome owns.
///
/// A generated hue landing inside one would say "press this" or "this is
/// wrong" about a part outline, on the largest painted surface in the window.
///
/// Each band is widened by a degree on each side first, because the colour
/// round-trips through 8-bit RGB on its way to `Color32` and comes back up to
/// ~0.3 degrees from where it started. Generating exactly onto an edge put it
/// back inside the band - which the test below caught.
fn spread_outside(hash: u32, reserved: &[(f32, f32)]) -> f32 {
    const GUARD: f32 = 1.0;
    let bands: Vec<(f32, f32)> = reserved.iter().map(|(lo, hi)| (lo - GUARD, hi + GUARD)).collect();
    let blocked: f32 = bands.iter().map(|(lo, hi)| hi - lo).sum();
    let span = 360.0 - blocked;
    if span <= 1.0 {
        // No theme does this, but a future palette that reserved almost the
        // whole wheel would otherwise divide by ~zero and hash everything
        // onto one hue.
        return 0.0;
    }
    let mut hue = (hash % span as u32) as f32;
    // Walk the bands in order, pushing the hue past each one it has reached.
    let mut sorted = bands;
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (lo, hi) in sorted {
        if hue >= lo {
            hue += hi - lo;
        }
    }
    hue
}

/// Draws a shape and, recursively, every nested child - holes and interior
/// features on other layers. A DXF part is a tree, not just its outer
/// boundary; drawing only `points` silently discards the layer identity this
/// app exists to preserve end to end. (That was a real bug once: `FLAT.dxf`'s
/// `drilling` layer rendered as nothing at all.)
///
/// Children share their parent's rigid transform, so one `map` closure
/// serves the whole tree. `override_color`, when given, replaces
/// `color_for_layer` for every node - used for unplaced parts.
pub fn draw_shape(painter: &egui::Painter, shape: &PolygonDto, map: &impl Fn(PointDto) -> Pos2, is_root: bool, override_color: Option<Color32>) {
    let color = override_color.unwrap_or_else(|| color_for_layer(&shape.layer));
    let pts: Vec<Pos2> = shape.points.iter().map(|p| map(*p)).collect();
    if pts.len() >= 2 {
        painter.add(egui::Shape::closed_line(pts, Stroke::new(if is_root { 1.4_f32 } else { 1.0 }, color)));
    }
    for child in &shape.children {
        draw_shape(painter, child, map, false, override_color);
    }
}

/// A shape drawn to fit a small square, for the shapes table's PREVIEW
/// column. Its own local bounds with a little padding, so a part is legible
/// regardless of where it sits in the source file's coordinate space.
pub fn thumbnail(ui: &mut egui::Ui, shape: &PolygonDto, size: f32, override_color: Option<Color32>) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::WELL());
        let b = bounds_of(&shape.points);
        let pad = (b.w().max(b.h()).max(1.0) * 0.08) as f32;
        let view = View::fit(b, rect.shrink(pad.min(size / 4.0)));
        draw_shape(&painter, shape, &|p| view.model_to_screen(p), true, override_color);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(minx: f64, miny: f64, maxx: f64, maxy: f64) -> Bounds {
        Bounds { minx, miny, maxx, maxy }
    }

    /// Drag correctness rests entirely on these two being inverses. If the Y
    /// flip is applied in only one of them, a part follows the pointer
    /// upward when it's dragged down - and no area or bounds assertion
    /// anywhere else would notice.
    #[test]
    fn model_to_screen_round_trips_through_screen_to_model() {
        let view = View::fit(bounds(-50.0, 20.0, 250.0, 180.0), Rect::from_min_size(Pos2::new(17.0, 9.0), egui::vec2(640.0, 400.0)));
        for p in [PointDto { x: -50.0, y: 20.0 }, PointDto { x: 250.0, y: 180.0 }, PointDto { x: 12.5, y: 101.25 }] {
            let back = view.screen_to_model(view.model_to_screen(p));
            assert!((back.x - p.x).abs() < 1e-3, "x: {back:?} vs {p:?}");
            assert!((back.y - p.y).abs() < 1e-3, "y: {back:?} vs {p:?}");
        }
    }

    #[test]
    fn screen_y_grows_downward_while_model_y_grows_upward() {
        let view = View::fit(bounds(0.0, 0.0, 100.0, 100.0), Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 200.0)));
        let low = view.model_to_screen(PointDto { x: 0.0, y: 0.0 });
        let high = view.model_to_screen(PointDto { x: 0.0, y: 100.0 });
        assert!(high.y < low.y, "a higher model Y must map to a smaller screen Y");
        // And the drag delta must agree with that, or a dragged part runs
        // away from the pointer instead of following it.
        let (dx, dy) = view.model_delta(egui::vec2(20.0, 20.0));
        assert!(dx > 0.0 && dy < 0.0, "dragging down-right must be +x, -y in model space (got {dx}, {dy})");
    }

    #[test]
    fn a_degenerate_extent_does_not_produce_a_nan_view() {
        let view = View::fit(bounds(5.0, 5.0, 5.0, 5.0), Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0)));
        let p = view.model_to_screen(PointDto { x: 5.0, y: 5.0 });
        assert!(p.x.is_finite() && p.y.is_finite(), "{p:?}");
    }

    #[test]
    fn rotation_is_applied_before_translation() {
        // 90 degrees takes (1,0) to (0,1); the offset is then added, not
        // rotated. Getting this backwards puts every part in the wrong place
        // by its own offset, rotated.
        let out = rotated_translated(&[PointDto { x: 1.0, y: 0.0 }], 90.0, 10.0, 0.0);
        assert!((out[0].x - 10.0).abs() < 1e-9 && (out[0].y - 1.0).abs() < 1e-9, "{out:?}");
    }

    #[test]
    fn layer_colours_are_stable_and_differ_between_layers() {
        // Reads the global palette, so it has to be serialised against the
        // tests that change it - and it genuinely would fail under a Mono
        // world, where two layers can share a brightness step.
        let _guard = theme::TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        theme::set(theme::Theme::Oxide);
        assert_eq!(color_for_layer("cut"), color_for_layer("cut"));

        assert_ne!(color_for_layer("cut"), color_for_layer("drilling"));
    }

    /// The same machine operation must draw the same colour however the file
    /// spelled it - the whole reason the known names are pinned.
    #[test]
    fn known_operations_are_spelling_insensitive() {
        let _guard = theme::TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for names in
 [["cut", "CUT", "Outer Cut"], ["drill", "DRILLING", "holes"], ["etch", "ENGRAVE", "Score 1"]] {
            let first = color_for_layer(names[0]);
            for n in names {
                assert_eq!(color_for_layer(n), first, "{n} should match {}", names[0]);
            }
        }
    }

    /// No colour drawn from data may impersonate the accent or the error
    /// signal. Exhaustive over a wide sample of names rather than a few, and
    /// over **every** theme: each palette reserves its own bands, so a band
    /// that is safe in OXIDE is exactly where CYBERPUNK puts its yellow.
    ///
    /// Runs serially and restores the theme it found, because the active
    /// palette is process-global.
    #[test]
    fn no_layer_colour_lands_in_the_chrome_hue_band() {
        let mut names: Vec<String> = (0..4000).map(|i| format!("layer{i}")).collect();
        names.extend(["cut", "drill", "etch", "0", "DEFAULT", "annotation", "text", "dim"].iter().map(|s| (*s).to_string()));

        let _guard = theme::TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let restore = theme::active();
        for t in theme::Theme::ALL {

            theme::set(t);
            let theme::CanvasStyle::Hue { reserved, .. } = theme::palette().canvas else {
                // A Mono world draws every layer in one hue by construction -
                // the theme's own - so there is no band to stay out of.
                continue;
            };
            for name in &names {
                let hsva = egui::ecolor::Hsva::from(color_for_layer(name));
                let hue = hsva.h * 360.0;
                for (lo, hi) in reserved {
                    assert!(
                        hue < *lo || hue > *hi,
                        "{}: layer {name:?} drew at hue {hue}, inside the reserved band {lo}..{hi}",
                        t.label()
                    );
                }
            }
        }
        theme::set(restore);
    }

}
