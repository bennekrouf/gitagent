//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each release and compares the
//! version field to this build's `CARGO_PKG_VERSION`. Designed to be cheap and
//! side-effect-free so it can run in the background at startup.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/gitagent/latest/latest.json";
/// Fallback when `latest.json` has no entry for this OS (e.g. an Intel Mac —
/// only Apple Silicon is built). Sends the user to pick a build by hand
/// instead of at a link that would 404.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

/// Sent on the update check so the download logs can tell a new install
/// (a browser hitting the site) from an existing user updating. Also
/// carries the version, which is what makes per-version adoption
/// visible — the number that says how many people are still on a build
/// with a bug that is already fixed.
const USER_AGENT: &str = concat!("gitagent/", env!("CARGO_PKG_VERSION"), " (updater)");

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
    platforms: Platforms,
}

/// Builds per OS, keyed by package format (`dmg`, `exe_or_msi`, `tarball`…)
/// — not by CPU architecture. A `BTreeMap` so the fallback pick in
/// `platform_url` is the same on every launch; `HashMap` iteration order is
/// randomised per process.
#[derive(Debug, Default, Deserialize)]
struct Platforms {
    #[serde(default)]
    macos: BTreeMap<String, Artifact>,
    #[serde(default)]
    windows: BTreeMap<String, Artifact>,
    #[serde(default)]
    linux: BTreeMap<String, Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    url: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    #[allow(dead_code)]
    pub latest_tag: String,
    /// Direct link to this OS's build, so the banner's button downloads the
    /// binary itself rather than opening a landing page to pick one from.
    ///
    /// The download happens in the user's browser, not here, so there is
    /// nothing for this app to verify a checksum against. `latest.json`
    /// publishes a `sha256` per artifact and this deliberately does not
    /// deserialise it: parsing a checksum that is never checked reads like
    /// integrity checking to the next person to touch the file. Verifying it
    /// means downloading the build in-process first, which is a different
    /// feature.
    pub release_url: String,
}

/// Returns `Some(UpdateInfo)` if a newer release is available, else `None`.
/// Any network / parse failure → `None`. Never panics.
/// Disabled if DISABLE_UPDATE_CHECK environment variable is set.
pub async fn check() -> Option<UpdateInfo> {
    if std::env::var("DISABLE_UPDATE_CHECK").is_ok() {
        return None;
    }

    let current = env!("CARGO_PKG_VERSION");
    let body = reqwest::Client::new()
        .get(LATEST_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let latest: LatestJson = serde_json::from_str(&body).ok()?;
    if is_newer(&latest.version, current) {
        Some(UpdateInfo {
            latest_version: latest.version,
            latest_tag: latest.tag,
            release_url: platform_url(std::env::consts::OS, &latest.platforms),
        })
    } else {
        None
    }
}

/// Formats to offer, best first, per OS. On Linux, AppImage runs on any
/// distribution without installing anything as root; the tarball is what is
/// published today.
fn preferred_formats(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["dmg"],
        "windows" => &["msi", "exe", "exe_or_msi"],
        "linux" => &["appimage", "deb", "tarball"],
        _ => &[],
    }
}

/// The download link for `os`: the preferred format that is published, else
/// any build for that OS, else the landing page.
///
/// `latest.json` keys builds by format, not architecture: looking up
/// `std::env::consts::ARCH` never matched, and the choice silently fell to
/// "whichever entry the map yielded first" — harmless only while each OS
/// publishes a single build.
fn platform_url(os: &str, platforms: &Platforms) -> String {
    let by_format = match os {
        "macos" => &platforms.macos,
        "windows" => &platforms.windows,
        "linux" => &platforms.linux,
        _ => return RELEASES_URL.to_string(),
    };
    preferred_formats(os)
        .iter()
        .find_map(|format| by_format.get(*format))
        .or_else(|| by_format.values().next())
        .map(|a| a.url.clone())
        .filter(|u| !u.is_empty())
        // Marks the hit as coming from an existing install. The banner opens
        // this in the user's browser, so the updater's own User-Agent is not
        // what fetches the file — without the marker the request is
        // indistinguishable from a first-time download off the website.
        // nginx serves the file regardless of the query string.
        .map(|u| format!("{u}?src=updater"))
        .unwrap_or_else(|| RELEASES_URL.to_string())
}

fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av > bv,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platforms(json: &str) -> Platforms {
        serde_json::from_str::<LatestJson>(json).unwrap().platforms
    }

    // The shape published today, plus a second Linux build.
    const FEED: &str = r#"{
        "version": "0.1.59", "tag": "v0.1.59",
        "platforms": {
            "macos": { "dmg": { "url": "https://x/gitagent-macos-arm64.dmg", "sha256": "a" } },
            "windows": { "exe_or_msi": { "url": "https://x/gitagent-setup.exe", "sha256": "b" } },
            "linux": {
                "tarball": { "url": "https://x/gitagent-linux.tar.gz", "sha256": "c" },
                "appimage": { "url": "https://x/gitagent-linux.AppImage", "sha256": "d" }
            }
        }
    }"#;

    #[test]
    fn links_straight_to_the_build_for_each_os() {
        let p = platforms(FEED);
        assert_eq!(platform_url("macos", &p), "https://x/gitagent-macos-arm64.dmg?src=updater");
        assert_eq!(platform_url("windows", &p), "https://x/gitagent-setup.exe?src=updater");
        // Two Linux builds: the preferred one, every time.
        assert_eq!(platform_url("linux", &p), "https://x/gitagent-linux.AppImage?src=updater");
    }

    #[test]
    fn falls_back_to_the_landing_page() {
        let p = platforms(FEED);
        assert_eq!(platform_url("freebsd", &p), RELEASES_URL);
        let no_mac = platforms(r#"{ "version": "1.0.0", "tag": "v1.0.0",
            "platforms": { "linux": { "tarball": { "url": "https://x/a.tar.gz" } } } }"#);
        assert_eq!(platform_url("macos", &no_mac), RELEASES_URL);
    }

    #[test]
    fn unknown_format_still_downloads() {
        let p = platforms(r#"{ "version": "1.0.0", "tag": "v1.0.0",
            "platforms": { "windows": { "zip": { "url": "https://x/a.zip" } } } }"#);
        assert_eq!(platform_url("windows", &p), "https://x/a.zip?src=updater");
    }

    #[test]
    fn semver_comparison() {
        assert!(is_newer("0.1.60", "0.1.59"));
        assert!(!is_newer("0.1.59", "0.1.59"));
        assert!(!is_newer("garbage", "0.1.59"));
    }
}
