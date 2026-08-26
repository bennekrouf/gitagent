//! Thin wrappers over `git` and `gh` as subprocesses.
//!
//! Same posture as ais-monitor's use of the `az` CLI: no reimplementation of
//! git plumbing or GitHub auth: the user already has both configured, so shell
//! out and read the exit code. Every function here is deterministic — no model
//! is involved, and the graph treats these as free.

use std::process::Stdio;
use tokio::process::Command;

/// Anything larger than this gets truncated before it reaches a model. A diff
/// past this size is a signal in itself: it wants splitting, not summarising.
pub const DIFF_CAP: usize = 60_000;

#[derive(Clone, PartialEq, Debug)]
pub struct FileChange {
    /// Two-character porcelain code, e.g. " M", "A ", "??".
    pub code: String,
    pub path: String,
}

impl FileChange {
    pub fn is_untracked(&self) -> bool {
        self.code.trim() == "??"
    }

    /// The porcelain code as a word, for the approval list.
    pub fn note(&self) -> &'static str {
        let code = self.code.trim();
        if code == "??" {
            "new"
        } else if code.contains('D') {
            "deleted"
        } else if code.contains('R') {
            "renamed"
        } else if code.contains('A') {
            "added"
        } else {
            "modified"
        }
    }
}

/// Runs a command in `repo` and returns stdout, or stderr as the error.
pub async fn run(repo: &str, program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("could not run `{program}`: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            detail.trim()
        ))
    }
}

pub async fn current_branch(repo: &str) -> Result<String, String> {
    Ok(run(repo, "git", &["branch", "--show-current"])
        .await?
        .trim()
        .to_string())
}

pub async fn head_sha(repo: &str) -> Result<String, String> {
    Ok(run(repo, "git", &["rev-parse", "--short", "HEAD"])
        .await?
        .trim()
        .to_string())
}

/// Branch names that must never receive a direct commit, whatever the base
/// detection below concludes. Belt and braces: base detection reads repository
/// config, and repository config can be wrong or absent.
pub const PROTECTED: &[&str] = &["master", "main", "trunk", "develop", "default"];

pub fn is_protected(branch: &str) -> bool {
    PROTECTED.contains(&branch.trim().to_lowercase().as_str())
}

pub async fn remote_url(repo: &str) -> Option<String> {
    run(repo, "git", &["remote", "get-url", "origin"])
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The branch a pull request should target, and how that was worked out.
///
/// Only **remote-tracking** refs count. A local branch proves nothing about
/// the remote — a stale local `main` in a repository whose default is `master`
/// is common, and trusting it once caused this flow to commit straight to
/// `master` because current-branch and detected-base disagreed.
pub async fn default_remote_branch(repo: &str) -> (String, String) {
    if let Ok(out) = run(
        repo,
        "git",
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    {
        if let Some(name) = out.trim().strip_prefix("origin/") {
            if !name.is_empty() {
                return (name.to_string(), "origin/HEAD".into());
            }
        }
    }

    for candidate in ["main", "master"] {
        let git_ref = format!("refs/remotes/origin/{candidate}");
        if run(repo, "git", &["rev-parse", "--verify", "--quiet", &git_ref])
            .await
            .is_ok()
        {
            return (candidate.to_string(), format!("origin/{candidate} exists"));
        }
    }

    // No remote-tracking ref at all — an unpushed repository. Fall back to
    // local heads, and say so, because this is the guess-iest branch.
    for candidate in ["main", "master"] {
        let git_ref = format!("refs/heads/{candidate}");
        if run(repo, "git", &["rev-parse", "--verify", "--quiet", &git_ref])
            .await
            .is_ok()
        {
            return (
                candidate.to_string(),
                format!("local {candidate}, no remote ref"),
            );
        }
    }

    ("master".to_string(), "fallback default".into())
}

pub async fn status(repo: &str) -> Result<Vec<FileChange>, String> {
    let out = run(repo, "git", &["status", "--porcelain"]).await?;
    Ok(out
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| FileChange {
            code: l[..2].to_string(),
            // Renames show as "old -> new"; the new path is what we act on.
            path: l[3..]
                .split(" -> ")
                .last()
                .unwrap_or(&l[3..])
                .trim()
                .to_string(),
        })
        .collect())
}

/// Like `run`, but returns stdout even on a non-zero exit. `git diff` uses
/// exit code 1 to mean "there were differences", which is not a failure.
async fn run_lenient(repo: &str, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// A diff for a file git does not track yet, produced without touching the
/// index — `--intent-to-add` would write to it, and scanning must not.
pub async fn untracked_diff(repo: &str, path: &str) -> String {
    run_lenient(
        repo,
        &["diff", "--no-index", "--unified=3", "--", "/dev/null", path],
    )
    .await
}

/// The diff of tracked changes against HEAD, capped.
pub async fn diff(repo: &str) -> Result<String, String> {
    let out = run(repo, "git", &["diff", "HEAD", "--unified=3"]).await?;
    Ok(cap(&out))
}

pub async fn diff_stat(repo: &str) -> Result<String, String> {
    Ok(run(repo, "git", &["diff", "HEAD", "--stat"])
        .await?
        .trim()
        .to_string())
}

pub fn cap(text: &str) -> String {
    if text.len() <= DIFF_CAP {
        return text.to_string();
    }
    let mut end = DIFF_CAP;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[…truncated at {} of {} bytes…]",
        &text[..end],
        DIFF_CAP,
        text.len()
    )
}

pub async fn create_branch(repo: &str, name: &str) -> Result<String, String> {
    run(repo, "git", &["checkout", "-b", name]).await
}

pub async fn branch_exists(repo: &str, name: &str) -> bool {
    run(repo, "git", &["rev-parse", "--verify", name])
        .await
        .is_ok()
}

/// Stages exactly the paths listed — never `git add -A`. The approval step
/// shows this list, so the two must not be able to drift apart.
pub async fn add(repo: &str, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["add", "--"];
    let owned: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
    args.extend(owned);
    run(repo, "git", &args).await
}

pub async fn commit(repo: &str, subject: &str, body: &str) -> Result<String, String> {
    let mut args = vec!["commit", "-m", subject];
    if !body.trim().is_empty() {
        args.push("-m");
        args.push(body);
    }
    run(repo, "git", &args).await
}

pub async fn push(repo: &str, branch: &str) -> Result<String, String> {
    run(repo, "git", &["push", "-u", "origin", branch]).await
}

/// Runs a remedy command anywhere on the machine, returning success plus the
/// combined output. Unlike `run`, the output is wanted either way — a failed
/// `brew install` explains itself in stdout as often as in stderr.
pub async fn run_command(program: &str, args: &[String]) -> (bool, String) {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), cap(text.trim()))
        }
        Err(e) => (false, format!("could not run `{program}`: {e}")),
    }
}

