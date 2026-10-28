//! Thin wrappers over `git` and `gh` as subprocesses.
//!
//! Same posture as ais-monitor's use of the `az` CLI: no reimplementation of
//! git plumbing or GitHub auth: the user already has both configured, so shell
//! out and read the exit code. Every function here is deterministic — no model
//! is involved, and the graph treats these as free.

use std::process::Stdio;
use tokio::process::Command;
mod streaming;
pub use streaming::*;
mod status;
pub use status::*;
mod remote;
pub use remote::*;

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

/// A git operation that stopped part-way and is waiting to be finished.
///
/// These states are the reason someone ends up "lost": the shell prompt says
/// `HEAD` instead of a branch, ordinary commands refuse, and nothing on screen
/// says which of continue/abort/skip applies. All of them are plainly
/// detectable, so the app can say where you are and what the three moves are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InProgress {
    Rebase,
    Merge,
    CherryPick,
    Revert,
}

impl InProgress {
    pub fn label(self) -> &'static str {
        match self {
            InProgress::Rebase => "rebase",
            InProgress::Merge => "merge",
            InProgress::CherryPick => "cherry-pick",
            InProgress::Revert => "revert",
        }
    }

    /// The git subcommand that owns `--continue` and `--abort`.
    pub fn command(self) -> &'static str {
        match self {
            InProgress::Rebase => "rebase",
            InProgress::Merge => "merge",
            InProgress::CherryPick => "cherry-pick",
            InProgress::Revert => "revert",
        }
    }

    /// A merge has no `--continue`: it is finished by committing.
    pub fn finish_args(self) -> Vec<String> {
        match self {
            InProgress::Merge => vec!["commit".into(), "--no-edit".into()],
            other => vec![other.command().into(), "--continue".into()],
        }
    }

    pub fn abort_args(self) -> Vec<String> {
        vec![self.command().into(), "--abort".into()]
    }
}

/// Which files git is waiting on a decision for.
///
/// Porcelain marks a conflict with `U` in either column, plus `AA` (both
/// added) and `DD` (both deleted), neither of which contains a `U`.
pub fn conflicted(changes: &[FileChange]) -> Vec<String> {
    changes
        .iter()
        .filter(|c| c.code.contains('U') || c.code == "AA" || c.code == "DD")
        .map(|c| c.path.clone())
        .collect()
}

/// Detects an interrupted operation from the marker files git leaves behind.
pub async fn in_progress(repo: &str) -> Option<InProgress> {
    let git_dir = run(repo, "git", &["rev-parse", "--git-dir"]).await.ok()?;
    let dir = std::path::Path::new(repo).join(git_dir.trim());

    for (marker, state) in [
        ("rebase-merge", InProgress::Rebase),
        ("rebase-apply", InProgress::Rebase),
        ("MERGE_HEAD", InProgress::Merge),
        ("CHERRY_PICK_HEAD", InProgress::CherryPick),
        ("REVERT_HEAD", InProgress::Revert),
    ] {
        if dir.join(marker).exists() {
            return Some(state);
        }
    }
    None
}

/// Runs a command in `repo` and returns stdout, or stderr as the error.
pub async fn run(repo: &str, program: &str, args: &[&str]) -> Result<String, String> {
    // Said here rather than left to the OS: a missing working directory
    // otherwise comes back in the system's display language, and on Windows
    // as NotFound, which `spawn_error` would read as `program` missing.
    if !std::path::Path::new(repo).is_dir() {
        return Err(format!("{repo}: no such folder"));
    }
    let output = Command::new(super::env::resolve_program(program))
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| spawn_error(program, &e))?;

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
            "{}\n\nwhile running: `{program} {}`",
            detail.trim(),
            short_command(args)
        ))
    }
}

