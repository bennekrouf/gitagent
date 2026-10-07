//! Making the app's `PATH` look like your terminal's.
//!
//! An app launched from Finder, the Dock or a `.dmg` does not inherit the
//! shell environment. It gets roughly `/usr/bin:/bin:/usr/sbin:/sbin`, which
//! is why `gh` reports as "not installed" in the packaged build and works
//! perfectly under `cargo run` — the terminal passed its own `PATH` down.
//!
//! On this machine `gh`, `az`, `git` and `ollama` all live in
//! `/opt/homebrew/bin` and `cargo` in `~/.cargo/bin`; a bundle sees none of
//! them. So at startup we ask the login shell what it thinks `PATH` should be
//! and adopt it, falling back to the usual locations when that fails.
//!
//! A *non-interactive* login shell is used deliberately: it reads the profile
//! where `brew shellenv` lives, without the risk that an interactive shell
//! blocks on something and hangs startup before a window ever appears.

/// Environment variables worth carrying across from the shell.
///
/// `PATH` is not the only thing a bundle loses. Anything exported from a
/// profile — an API key, a personal access token — is equally invisible, so
/// "export it and restart the app" quietly never works outside a terminal.
///
/// This is an allowlist rather than a wholesale copy: only variables the app
/// actually reads. Nothing new is exposed either way, since launching from a
/// terminal would hand over the entire environment.
const INHERITED: &[&str] = &[
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY",
    "MISTRAL_API_KEY",
    "COHERE_API_KEY",
    "GROQ_API_KEY",
    "OPENROUTER_API_KEY",
    "AZURE_DEVOPS_EXT_PAT",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    // Not credentials, but the same problem: an opt-out exported from a
    // profile is invisible to a bundle launched from Finder unless it is
    // carried across, so "I set DISABLE_UPDATE_CHECK" quietly did nothing
    // outside a terminal. See telemetry.rs for what they switch off.
    "DISABLE_UPDATE_CHECK",
    "DO_NOT_TRACK",
    "GITAGENT_NO_TELEMETRY",
];

/// Directories worth having even if the shell tells us nothing.
const FALLBACKS: &[&str] = &[
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// Adopts the login shell's `PATH`, merged with whatever we already have.
/// Call once, before anything runs a subprocess.
/// Splits `KEY=value` lines into pairs, ignoring blanks and malformed rows.
pub fn parse_env_lines(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

/// Carries the allowlisted variables over from the login shell.
///
/// Anything already set in the real environment wins — a value passed on the
/// command line or by a parent process is more deliberate than a profile.
pub fn adopt_login_env() {
    let Some(text) = login_shell_env() else {
        return;
    };
    for (key, value) in adoptable(&text) {
        if std::env::var(&key).ok().filter(|v| !v.is_empty()).is_none() {
            std::env::set_var(&key, value);
        }
    }
}

/// The variables worth adopting out of the shell's output.
///
/// Filtering here and not only when building the probe script is the point.
/// `-lc` runs the login profile, and profiles talk: nvm, conda, direnv and
/// corporate banners all print to stdout, and anything among that noise
/// shaped like `KEY=value` would otherwise be adopted as if we had asked for
/// it. The allowlist has to govern what is accepted, not merely what is
/// requested.
fn adoptable(text: &str) -> Vec<(String, String)> {
    parse_env_lines(text)
        .into_iter()
        .filter(|(key, _)| INHERITED.contains(&key.as_str()))
        .collect()
}

#[cfg(unix)]
fn login_shell_env() -> Option<String> {
    use std::process::{Command, Stdio};

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let script = INHERITED
        .iter()
        .map(|key| format!("printf '{key}=%s\\n' \"${key}\""))
        .collect::<Vec<_>>()
        .join("; ");

    let output = Command::new(&shell)
        .args(["-lc", &script])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(not(unix))]
fn login_shell_env() -> Option<String> {
    None
}

pub fn adopt_login_path() {
    let current = std::env::var("PATH").unwrap_or_default();
    let login = login_shell_path();
    let cargo_bin = dirs::home_dir().map(|h| h.join(".cargo/bin").to_string_lossy().to_string());

    let mut extras: Vec<String> = FALLBACKS.iter().map(|s| s.to_string()).collect();
    if let Some(bin) = cargo_bin {
        extras.push(bin);
    }

    let merged = merge_paths(&current, login.as_deref(), &extras);
    std::env::set_var("PATH", merged);
}

/// Login shell first (it is the informed answer), then what we already had,
/// then the fallbacks. Order is preserved and duplicates are dropped, so the
/// first place a tool is found stays the one that wins.
pub fn merge_paths(current: &str, login: Option<&str>, extras: &[String]) -> String {
    merge_path_lists(
        ':',
        &[
            login.unwrap_or_default().to_string(),
            current.to_string(),
            extras.join(":"),
        ],
    )
}

/// Joins `sep`-separated lists in order, dropping blanks and repeats.
fn merge_path_lists(sep: char, sources: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = vec![];

    for source in sources {
        for entry in source.split(sep) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if seen.insert(entry.to_string()) {
                out.push(entry.to_string());
            }
        }
    }
    out.join(&sep.to_string())
}

#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    use std::process::{Command, Stdio};

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = Command::new(&shell)
        // -l reads the profile (where `brew shellenv` normally is); no -i, so
        // an interactive prompt can never block startup.
        .args(["-lc", "printf '%s' \"$PATH\""])
        .stdin(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(not(unix))]
fn login_shell_path() -> Option<String> {
    None
}

/// What to actually start when asked for `program`.
///
/// On Windows, starting a bare name only ever finds `name.exe`: unlike a
/// shell, Rust does not consult `PATHEXT`. The Azure CLI installs as
/// `az.cmd`, so `az` read as "program not found" here while every terminal
/// (and every tool that starts commands through a shell) found it. Looking
/// the name up ourselves gives a full path, which starts correctly whatever
/// its extension. Elsewhere, and for anything already a path, the name is
/// handed back unchanged.
pub fn resolve_program(program: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let lookup = || {
            find_program(
                program,
                &std::env::var("PATH").unwrap_or_default(),
                &std::env::var("PATHEXT").unwrap_or_default(),
                ';',
                &|p: &std::path::Path| p.is_file(),
            )
        };
        if let Some(found) = lookup() {
            return found;
        }
        // Installed since GitAgent started? The PATH it was handed predates
        // the installer, so re-read the one Windows has now and look again.
        if refresh_windows_path() {
            if let Some(found) = lookup() {
                return found;
            }
        }
    }
    std::path::PathBuf::from(program)
}

