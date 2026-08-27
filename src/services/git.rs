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
/// Which of `wanted` still need `git add`.
///
/// A porcelain code is two columns: index, then worktree. A space in the
/// worktree column means the path is already fully staged and there is
/// nothing left to add — and calling `git add` on one of those is not merely
/// redundant, it fails outright when the path is also gitignored, because git
/// treats it as a new file being added rather than a tracked one being
/// removed. A staged deletion of an ignored file hits exactly that.
///
/// Anything we cannot classify is included, so unknown states behave as before.
pub fn needs_staging(changes: &[FileChange], wanted: &[String]) -> Vec<String> {
    wanted
        .iter()
        .filter(|path| {
            changes
                .iter()
                .find(|c| &c.path == *path)
                .map(|c| c.code.chars().nth(1).unwrap_or(' ') != ' ')
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

pub async fn add(repo: &str, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["add", "--"];
    let owned: Vec<&str> = paths.iter().map(|p| p.as_str()).collect();
    args.extend(owned);
    run(repo, "git", &args).await
}

/// Paths with something staged: the index column is neither a space (nothing
/// staged) nor `?` (untracked, so nothing is in the index at all).
pub fn staged_paths(changes: &[FileChange]) -> Vec<String> {
    changes
        .iter()
        .filter(|c| {
            let index = c.code.chars().next().unwrap_or(' ');
            index != ' ' && index != '?'
        })
        .map(|c| c.path.clone())
        .collect()
}

/// Staged work the human did not approve — what has to leave the index before
/// committing it would be honest.
pub fn staged_but_unapproved(changes: &[FileChange], approved: &[String]) -> Vec<String> {
    staged_paths(changes)
        .into_iter()
        .filter(|path| !approved.contains(path))
        .collect()
}

/// Takes paths out of the index, leaving the working tree untouched.
pub async fn unstage(repo: &str, paths: &[String]) -> Result<String, String> {
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(paths.iter().map(|p| p.as_str()));
    run(repo, "git", &args).await
}

/// Commits whatever is in the index.
///
/// Deliberately no pathspec: `git commit -- <paths>` would commit the *working
/// tree* content of those paths and throw away any partial staging, so a
/// carefully `git add -p`-ed hunk would silently become the whole file. The
/// caller makes the index match the approval instead — staging what was
/// checked, unstaging what was not — and then this commits it as it stands.
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

/// Spawns `program args...` — in `cwd` when given — writes `stdin` to it
/// first if non-empty, and streams combined stdout+stderr to `on_line` one
/// line at a time as it arrives, not only once the process exits. Every
/// other runner in this module is a thin wrapper over this with a no-op
/// callback, so there is exactly one place that spawns a child and reads its
/// output — a live "Running…" log and a final captured one are the same
/// data, read at a different time.
pub async fn run_streaming(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    stdin: &str,
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;

    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(if stdin.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (false, format!("could not run `{program}`: {e}")),
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

    // Both streams feed one channel so a line reaches `on_line` in the order
    // it actually arrived, rather than all of stdout followed by all of
    // stderr the way a single buffered `.output()` read would show it.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let out_tx = tx.clone();
    let out_task = tokio::spawn(async move {
        if let Some(out) = stdout {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = out_tx.send(line);
            }
        }
    });
    let err_task = tokio::spawn(async move {
        if let Some(err) = stderr {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(line);
            }
        }
    });

    let mut collected = String::new();
    while let Some(line) = rx.recv().await {
        on_line(&line);
        if !collected.is_empty() {
            collected.push('\n');
        }
        collected.push_str(&line);
    }
    let _ = out_task.await;
    let _ = err_task.await;

    let ok = matches!(child.wait().await, Ok(status) if status.success());
    (ok, cap(collected.trim()))
}

/// Runs a remedy command anywhere on the machine, returning success plus the
/// combined output. Unlike `run`, the output is wanted either way — a failed
/// `brew install` explains itself in stdout as often as in stderr.
pub async fn run_command(program: &str, args: &[String]) -> (bool, String) {
    run_streaming(program, args, None, "", &mut |_| {}).await
}

/// Runs a shell command in `repo`, optionally writing `stdin` to it first,
/// reporting each line of output as it arrives — a step that streams into
/// the run's live log, rather than only showing it once the command
/// finishes. Returns success plus the full combined output either way.
pub async fn run_shell_streaming(
    repo: &str,
    command: &str,
    stdin: &str,
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    run_streaming(
        "sh",
        &["-c".to_string(), command.to_string()],
        Some(repo),
        stdin,
        on_line,
    )
    .await
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

    /// No built-in step may ever discard local work. `git checkout` and
    /// `git pull --ff-only` already refuse rather than overwrite, and that is
    /// the only shape a step is allowed to take — this pins it down so a
    /// future step cannot quietly reach for a hard reset, a forced clean, a
    /// forced checkout, a forced push, or force-deleting a branch instead.
    ///
    /// This only covers steps gitagent itself runs. A "Run a script" or
    /// "Run on a server" step executes whatever command the user configured,
    /// which can be anything — that is its whole point, and no scan of this
    /// crate can constrain it.
    ///
    /// The banned flags are assembled at runtime, not written out literally —
    /// otherwise this test's own source would flag itself when it scans
    /// `git.rs`.
    #[test]
    fn no_builtin_step_ever_runs_a_destructive_git_command() {
        let dash = "-";
        let banned: Vec<String> = vec![
            format!("reset {dash}{dash}hard"),
            format!("clean {dash}f"),
            format!("clean {dash}x"),
            format!("checkout {dash}f"),
            format!("checkout {dash}{dash}force"),
            format!("push {dash}{dash}force"),
            format!("push {dash}f"),
            format!("branch {dash}D"),
            format!("branch {dash}{dash}delete {dash}{dash}force"),
        ];
        for (path, source) in [
            ("src/services/git.rs", include_str!("git.rs")),
            ("src/services/flow.rs", include_str!("flow.rs")),
            ("src/services/review.rs", include_str!("review.rs")),
        ] {
            for flag in &banned {
                assert!(
                    !source.contains(flag.as_str()),
                    "{path} contains `{flag}` — a built-in step must never discard local work"
                );
            }
        }
    }

    fn change(code: &str, path: &str) -> FileChange {
        FileChange {
            code: code.into(),
            path: path.into(),
        }
    }

    #[test]
    fn an_already_staged_deletion_is_not_added_again() {
        // ais_tom_platform: `D ` plus a .gitignore rule made `git add` fail.
        let changes = vec![change("D ", "logic_apps/local.settings.json")];
        let wanted = vec!["logic_apps/local.settings.json".to_string()];
        assert!(needs_staging(&changes, &wanted).is_empty());
    }

    #[test]
    fn untracked_files_are_not_in_the_index() {
        let changes = vec![change("??", "new.rs"), change("A ", "added.rs")];
        assert_eq!(staged_paths(&changes), vec!["added.rs".to_string()]);
    }

    #[test]
    fn a_staged_file_nobody_approved_is_singled_out() {
        // Unchecking a file that was already staged has to actually exclude it.
        let changes = vec![change("M ", "wanted.rs"), change("M ", "sneaky.rs")];
        let approved = vec!["wanted.rs".to_string()];
        assert_eq!(
            staged_but_unapproved(&changes, &approved),
            vec!["sneaky.rs".to_string()]
        );
    }

    #[test]
    fn nothing_is_unstaged_when_the_index_already_matches() {
        let changes = vec![change("M ", "a.rs"), change(" M", "b.rs")];
        let approved = vec!["a.rs".to_string(), "b.rs".to_string()];
        assert!(staged_but_unapproved(&changes, &approved).is_empty());
    }

    #[test]
    fn a_staged_deletion_counts_as_staged() {
        let changes = vec![change("D ", "logic_apps/local.settings.json")];
        assert_eq!(staged_paths(&changes).len(), 1);
    }

    #[test]
    fn an_unstaged_deletion_still_needs_staging() {
        let changes = vec![change(" D", "gone.rs")];
        let wanted = vec!["gone.rs".to_string()];
        assert_eq!(needs_staging(&changes, &wanted), wanted);
    }

    #[test]
    fn untracked_and_modified_files_need_staging() {
        let changes = vec![change("??", "new.rs"), change(" M", "edited.rs")];
        let wanted = vec!["new.rs".to_string(), "edited.rs".to_string()];
        assert_eq!(needs_staging(&changes, &wanted), wanted);
    }

    #[test]
    fn a_fully_staged_change_needs_nothing_further() {
        let changes = vec![change("M ", "done.rs"), change("A ", "added.rs")];
        let wanted = vec!["done.rs".to_string(), "added.rs".to_string()];
        assert!(needs_staging(&changes, &wanted).is_empty());
    }

    #[test]
    fn a_partly_staged_file_is_staged_the_rest_of_the_way() {
        let changes = vec![change("MM", "half.rs")];
        let wanted = vec!["half.rs".to_string()];
        assert_eq!(needs_staging(&changes, &wanted), wanted);
    }

    #[test]
    fn a_path_git_did_not_report_is_attempted_anyway() {
        assert_eq!(
            needs_staging(&[], &["mystery.rs".to_string()]),
            vec!["mystery.rs".to_string()]
        );
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
