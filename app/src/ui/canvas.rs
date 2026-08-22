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
pub const UNPLACED: Color32 = theme::ERROR;

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

/// Deterministic colour per layer name.
///
/// Real DXF layers are arbitrary user-given names (cut/etch/drill/whatever a
/// given job uses), so there's no fixed palette that could make sense - hash
/// the name to a hue instead. The same layer always gets the same colour, in
/// thumbnails and in the nested result alike, with no legend and no
/// per-job configuration. Same hash as the web UI's, so colours don't shift
/// between the two versions of the app.
pub fn color_for_layer(layer: &str) -> Color32 {
    let mut hash: u32 = 0;
    for b in layer.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(b as u32);
    }
    // Full value and slightly off-full saturation: these sit on `WELL`,
    // the darkest surface in the palette, so they can afford to be the only
    // saturated colour on screen. The chrome around them is zero-chroma by
    // design and does not compete.
    let hsva = egui::ecolor::Hsva::new((hash % 360) as f32 / 360.0, 0.78, 1.0, 1.0);
    hsva.into()
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
        painter.rect_filled(rect, 0.0, theme::WELL);
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
        assert_eq!(color_for_layer("cut"), color_for_layer("cut"));
        assert_ne!(color_for_layer("cut"), color_for_layer("drilling"));
    }
}
