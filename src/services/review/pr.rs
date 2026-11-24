//! The pull request under review: finding it, its checks and its diff.

use super::*;

pub(super) async fn find_pr(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    let branch = git::current_branch(repo).await?;
    // Set when this run was started from a specific pull request in the
    // sidebar's PR list, rather than from "whatever the checked-out branch
    // has open" — lets a review run against any open PR, not only the one
    // for the branch you currently happen to be sitting on.
    let selected = state.artifact("selected_pr_number").trim();

    match forge {
        Forge::GitHub => {
            let mut args: Vec<&str> = vec!["pr", "view"];
            if !selected.is_empty() {
                args.push(selected);
            }
            args.extend([
                "--json",
                "number,title,url,state,baseRefName,headRefName,isDraft",
            ]);
            let out = git::run(repo, "gh", &args).await.map_err(|e| {
                if e.contains("no pull requests found") || e.contains("no open pull requests") {
                    StepFailure::from(if selected.is_empty() {
                        format!("No open pull request for `{branch}`. Run the commit flow first.")
                    } else {
                        format!("Pull request #{selected} is not open.")
                    })
                } else {
                    StepFailure::from(e)
                }
            })?;

            let value: serde_json::Value = serde_json::from_str(&out)
                .map_err(|e| format!("could not read the gh response: {e}"))?;

            // A merged or closed pull request is not an error, there is simply
            // nothing to review — and its branch is usually gone from the
            // remote, which would fail confusingly at `pr_diff` instead.
            if !crate::services::probe::is_open(&value) {
                let state = value["state"].as_str().unwrap_or("not open");
                let number = value["number"].as_i64().unwrap_or_default();
                return Ok(StepOutcome::nothing(format!(
                    "Pull request #{number} for `{branch}` is {state}. Nothing to review — \
                     switch to {} and pull.",
                    value["baseRefName"].as_str().unwrap_or("the base branch"),
                )));
            }

            let number = value["number"].as_i64().unwrap_or_default().to_string();
            let title = value["title"].as_str().unwrap_or_default().to_string();
            let url = value["url"].as_str().unwrap_or_default().to_string();
            let base = value["baseRefName"]
                .as_str()
                .unwrap_or("master")
                .to_string();
            let head = value["headRefName"].as_str().unwrap_or(&branch).to_string();
            let draft = value["isDraft"].as_bool().unwrap_or(false);

            Ok(StepOutcome {
                summary: format!("#{number} {title}"),
                log: format!(
                    "#{number}  {title}\n{url}\n\n{head} → {base}{}",
                    if draft { "\n\nThis is a draft." } else { "" }
                ),
                artifacts: vec![
                    ("pr_number".into(), number),
                    ("pr_title".into(), title),
                    ("pr_url".into(), url),
                    ("pr_base".into(), base),
                    ("pr_head".into(), head),
                ],
                nothing_to_do: false,
                items: vec![],
                remedies: vec![],
            })
        }
        Forge::AzureDevOps => {
            let (pr, head): (serde_json::Value, String) = if !selected.is_empty() {
                let scope = forge::azure_scope(repo, Scope::Organization).await;
                let mut args = vec!["repos", "pr", "show", "--id", selected, "--output", "json"];
                args.extend(scope.iter().map(String::as_str));
                let out = git::run(repo, "az", &args).await?;
                let pr: serde_json::Value = serde_json::from_str(&out)
                    .map_err(|e| format!("could not read az output: {e}"))?;
                let head = pr["sourceRefName"]
                    .as_str()
                    .unwrap_or(&branch)
                    .trim_start_matches("refs/heads/")
                    .to_string();
                (pr, head)
            } else {
                let scope = forge::azure_scope(repo, Scope::Repository).await;
                let mut args = vec![
                    "repos",
                    "pr",
                    "list",
                    "--source-branch",
                    &branch,
                    "--status",
                    "active",
                    "--output",
                    "json",
                ];
                args.extend(scope.iter().map(String::as_str));
                let out = git::run(repo, "az", &args).await?;
                let list: serde_json::Value = serde_json::from_str(&out)
                    .map_err(|e| format!("could not read az output: {e}"))?;
                let pr = list
                    .as_array()
                    .and_then(|a| a.first())
                    .cloned()
                    .ok_or_else(|| format!("No active pull request for `{branch}`."))?;
                (pr, branch.clone())
            };

            // `pr show` answers for a pull request in any state. A completed
            // or abandoned one has nothing to review, and its source branch
            // is usually deleted, which used to fail at "Fetch the diff".
            if let Some(reason) = azure_not_active(&pr, &head) {
                return Ok(StepOutcome::nothing(reason));
            }

            let number = pr["pullRequestId"].as_i64().unwrap_or_default().to_string();
            let title = pr["title"].as_str().unwrap_or_default().to_string();
            // Never guess the base: diffing against the default branch when
            // the pull request targets another one reviews the wrong change.
            let base = azure_ref(&pr, "targetRefName").ok_or_else(|| {
                StepFailure::from(format!(
                    "Azure DevOps did not say which branch pull request !{number} targets."
                ))
            })?;
            let url = forge::azure_pr_page(repo, &pr).await;

            Ok(StepOutcome {
                summary: format!("!{number} {title}"),
                log: format!("!{number}  {title}\n{url}\n\n{head} → {base}"),
                artifacts: vec![
                    ("pr_number".into(), number),
                    ("pr_title".into(), title),
                    ("pr_url".into(), url),
                    ("pr_base".into(), base),
                    ("pr_head".into(), head),
                ],
                nothing_to_do: false,
                items: vec![],
                remedies: vec![],
            })
        }
        other => Err(StepFailure::from(format!(
            "Cannot look up pull requests on {}.",
            other.label()
        ))),
    }
}

