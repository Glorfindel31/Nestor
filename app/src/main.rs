// No console window when launched from Explorer. This attribute is the whole
// of that fix - the Tauri template that this project started from ships it by
// default and it had simply never been added here, so every release build
// opened a stray terminal alongside the window. `not(debug_assertions)` keeps
// stdout/stderr visible under `cargo run`, where it's wanted.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod dto;
mod paths;
mod ui;
mod worker;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Nestor")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("rustynesting", options, Box::new(|cc| Ok(Box::new(ui::App::new(cc)))))
}
