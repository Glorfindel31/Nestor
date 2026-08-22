//! Keyboard shortcuts.
//!
//! A fixed set, not a remappable one: every binding is printed in its own
//! control's tooltip and listed in the help overlay, which is the whole of
//! the discoverability problem a remapping UI would otherwise have to solve.
//!
//! Ctrl-modified throughout, deliberately. This app has real text fields
//! (layer names, numeric entries) and a bare-letter binding would either
//! swallow a keystroke meant for one of them or need suppressing everywhere
//! - and it is suppressed while typing regardless (see `handle`), so a
//! modifier costs nothing and removes the whole class of accident.
//!
//! Every read is `consume_key`, never `key_pressed`: several consumers can
//! be live in one frame (a dialog stacked over the shortcuts here), and a
//! plain read lets one press answer all of them at once. That has already
//! shipped as a bug in this UI once - see `docs/PORT_STATUS.md`'s Pure-Rust
//! UI section.

use egui::{Key, Modifiers};

use super::App;

/// One binding, for the help overlay and tooltips to render.
pub struct Binding {
    pub keys: &'static str,
    pub description_key: &'static str,
}

/// Everything bound, in the order the help overlay lists them.
pub const BINDINGS: &[Binding] = &[
    Binding { keys: "Ctrl+R", description_key: "help_keys_run" },
    Binding { keys: "Ctrl+Z", description_key: "help_keys_undo" },
    Binding { keys: "Ctrl+Y", description_key: "help_keys_redo" },
    Binding { keys: "Ctrl+L", description_key: "help_keys_console" },
    Binding { keys: "Ctrl+,", description_key: "help_keys_config" },
    Binding { keys: "F1", description_key: "help_keys_help" },
    Binding { keys: "Esc", description_key: "help_keys_escape" },
];

/// Renders a shortcut for a tooltip: `"Start a nest run"` -> `"Start a nest
/// run  (Ctrl+R)"`. Keeps the key out of the translated string, since
/// `Ctrl+R` is the same in every language and a translator has no business
/// being able to break a binding's label.
#[must_use]
pub fn hint(text: &str, keys: &str) -> String {
    format!("{text}  ({keys})")
}

pub fn handle(app: &mut App, ctx: &egui::Context) {
    // A focused text field owns every key. Without this, typing a layer
    // name containing "r" with Ctrl held - or any stray modifier state -
    // could start a nest run mid-word.
    if ctx.wants_keyboard_input() {
        return;
    }
    // A modal is a question waiting for an answer; acting on a shortcut
    // behind one would apply to a UI the user cannot currently see. The
    // settings *window* is deliberately not in this list - it is a
    // non-modal palette, and shortcuts stay live while it is open.
    if app.help_open || app.recover_prompt.is_some() || app.confirm_reset || app.confirm_remove || app.pending_svg_batch.is_some() {
        // F1 still toggles the help overlay shut, so the key that opened it
        // also closes it.
        if app.help_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
            app.help_open = false;
        }
        return;
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::R)) {
        // Same rules as the button, not a second set: stop while running,
        // start only when there is something to nest.
        if app.running {
            app.worker.cancel.cancel();
            app.run_status.ok(app.t("run_status_stopped"));
        } else if !app.shapes.is_empty() {
            app.start_run();
        }
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Z)) {
        app.undo();
    }

    // Both spellings: Ctrl+Y is the Windows convention this app is built for,
    // Ctrl+Shift+Z is what anyone coming from a drawing tool will try first.
    if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Y)) || ctx.input_mut(|i| i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Z)) {
        app.redo();
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::L)) {
        app.console_open = !app.console_open;
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Comma)) {
        app.settings_open = !app.settings_open;
    }

    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F1)) {
        app.help_open = true;
    }
}