/// A branch name out of an Azure DevOps ref field (`refs/heads/x` → `x`).
pub(super) fn azure_ref(pr: &serde_json::Value, field: &str) -> Option<String> {
    pr[field]
        .as_str()
        .map(|r| r.trim_start_matches("refs/heads/").to_string())
        .filter(|r| !r.is_empty())
}

/// Why there is nothing to review on an Azure DevOps pull request that is no
/// longer active (`completed` or `abandoned`), or `None` while it is open.
pub(super) fn azure_not_active(pr: &serde_json::Value, head: &str) -> Option<String> {
    let status = pr["status"].as_str()?;
    if status.eq_ignore_ascii_case("active") {
        return None;
    }
    let number = pr["pullRequestId"].as_i64().unwrap_or_default();
    let word = if status.eq_ignore_ascii_case("completed") {
        "already merged"
    } else {
        status
    };
    let base = azure_ref(pr, "targetRefName").unwrap_or_else(|| "the base branch".into());
    Some(format!(
        "Pull request !{number} for `{head}` is {word}. Nothing to review — switch to \
         {base} and pull."
    ))
}

/// A fetch that failed because the pull request's branch is gone from the
/// remote: the usual sign it was merged and its branch deleted after this run
/// looked it up.
pub(super) fn head_gone(error: &str, head: &str) -> bool {
    error.contains(&format!("couldn't find remote ref {head}"))
        || error.contains(&format!("couldn't find remote ref refs/heads/{head}"))
}

/// Boils a GitHub Actions job log down to the failure.
///
/// `gh run view --log-failed` returns the entire job — over a megabyte of
/// runner boilerplate, with post-job cleanup *after* the error. Neither the
/// head nor the tail is the interesting part. The `##[error]` line is, so this
/// keeps that line and the lines leading up to it, which is where the compiler
/// output, the failing assertion or the rustfmt diff actually is.
/// Removes ANSI colour codes.
///
/// cargo colours its output, and GitHub Actions keeps the escapes in the
/// stored log. Rendered as text they show up as `^[[1m^[[92m` in front of
/// every interesting line, which is worse than no colour at all.
pub(crate) fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI: ESC '[' params… final byte in @-~. Anything else, drop the
        // escape alone rather than guessing at a length.
        if chars.next() == Some('[') {
            for p in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&p) {
                    break;
                }
            }
        }
    }
    out
}

