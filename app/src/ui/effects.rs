//! Per-theme motion, painted over the whole window.
//!
//! Every world's flourish lives here and nowhere else, so the panels stay
//! ignorant of which theme is running and **the layout cannot move**: this
//! draws into a single full-screen `Area` that takes no space and accepts no
//! input. Deleting this module would cost the six worlds their motion and
//! nothing else.
//!
//! **Operate mode governs.** This is a tool someone runs a job on, so every
//! effect here is deliberately near the threshold of noticing: alphas in the
//! 0.04-0.12 range, nothing that moves under the pointer, nothing that
//! obscures a number. A flourish that makes a utilisation figure harder to
//! read is a bug, not a feature.
//!
//! **The cost is one repaint at `FPS`** while a themed world is active, which
//! is real on an idle window and is why OXIDE - the default - paints nothing
//! and requests nothing. During a nest the app is already repainting at 30fps
//! for the progress bar, so the effects add no wake-ups to the case that
//! actually matters.

use egui::{Color32, Pos2, Rect, Stroke};

use super::theme::{self, Theme};

/// Animation rate. Deliberately below the 30fps the progress bar asks for:
/// none of these effects is tracking anything the eye follows closely, and
/// halving the wake-ups halves the idle cost.
const FPS: f32 = 20.0;

/// Painted behind every panel. Only reaches the screen where a panel does not
/// cover it, which on this layout is almost nowhere - kept because a world
/// that wants a true background has somewhere to put it without fighting the
/// panels for the foreground.
pub fn background(ctx: &egui::Context) {
    if theme::active() == Theme::Oxide {
        return;
    }
    // Nothing yet uses the background layer; the foreground pass below is
    // where all six worlds actually paint. Kept as the seam rather than
    // deleted, because `update()` already routes both and adding the second
    // one later would mean touching the router again.
    let _ = ctx;
}

/// Painted over everything, including the sheet canvas.
pub fn foreground(ctx: &egui::Context) {
    let theme = theme::active();
    if theme == Theme::Oxide || theme == Theme::Kawaii {
        // KAWAII carries its motion in `Style::animation_time` - controls
        // that ease rather than snap - so it paints no overlay at all. Its
        // one painted flourish is the nest-complete sparkle below, which is
        // event-driven and not part of this always-on pass.
        return;
    }

    let screen = ctx.screen_rect();
    let t = ctx.input(|i| i.time) as f32;

    egui::Area::new(egui::Id::new("theme-effects"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            // Never intercept the pointer: this covers the whole window, and
            // a transparent overlay that swallows clicks would break every
            // control under it.
            ui.set_min_size(egui::Vec2::ZERO);
            let painter = ui.painter();
            match theme {
                Theme::Matrix => rain(painter, screen, t),
                Theme::Terminator => {
                    scanlines(painter, screen, 3.0, theme::ACCENT().gamma_multiply(0.055));
                    sweep(painter, screen, t);
                }
                Theme::Fallout => {
                    scanlines(painter, screen, 3.0, theme::ACCENT().gamma_multiply(0.05));
                    vignette(painter, screen);
                    flicker(painter, screen, t);
                }
                Theme::Cyberpunk => glitch(painter, screen, t),
                Theme::Oxide | Theme::Kawaii => {}
            }
        });

    ctx.request_repaint_after(std::time::Duration::from_secs_f32(1.0 / FPS));
}

