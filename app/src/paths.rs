//! Where the app's own files live on disk.
//!
//! These used to come from Tauri's `AppHandle::path()` (`app_config_dir()` /
//! `app_log_dir()`), which derived them from `tauri.conf.json`'s
//! `identifier`. The identifier string is hardcoded below deliberately: it is
//! what makes these resolve to the *same* directories the Tauri build used,
//! so an existing user's `config.json` and `best_result.json` keep loading
//! across the rewrite instead of silently starting from scratch.
//!
//! This module read `%APPDATA%`/`%LOCALAPPDATA%` directly for one release,
//! on the grounds that two env vars beat a dependency for what is four lines.
//! That was true and it was also what quietly made the app Windows-only:
//! neither variable exists on macOS or Linux, so every save, load and log
//! write there returned `Err` and the app forgot everything between runs.
//! `dirs` resolves the same two directories on Windows - `config_dir()` *is*
//! `%APPDATA%` and `data_local_dir()` *is* `%LOCALAPPDATA%` - so existing
//! Windows users' files stay exactly where they are, and the other two
//! platforms get their own conventional locations instead of an error.

use std::path::PathBuf;

/// Matches the old `tauri.conf.json` `identifier` exactly. Do not "tidy" this
/// into the crate name - it is the on-disk directory users already have.
const IDENTIFIER: &str = "net.deepnest.rust";

fn dir_from(base: Option<PathBuf>, what: &str, sub: Option<&str>) -> Result<PathBuf, String> {
    let mut dir = base.ok_or_else(|| format!("this system has no {what} directory"))?;
    dir.push(IDENTIFIER);
    if let Some(sub) = sub {
        dir.push(sub);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// `%APPDATA%\net.deepnest.rust\config.json` on Windows (was
/// `app_config_dir()`); `~/Library/Application Support` / `~/.config`
/// elsewhere.
pub fn config_file() -> Result<PathBuf, String> {
    Ok(dir_from(dirs::config_dir(), "config", None)?.join("config.json"))
}

/// Alongside `config_file`. Note this is the *config* dir, not the data dir -
/// it was already that way under Tauri despite the name, and moving it now
/// would orphan existing recovery files.
pub fn best_result_file() -> Result<PathBuf, String> {
    Ok(dir_from(dirs::config_dir(), "config", None)?.join("best_result.json"))
}

/// The saved parts library and remnant shelf, alongside `config_file`.
///
/// One file for both, because they are the same thing to the code that reads
/// them - a named polygon someone wants back later - and splitting them would
/// mean two versioned formats, two atomic writes and two failure modes for no
/// difference the user can see.
pub fn shape_store_file() -> Result<PathBuf, String> {
    Ok(dir_from(dirs::config_dir(), "config", None)?.join("shapes.json"))
}

/// `%LOCALAPPDATA%\net.deepnest.rust\logs\rustynesting.log` on Windows (was
/// `app_log_dir()`); the platform's local-data directory elsewhere.
pub fn log_file() -> Result<PathBuf, String> {
    Ok(dir_from(dirs::data_local_dir(), "local data", Some("logs"))?.join("rustynesting.log"))
}

#[cfg(test)]
mod tests {
    /// The whole point of the `dirs` swap is that Windows users' existing
    /// files keep resolving. If this ever drifts, everyone silently starts
    /// from a blank config.
    #[cfg(windows)]
    #[test]
    fn windows_paths_are_still_the_env_vars_the_tauri_build_used() {
        for (var, resolved) in [("APPDATA", super::config_file()), ("LOCALAPPDATA", super::log_file())] {
            let expected = std::path::PathBuf::from(std::env::var(var).expect("set on Windows")).join(super::IDENTIFIER);
            assert!(resolved.expect("should resolve").starts_with(&expected), "{var} moved: {expected:?}");
        }
    }
}