pub(super) fn distil_log(raw: &str, context: usize) -> String {
    let lines: Vec<String> = raw
        .lines()
        .map(|line| {
            // Rows arrive as `job\tstep\t<ISO timestamp> text`.
            let text = line.rsplit('\t').next().unwrap_or(line);
            match text.split_once(' ') {
                Some((first, rest))
                    if first.len() > 19 && first.ends_with('Z') && first.contains('T') =>
                {
                    rest.to_string()
                }
                _ => text.to_string(),
            }
        })
        .map(|line| strip_ansi(&line.trim_start_matches('\u{feff}').replace("^[", "\u{1b}")))
        .filter(|line| {
            let l = line.trim();
            !l.is_empty()
                && !l.starts_with("##[group]")
                && !l.starts_with("##[endgroup]")
                && !l.starts_with("##[warning]")
        })
        .collect();

    if lines.is_empty() {
        return String::new();
    }

    let end = lines
        .iter()
        .position(|l| l.contains("##[error]"))
        .unwrap_or(lines.len().saturating_sub(1));
    let start = end.saturating_sub(context);

    lines[start..=end.min(lines.len().saturating_sub(1))]
        .join("\n")
        .replace("##[error]", "")
}

pub(super) async fn pr_status(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    if forge == Forge::AzureDevOps {
        return azure_pr_status(repo, state).await;
    }
    if forge != Forge::GitHub {
        // Azure policy evaluation is a different shape entirely; saying so beats
        // reporting a green build that was never checked.
        return Ok(StepOutcome {
            summary: "not available".into(),
            log: "Check status is only read from GitHub so far. Review the policies \
                  in the browser before merging."
                .into(),
            artifacts: vec![
                (
                    "checks_summary".into(),
                    "unknown — not read on this forge".into(),
                ),
                ("checks_state".into(), "unknown".into()),
                ("merge_state".into(), "unknown".into()),
            ],
            nothing_to_do: false,
            items: vec![],
            remedies: vec![],
        });
    }

    // `find_pr` already resolved which PR this run is about — reusing that
    // number here (rather than a bare `gh pr view`, which re-resolves off
    // the checked-out branch) is what lets this step work for a PR picked
    // from the sidebar's list, not just the one the local branch happens to
    // have open.
    let number = state.artifact("pr_number");
    // One look, never a wait: checks still running are reported as pending
    // and the run goes on. Under Trust all the merge then goes ahead; if the
    // base branch requires those checks, GitHub refuses it and the merge
    // step offers to hand it to auto-merge.
    let out = git::run(
        repo,
        "gh",
        &[
            "pr",
            "view",
            number,
            "--json",
            "statusCheckRollup,mergeable,mergeStateStatus",
        ],
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("could not read the gh response: {e}"))?;
    let (lines, failing, pass, fail, pending) = tally(&value);

    let merge_state = value["mergeStateStatus"]
        .as_str()
        .unwrap_or("UNKNOWN")
        .to_string();
    let mergeable = value["mergeable"].as_str().unwrap_or("UNKNOWN");
    // No checks at all is not a pass: nothing ran. Saying "passing" over
    // 0 of 0 is how a pull request nobody tested looked green.
    let state_word = if fail > 0 {
        "failing"
    } else if pending > 0 {
        "pending"
    } else if pass == 0 {
        "none"
    } else {
        "passing"
    };

    let summary = if state_word == "none" {
        "no checks reported for this pull request".to_string()
    } else {
        format!(
            "{pass} passing, {fail} failing, {pending} pending\n{}",
            lines.join("\n")
        )
    };

    // The log of the first failed check only: each is a whole log downloaded
    // against the hourly budget, and the jobs of one matrix nearly always fail
    // for the same reason. The others are listed with their links.
    let mut detail = String::new();
    for (shown, (name, url)) in failing.iter().enumerate() {
        detail.push_str(&format!("\n── {name} ──\n"));
        match job_id(url).filter(|_| shown == 0) {
            Some(id) => {
                let raw = git::job_log(repo, id).await.unwrap_or_else(|e| e);
                let boiled = distil_log(&raw, 30);
                detail.push_str(if boiled.trim().is_empty() {
                    "(could not read the log)"
                } else {
                    &boiled
                });
                detail.push('\n');
            }
            None if shown > 0 => detail.push_str(
                "(log not downloaded: usually the same failure as above; the link has it)\n",
            ),
            None => detail.push_str("(no job log for this check)\n"),
        }
        if !url.is_empty() {
            detail.push_str(&format!("{url}\n"));
        }
    }

    // GitHub has already said it cannot merge this. Offering the merge anyway
    // only moves the refusal one approval later, after the person has said yes.
    if merge_state == "DIRTY" || mergeable == "CONFLICTING" {
        let base = state.artifact("pr_base");
        let head = state.artifact("pr_head");
        let checks = format!("{summary}\n\nmergeable:   {mergeable}\nmerge state: {merge_state}");
        return Err(match squashed_before(repo, number, head).await {
            Some(earlier) => {
                let new = flow::free_branch_name(repo, head).await;
                replay_after(number, base, head, &earlier, &new, &checks)
            }
            None => conflict_guide(number, base, &checks),
        });
    }

    Ok(StepOutcome {
        summary: format!("{state_word} · {merge_state}"),
        log: format!("{summary}\n\nmergeable:   {mergeable}\nmerge state: {merge_state}\n{detail}"),
        artifacts: vec![
            ("checks_summary".into(), summary),
            ("checks_detail".into(), detail),
            ("checks_state".into(), state_word.into()),
            ("merge_state".into(), merge_state),
        ],
        nothing_to_do: false,
        items: vec![],
        remedies: vec![],
    })
}