/// A stable pseudo-random number for a column/seed pair.
///
/// A hash rather than an RNG so nothing has to own state between frames: the
/// same column produces the same stream every time, and only the clock moves.
fn noise(seed: u32) -> f32 {
    let mut h = seed.wrapping_mul(0x9E37_79B9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    (h % 10_000) as f32 / 10_000.0
}

/// Falling glyph columns.
///
/// Each column's *text* is fixed for the life of the process and only its
/// position moves. That is the whole performance trick: egui caches a laid-out
/// galley by its text, font and colour, so a column whose glyphs never change
/// is laid out once and then merely blitted, where re-rolling the characters
/// every frame would mean thousands of fresh glyph layouts a second.
fn rain(painter: &egui::Painter, screen: Rect, t: f32) {
    const COL_W: f32 = 22.0;
    const TRAIL: usize = 14;
    const GLYPHS: &[u8] = b"01<>[]{}/\\|=+*-#$%&@ABCDEFGHJKLMNPQRSTUVWXYZ";

    let font = egui::FontId::monospace(13.0);
    let accent = theme::ACCENT();
    let columns = (screen.width() / COL_W).ceil() as u32;

    for c in 0..columns {
        let x = screen.left() + c as f32 * COL_W + 4.0;
        // Speed and phase vary per column, or every trail falls in lockstep
        // and it reads as a descending grid rather than rain.
        let speed = 40.0 + noise(c) * 90.0;
        let phase = noise(c ^ 0x5F5F) * screen.height();
        let span = screen.height() + TRAIL as f32 * 16.0;
        let head_y = screen.top() + ((t * speed + phase) % span) - TRAIL as f32 * 16.0;

        let text: String = (0..TRAIL).map(|i| GLYPHS[(c as usize * 31 + i * 17) % GLYPHS.len()] as char).map(|ch| format!("{ch}\n")).collect();
        painter.text(Pos2::new(x, head_y), egui::Align2::LEFT_TOP, text, font.clone(), accent.gamma_multiply(0.10));
        // The leading glyph, brighter - the one part of a Matrix column the
        // eye actually tracks.
        let head = GLYPHS[(c as usize * 31 + TRAIL * 17) % GLYPHS.len()] as char;
        painter.text(Pos2::new(x, head_y + TRAIL as f32 * 16.0), egui::Align2::LEFT_TOP, head, font.clone(), accent.gamma_multiply(0.30));
    }
}

/// Horizontal CRT lines, every `step` pixels.
fn scanlines(painter: &egui::Painter, screen: Rect, step: f32, color: Color32) {
    let stroke = Stroke::new(1.0_f32, color);
    let mut y = screen.top();
    while y < screen.bottom() {
        painter.line_segment([Pos2::new(screen.left(), y), Pos2::new(screen.right(), y)], stroke);
        y += step;
    }
}

/// A single bright band travelling down the screen - a machine looking at
/// something, one pass at a time.
fn sweep(painter: &egui::Painter, screen: Rect, t: f32) {
    const PERIOD: f32 = 6.0;
    const BAND: f32 = 90.0;
    let y = screen.top() + ((t % PERIOD) / PERIOD) * (screen.height() + BAND) - BAND;
    let accent = theme::ACCENT();
    // Four stacked bands of decreasing alpha rather than a gradient, which
    // egui's painter has no primitive for. Four is where the banding stops
    // being visible at this alpha.
    for i in 0..4 {
        let f = i as f32 / 4.0;
        let rect = Rect::from_min_max(Pos2::new(screen.left(), y + f * BAND), Pos2::new(screen.right(), y + (f + 0.25) * BAND));
        painter.rect_filled(rect, 0.0, accent.gamma_multiply(0.05 * (1.0 - f)));
    }
}

/// Darkened edges, as a tube has.
fn vignette(painter: &egui::Painter, screen: Rect) {
    const RINGS: i32 = 14;
    for i in 0..RINGS {
        let inset = i as f32 * 3.0;
        let rect = screen.shrink(inset);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            break;
        }
        let alpha = 0.05 * (1.0 - i as f32 / RINGS as f32);
        painter.rect_stroke(rect, 0.0, Stroke::new(3.0_f32, Color32::from_black_alpha((alpha * 255.0) as u8)), egui::StrokeKind::Inside);
    }
}

/// An occasional dip in brightness, the way an old tube browns out.
///
/// Rare and shallow on purpose: a screen that visibly pulses while someone is
/// reading a utilisation figure off it is an effect that has overstayed.
fn flicker(painter: &egui::Painter, screen: Rect, t: f32) {
    let bucket = (t * 3.0) as u32;
    let n = noise(bucket);
    if n > 0.06 {
        return;
    }
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha((n * 180.0) as u8));
}