/// Why `program` could not be started, in words that say what to do next.
///
/// "program not found" was all a missing tool used to produce: true, but it
/// reads like a fault in GitAgent rather than a CLI to install.
pub fn spawn_error(program: &str, e: &std::io::Error) -> String {
    if e.kind() != std::io::ErrorKind::NotFound {
        return format!("could not run `{program}`: {e}");
    }
    let name = match program {
        "az" => "the Azure CLI (`az`)".to_string(),
        "gh" => "the GitHub CLI (`gh`)".to_string(),
        other => format!("`{other}`"),
    };
    let hint = match install_command(program) {
        Some((cmd, args)) => format!(" Install it with `{cmd} {}`, then refresh.", args.join(" ")),
        None => String::new(),
    };
    format!("{name} is not installed, or not on the PATH GitAgent can see.{hint}")
}

/// The usual one-line install for a CLI the app depends on, on this platform.
///
/// None on Linux, where the package manager depends on the distribution and
/// guessing wrong is worse than saying nothing.
pub fn install_command(program: &str) -> Option<(&'static str, Vec<&'static str>)> {
    if cfg!(windows) {
        let id = match program {
            "az" => "Microsoft.AzureCLI",
            "gh" => "GitHub.cli",
            "git" => "Git.Git",
            _ => return None,
        };
        // winget asks to accept the source and package agreements on first
        // use, and there is no terminal here to answer it in.
        Some((
            "winget",
            vec![
                "install",
                "--exact",
                "--id",
                id,
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
        ))
    } else if cfg!(target_os = "macos") {
        let formula = match program {
            "az" => "azure-cli",
            "gh" => "gh",
            "git" => "git",
            _ => return None,
        };
        Some(("brew", vec!["install", formula]))
    } else {
        None
    }
}

/// The command line, short enough to stay readable in an error.
///
/// A pull request body is a legitimate argument and can run to hundreds of
/// words; echoing it verbatim buries the one line that says what went wrong.
/// Long arguments are elided rather than dropped, so the shape of the command
/// is still recognisable.
pub fn short_command(args: &[&str]) -> String {
    const ARG_CAP: usize = 40;
    args.iter()
        .map(|arg| {
            if arg.chars().count() <= ARG_CAP {
                (*arg).to_string()
            } else {
                let head: String = arg.chars().take(ARG_CAP).collect();
                format!("{head}… ({} chars)", arg.chars().count())
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

fn holder_in(porcelain: &str, branch: &str, here: &str) -> Option<String> {
    let wanted = format!("branch refs/heads/{branch}");
    let same = |a: &str, b: &str| {
        let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
        a == b || canon(a) == canon(b)
    };
    porcelain.split("\n\n").find_map(|block| {
        let path = block.lines().find_map(|l| l.strip_prefix("worktree "))?;
        (block.lines().any(|l| l == wanted) && !same(path, here)).then(|| path.to_string())
    })
}

pub async fn fetch(repo: &str, args: &[&str]) -> Result<String, String> {
    let mut full = vec!["fetch"];
    full.extend_from_slice(args);
    match run(repo, "git", &full).await {
        Err(e) if ref_moved_underneath(&e) => {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            run(repo, "git", &full).await
        }
        done => done,
    }
}

/// Whether a fetch failed only because another git process updated a
/// remote-tracking ref at the same moment. See `fetch`.
pub fn ref_moved_underneath(err: &str) -> bool {
    err.contains("cannot lock ref") || err.contains("unable to update local ref")
}

/// Whether a merge refused because uncommitted edits to tracked files are in
/// its way — the case `--autostash` handles. Untracked files in the way read
/// differently, and a stash would not take them, so they do not match.
pub fn local_changes_in_the_way(err: &str) -> bool {
    err.contains("Your local changes to the following files would be overwritten")
}

/// Files with unresolved conflicts — what an `--autostash` whose edits
/// clashed with the update leaves behind. Git refuses to check out or merge
/// anything until they are resolved.
pub async fn unmerged_paths(repo: &str) -> Vec<String> {
    status(repo)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|c| is_unmerged(&c.code))
        .map(|c| c.path)
        .collect()
}

/// The porcelain codes git uses for a conflicted path.
fn is_unmerged(code: &str) -> bool {
    matches!(code, "UU" | "AA" | "DD" | "AU" | "UA" | "DU" | "UD")
}

/// Whether `a` and `b` name the same commit.
pub async fn same_commit(repo: &str, a: &str, b: &str) -> bool {
    match (
        run(repo, "git", &["rev-parse", "--verify", "--quiet", a]).await,
        run(repo, "git", &["rev-parse", "--verify", "--quiet", b]).await,
    ) {
        (Ok(a), Ok(b)) => a.trim() == b.trim(),
        _ => false,
    }
}

/// `git rev-list --left-right --count A...B` prints `<only in A>\t<only in B>`.
fn parse_left_right(out: &str) -> Option<(usize, usize)> {
    let mut it = out.split_whitespace().map(|n| n.parse::<usize>().ok());
    Some((it.next()??, it.next()??))
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

/// How long `check_access` waits for the remote before calling it unreachable.
const ACCESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

pub async fn status(repo: &str) -> Result<Vec<FileChange>, String> {
    let out = run(
        repo,
        "git",
        &["status", "--porcelain", "-z", IGNORE_DIRTY_SUBMODULES],
    )
    .await?;
    Ok(parse_status_z(&out))
}

/// A submodule, or another repository committed inside this one (a nested
/// worktree, say), shows as modified whenever it has edits of its own. Those
/// are not this repository's to commit — `git add` stages nothing for them —
/// so a run that took them for changes failed at the commit. A submodule
/// moved to another commit is still reported: that one can be committed.
pub const IGNORE_DIRTY_SUBMODULES: &str = "--ignore-submodules=dirty";

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

/// Whether the checked-out branch has a commit yet. Straight after
/// `git init` it does not, and there is no `HEAD` to diff against.
pub async fn has_commits(repo: &str) -> bool {
    run(repo, "git", &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .is_ok()
}

/// `HEAD`, or git's empty tree before the first commit — so a first commit's
/// diff is everything staged, instead of an error about an unknown revision.
/// Hashed rather than hard-coded: a SHA-256 repository's empty tree differs.
async fn diff_base(repo: &str) -> String {
    if has_commits(repo).await {
        return "HEAD".into();
    }
    run(repo, "git", &["hash-object", "-t", "tree", "--stdin"])
        .await
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|_| "4b825dc642cb6eb9a060e54bf8d69288fbee4904".into())
}

/// The diff of tracked changes against HEAD, capped.
pub async fn diff(repo: &str) -> Result<String, String> {
    let base = diff_base(repo).await;
    let out = run(repo, "git", &["diff", &base, "--unified=3"]).await?;
    Ok(cap(&out))
}

pub async fn diff_stat(repo: &str) -> Result<String, String> {
    let base = diff_base(repo).await;
    Ok(run(repo, "git", &["diff", &base, "--stat"])
        .await?
        .trim()
        .to_string())
}

pub fn cap(text: &str) -> String {
    fit(text, DIFF_CAP).text
}

/// A diff cut down to `budget` bytes, and what was left out of it.
#[derive(Clone, PartialEq, Debug)]
pub struct Fitted {
    pub text: String,
    pub left_out: Vec<String>,
}

impl Fitted {
    /// One line for a step's log, empty when nothing was left out.
    pub fn note(&self) -> String {
        if self.left_out.is_empty() {
            return String::new();
        }
        format!(
            "\n\nThe diff was too large for the model, so it did not see: {}",
            self.left_out.join(", ")
        )
    }
}

fn truncate(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let marker = format!("\n\n[…truncated at {budget} of {} bytes…]", text.len());
    let mut end = budget.saturating_sub(marker.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}

pub async fn create_branch(repo: &str, name: &str) -> Result<String, String> {
    run(repo, "git", &["checkout", "-b", name]).await
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

pub async fn push(repo: &str, branch: &str) -> Result<String, String> {
    run(repo, "git", &["push", "-u", "origin", branch]).await
}

/// Runs a remedy command anywhere on the machine, returning success plus the
/// combined output. Unlike `run`, the output is wanted either way — a failed
/// `brew install` explains itself in stdout as often as in stderr.
pub async fn run_command(program: &str, args: &[String]) -> (bool, String) {
    run_streaming(program, args, None, "", &mut |_| {}).await
}

pub async fn has_gh() -> bool {
    Command::new(super::env::resolve_program("gh"))
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