/// Pull request `number` on Azure DevOps as it is now.
pub(super) async fn azure_pr_now(repo: &str, number: &str) -> Result<serde_json::Value, String> {
    let scope = forge::azure_scope(repo, Scope::Organization).await;
    let mut args = vec!["repos", "pr", "show", "--id", number, "--output", "json"];
    args.extend(scope.iter().map(String::as_str));
    let out = git::run(repo, "az", &args).await?;
    serde_json::from_str(&out).map_err(|e| format!("could not read az output: {e}"))
}

/// Azure's `mergeStatus`: `succeeded`, `conflicts`, `queued`, `failure`,
/// `rejectedByPolicy`, `notSet`. Empty when the reply has none.
pub(super) fn azure_merge_status(pr: &serde_json::Value) -> String {
    pr["mergeStatus"].as_str().unwrap_or_default().to_string()
}

/// The files `base` and `head` both changed in ways git cannot merge, from
/// a merge done in memory: nothing in the checkout is touched. Empty when it
/// merges cleanly or cannot be worked out here.
pub(super) async fn conflicting_files(repo: &str, base: &str, head: &str) -> Vec<String> {
    let _ = git::fetch(repo, &["origin", base, head]).await;
    let (theirs, ours) = (format!("origin/{base}"), format!("origin/{head}"));
    // A clash exits 1 with the report on stdout, which `git::run` hands back
    // as the error; either way the CONFLICT lines are what matter.
    let out = match git::run(
        repo,
        "git",
        &["merge-tree", "--write-tree", "--name-only", &theirs, &ours],
    )
    .await
    {
        Ok(out) | Err(out) => out,
    };
    merge_tree_conflicts(&out)
}

/// The paths out of `git merge-tree`'s `CONFLICT (…): Merge conflict in <path>`
/// lines, each once.
pub(super) fn merge_tree_conflicts(output: &str) -> Vec<String> {
    let mut files: Vec<String> = output
        .lines()
        .filter(|l| l.starts_with("CONFLICT"))
        .filter_map(|l| {
            l.split_once("Merge conflict in ")
                .map(|(_, f)| f.trim().to_string())
        })
        .collect();
    files.dedup();
    files
}

/// Azure DevOps' answer to "can this be merged": its policies are not read
/// yet, but its merge status is, and a pull request that conflicts with its
/// base stops here — as on GitHub — rather than at a merge Azure quietly
/// declines, which reported "merged" and sent the run back to the start.
async fn azure_pr_status(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let number = state.artifact("pr_number");
    let not_read = |why: &str| StepOutcome {
        summary: "policies not read".into(),
        log: format!(
            "{why}Branch policies are not read from Azure DevOps yet. Review them in the \
             browser before merging."
        ),
        artifacts: vec![
            (
                "checks_summary".into(),
                "unknown — not read on this forge".into(),
            ),
            ("checks_state".into(), "unknown".into()),
            ("merge_state".into(), "unknown".into()),
        ],
        nothing_to_do: false,
        items: vec![],
        remedies: vec![],
    };
    let pr = match azure_pr_now(repo, number).await {
        Ok(pr) => pr,
        // Not being able to ask is no reason to stop the review; the merge
        // asks again and checks what Azure did.
        Err(e) => return Ok(not_read(&format!("Could not read !{number}: {e}\n\n"))),
    };
    let status = azure_merge_status(&pr);
    if status.eq_ignore_ascii_case("conflicts") {
        let (base, head) = (state.artifact("pr_base"), state.artifact("pr_head"));
        return Err(azure_conflicts(repo, number, base, head).await);
    }
    let mut outcome = not_read(&format!("merge status: {}\n\n", or_unknown(&status)));
    outcome.summary = format!("policies not read · merge {}", or_unknown(&status));
    outcome.artifacts.retain(|(k, _)| k != "merge_state");
    outcome
        .artifacts
        .push(("merge_state".into(), or_unknown(&status).to_string()));
    Ok(outcome)
}