/// Brief chromatic tearing: a few horizontal slices offset into cyan and
/// magenta, on a duty cycle low enough that it reads as an event rather than
/// a texture.
fn glitch(painter: &egui::Painter, screen: Rect, t: f32) {
    let bucket = (t * 2.5) as u32;
    if noise(bucket) > 0.18 {
        return;
    }
    let cyan = theme::OK();
    let magenta = theme::ERROR();
    for i in 0..4u32 {
        let n = noise(bucket ^ (i * 0x1234_5));
        let y = screen.top() + n * screen.height();
        let h = 3.0 + noise(bucket ^ (i * 0x999)) * 14.0;
        let dx = (noise(bucket ^ (i * 0x77)) - 0.5) * 26.0;
        let band = Rect::from_min_max(Pos2::new(screen.left() + dx, y), Pos2::new(screen.right() + dx, y + h));
        painter.rect_filled(band, 0.0, if i % 2 == 0 { cyan } else { magenta }.gamma_multiply(0.13));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rain columns must not all fall together, which is what a single
    /// shared phase would produce - and is the difference between rain and a
    /// descending grid.
    #[test]
    fn column_noise_is_stable_per_column_and_differs_between_them() {
        for c in 0..64u32 {
            assert_eq!(noise(c), noise(c), "noise must be a pure function of its seed");
        }
        let values: Vec<f32> = (0..64).map(noise).collect();
        let mut sorted = values.clone();
        sorted.sort_by(f32::total_cmp);
        sorted.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        assert!(sorted.len() > 55, "64 columns collapsed to {} distinct phases", sorted.len());
        assert!(values.iter().all(|v| (0.0..1.0).contains(v)), "noise must stay in 0..1 - it scales screen positions");
    }

    /// Runs one real frame with the effects pass in it and reports whether
    /// the overlay was allocated.
    ///
    /// The presence of the `Area` is the honest signal. Checking the frame's
    /// repaint request instead does not work: a freshly built `Context`
    /// schedules its own start-up repaints, so that assertion passes and
    /// fails for reasons that have nothing to do with this module.
    /// Shapes emitted by one frame that runs `run_effects`.
    ///
    /// egui emits a handful of shapes for a frame on its own, so the number
    /// here is only meaningful against `BASELINE` below - the same frame with
    /// the effects pass left out.
    fn frame_shapes(run_effects: bool, theme: Theme) -> usize {
        theme::set(theme);
        let ctx = egui::Context::default();

        // A real screen rect, not the default: every effect derives its
        // geometry from it, so at zero size they all correctly draw nothing
        // and the test would pass without exercising a single one of them.
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| {
            if run_effects {
                background(ctx);
                foreground(ctx);
            }
        });
        output.shapes.len()
    }

    /// What an otherwise-empty frame costs, so the assertions below measure
    /// this module rather than egui.
    fn baseline() -> usize {
        frame_shapes(false, Theme::Oxide)
    }

    fn painted(theme: Theme) -> usize {
        frame_shapes(true, theme)
    }



    /// OXIDE is the default and must stay exactly as inherited - no overlay
    /// at all, not merely a quiet one. KAWAII paints none either: its motion
    /// is easing on the controls themselves.
    #[test]
    fn the_still_worlds_allocate_no_overlay() {
        let _guard = theme::TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let restore = theme::active();

        assert_eq!(painted(Theme::Oxide), baseline(), "OXIDE must paint no overlay");
        assert_eq!(painted(Theme::Kawaii), baseline(), "KAWAII carries its motion in animation_time, not an overlay");
        theme::set(restore);
    }

    /// The other four must survive a real frame and actually draw something.
    /// These run against a live painter, where a bad rect or a NaN position
    /// panics inside egui rather than degrading quietly.
    #[test]
    fn the_moving_worlds_paint_without_panicking() {
        let _guard = theme::TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let restore = theme::active();

        for t in [Theme::Matrix, Theme::Terminator, Theme::Fallout, Theme::Cyberpunk] {
            assert!(painted(t) > baseline(), "{} should paint an overlay", t.label());
        }
        theme::set(restore);
    }

}
