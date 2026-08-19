// Only job left now that Tauri is gone: stamp the Windows exe with an icon so
// Explorer/taskbar don't show the generic binary glyph. `winresource` is a
// no-op on non-Windows hosts, so the unconditional call is fine.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new().set_icon("icons/icon.ico").compile().unwrap();
    }
}
