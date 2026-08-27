//! The run-progress chart above the attempt selector: utilisation and
//! unplaced-part count across every attempt in `App::history`.
//!
//! Exists to answer the one question the stats row can't: *is this as good as
//! it gets?* Without it the operator sees a single number and has to guess
//! whether another run would help, so they either stop early or burn ten
//! minutes for 0.3%. The data was already being collected and thrown away
//! visually - every `NestSnapshotDto` carries what this draws.
//!
//! Hand-painted rather than pulling in `egui_plot`: `canvas.rs` already
//! establishes the "map a model range onto a `Rect`" pattern this needs, and
//! one un-zoomable two-series chart doesn't justify a dependency that would
//! also have to be kept in step with the pinned egui 0.32.
//!
//! Two deliberate choices, both about not lying to the operator:
//!
//! - **Utilisation is plotted on a fixed 0-100 axis, never auto-scaled.**
//!   Auto-scaling makes a 0.2% wobble fill the panel and read as a
//!   breakthrough. The whole point is to show the curve *flattening*, which
//!   an auto-scaled axis structurally cannot do.
//! - **X is the attempt index, evenly spaced - not `generation`.** Generation
//!   numbers restart across the escalating runs `run_nest_with_progress`
//!   performs, so spacing by them would draw gaps and backward jumps that
//!   look like stalls the engine never had.

use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke};

use super::{theme, App};
use crate::dto::NestSnapshotDto;

/// Height of the plot body, excluding the caption line under it.
const PLOT_HEIGHT: f32 = 84.0;

/// Radius of the marker drawn on the currently-selected attempt.
const MARKER_RADIUS: f32 = 3.0;

/// Maps an attempt index and a 0-100 value onto the plot rect.
///
/// A single-entry history has no horizontal range to divide by; it maps to
/// the left edge rather than producing NaN and painting nothing (or, worse,
/// an infinite coordinate that takes the whole frame's tessellation with it).
#[derive(Clone, Copy)]
struct Plot {
    rect: Rect,
    last_index: usize,
}

impl Plot {
    fn new(rect: Rect, count: usize) -> Self {
        Self { rect, last_index: count.saturating_sub(1) }
    }

    fn x(&self, index: usize) -> f32 {
        if self.last_index == 0 {
            return self.rect.left();
        }
        self.rect.left() + self.rect.width() * (index as f32 / self.last_index as f32)
    }

    /// `percent` is clamped, so a utilisation that somehow exceeds 100 (or a
    /// normalised unplaced count at exactly the maximum) still lands inside
    /// the plot instead of drawing over the panel around it.
    fn y(&self, percent: f64) -> f32 {
        let t = (percent.clamp(0.0, 100.0) / 100.0) as f32;
        self.rect.bottom() - self.rect.height() * t
    }

    /// Which attempt index is nearest a screen X - for hover and click.
    fn index_at(&self, x: f32) -> usize {
        if self.last_index == 0 || self.rect.width() <= 0.0 {
            return 0;
        }
        let t = ((x - self.rect.left()) / self.rect.width()).clamp(0.0, 1.0);
        (t * self.last_index as f32).round() as usize
    }
}

/// Scales the unplaced counts into the same 0-100 box as utilisation, so both
/// series share one axis.
///
/// Returns `None` when nothing was ever unplaced - the series is then not
/// drawn at all, rather than drawn as a flat line along the bottom that
/// invites the reader to wonder what it means.
fn unplaced_scale(history: &[NestSnapshotDto]) -> Option<f64> {
    let peak = history.iter().map(|h| h.unplaced_count).max()?;
    (peak > 0).then_some(peak as f64)
}

/// Draws the chart. Returns the attempt index the user clicked, if any.
///
/// Takes `&App` and returns the click rather than mutating: switching
/// attempts has to rebuild `App::snapshot` too, and that sequence already
/// exists in `result.rs`'s selector. Returning the index keeps one path for
/// it instead of a second, subtly different copy here.
pub fn chart(app: &App, ui: &mut egui::Ui) -> Option<usize> {
    // One attempt is a dot, not a curve - the selector is hidden at that
    // point too, for the same reason.
    if app.history.len() <= 1 {
        return None;
    }
    let lang = app.prefs.lang;

    ui.add_space(4.0);
    ui.label(RichText::new(super::i18n::t(lang, "chart_label")).color(theme::DIM()).small());

    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), PLOT_HEIGHT), Sense::click());
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::WELL());

    // Inset so a point at exactly 0% or 100% still draws its full marker
    // inside the well instead of being clipped in half by its edge.
    let plot = Plot::new(rect.shrink(MARKER_RADIUS + 1.0), app.history.len());

    // Gridlines at 25/50/75%, plus the 100% ceiling - enough to read a value
    // off by eye, few enough not to compete with the data.
    for percent in [0.0, 25.0, 50.0, 75.0, 100.0] {
        let y = plot.y(percent);
        theme::hairline(&painter, Rect::from_min_max(Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)), theme::LINE(), 1.0);
    }

    let scale = unplaced_scale(&app.history);
    if let Some(peak) = scale {
        let points: Vec<Pos2> = app.history.iter().enumerate().map(|(i, h)| Pos2::new(plot.x(i), plot.y(h.unplaced_count as f64 / peak * 100.0))).collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, theme::ERROR())));
    }

    let utilisation: Vec<Pos2> = app.history.iter().enumerate().map(|(i, h)| Pos2::new(plot.x(i), plot.y(h.utilisation))).collect();
    painter.add(egui::Shape::line(utilisation.clone(), Stroke::new(1.6_f32, theme::ACCENT())));

    // The attempt currently being displayed, so the chart and the canvas
    // below it can't disagree about what is on screen.
    if let Some(&at) = utilisation.get(app.history_index) {
        painter.circle_filled(at, MARKER_RADIUS, theme::ACCENT());
    }

    let hovered = response.hover_pos().map(|p| plot.index_at(p.x));
    if let Some(i) = hovered {
        if let (Some(h), Some(&at)) = (app.history.get(i), utilisation.get(i)) {
            let x = plot.x(i);
            theme::hairline(&painter, Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())), theme::LINE_STRONG(), 1.0);
            painter.circle_stroke(at, MARKER_RADIUS, Stroke::new(1.0_f32, theme::TEXT()));
            response.clone().on_hover_text(readout(app, i, h));
        }
    }

    caption(app, ui, scale);

    // Only a click that landed on a real attempt counts; `index_at` clamps,
    // so this can't return an index outside the history.
    response.clicked().then(|| plot.index_at(response.interact_pointer_pos().map_or(rect.left(), |p| p.x)))
}

