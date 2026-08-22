// No console window when launched from Explorer. This attribute is the whole
// of that fix - the Tauri template that this project started from ships it by
// default and it had simply never been added here, so every release build
// opened a stray terminal alongside the window. `not(debug_assertions)` keeps
// stdout/stderr visible under `cargo run`, where it's wanted.
//
// It is also why the headless CLI is a *separate* binary (`src/bin/nest.rs`)
// rather than a subcommand of this one: a `windows` subsystem executable run
// from a terminal has no stdout attached to it at all, so a CLI mode built
// into this binary would print into the void on the one platform this app
// ships on.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    rustynesting::run_gui()
}