/// Runs a shell command in `repo`, optionally writing `stdin` to it first.
/// Returns success plus the combined output — a failing script explains itself
/// in stdout at least as often as in stderr.
pub async fn run_shell(repo: &str, command: &str, stdin: &str) -> (bool, String) {
    use tokio::io::AsyncWriteExt;

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(repo)
        .stdin(if stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (false, format!("could not start `{command}`: {e}")),
    };

    if !stdin.is_empty() {
        if let Some(mut pipe) = child.stdin.take() {
            let mut answer = stdin.to_string();
            if !answer.ends_with('\n') {
                answer.push('\n');
            }
            let _ = pipe.write_all(answer.as_bytes()).await;
            let _ = pipe.shutdown().await;
        }
    }

    match child.wait_with_output().await {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), cap(text.trim()))
        }
        Err(e) => (false, format!("`{command}` did not finish: {e}")),
    }
}

pub async fn has_gh() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub async fn gh_pr_create(
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let out = run(
        repo,
        "gh",
        &[
            "pr", "create", "--base", base, "--head", head, "--title", title, "--body", body,
        ],
    )
    .await?;
    // gh prints the PR URL on its own line.
    Ok(out
        .lines()
        .find(|l| l.starts_with("https://"))
        .unwrap_or(out.trim())
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_codes_separate_untracked_from_modified() {
        let modified = FileChange {
            code: " M".into(),
            path: "src/lib.rs".into(),
        };
        let untracked = FileChange {
            code: "??".into(),
            path: "notes.txt".into(),
        };
        assert!(!modified.is_untracked());
        assert!(untracked.is_untracked());
    }

    #[test]
    fn capping_leaves_a_short_diff_alone() {
        assert_eq!(cap("small"), "small");
    }

    #[test]
    fn capping_a_long_diff_says_so() {
        let long = "x".repeat(DIFF_CAP + 500);
        let out = cap(&long);
        assert!(out.len() < long.len());
        assert!(out.contains("truncated"));
    }

    #[test]
    fn porcelain_codes_read_as_words_in_the_approval_list() {
        let cases = [
            ("??", "new"),
            (" M", "modified"),
            ("A ", "added"),
            (" D", "deleted"),
            ("R ", "renamed"),
        ];
        for (code, expected) in cases {
            let change = FileChange {
                code: code.into(),
                path: "f".into(),
            };
            assert_eq!(change.note(), expected, "code {code:?}");
        }
    }

    #[test]
    fn the_default_branches_are_all_protected_from_direct_commits() {
        for name in ["master", "main", "trunk", "develop"] {
            assert!(is_protected(name));
        }
    }

    #[test]
    fn protection_ignores_case_and_stray_whitespace() {
        assert!(is_protected("  Master "));
        assert!(is_protected("MAIN"));
    }

    #[test]
    fn a_topic_branch_is_not_protected() {
        assert!(!is_protected("fix/stale-lockfile"));
        assert!(!is_protected("mainline-refactor"));
    }

    #[test]
    fn capping_never_splits_a_multibyte_character() {
        // é is two bytes, so the cap lands mid-character for some inputs.
        let long = "é".repeat(DIFF_CAP);
        let out = cap(&long);
        assert!(out.contains("truncated"));
    }
}