fn or_unknown(status: &str) -> &str {
    if status.is_empty() {
        "unknown"
    } else {
        status
    }
}

/// The stop for an Azure pull request that conflicts with its base, naming
/// the files when git can tell which.
pub(super) async fn azure_conflicts(
    repo: &str,
    number: &str,
    base: &str,
    head: &str,
) -> StepFailure {
    let files = conflicting_files(repo, base, head).await;
    let mut detail = "merge status: conflicts".to_string();
    if !files.is_empty() {
        detail.push_str("\n\nConflicting files:\n");
        for f in &files {
            detail.push_str(&format!("  {f}\n"));
        }
    }
    let scope = forge::azure_scope(repo, Scope::Organization).await;
    azure_conflict_guide(number, base, head, &scope, detail.trim_end())
}

/// Each check on the pull request as a line, the failing ones with their
/// links, and the passing, failing and pending counts.
type Tally = (Vec<String>, Vec<(String, String)>, usize, usize, usize);

fn tally(value: &serde_json::Value) -> Tally {
    let mut lines = vec![];
    let mut failing: Vec<(String, String)> = vec![];
    let (mut pass, mut fail, mut pending) = (0, 0, 0);
    if let Some(checks) = value["statusCheckRollup"].as_array() {
        for check in checks {
            let name = check["name"]
                .as_str()
                .or_else(|| check["context"].as_str())
                .unwrap_or("check");
            let status = check["status"].as_str().unwrap_or("");
            let conclusion = check["conclusion"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .unwrap_or("");

            let verdict = if status == "IN_PROGRESS" || status == "QUEUED" || status == "PENDING" {
                pending += 1;
                "pending"
            } else if matches!(conclusion, "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                pass += 1;
                "pass"
            } else {
                fail += 1;
                failing.push((
                    name.to_string(),
                    check["detailsUrl"].as_str().unwrap_or_default().to_string(),
                ));
                "fail"
            };
            lines.push(format!("  {verdict:<8} {name}"));
        }
    }
    (lines, failing, pass, fail, pending)
}

/// An earlier pull request from the same branch, squash-merged already:
/// `(number, the branch tip it was merged at)`. Only when that tip is still
/// on the branch — the commits are there twice, once as they were and once
/// as the base's squash of them.
pub(super) async fn squashed_before(
    repo: &str,
    number: &str,
    head: &str,
) -> Option<(String, String)> {
    if head.is_empty() {
        return None;
    }
    let out = git::prs_with_head(repo, head).await.ok()?;
    let list: serde_json::Value = serde_json::from_str(&out).ok()?;
    let earlier = list
        .as_array()?
        .iter()
        .filter(|pr| pr["state"] == "MERGED")
        .find(|pr| pr["number"].as_i64().map(|n| n.to_string()).as_deref() != Some(number))?;
    let tip = earlier["headRefOid"].as_str()?.to_string();
    let _ = git::fetch(repo, &["origin", head]).await;
    git::run(
        repo,
        "git",
        &[
            "merge-base",
            "--is-ancestor",
            &tip,
            &format!("origin/{head}"),
        ],
    )
    .await
    .ok()?;
    Some((earlier["number"].as_i64()?.to_string(), tip))
}

/// The conflict a branch reused after its pull request was squash-merged
/// runs into: the base has that work as one new commit, the branch has the
/// original commits plus what came after, and git sees both sides changing
/// the same lines. Replaying only what came after onto the base removes the
/// conflict without anyone picking sides — which is where a merge of the
/// base into the branch goes wrong.
///
/// The replay goes on a fresh branch, `new`, with its own pull request, and
/// the old one is closed pointing at it. Rewriting the old branch in place
/// would need a force push, and no built-in step pushes with force.
pub(super) fn replay_after(
    number: &str,
    base: &str,
    head: &str,
    earlier: &(String, String),
    new: &str,
    checks: &str,
) -> StepFailure {
    let (was, tip) = earlier;
    let short = &tip[..tip.len().min(10)];
    // $1 the pull request, $2 base, $3 its branch, $4 the new branch, $5 the
    // earlier pull request's tip, $6 the earlier pull request.
    let script = r#"git fetch origin "$2" "$3" && git checkout -b "$4" "origin/$2" && git cherry-pick "$5..origin/$3" && git push -u origin "$4" || exit 1
title=$(gh pr view "$1" --json title -q .title)
url=$(gh pr create --base "$2" --head "$4" --title "$title" --body "Replaces #$1 with only its own commits: its branch still carried those of #$6, which $2 already has.") || exit 1
gh pr close "$1" --comment "Replaced by $url — the same change, without the commits of #$6." || exit 1
echo "Opened $url and closed #$1.""#;
    let replay = Remedy {
        display: format!(
            "git checkout -b {new} origin/{base} && git cherry-pick {short}..origin/{head} \
             && git push -u origin {new} && gh pr create … && gh pr close {number}"
        ),
        ..Remedy::terminal(
            &format!("Move #{number}'s own commits to a fresh pull request from {base}"),
            "sh",
            &["-c", script, "sh", number, base, head, new, tip, was],
        )
    };
    let mut fallback = conflict_guide(number, base, checks);
    let mut remedies = vec![replay];
    remedies.append(&mut fallback.remedies);
    StepFailure {
        message: format!(
            "#{number} conflicts with {base}, so it is not offered for merging. Nothing has \
             been merged or changed.\n\n\
             Why: {head} was already merged once, in #{was}, and {base} has that work as \
             one squashed commit. The branch still carries #{was}'s original commits under \
             #{number}'s own, so #{number} shows #{was}'s files again and both sides \
             change the same lines.\n\n\
             The first button below takes only the commits made after #{was}, puts them \
             on {new}, a fresh branch from {base}, and opens a pull request for them with \
             #{number}'s title — then closes #{number}, pointing at it. No side has to be \
             picked and nothing is force-pushed. Review the new pull request after. \
             Merging {base} into {head} instead means settling each conflict by hand.\n\n{checks}"
        ),
        remedies,
    }
}

/// What a pull request changes, as `(stat, diff)`: its base and head fetched
/// from origin and compared here. The review step uses it, and so does the
/// run view's Diff button on a pull request's card — git, not the forge, so
/// reading a diff costs none of the hourly API budget and works the same on
/// both platforms.
pub async fn pull_request_diff(
    repo: &str,
    base: &str,
    head: &str,
) -> Result<(String, String), String> {
    // Swallowing this used to leave a failed fetch looking like a missing
    // ref two lines later — "ambiguous argument origin/X...origin/Y" is a
    // confusing way to learn the actual problem was "couldn't fetch base
    // or head from origin" (wrong remote, network, a base branch that no
    // longer exists). Surface the real reason instead.
    git::fetch(repo, &["origin", base, head])
        .await
        .map_err(|e| format!("could not fetch {base} and {head} from origin: {e}"))?;

    let range = format!("origin/{base}...origin/{head}");
    let stat = git::run(repo, "git", &["diff", &range, "--stat"])
        .await
        .unwrap_or_default();
    let diff = git::run(repo, "git", &["diff", &range, "--unified=3"]).await?;
    Ok((stat, diff))
}

/// The diff comes from git, not the forge — one code path for both platforms,
/// and no dependency on a CLI being installed to read it.
pub(super) async fn pr_diff(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let base = state.artifact("pr_base");
    let head = state.artifact("pr_head");
    let (stat, diff) = match pull_request_diff(repo, base, head).await {
        Ok(found) => found,
        Err(e) if head_gone(&e, head) => {
            let number = state.artifact("pr_number");
            return Err(StepFailure::from(format!(
                "The branch `{head}` of pull request {number} no longer exists on origin, so \
                 there is no diff to review. It was most likely merged and its branch \
                 deleted: check the pull request in the browser. If it is merged, there is \
                 nothing left to do — switch to {base} and pull.\n\n{e}"
            )));
        }
        Err(e) => return Err(e.into()),
    };

    if diff.trim().is_empty() {
        let ahead = git::run(
            repo,
            "git",
            &[
                "rev-list",
                "--count",
                &format!("origin/{base}..origin/{head}"),
            ],
        )
        .await
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
        let forge = Forge::from_key(state.artifact("forge"));
        let scope = if forge == Forge::AzureDevOps {
            forge::azure_scope(repo, Scope::Organization).await
        } else {
            vec![]
        };
        return Err(empty_pr(
            &forge,
            &scope,
            state.artifact("pr_number"),
            base,
            head,
            ahead,
        ));
    }

    let files = stat.lines().count().saturating_sub(1);
    Ok(StepOutcome {
        summary: format!("{files} file(s)"),
        log: stat.trim().to_string(),
        artifacts: vec![
            ("pr_diff".into(), git::cap(&diff)),
            ("pr_stat".into(), stat.trim().to_string()),
        ],
        nothing_to_do: false,
        items: vec![],
        remedies: vec![],
    })
}

/// An open pull request that changes nothing still wants a decision, so this
/// is a failure with a fix on offer rather than a quiet "nothing to do" that
/// leaves the sidebar saying "ready to merge" and no way forward. `az_scope`
/// is what `forge::azure_scope` gave for the repository.
pub(super) fn empty_pr(
    forge: &Forge,
    az_scope: &[String],
    number: &str,
    base: &str,
    head: &str,
    ahead: usize,
) -> StepFailure {
    let why = if ahead == 0 {
        format!("Everything on `{head}` is already in `{base}`.")
    } else {
        format!(
            "`{head}` has {ahead} commit(s) that `{base}` does not, but together they change \
             no files — typically merge commits whose conflict resolution kept `{base}`'s side. \
             If those commits were meant to change something, that work was lost in the merge."
        )
    };
    let message = format!(
        "Pull request #{number} ({head} → {base}) has no changes, so there is nothing to review \
         and merging it would change nothing.\n\n{why}\n\nClose it below. The branch is kept, \
         so its history stays reachable."
    );
    let comment = "Closing — this pull request no longer changes any files.";
    let remedies = match forge {
        Forge::GitHub => vec![Remedy::terminal(
            &format!("Close #{number} — it changes nothing"),
            "gh",
            &["pr", "close", number, "--comment", comment],
        )],
        Forge::AzureDevOps => {
            let mut args = vec![
                "repos",
                "pr",
                "update",
                "--id",
                number,
                "--status",
                "abandoned",
                "--output",
                "none",
            ];
            args.extend(az_scope.iter().map(String::as_str));
            vec![Remedy::terminal(
                &format!("Abandon !{number} — it changes nothing"),
                "az",
                &args,
            )]
        }
        _ => vec![],
    };
    StepFailure { message, remedies }
}

/// The command that closes pull request `number` from its card — on Azure,
/// abandons it — or `None` on a forge GitAgent cannot open one on. GitHub and
/// Azure keep every pull request for good, so closing is as close to deleting
/// one as either allows; the branch stays, for the Branches panel to clean up.
pub fn close_command(
    forge: &Forge,
    az_scope: &[String],
    number: &str,
    empty: bool,
) -> Option<(&'static str, Vec<String>)> {
    let owned = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    match forge {
        Forge::GitHub => {
            let comment = if empty {
                "Closing — this pull request no longer changes any files."
            } else {
                "Closing without merging."
            };
            Some(("gh", owned(&["pr", "close", number, "--comment", comment])))
        }
        Forge::AzureDevOps => {
            let mut args = owned(&[
                "repos",
                "pr",
                "update",
                "--id",
                number,
                "--status",
                "abandoned",
                "--output",
                "none",
            ]);
            args.extend(az_scope.iter().cloned());
            Some(("az", args))
        }
        _ => None,
    }
}

/// Closes (on Azure, abandons) pull request `number` of the repository at
/// `repo`, and says so in a line.
pub async fn close_pr(
    repo: &str,
    forge: &Forge,
    number: &str,
    empty: bool,
) -> Result<String, String> {
    let scope = if *forge == Forge::AzureDevOps {
        forge::azure_scope(repo, Scope::Organization).await
    } else {
        vec![]
    };
    let Some((program, args)) = close_command(forge, &scope, number, empty) else {
        return Err("GitAgent can't close pull requests on this remote.".into());
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git::run(repo, program, &args).await?;
    Ok(match forge {
        Forge::AzureDevOps => format!("Abandoned #{number}."),
        _ => format!("Closed #{number}."),
    })
}
