//! Port of `main/benchmarkLogger.js`: git-tagged CSV logging so runs from
//! different code versions can be compared side by side. Two files, same
//! split the original uses:
//! - a per-generation detail log (`append_benchmark_line`), 5MB-rotated to
//!   `.old` so it doesn't grow unbounded but still keeps some recent history
//! - a per-run summary CSV (`append_run_summary_row`), one row per completed
//!   run, header written once - meant for lining up many runs side by side
//!   (e.g. in a spreadsheet), not generation-by-generation detail.
//!
//! Not a Tauri/UI concern - this is diagnostic tooling for tuning the engine
//! itself (see `docs/PORT_STATUS.md`'s Phase 8 row and the empirical
//! rotation-angle-grid/mutation-rate-cap sweeps that produced some of this
//! codebase's other preserved gotchas), hence living in `nesting` rather
//! than `app`.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// Past this size, the current log rolls to `.old` (overwriting any
/// previous `.old`) and a fresh one starts - bounds total disk use to ~2x
/// this, while still keeping some recent history around instead of just
/// deleting it outright.
pub const BENCHMARK_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

fn rotate_log_if_needed(path: &Path) {
    let Ok(metadata) = fs::metadata(path) else {
        return; // doesn't exist yet - nothing to rotate
    };
    if metadata.len() < BENCHMARK_LOG_MAX_BYTES {
        return;
    }
    let old_path = path.with_extension(match path.extension() {
        Some(ext) => format!("{}.old", ext.to_string_lossy()),
        None => "old".to_string(),
    });
    // Failures here are swallowed the same way `append_benchmark_line`'s own
    // write failure is (a diagnostic log falling behind on rotation
    // shouldn't crash the run it's observing) - but printed to stderr for
    // the same reason too, so a permissions/disk-space problem isn't
    // silently invisible, consistent with this file's other two functions.
    if let Err(e) = fs::remove_file(&old_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("benchmark log rotation: couldn't remove old {}: {e}", old_path.display());
        }
    }
    if let Err(e) = fs::rename(path, &old_path) {
        eprintln!("benchmark log rotation: couldn't rename {} to {}: {e}", path.display(), old_path.display());
    }
}

static CACHED_GIT_REV: OnceLock<String> = OnceLock::new();

/// `git rev-parse --short HEAD`, with a `-dirty` suffix if
/// `git status --porcelain` isn't empty. Cached for the process's lifetime
/// (matches the original's `cachedGitRev` memoization) - `git` calls aren't
/// free, and this doesn't change mid-run. Falls back to `"unknown"` if git
/// isn't available or this isn't a git checkout at all.
pub fn git_revision() -> &'static str {
    CACHED_GIT_REV.get_or_init(|| {
        let rev = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());

        let Some(mut rev) = rev else {
            return "unknown".to_string();
        };

        let dirty = Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);
        if dirty {
            rev.push_str("-dirty");
        }
        rev
    })
}

/// Appends one line (a per-generation detail row, or any free-form
/// diagnostic line) to `path`, rotating first if it's grown past
/// `BENCHMARK_LOG_MAX_BYTES`. Failures are swallowed (matches the original -
/// a benchmark log write failing shouldn't crash the run it's observing),
/// but printed to stderr so they're not silently invisible either.
pub fn append_benchmark_line(path: &Path, line: &str) {
    rotate_log_if_needed(path);
    if let Err(e) = append_line_to(path, line) {
        eprintln!("benchmark log write failed: {e}");
    }
}

/// Appends one row to a per-run summary CSV at `path`, writing `header`
/// first if the file doesn't exist yet. `fields` are joined with commas -
/// callers are responsible for pre-formatting/escaping any field that might
/// itself contain a comma (none of this project's benchmark fields do:
/// numbers, git revs, enum names).
pub fn append_run_summary_row(path: &Path, header: &str, fields: &[String]) {
    rotate_log_if_needed(path);
    if !path.exists() {
        if let Err(e) = append_line_to(path, header) {
            eprintln!("benchmark runs csv header write failed: {e}");
            return;
        }
    }
    if let Err(e) = append_line_to(path, &fields.join(",")) {
        eprintln!("benchmark runs csv write failed: {e}");
    }
}

fn append_line_to(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_revision_is_never_empty() {
        // whatever the actual result (a real rev, "-dirty" suffixed, or
        // "unknown" if git isn't available), it must never be blank
        assert!(!git_revision().is_empty());
    }

    #[test]
    fn append_run_summary_row_writes_header_once() {
        let dir = std::env::temp_dir().join(format!("bench_log_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("runs.csv");
        let _ = fs::remove_file(&path);

        append_run_summary_row(&path, "a,b,c", &["1".into(), "2".into(), "3".into()]);
        append_run_summary_row(&path, "a,b,c", &["4".into(), "5".into(), "6".into()]);

        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines, vec!["a,b,c", "1,2,3", "4,5,6"]);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_benchmark_line_appends_across_calls() {
        let dir = std::env::temp_dir().join(format!("bench_log_test2_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("detail.log");
        let _ = fs::remove_file(&path);

        append_benchmark_line(&path, "line one");
        append_benchmark_line(&path, "line two");

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "line one\nline two\n");

        fs::remove_dir_all(&dir).ok();
    }
}
