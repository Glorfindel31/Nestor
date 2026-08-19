//! Where the app's own files live on disk.
//!
//! These used to come from Tauri's `AppHandle::path()` (`app_config_dir()` /
//! `app_log_dir()`), which derived them from `tauri.conf.json`'s
//! `identifier`. The identifier string is hardcoded below deliberately: it is
//! what makes these resolve to the *same* directories the Tauri build used,
//! so an existing user's `config.json` and `best_result.json` keep loading
//! across the rewrite instead of silently starting from scratch.
//!
//! Windows-only, matching the rest of this project's packaging - two env vars
//! rather than a `dirs`/`directories` dependency for what is four lines.

use std::path::PathBuf;

/// Matches the old `tauri.conf.json` `identifier` exactly. Do not "tidy" this
/// into the crate name - it is the on-disk directory users already have.
const IDENTIFIER: &str = "net.deepnest.rust";

fn dir_from(env_var: &str, sub: Option<&str>) -> Result<PathBuf, String> {
    let mut dir = PathBuf::from(std::env::var(env_var).map_err(|_| format!("{env_var} is not set"))?);
    dir.push(IDENTIFIER);
    if let Some(sub) = sub {
        dir.push(sub);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// `%APPDATA%\net.deepnest.rust\config.json` (was `app_config_dir()`).
pub fn config_file() -> Result<PathBuf, String> {
    Ok(dir_from("APPDATA", None)?.join("config.json"))
}

/// `%APPDATA%\net.deepnest.rust\best_result.json`. Note this is the *config*
/// dir, not the data dir - it was already that way under Tauri despite the
/// name, and moving it now would orphan existing recovery files.
pub fn best_result_file() -> Result<PathBuf, String> {
    Ok(dir_from("APPDATA", None)?.join("best_result.json"))
}

/// `%LOCALAPPDATA%\net.deepnest.rust\logs\rustynesting.log` (was `app_log_dir()`).
pub fn log_file() -> Result<PathBuf, String> {
    Ok(dir_from("LOCALAPPDATA", Some("logs"))?.join("rustynesting.log"))
}
