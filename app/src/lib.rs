//! The application, as a library, so that more than one binary can use it.
//!
//! **Why this exists rather than everything living in `main.rs`.** `commands`
//! is a set of plain, UI-free functions - that was true before the Tauri
//! layer was removed and is what let the egui rewrite land without touching
//! the engine. The headless `nest` binary is the second consumer of exactly
//! that fact. A `[lib]` is the only way Cargo will let two binaries share it.
//!
//! `ui` and `worker` live here too rather than staying behind in the bin, for
//! one reason: every `crate::commands` / `crate::dto` path inside them stays
//! correct. Moving only the two engine modules into a library would have
//! meant rewriting those paths in every UI file, which is a large diff for no
//! behavioural change and a good way to lose something in the noise.

pub mod commands;
pub mod dto;
pub mod paths;
mod ui;
mod worker;

/// Opens the window. The GUI binary is a shim around this.
///
/// # Errors
/// Whatever `eframe` fails with - it owns the window, the GL context and the
/// event loop, and there is nothing useful to add to its own reporting.
pub fn run_gui() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nestor")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("rustynesting", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc)))))
}