/// The first `program` in `path`, trying each extension in `pathext`.
///
/// A name with no extension is only ever matched with one. The Azure CLI
/// ships an extensionless `az` beside `az.cmd`: a shell script for Git Bash
/// that Windows cannot start, so finding it first would trade "not found"
/// for a stranger failure.
// Only called on Windows; built everywhere so the tests run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn find_program(
    program: &str,
    path: &str,
    pathext: &str,
    sep: char,
    is_file: &dyn Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    if program.is_empty() || program.contains(['/', '\\']) {
        return None;
    }
    let pathext = if pathext.trim().is_empty() {
        ".COM;.EXE;.BAT;.CMD"
    } else {
        pathext
    };
    let exts: Vec<&str> = pathext
        .split(';')
        .map(str::trim)
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .collect();
    let has_ext = exts.iter().any(|e| {
        program
            .to_ascii_lowercase()
            .ends_with(&e.to_ascii_lowercase())
    });
    let names: Vec<String> = if has_ext {
        vec![program.to_string()]
    } else {
        exts.iter()
            .map(|e| format!("{program}{}", e.to_ascii_lowercase()))
            .collect()
    };

    path.split(sep)
        .map(|dir| dir.trim().trim_matches('"'))
        .filter(|dir| !dir.is_empty())
        .flat_map(|dir| {
            names
                .iter()
                .map(move |name| std::path::Path::new(dir).join(name))
        })
        .find(|candidate| is_file(candidate))
}

/// Merges the PATH Windows currently has on record into ours. Returns
/// whether anything was added.
///
/// A process keeps the environment it was started with, and GitAgent is
/// usually started by Explorer, so a CLI installed while it is open stays
/// invisible until a restart. The registry holds the up-to-date value. Read
/// at most every thirty seconds: a tool that is genuinely missing is looked
/// up on every refresh of every repository.
#[cfg(windows)]
fn refresh_windows_path() -> bool {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let Ok(mut last) = LAST.lock() else {
            return false;
        };
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return false;
        }
        *last = Some(Instant::now());
    }

    let machine =
        registry_path(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment");
    let user = registry_path(r"HKCU\Environment");
    let fresh: Vec<String> = [machine, user].into_iter().flatten().collect();
    if fresh.is_empty() {
        return false;
    }

    let current = std::env::var("PATH").unwrap_or_default();
    let merged = merge_path_lists(';', &[current.clone(), fresh.join(";")]);
    if merged == current {
        return false;
    }
    std::env::set_var("PATH", merged);
    true
}

