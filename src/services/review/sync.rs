//! Merging a reviewed pull request and bringing the local branch in line
//! afterwards.

use super::*;

/// What the Merge step becomes once "Bring the base in" stopped on conflicts:
/// the merge is half done in the working tree, and the next move is the
/// person's — so the step says which files, and offers to finish or abandon
/// right there instead of leaving the old message and a button that looks
/// like it did nothing. `None` when the output is not a conflict.
pub fn conflict_followup(number: &str, base: &str, output: &str) -> Option<StepFailure> {
    let files: Vec<&str> = output
        .lines()
        .filter_map(|l| l.split_once("Merge conflict in ").map(|(_, f)| f.trim()))
        .collect();
    if files.is_empty() {
        return None;
    }
    let message = format!(
        "{base} is merged into #{number}'s branch everywhere except where both sides \
         changed the same lines. Nothing has been pushed.\n\n\
         Resolve {} above — pick a side for each conflict — or in your editor, \
         where each sits between <<<<<<< and >>>>>>>:\n{}\n\n\
         In each conflict, Current is #{number}'s branch — the pull request's own \
         changes — and Incoming is {base}. When {base} already has an earlier \
         version of the same change, from a pull request merged before, Current is \
         usually the side to keep, and Both keeps two versions of the same lines.\n\n\
         Then Finish below. It commits the merge, pushes, waits for GitHub to \
         recheck #{number}, and tries the merge again. It refuses while any of \
         these files still has a marker, and when a file the pull request changed \
         would end up exactly as {base} has it — its change lost.\n\n\
         Or abandon the merge to put the branch back as it was.",
        if files.len() == 1 {
            "this file"
        } else {
            "these files"
        },
        files
            .iter()
            .map(|f| format!("  {f}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let script = format!(
        r#"left=$(git diff --name-only --diff-filter=U | while IFS= read -r f; do grep -l '^<<<<<<< ' "$f"; done)
if [ -n "$left" ]; then printf 'Still has conflict markers, fix it first:\n%s\n' "$left"; exit 1; fi
git diff --name-only --diff-filter=U | while IFS= read -r f; do git add -- "$f"; done
{KEEPS_THE_CHANGE}
git commit --no-edit && git push || exit 1
{WAIT_FOR_GITHUB}"#
    );
    let finish = Remedy::new(
        &format!("Finish the merge, push, and merge #{number} again"),
        "sh",
        &["-c", &script, "sh", number, base],
    );
    let finish = Remedy {
        display: format!(
            "git add <fixed files> && git commit --no-edit && git push   (then waits for GitHub to recheck #{number})"
        ),
        ..finish
    }
    .once_resolved();
    Some(StepFailure {
        message,
        remedies: vec![
            finish,
            Remedy::terminal(
                "Abandon the merge — put the branch back as it was",
                "git",
                &["merge", "--abort"],
            ),
        ],
    })
}

/// "not mergeable: the merge commit cannot be cleanly created" means a real
/// conflict against the base branch. Nothing here can resolve a content
/// conflict for a person, but it can take them to the point where only that
/// is left — and say, right where the failure is, every step after it.
pub(super) fn merge_failure(number: &str, base: &str, message: String) -> StepFailure {
    if !message.contains("not mergeable") {
        return StepFailure {
            message,
            remedies: vec![],
        };
    }
    conflict_guide(number, base, &message)
}

/// A pull request that conflicts with its base, and the ways out of it.
pub(super) fn conflict_guide(number: &str, base: &str, message: &str) -> StepFailure {
    let guide = format!(
        "#{number} conflicts with {base}: GitHub cannot merge it as it stands. Nothing \
         has been merged or changed.\n\n\
         To keep the change, Bring {base} in (below). It switches to #{number}'s branch \
         and merges the latest {base} into it.\n\
         - If that merges cleanly, it pushes the branch, waits for GitHub to recheck \
           #{number}, and tries the merge again.\n\
         - If both sides changed the same lines, git stops and marks them in the files \
           with <<<<<<< and >>>>>>>, and nothing is pushed. Fix those files, start any \
           flow — Preflight offers Finish the merge — then run Commit → PR to push the \
           branch (it finds #{number} rather than opening another), and Review → Merge \
           again.\n\n\
         Otherwise, close #{number} without merging.\n\n{message}"
    );
    // Positional arguments rather than text spliced into the script: a branch
    // name may legally contain `;`, `$` or `&`.
    //
    // GitHub works out mergeability after a push, not during it, and for a few
    // seconds can still report the old conflict — merging again straight away
    // would fail on stale news. So the script waits for a fresh answer.
    let script = format!(
        r#"gh pr checkout "$1" && git fetch origin "$2" && git merge --no-edit "origin/$2" && git push || exit 1
{WAIT_FOR_GITHUB}"#
    );
    let bring_in = Remedy::new(
        &format!("Bring {base} in — merge it into #{number}'s branch, push, and merge again"),
        "sh",
        &["-c", &script, "sh", number, base],
    );
    let bring_in = Remedy {
        display: format!(
            "gh pr checkout {number} && git fetch origin {base} && git merge origin/{base} \
             && git push   (then waits for GitHub to recheck #{number})"
        ),
        ..bring_in
    };
    StepFailure {
        message: guide,
        remedies: vec![
            bring_in,
            Remedy::terminal(
                &format!("Abandon — close #{number} without merging"),
                "gh",
                &[
                    "pr",
                    "close",
                    number,
                    "--comment",
                    "Closing — conflicts with the base branch and this run is being abandoned rather than resolved.",
                ],
            ),
        ],
    }
}

pub(super) async fn merge(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    let number = state.artifact("pr_number").to_string();

    let out = match forge {
        Forge::GitHub => match git::run(
            repo,
            "gh",
            &["pr", "merge", &number, "--squash", "--delete-branch"],
        )
        .await
        {
            Ok(out) => out,
            Err(e) => match merged_anyway(repo, &number, state.artifact("pr_head"), &e).await {
                Some(out) => out,
                None => return Err(merge_failure(&number, state.artifact("pr_base"), e)),
            },
        },
        Forge::AzureDevOps => {
            let scope = forge::azure_scope(repo, Scope::Organization).await;
            let mut args = vec![
                "repos",
                "pr",
                "update",
                "--id",
                &number,
                "--status",
                "completed",
                "--delete-source-branch",
                "true",
                "--output",
                "none",
            ];
            args.extend(scope.iter().map(String::as_str));
            git::run(repo, "az", &args).await?
        }
        other => {
            return Err(StepFailure::from(format!(
                "Cannot merge on {}.",
                other.label()
            )))
        }
    };

    Ok(StepOutcome {
        summary: format!("merged #{number}"),
        log: if out.trim().is_empty() {
            format!("merged #{number}")
        } else {
            out.clone()
        },
        artifacts: vec![("merge_output".into(), out)],
        nothing_to_do: false,
        items: vec![],
    })
}

/// `gh pr merge --delete-branch` merges on GitHub first, then tidies up here:
/// switches off the branch, deletes it locally, then on origin. When that
/// tidying up fails — uncommitted edits in the way of the switch, usually —
/// gh exits with an error although the pull request is merged. The step asks
/// GitHub which it was, so a merge that happened is reported as one, and
/// finishes the one bit of tidying that matters off this machine: the
/// branch on origin, which the person approved deleting. The local side is
/// the Sync step's.
async fn merged_anyway(repo: &str, number: &str, head: &str, err: &str) -> Option<String> {
    let state = git::run(
        repo,
        "gh",
        &["pr", "view", number, "--json", "state", "-q", ".state"],
    )
    .await
    .ok()?;
    if state.trim() != "MERGED" {
        return None;
    }
    let deleted = if head.is_empty() {
        false
    } else {
        git::run(repo, "git", &["push", "origin", "--delete", head])
            .await
            .is_ok()
    };
    Some(format!(
        "Merged #{number} on GitHub. Only the tidying up on this machine failed \
         afterwards — gh could not switch off the branch here:\n\n{}\n\n{}\
         The Sync step brings the base branch up to date without touching your edits.",
        err.trim(),
        if deleted {
            format!("{head} was deleted on origin. ")
        } else {
            String::new()
        },
    ))
}

pub(super) async fn sync(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let base = state.artifact("pr_base");
    if let Some(there) = git::worktree_holding(repo, base).await {
        return sync_elsewhere(repo, base, &there).await;
    }
    // Work in progress is never touched to bring the base up to date: the
    // merge happened on the forge, and this step is a convenience. Checking
    // out or fast-forwarding over uncommitted edits is what failed here
    // whenever a review ran while something else was being worked on.
    let edited = git::run(
        repo,
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            git::IGNORE_DIRTY_SUBMODULES,
        ],
    )
    .await
    .map(|out| !out.trim().is_empty())
    .unwrap_or(false);
    // Files left in conflict by an earlier set-aside-and-update are not
    // "edits": they need resolving, and the usual path below says which.
    if edited && git::unmerged_paths(repo).await.is_empty() {
        return sync_around_edits(repo, base).await;
    }
    // Already where it needs to be, git is not asked again: after an
    // autostash whose edits clashed, a checkout or merge would refuse over
    // the conflicts even though there is nothing left for either to do.
    let mut log = if git::current_branch(repo).await.ok().as_deref() == Some(base) {
        String::new()
    } else {
        git::run(repo, "git", &["checkout", base]).await?
    };
    // Fetch only the base, then fast-forward, rather than `git pull`: a pull
    // fetches every branch (and prunes, where configured), which widens the
    // window for another git process to move a ref under it — and that race
    // is the one failure here with a fix that is simply "do it again".
    match git::fetch(repo, &["origin", base]).await {
        Ok(out) => log.push_str(&out),
        Err(e) => return Err(sync_fetch_failure(base, e)),
    }
    let target = format!("origin/{base}");
    if !git::same_commit(repo, "HEAD", &target).await {
        match git::run(repo, "git", &["merge", "--ff-only", &target]).await {
            Ok(out) => log.push_str(&out),
            // Local base has commits of its own. GitAgent's release notes are
            // the usual ones, and are settled here; anything else gets a button.
            Err(e) if e.contains("Not possible to fast-forward") => {
                match flow::settle_own_notes(repo, &target).await {
                    Ok(Some(note)) => log.push_str(&note),
                    Ok(None) => return Err(sync_diverged(base, e, "")),
                    Err(why) => return Err(sync_diverged(base, e, &why)),
                }
            }
            Err(e) => return Err(sync_merge_failure(base, e)),
        }
    }
    let clashed = git::unmerged_paths(repo).await;
    if !clashed.is_empty() {
        log = format!("{}\n\n{}", clashes_note(&clashed), log.trim());
    }
    Ok(StepOutcome {
        summary: if clashed.is_empty() {
            format!("on {base}, up to date")
        } else {
            format!(
                "on {base}, up to date; {} to resolve",
                plural_files(clashed.len())
            )
        },
        log: log.trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
        nothing_to_do: false,
        items: vec![],
    })
}

/// Brings the base up to date without touching uncommitted edits, and says
/// where that leaves things. Never a failure: the merge is done, and nothing
/// here is in the way of it.
pub(super) async fn sync_around_edits(repo: &str, base: &str) -> Result<StepOutcome, StepFailure> {
    let here = git::current_branch(repo).await.unwrap_or_default();
    let target = format!("origin/{base}");
    if here == base {
        let log = match git::fetch(repo, &["origin", base]).await {
            Ok(out) => out,
            Err(e) => return Err(sync_fetch_failure(base, e)),
        };
        let behind: usize = git::run(
            repo,
            "git",
            &["rev-list", "--count", &format!("HEAD..{target}")],
        )
        .await
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
        let (summary, note) = if behind == 0 {
            (
                format!("on {base}, up to date"),
                format!(
                    "{base} was already up to date; your uncommitted edits are as you left them."
                ),
            )
        } else {
            (
                format!("on {base}, {behind} behind; your edits kept"),
                format!(
                    "You have uncommitted edits on {base}, so it was not updated under them. \
                     It is {behind} commit{} behind {target}: run `git pull --ff-only` once \
                     they are committed or put away.",
                    if behind == 1 { "" } else { "s" }
                ),
            )
        };
        return Ok(StepOutcome {
            summary,
            log: format!("{note}\n\n{}", log.trim()).trim().to_string(),
            artifacts: vec![("sync_output".into(), log)],
            nothing_to_do: false,
            items: vec![],
        });
    }

    // On another branch, the base can move without being checked out:
    // fetching straight into it fast-forwards the ref and touches no file.
    let into = format!("{base}:{base}");
    let (summary, note, log) = match git::fetch(repo, &["origin", &into]).await {
        Ok(out) => (
            format!("{base} up to date; you stay on {here}"),
            format!(
                "You have uncommitted edits on {here}, so the checkout stayed where it is. \
                 {base} was brought up to date without switching to it."
            ),
            out,
        ),
        // Git refuses only when the local base has commits origin does not —
        // not this step's to resolve.
        Err(e) => {
            let log = match git::fetch(repo, &["origin", base]).await {
                Ok(out) => out,
                Err(e) => return Err(sync_fetch_failure(base, e)),
            };
            (
                format!("fetched {base}; local {base} left as is"),
                format!(
                    "You have uncommitted edits on {here}, so the checkout stayed where it \
                     is. Your local {base} has commits {target} does not, so it was not \
                     moved either:\n\n{e}"
                ),
                log,
            )
        }
    };
    Ok(StepOutcome {
        summary,
        log: format!("{note}\n\n{}", log.trim()).trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
        nothing_to_do: false,
        items: vec![],
    })
}

/// Run from a second worktree whose base branch is checked out in another
/// one. Git refuses to check a branch out twice, and there is nothing the
/// person could do about that here short of removing a worktree. So the base
/// is brought up to date where it lives, and this worktree is left on its
/// branch — which is merged now, and theirs to remove when they are done.
pub(super) async fn sync_elsewhere(
    repo: &str,
    base: &str,
    there: &str,
) -> Result<StepOutcome, StepFailure> {
    let mut log = match git::fetch(repo, &["origin", base]).await {
        Ok(out) => out,
        Err(e) => return Err(sync_fetch_failure(base, e)),
    };
    let stays = git::current_branch(repo).await.unwrap_or_default();
    let (summary, note) = match git::run(
        there,
        "git",
        &["merge", "--ff-only", &format!("origin/{base}")],
    )
    .await
    {
        Ok(out) => {
            log.push_str(&out);
            (
                format!("{base} up to date in {there}"),
                format!(
                    "{base} is checked out in {there}, so it was updated there. This \
                     worktree stays on {stays}."
                ),
            )
        }
        // Not a failure of this run: the merge is done and origin/{base} is
        // fetched. Git refused to move a working tree with changes in the way,
        // which is exactly what it should do, and pulling there later finishes it.
        Err(e) => (
            format!("fetched {base}; {there} not updated"),
            format!(
                "{base} is checked out in {there}, and git would not fast-forward it \
                 there:\n\n{e}\n\nThe merge is done. Run `git pull` in {there} once \
                 its changes are committed or put away. This worktree stays on {stays}."
            ),
        ),
    };
    Ok(StepOutcome {
        summary,
        log: format!("{note}\n\n{}", log.trim()).trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
        nothing_to_do: false,
        items: vec![],
    })
}

/// The fetch has already been retried once by `git::fetch`. If it is still
/// another process moving the ref, say so — the raw git message reads like
/// corruption, and it is not — and put the retry on a button.
pub(super) fn sync_fetch_failure(base: &str, err: String) -> StepFailure {
    if !git::ref_moved_underneath(&err) {
        return StepFailure::from(err);
    }
    StepFailure {
        message: format!(
            "Another git program updated origin/{base} at the same moment GitAgent \
             did — an editor's background fetch, usually. Nothing is wrong with the \
             repository and the merge is done; fetching again picks up where it left \
             off. If this keeps happening, close whatever else is fetching here.\n\n{err}"
        ),
        remedies: vec![Remedy::new(
            "Fetch again and finish",
            "git",
            &["fetch", "origin", base],
        )],
    }
}

/// The pull request is merged and this copy is on the base; only the
/// fast-forward is left. When uncommitted edits are what stopped it, say so
/// and offer to move them out of the way and back.
/// The merge is done; only this copy's base is out of step — it has commits
/// origin's does not. Replaying them on top is the usual answer, offered here
/// rather than left to a terminal.
pub(super) fn sync_diverged(base: &str, err: String, why: &str) -> StepFailure {
    let target = format!("origin/{base}");
    StepFailure {
        message: format!(
            "The merge is done, but your local {base} has commits {target} does not, so it \
             could not simply be moved forward. Replay them on top of {target} below, then \
             push them when you are ready.{}\n\n{err}",
            if why.is_empty() {
                String::new()
            } else {
                format!("\n\n{why}")
            }
        ),
        remedies: vec![Remedy::new(
            &format!("Replay your local commits on top of {target}"),
            "git",
            &["rebase", &target],
        )],
    }
}

pub(super) fn sync_merge_failure(base: &str, err: String) -> StepFailure {
    if !git::local_changes_in_the_way(&err) {
        return StepFailure::from(err);
    }
    StepFailure {
        message: format!(
            "The merge is done, but {base} could not be brought up to date: you have \
             uncommitted edits to files that changed on origin/{base}. Nothing was \
             touched. Set them aside and put them back after updating — if they clash \
             with what came in, both versions are left marked in the file for you to \
             pick from, a copy stays in the stash, and this step still finishes.\n\n{err}"
        ),
        remedies: vec![flow::autostash_remedy(&format!("origin/{base}"))],
    }
}
