//! Build script — embeds the Windows app icon into the .exe resource.
//!
//! Looks for `assets/icon.ico`. If present, the icon is compiled into the
//! executable so it appears in the taskbar, Start menu, alt-tab list, and
//! window title bar. If absent, the build still succeeds (icon-less, like
//! before) with a `cargo:warning` so the gap is visible in build logs.
//!
//! The icon part is a no-op on non-Windows targets; the release date (below)
//! is set everywhere.

fn main() {
    // Rebuild only when the icon changes.
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // What a Pro licence's `updates_until` is compared with: GITAGENT_RELEASE_DATE
    // if set, else the date of the commit being built (the release commit, in
    // CI), else empty (unknown: every licence covers it). The public key is read
    // by the app with `option_env!`; listed here so changing it rebuilds.
    println!("cargo:rerun-if-env-changed=GITAGENT_RELEASE_DATE");
    println!("cargo:rerun-if-env-changed=GITAGENT_LICENSE_PUBLIC_KEY");
    let date = std::env::var("GITAGENT_RELEASE_DATE")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(commit_date);
    println!(
        "cargo:rustc-env=GITAGENT_RELEASE_DATE={}",
        date.unwrap_or_default()
    );

    #[cfg(target_os = "windows")]
    {
        let icon_path = std::path::Path::new("assets/icon.ico");
        if !icon_path.exists() {
            println!(
                "cargo:warning=assets/icon.ico not found — the .exe will ship without an embedded icon. \
                 Drop a multi-resolution .ico file there to brand the Windows build."
            );
            return;
        }

        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "GitAgent");
        res.set("ProductName", "GitAgent");
        res.set("CompanyName", "Bennekrouf");
        res.set("LegalCopyright", "© Bennekrouf");
        if let Err(e) = res.compile() {
            // Don't hard-fail — the rc.exe/windres dependency isn't always
            // available on every Windows runner. Warn so the gap is visible
            // but let the build go through icon-less.
            println!(
                "cargo:warning=Failed to embed Windows icon resource: {} \
                 (the build will continue without an icon)",
                e
            );
        }
    }
}

/// The committer date of HEAD, `YYYY-MM-DD`.
fn commit_date() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%cs"])
        .output()
        .ok()?;
    let date = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && date.len() == 10).then_some(date)
}