#[cfg(windows)]
fn registry_path(key: &str) -> Option<String> {
    use std::process::{Command, Stdio};

    let output = Command::new("reg")
        .args(["query", key, "/v", "Path"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = parse_reg_value(&String::from_utf8_lossy(&output.stdout))?;
    Some(expand_percent_vars(&raw, &|name| std::env::var(name).ok()))
}

/// The data of the value `reg query` printed, e.g. out of
/// `    Path    REG_EXPAND_SZ    C:\Tools;%USERPROFILE%\bin`.
// Only called on Windows; built everywhere so the tests run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_reg_value(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        ["REG_EXPAND_SZ", "REG_SZ"].iter().find_map(|kind| {
            let (_, data) = line.split_once(kind)?;
            let data = data.trim();
            (!data.is_empty()).then(|| data.to_string())
        })
    })
}

/// Expands `%NAME%` the way Windows does for a `REG_EXPAND_SZ` value; a
/// name that is not set is left as written.
// Only called on Windows; built everywhere so the tests run everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn expand_percent_vars(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match lookup(name).filter(|_| !name.is_empty()) {
                    Some(value) => out.push_str(&value),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extras() -> Vec<String> {
        vec!["/opt/homebrew/bin".to_string(), "/usr/bin".to_string()]
    }

    #[test]
    fn exported_secrets_are_read_out_of_the_shell_output() {
        let pairs = parse_env_lines("DEEPSEEK_API_KEY=sk-abc\nGH_TOKEN=ghp_1\n");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("DEEPSEEK_API_KEY".into(), "sk-abc".into()));
    }

    #[test]
    fn a_variable_that_is_not_set_contributes_nothing() {
        // The shell prints `KEY=` for anything unset.
        assert!(parse_env_lines("DEEPSEEK_API_KEY=\nGH_TOKEN=\n").is_empty());
    }

    #[test]
    fn a_value_containing_an_equals_sign_survives_intact() {
        let pairs = parse_env_lines("AZURE_DEVOPS_EXT_PAT=abc=def==");
        assert_eq!(pairs[0].1, "abc=def==");
    }

    #[test]
    fn malformed_rows_are_skipped_rather_than_panicking() {
        assert!(parse_env_lines("no equals sign here\n\n").is_empty());
    }

    #[test]
    fn only_variables_the_app_reads_are_carried_over() {
        // An allowlist, not a wholesale copy of the shell environment.
        assert!(INHERITED.contains(&"DEEPSEEK_API_KEY"));
        assert!(INHERITED.contains(&"AZURE_DEVOPS_EXT_PAT"));
        assert!(!INHERITED.contains(&"AWS_SECRET_ACCESS_KEY"));
    }

    /// A login profile is not a clean pipe. Whatever it prints alongside the
    /// answer must not be adopted just for being shaped like an assignment.
    #[test]
    fn a_chatty_login_profile_cannot_smuggle_variables_in() {
        let stdout = "nvm: using node v20.11.0\n\
                      CONDA_PREFIX=/opt/miniconda3\n\
                      AWS_SECRET_ACCESS_KEY=leaked\n\
                      DEEPSEEK_API_KEY=sk-real\n\
                      AZURE_DEVOPS_EXT_PAT=\n\
                      GH_TOKEN=\n";
        let found = adoptable(stdout);
        let adopted: Vec<&str> = found.iter().map(|(k, _)| k.as_str()).collect();
        // Only what was asked for, and only what was actually set.
        assert_eq!(adopted, vec!["DEEPSEEK_API_KEY"]);
    }

    #[test]
    fn the_requested_variables_still_come_through() {
        let stdout = "DEEPSEEK_API_KEY=sk-abc\nAZURE_DEVOPS_EXT_PAT=pat-1\nGH_TOKEN=ghp_1\n";
        assert_eq!(adoptable(stdout).len(), 3);
    }

    #[test]
    fn a_bundles_bare_path_gains_the_places_tools_actually_live() {
        // The exact failure: gh is in /opt/homebrew/bin, the bundle sees neither.
        let merged = merge_paths("/usr/bin:/bin", None, &extras());
        assert!(merged.split(':').any(|p| p == "/opt/homebrew/bin"));
    }

    #[test]
    fn the_login_shell_is_believed_before_anything_else() {
        let merged = merge_paths("/usr/bin", Some("/opt/homebrew/bin:/usr/bin"), &extras());
        assert!(merged.starts_with("/opt/homebrew/bin"));
    }

    #[test]
    fn a_directory_never_appears_twice() {
        let merged = merge_paths(
            "/usr/bin:/bin",
            Some("/usr/bin:/opt/homebrew/bin"),
            &extras(),
        );
        let count = merged.split(':').filter(|p| *p == "/usr/bin").count();
        assert_eq!(count, 1, "got {merged}");
    }

    #[test]
    fn order_decides_which_copy_of_a_tool_wins() {
        let merged = merge_paths("/usr/bin", Some("/opt/homebrew/bin:/usr/bin"), &extras());
        let entries: Vec<&str> = merged.split(':').collect();
        let brew = entries
            .iter()
            .position(|p| *p == "/opt/homebrew/bin")
            .unwrap();
        let usr = entries.iter().position(|p| *p == "/usr/bin").unwrap();
        assert!(brew < usr);
    }

    #[test]
    fn empty_and_ragged_input_does_not_produce_empty_entries() {
        let merged = merge_paths("::/usr/bin: :", Some(""), &extras());
        assert!(!merged.split(':').any(|p| p.trim().is_empty()));
    }

    #[test]
    fn nothing_at_all_still_yields_a_usable_path() {
        let merged = merge_paths("", None, &extras());
        assert!(merged.split(':').any(|p| p == "/usr/bin"));
    }

    /// Not an assertion about this machine — just proof the probe returns
    /// something shaped like a PATH when a shell is available.
    #[test]
    fn the_login_shell_probe_returns_a_path_or_nothing() {
        if let Some(path) = login_shell_path() {
            assert!(path.contains('/'), "got {path}");
        }
    }

    fn on_disk(files: &'static [&'static str]) -> impl Fn(&std::path::Path) -> bool {
        move |p| files.iter().any(|f| std::path::Path::new(f) == p)
    }

    /// The reported failure: the Azure CLI is `az.cmd`, and starting `az`
    /// without looking at PATHEXT never finds it.
    #[test]
    fn a_batch_file_is_found_by_its_bare_name() {
        let found = find_program(
            "az",
            r"C:\Windows;C:\CLI2\wbin",
            ".COM;.EXE;.BAT;.CMD",
            ';',
            &on_disk(&[r"C:\CLI2\wbin/az.cmd"]),
        );
        assert_eq!(
            found,
            Some(std::path::PathBuf::from(r"C:\CLI2\wbin/az.cmd"))
        );
    }

    /// `wbin` also holds an extensionless `az` for Git Bash, which Windows
    /// cannot start.
    #[test]
    fn the_extensionless_shell_script_beside_it_is_never_picked() {
        let found = find_program(
            "az",
            r"C:\CLI2\wbin",
            ".EXE;.CMD",
            ';',
            &on_disk(&[r"C:\CLI2\wbin/az", r"C:\CLI2\wbin/az.cmd"]),
        );
        assert_eq!(
            found,
            Some(std::path::PathBuf::from(r"C:\CLI2\wbin/az.cmd"))
        );
    }

    #[test]
    fn path_order_wins_over_extension_order() {
        let found = find_program(
            "tool",
            "first;second",
            ".EXE;.CMD",
            ';',
            &on_disk(&["first/tool.cmd", "second/tool.exe"]),
        );
        assert_eq!(found, Some(std::path::PathBuf::from("first/tool.cmd")));
    }

    #[test]
    fn an_empty_pathext_falls_back_to_the_windows_default() {
        let found = find_program("az", "bin", "", ';', &on_disk(&["bin/az.cmd"]));
        assert!(found.is_some());
    }

    #[test]
    fn a_name_that_already_has_its_extension_is_taken_as_written() {
        let found = find_program("git.exe", "bin", ".EXE", ';', &on_disk(&["bin/git.exe"]));
        assert_eq!(found, Some(std::path::PathBuf::from("bin/git.exe")));
    }

    #[test]
    fn a_path_is_left_for_the_caller_to_start_as_is() {
        assert!(find_program(r"C:\x\az", "bin", ".CMD", ';', &|_| true).is_none());
    }

    #[test]
    fn nothing_on_the_path_is_none_not_a_guess() {
        assert!(find_program("az", "bin", ".CMD", ';', &|_| false).is_none());
    }

    #[test]
    fn the_value_is_read_out_of_reg_query_output() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    Path    REG_EXPAND_SZ    \
                   C:\\Tools;%USERPROFILE%\\bin\r\n\r\n";
        assert_eq!(
            parse_reg_value(out).as_deref(),
            Some(r"C:\Tools;%USERPROFILE%\bin")
        );
    }

    #[test]
    fn percent_variables_expand_and_unknown_ones_survive() {
        let lookup = |name: &str| (name == "USERPROFILE").then(|| r"C:\Users\mb".to_string());
        assert_eq!(
            expand_percent_vars(r"%USERPROFILE%\bin;%NOPE%;50%", &lookup),
            r"C:\Users\mb\bin;%NOPE%;50%"
        );
    }

    #[test]
    fn windows_lists_merge_on_semicolons() {
        let merged = merge_path_lists(';', &["a;b".into(), "b;c".into()]);
        assert_eq!(merged, "a;b;c");
    }
}
