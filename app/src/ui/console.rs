//! The floating CONSOLE window and the log buffer behind it.
//!
//! Same deal as the web UI's version: every line is timestamped, kept in a
//! capped in-memory ring for display, and *also* appended to a file that
//! outlives the session so a previous run's import/error/cancel history is
//! still readable afterwards. The cap is display-only - the on-disk log is
//! uncapped (well, 5MB-rotated by `nesting::benchmark_log`), because that's
//! the copy you go looking through after something went wrong.
//!
//! The window is deliberately not closable, only minimisable - the same
//! decision the web version made. Losing the only narration channel behind
//! a stray click is worse than the screen space it costs.

use std::collections::VecDeque;
use std::sync::mpsc;

/// Display cap only. Old lines fall off the top; the disk log keeps them.
const MAX_LINES: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    /// Run lifecycle narration (start/complete) - accent-coloured.
    Run,
    /// A genuinely better nest was found.
    Best,
    Error,
}

pub struct Line {
    pub stamp: String,
    pub text: String,
    pub kind: Kind,
}

pub struct Console {
    lines: VecDeque<Line>,
    /// Sender into the disk-writer thread. A log line must never make the UI
    /// thread touch the filesystem: lines arrive per generation and per
    /// individual placed, so a synchronous append here would put a file
    /// write in the middle of the frame loop hundreds of times a run.
    to_disk: mpsc::Sender<String>,
}

impl Default for Console {
    fn default() -> Self {
        let (to_disk, rx) = mpsc::channel::<String>();
        // One long-lived thread rather than one per line. It ends when the
        // App (and so the Sender) is dropped, i.e. at exit.
        std::thread::spawn(move || {
            while let Ok(line) = rx.recv() {
                // A failure here is not worth surfacing in the UI - the line
                // is already on screen, and the alternative is an error
                // dialog for every line once a disk fills up.
                let _ = crate::commands::append_log(&line);
            }
        });
        Self { lines: VecDeque::new(), to_disk }
    }
}

impl Console {
    pub fn log(&mut self, kind: Kind, text: impl Into<String>) {
        let text = text.into();
        let stamp = stamp_now();
        let _ = self.to_disk.send(format!("[{stamp}] {text}"));
        self.lines.push_back(Line { stamp, text, kind });
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.log(Kind::Error, text);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }
}

/// Local wall-clock `HH:MM:SS`, derived from the Unix epoch rather than
/// pulling in `chrono`/`time` for one format string. Deliberately ignores
/// timezone and DST: this is a relative-ordering aid inside one session's
/// log, not a timestamp anything is computed from, and it matches what the
/// web UI's `toLocaleTimeString` produced closely enough to read the same.
///
/// ponytail: UTC, so it will read as offset from local time. Swap in a
/// timezone-aware crate if the log ever needs to line up with an external
/// system's timestamps.
fn stamp_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}")
}


/// The log, as a collapsible left-hand side panel - the mirror of
/// `shell::config_panel` on the right, and built the same way: a heading
/// with the accent rule under it, a close button, and the header carrying
/// the toggle that brings it back.
///
/// It used to be a floating `egui::Window`, which meant it covered whatever
/// it was reporting on (including, once CONFIGURATION moved to the right,
/// the settings themselves) and remembered a bad position across sessions.
/// A panel narrows the central column instead of overlapping it, so the
/// result stays readable with the log open.
pub fn panel(app: &mut super::App, ctx: &egui::Context) {
    if !app.console_open {
        return;
    }
    let accent = super::theme::ACCENT();
    egui::SidePanel::left("console")
        .frame(egui::Frame::new().fill(super::theme::PANEL()).inner_margin(8.0))
        .default_width(380.0)
        .width_range(280.0..=680.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(app.t("console_title")).strong().family(super::theme::heavy()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("<<").on_hover_text(app.t("console_title")).clicked() {
                        app.console_open = false;
                    }
                });
            });
            super::shell::heading_rule(ui);
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in app.console.iter() {
                    let color = match line.kind {
                        Kind::Plain => super::theme::TEXT(),
                        Kind::Run => accent,
                        Kind::Best => super::theme::OK(),
                        Kind::Error => super::theme::ERROR(),
                    };
                    ui.label(egui::RichText::new(format!("[{}] {}", line.stamp, line.text)).color(color).small().monospace());
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_display_buffer_is_capped_and_keeps_the_newest_lines() {
        let mut c = Console::default();
        for i in 0..MAX_LINES + 50 {
            c.log(Kind::Plain, format!("line {i}"));
        }
        assert_eq!(c.lines.len(), MAX_LINES);
        assert_eq!(c.lines.front().unwrap().text, format!("line {}", 50));
        assert_eq!(c.lines.back().unwrap().text, format!("line {}", MAX_LINES + 49));
    }

    #[test]
    fn the_stamp_is_a_fixed_width_clock() {
        let s = stamp_now();
        assert_eq!(s.len(), 8, "{s}");
        assert!(s.chars().enumerate().all(|(i, c)| if i == 2 || i == 5 { c == ':' } else { c.is_ascii_digit() }), "{s}");
    }
}