/// The hover tooltip: which attempt, and its four headline numbers.
fn readout(app: &App, index: usize, h: &NestSnapshotDto) -> String {
    super::i18n::tv(
        app.prefs.lang,
        "chart_readout",
        &[
            ("i", &(index + 1).to_string()),
            ("gen", &h.generation.to_string()),
            ("util", &format!("{:.1}", h.utilisation)),
            ("unplaced", &h.unplaced_count.to_string()),
        ],
    )
}

/// The legend line under the plot. The unplaced series names its own peak,
/// because it is the one series whose axis isn't self-evident - it shares
/// utilisation's 0-100 box and would otherwise be an unlabelled squiggle.
fn caption(app: &App, ui: &mut egui::Ui, scale: Option<f64>) {
    let lang = app.prefs.lang;
    ui.horizontal(|ui| {
        swatch(ui, theme::ACCENT());
        ui.label(RichText::new(super::i18n::t(lang, "chart_series_util")).color(theme::DIM()).small());
        if let Some(peak) = scale {
            ui.add_space(10.0);
            swatch(ui, theme::ERROR());
            ui.label(RichText::new(super::i18n::tv(lang, "chart_series_unplaced", &[("peak", &format!("{peak:.0}"))])).color(theme::DIM()).small());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(super::i18n::t(lang, "chart_hint")).color(theme::DIM()).small());
        });
    });
}

fn swatch(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 2.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(200.0, 100.0))
    }

    fn snapshot(generation: usize, utilisation: f64, unplaced_count: usize) -> NestSnapshotDto {
        NestSnapshotDto { generation, placements: Vec::new(), fitness: 0.0, utilisation, unplaced_count, unplaced_ids: Vec::new() }
    }

    #[test]
    fn a_single_entry_history_maps_to_the_left_edge_instead_of_nan() {
        let plot = Plot::new(rect(), 1);
        assert!(plot.x(0).is_finite());
        assert_eq!(plot.x(0), rect().left());
        assert_eq!(plot.index_at(999.0), 0);
    }

    #[test]
    fn an_empty_history_does_not_divide_by_zero() {
        let plot = Plot::new(rect(), 0);
        assert!(plot.x(0).is_finite());
    }

    /// The fixed axis is the whole point: 100% must be the top and 0% the
    /// bottom regardless of what the data actually spans. If this ever
    /// becomes auto-scaled, a flat curve starts looking like progress.
    #[test]
    fn the_utilisation_axis_is_fixed_not_data_relative() {
        let plot = Plot::new(rect(), 5);
        assert_eq!(plot.y(100.0), rect().top());
        assert_eq!(plot.y(0.0), rect().bottom());
        // Two runs that differ only in their range must map identically.
        assert_eq!(plot.y(78.5), Plot::new(rect(), 99).y(78.5));
    }

    #[test]
    fn out_of_range_values_are_clamped_into_the_plot() {
        let plot = Plot::new(rect(), 3);
        assert_eq!(plot.y(140.0), rect().top());
        assert_eq!(plot.y(-20.0), rect().bottom());
    }

    #[test]
    fn x_spans_the_full_width_end_to_end() {
        let plot = Plot::new(rect(), 4);
        assert_eq!(plot.x(0), rect().left());
        assert_eq!(plot.x(3), rect().right());
    }

    #[test]
    fn index_at_snaps_to_the_nearest_attempt_and_stays_in_range() {
        let plot = Plot::new(rect(), 5);
        assert_eq!(plot.index_at(plot.x(2)), 2);
        assert_eq!(plot.index_at(-500.0), 0);
        assert_eq!(plot.index_at(9999.0), 4);
    }

    /// A run where everything placed every time must not draw a flat line
    /// along the bottom - there is nothing to say, so the series is omitted.
    #[test]
    fn the_unplaced_series_is_omitted_when_nothing_was_ever_unplaced() {
        let history = vec![snapshot(1, 70.0, 0), snapshot(2, 72.0, 0)];
        assert!(unplaced_scale(&history).is_none());
    }

    #[test]
    fn the_unplaced_series_scales_to_its_own_peak() {
        let history = vec![snapshot(1, 70.0, 4), snapshot(2, 72.0, 1)];
        assert_eq!(unplaced_scale(&history), Some(4.0));
    }
}
