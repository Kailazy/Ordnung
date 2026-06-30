//! Update check — ask GitHub Releases whether a newer Ordnung is published.
//!
//! Engine-shaped per `ordnung-architecture`: pure library, no UI, no policy,
//! no `println!`. The caller (GUI) decides when to run the check, supplies its
//! own running version, and chooses how to surface a hit (we just open the
//! browser at the release page — the build isn't an auto-updater). The check is
//! a single unauthenticated GitHub API request, well under the 60 req/hr/IP
//! limit at once-per-launch.

use crate::error::{Error, Result};
use std::time::Duration;

/// GitHub "latest release" endpoint for the public Ordnung repo. Returns the
/// most recent non-prerelease, non-draft release as JSON.
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/Kailazy/Ordnung/releases/latest";

/// A published release newer than the running build, ready for the GUI to offer
/// as a download. `version` is the tag with any leading `v` stripped (e.g.
/// `0.0.3`); `url` is the human-facing release page to open in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub url: String,
}

/// Ask GitHub for the latest release and compare it against `current` (the
/// running build's `CARGO_PKG_VERSION`). Returns:
/// - `Ok(Some(info))` when the published tag parses as strictly newer,
/// - `Ok(None)` when we're up to date (or the tag isn't newer / can't be parsed),
/// - `Err(_)` only on a network/transport failure — the caller treats this as
///   "couldn't check" and stays silent rather than alarming the user.
pub fn check_latest(current: &str) -> Result<UpdateOutcome> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let resp = agent
        .get(LATEST_RELEASE_URL)
        // GitHub rejects requests without a User-Agent; identify ourselves.
        .set("User-Agent", "Ordnung-update-check")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| Error::Network(e.to_string()))?;
    let body: serde_json::Value = resp
        .into_json()
        .map_err(|e| Error::Network(e.to_string()))?;
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let latest = tag.trim_start_matches('v').trim();
    if latest.is_empty() {
        return Ok(UpdateOutcome::UpToDate);
    }
    if is_newer(latest, current) {
        // Prefer the release page; fall back to the repo's releases listing.
        let url = body
            .get("html_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("https://github.com/Kailazy/Ordnung/releases/latest")
            .to_string();
        Ok(UpdateOutcome::Update(UpdateInfo {
            version: latest.to_string(),
            url,
        }))
    } else {
        Ok(UpdateOutcome::UpToDate)
    }
}

/// Result of a successful check: either an available update or "you're current".
/// Distinct from the `Err` case, which means the check itself failed to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    Update(UpdateInfo),
    UpToDate,
}

/// True when `latest` is a strictly greater version than `current`. Both are
/// dotted numeric strings (`0.0.3`); a pre-release suffix (`0.1.0-rc1`) is
/// ignored — we compare on the numeric release components only. An unparseable
/// component counts as 0, and a string that yields no numbers never wins, so a
/// malformed tag can't spuriously trigger an "update available" prompt.
fn is_newer(latest: &str, current: &str) -> bool {
    let a = parse_version(latest);
    let b = parse_version(current);
    a > b
}

/// Parse a dotted version into comparable numeric components, dropping any
/// `-suffix`/`+build` and treating non-numeric parts as 0. `0.0.3` → `[0,0,3]`.
fn parse_version(v: &str) -> Vec<u64> {
    let core = v
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    core.split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_patch_minor_major() {
        assert!(is_newer("0.0.3", "0.0.2"));
        assert!(is_newer("0.1.0", "0.0.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn equal_or_older_is_not_newer() {
        assert!(!is_newer("0.0.2", "0.0.2"));
        assert!(!is_newer("0.0.1", "0.0.2"));
        assert!(!is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn shorter_versions_compare_by_components() {
        // [1] vs [1,0,0]: Vec ordering makes the longer one greater, so a bare
        // "1" is *not* newer than "1.0.0" — and "1.0.1" is newer than "1".
        assert!(!is_newer("1", "1.0.0"));
        assert!(is_newer("1.0.1", "1"));
    }

    #[test]
    fn prerelease_suffix_ignored() {
        // The numeric core wins; the -rc suffix is dropped on both sides.
        assert!(is_newer("0.1.0-rc1", "0.0.9"));
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
    }

    #[test]
    fn garbage_never_wins() {
        assert!(!is_newer("", "0.0.1"));
        assert!(!is_newer("not-a-version", "0.0.1"));
    }

    #[test]
    fn parse_strips_v_prefix() {
        assert_eq!(parse_version("v0.0.2"), vec![0, 0, 2]);
        assert_eq!(parse_version("0.0.2"), vec![0, 0, 2]);
    }
}
