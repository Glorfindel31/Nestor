//! "There is a newer version" - nothing more.
//!
//! One unauthenticated GET to GitHub's `releases/latest`, a version compare,
//! and the release page's URL for the UI to open in a browser. The app does
//! **not** download or replace its own binary.
//!
//! ponytail: notify-and-open rather than self-replace. Swapping a running
//! .exe means the rename trick on Windows, a restart prompt, and a way to
//! half-brick an install over a flaky connection - for a build that is
//! unsigned either way, so SmartScreen warns on the download regardless.
//! Upgrade path if that ever stops being enough: the `self_update` crate
//! does exactly this plus the swap, and the release assets are already
//! plain binaries it can consume.

use std::time::Duration;

/// Where releases are published. The workflow that creates them lives in
/// `.github/workflows/release.yml`.
const REPO: &str = "Glorfindel31/Nestor";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The tag with its leading `v` stripped, e.g. `2.6.0`.
    pub version: String,
    /// The human release page, not an asset - the user picks their platform.
    pub url: String,
}

#[derive(serde::Deserialize)]
struct LatestRelease {
    tag_name: String,
    html_url: String,
}

/// `Ok(None)` means "already current". Network failure is an `Err` the
/// caller is expected to swallow quietly - an offline shop machine should
/// not be told off once per launch.
///
/// # Errors
/// The request failing, GitHub answering with something that isn't the
/// expected JSON, or the tag not parsing as a version.
pub fn check(current: &str) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = ureq::get(&url)
        // GitHub rejects requests without one.
        .set("User-Agent", concat!("Nestor/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let latest: LatestRelease = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let version = latest.tag_name.trim_start_matches('v').to_string();
    Ok(is_newer(&version, current).then_some(Release { version, url: latest.html_url }))
}

/// Numeric dotted compare. Not semver: these tags are always `vX.Y.Z`, and a
/// pre-release suffix would be a deliberate decision to make here rather
/// than something to guess at now.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
    }
    parts(candidate) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_only_when_it_really_is() {
        assert!(is_newer("2.6.0", "2.5.0"));
        assert!(is_newer("2.5.1", "2.5.0"));
        assert!(is_newer("10.0.0", "9.9.9"), "dotted parts compare as numbers, not text");
        assert!(!is_newer("2.5.0", "2.5.0"));
        assert!(!is_newer("2.4.9", "2.5.0"));
        // A tag that stops parsing partway compares on what it did parse,
        // rather than reading as version zero and nagging forever.
        assert!(!is_newer("2.5.0-rc1", "2.5.0"));
    }

    /// Needs the network, so it is not part of the default run. It is the
    /// only thing that catches GitHub changing the JSON out from under us:
    /// `cargo test -p rustynesting -- --ignored real_release`.
    #[test]
    #[ignore = "hits api.github.com"]
    fn the_real_api_still_answers_in_the_shape_we_parse() {
        let release = check("0.0.0").expect("request should succeed").expect("0.0.0 is older than anything published");
        assert!(release.version.starts_with(char::is_numeric), "got {:?}", release.version);
        assert!(release.url.contains("/releases/tag/"), "got {:?}", release.url);
    }
}
