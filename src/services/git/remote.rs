//! The remote side: the upstream branch, access to the remote, its default
//! branch, and worktrees.

use super::*;

/// `git fetch <args>`, tried a second time when another git process moved a
/// remote-tracking ref while this one was writing it.
///
/// That race is ordinary — an editor's background fetch, a terminal, a second
/// GitAgent step — and git reports it as "cannot lock ref … is at X but
/// expected Y". By the time it says so the other process has already written
/// the ref, usually to the very commit this fetch wanted, so fetching again
/// succeeds. Fetching is idempotent, which is what makes retrying it safe;
/// nothing else in this module is retried.
/// The other worktree of this repository that has `branch` checked out, if
/// one does. Git lets a branch be checked out in only one worktree at a time,
/// so `git checkout <branch>` here fails while that one holds it.
pub async fn worktree_holding(repo: &str, branch: &str) -> Option<String> {
    let list = run(repo, "git", &["worktree", "list", "--porcelain"])
        .await
        .ok()?;
    let here = run(repo, "git", &["rev-parse", "--show-toplevel"])
        .await
        .ok()?;
    holder_in(&list, branch, here.trim())
}

/// Where the current branch stands against the branch it tracks on origin.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Upstream {
    /// Nothing to compare against: no upstream, a detached HEAD, or a
    /// repository git could not read.
    #[default]
    Unknown,
    /// Origin has nothing this branch lacks. Local commits origin has yet to
    /// receive are fine — pushing them is what the next step is for.
    UpToDate,
    /// Origin has `behind` commits this branch does not; `ahead` are local
    /// commits origin does not have, so a non-zero `ahead` means the two have
    /// diverged and only a rebase or merge brings them together.
    Behind {
        tracking: String,
        behind: usize,
        ahead: usize,
    },
}

/// Fetches the branch this one tracks, then compares the two.
///
/// The fetch is the point: the remote-tracking ref is only as fresh as the
/// last fetch, and a pull request merged on the forge's website leaves it
/// stale until something fetches. A failed fetch (offline, say) is not fatal
/// — the comparison then runs on what is already known, which is still the
/// best answer available.
pub async fn upstream(repo: &str) -> Upstream {
    let Ok(tracking) = run(
        repo,
        "git",
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .await
    else {
        return Upstream::Unknown;
    };
    let tracking = tracking.trim().to_string();
    if let Some((remote, branch)) = tracking.split_once('/') {
        let _ = fetch(repo, &["--quiet", remote, branch]).await;
    }
    let Ok(counts) = run(
        repo,
        "git",
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
    )
    .await
    else {
        return Upstream::Unknown;
    };
    match parse_left_right(&counts) {
        Some((_, 0)) => Upstream::UpToDate,
        Some((ahead, behind)) => Upstream::Behind {
            tracking,
            behind,
            ahead,
        },
        None => Upstream::Unknown,
    }
}

/// The tag of a release commit that was made here but never reached origin:
/// HEAD is a release commit, a `v…` tag points at it, and origin has no such
/// tag.
///
/// This is what `release.sh` leaves behind when its push is rejected — it
/// commits and tags before it pushes. Retrying from there would tag the same
/// version twice, and pulling would replay the bump onto newer work and leave
/// the tag on the old commit.
pub async fn unpushed_release_tag(repo: &str) -> Option<String> {
    let subject = run(repo, "git", &["log", "-1", "--format=%s"]).await.ok()?;
    if !subject.to_lowercase().contains("release") {
        return None;
    }
    let tags = run(repo, "git", &["tag", "--points-at", "HEAD"])
        .await
        .ok()?;
    let tag = tags
        .lines()
        .map(str::trim)
        .find(|t| t.starts_with('v'))?
        .to_string();
    // Only a definite "origin has no such tag" counts: if origin cannot be
    // reached, the tag may well be there, and undoing a published release
    // locally would be the wrong fix.
    let remote = run(
        repo,
        "git",
        &["ls-remote", "--tags", "origin", &format!("refs/tags/{tag}")],
    )
    .await
    .ok()?;
    remote.trim().is_empty().then_some(tag)
}

/// Whether this repository can be worked on from here: git can read it, and
/// `origin`, if it has one, lets us in. Asked before a folder opens, so a
/// repository we have no rights to is reported on the welcome screen rather
/// than leaving a window waiting on it.
///
/// Read-only, and never interactive. With a credential prompt allowed, a
/// remote that wants a sign-in — an Azure DevOps project you were never added
/// to, typically — waits on a window that may never be shown. Here it fails at
/// once, and the time limit covers a host that does not answer at all.
pub async fn check_access(repo: &str) -> Result<(), String> {
    run(repo, "git", &["rev-parse", "--git-dir"]).await?;
    if remote_url(repo).await.is_none() {
        return Ok(());
    }
    let child = Command::new(crate::services::env::resolve_program("git"))
        .args(["ls-remote", "--quiet", "origin", "HEAD"])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(ACCESS_TIMEOUT, child).await {
        Err(_) => {
            return Err(format!(
                "origin did not answer within {} seconds",
                ACCESS_TIMEOUT.as_secs()
            ))
        }
        Ok(Err(e)) => return Err(spawn_error("git", &e)),
        Ok(Ok(output)) => output,
    };
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            "origin refused access".to_string()
        } else {
            stderr
        })
    }
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
