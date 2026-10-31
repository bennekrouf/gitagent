//! The commit step: what changed, the message, the branch it goes on.

use super::*;

/// `Some` when the working tree is clean but the branch is already ahead of
/// `base` — commits made outside this run, still unpushed. `None` when there
/// is truly nothing (a fresh checkout of `base` itself, or a branch already
/// level with it), in which case `scan` falls back to its usual "nothing to
/// commit" outcome.
pub(super) async fn already_committed(
    repo: &str,
    branch: &str,
    state: &RunState,
) -> Result<Option<StepOutcome>, StepFailure> {
    let base = state.artifact("base");
    if base.is_empty() {
        return Ok(None);
    }
    let tracked = git::run(
        repo,
        "git",
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{base}"),
        ],
    )
    .await
    .is_ok();
    // Sitting on `base` itself, only origin's copy says what is unpushed:
    // the local one is HEAD. Commits made straight on `base` are exactly
    // what the sidebar offers to push, and the commit step moves them onto
    // a branch of their own.
    let base_ref = match (tracked, base == branch) {
        (true, _) => format!("origin/{base}"),
        (false, true) => return Ok(None),
        (false, false) => base.to_string(),
    };

    let range = format!("{base_ref}..HEAD");
    let ahead: usize = git::run(repo, "git", &["rev-list", "--count", &range])
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if ahead == 0 {
        return Ok(None);
    }

    let stat = git::run(repo, "git", &["diff", &range, "--stat"])
        .await
        .unwrap_or_default();
    let diff = git::run(repo, "git", &["diff", &range, "--unified=3"]).await?;
    let plural = if ahead == 1 { "" } else { "s" };

    Ok(Some(StepOutcome {
        summary: format!("{ahead} commit{plural} already on {branch}, ready to push"),
        log: format!(
            "branch: {branch}\n\n{stat}\n\n{ahead} commit{plural} ahead of {base}, none pushed yet\n"
        ),
        artifacts: vec![
            ("branch".into(), branch.to_string()),
            ("stat".into(), stat),
            ("diff".into(), git::cap(&diff)),
            ("commit_paths".into(), String::new()),
            ("file_notes".into(), String::new()),
            ("untracked".into(), String::new()),
        ],
        nothing_to_do: false,
            items: vec![],
        }))
}

/// The pull request `branch` was already merged through, when it was and
/// nothing open has taken the branch up again since.
#[derive(Clone, Debug, PartialEq)]
pub struct MergedBranch {
    pub number: String,
    /// The branch's tip when it was merged.
    pub head: String,
    /// Commits on the branch since then — in no pull request at all.
    pub beyond: usize,
}

impl MergedBranch {
    fn artifacts(merged: Option<&MergedBranch>) -> Vec<(String, String)> {
        vec![
            (
                "merged_pr".into(),
                merged.map(|m| m.number.clone()).unwrap_or_default(),
            ),
            (
                "merged_head".into(),
                merged.map(|m| m.head.clone()).unwrap_or_default(),
            ),
        ]
    }
}

/// From `gh pr list --head <branch> --state all --json number,state,headRefOid`,
/// newest first: the merged pull request, unless one is open on the branch —
/// then the branch is in use again and new work rightly goes on it.
pub fn merged_from_list(json: &str) -> Option<(String, String)> {
    let list: serde_json::Value = serde_json::from_str(json).ok()?;
    let list = list.as_array()?;
    if list.iter().any(|pr| pr["state"] == "OPEN") {
        return None;
    }
    let merged = list.iter().find(|pr| pr["state"] == "MERGED")?;
    Some((
        merged["number"].as_i64()?.to_string(),
        merged["headRefOid"].as_str()?.to_string(),
    ))
}

/// Whether the checked-out branch's pull request was already merged.
///
/// A squash merge leaves the branch looking unmerged to git — main has a new
/// commit, not the branch's — so committing on it again stacks new work on
/// top of what main already has. The next pull request then shows both, and
/// conflicts with main over the part that is already there. Only the forge
/// knows, so this asks it; GitHub only, and any failure reads as "not merged".
pub(super) async fn merged_branch(
    repo: &str,
    branch: &str,
    state: &RunState,
) -> Option<MergedBranch> {
    if Forge::from_key(state.artifact("forge")) != Forge::GitHub
        || branch.is_empty()
        || branch == state.artifact("base")
        || git::is_protected(branch)
    {
        return None;
    }
    let out = git::run(
        repo,
        "gh",
        &[
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state,headRefOid",
            "--limit",
            "5",
        ],
    )
    .await
    .ok()?;
    let (number, head) = merged_from_list(&out)?;
    // A tip this copy does not have, or one the branch has moved off since
    // (amended, reset), says nothing reliable about what is on it now.
    git::run(repo, "git", &["merge-base", "--is-ancestor", &head, "HEAD"])
        .await
        .ok()?;
    let beyond = git::run(
        repo,
        "git",
        &["rev-list", "--count", &format!("{head}..HEAD")],
    )
    .await
    .ok()?
    .trim()
    .parse()
    .ok()?;
    Some(MergedBranch {
        number,
        head,
        beyond,
    })
}

/// Commits made on a branch after its pull request was merged. Pushing them
/// there would open a pull request carrying the merged work again, so they
/// are moved onto a fresh branch from the base first.
fn commits_after_merge(branch: &str, base: &str, merged: &MergedBranch, new: &str) -> StepFailure {
    let n = plural(merged.beyond, "commit");
    let script = r#"git fetch origin "$3" && git rebase --autostash --onto "origin/$3" "$2" && git branch -m "$1" "$4""#;
    StepFailure {
        message: format!(
            "{branch} was already merged in #{number}, and has {n} since that are in no \
             pull request. Committing or pushing here would open a pull request that \
             carries #{number}'s changes again — and conflicts with {base} over them.\n\n\
             Move the {n} onto a fresh branch from {base} below: only {they} replayed, \
             under the name {new}. Then run Commit → PR again.",
            number = merged.number,
            they = if merged.beyond == 1 { "it is" } else { "they are" },
        ),
        remedies: vec![Remedy {
            display: format!(
                "git fetch origin {base} && git rebase --autostash --onto origin/{base} {} && git branch -m {branch} {new}",
                &merged.head[..merged.head.len().min(10)]
            ),
            retry_after: false,
            ..Remedy::new(
                &format!("Move the {n} onto a fresh branch from {base}"),
                "sh",
                &["-c", script, "sh", branch, &merged.head, base, new],
            )
        }],
    }
}

pub(super) async fn scan(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let branch = git::current_branch(repo).await?;
    let changes = git::status(repo).await?;
    let first_commit = !git::has_commits(repo).await;
    let merged = merged_branch(repo, &branch, state).await;

    if let Some(merged) = merged.as_ref().filter(|m| m.beyond > 0) {
        let new = free_branch_name(repo, &branch).await;
        return Err(commits_after_merge(
            &branch,
            state.artifact("base"),
            merged,
            &new,
        ));
    }

    if changes.is_empty() {
        if let Some(merged) = &merged {
            return Ok(StepOutcome::nothing(format!(
                "No changes in the working tree, and {branch} was already merged in #{} \
                 — nothing to commit. Switch to {} and pull to carry on from there.",
                merged.number,
                state.artifact("base"),
            )));
        }
        // A clean tree isn't necessarily nothing to do — a commit made
        // outside GitAgent (or by a previous run) can already sit ahead of
        // `base`, unpushed. Diff those commits against `base` so the rest
        // of the flow — draft, commit (a no-op here), push, open PR — has
        // something to work from instead of everything downstream getting
        // skipped just because there was nothing to *stage*.
        if let Some(outcome) = already_committed(repo, &branch, state).await? {
            return Ok(outcome);
        }
        return Ok(StepOutcome::nothing(
            "No changes in the working tree — nothing to commit.",
        ));
    }

    // Everything git reports is a candidate, new files included. `git status`
    // already filters out anything .gitignore covers, and the commit approval
    // lists each file individually with a checkbox — that is the safety net,
    // not a blanket rule about which kinds of change are allowed.
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let stat = git::diff_stat(repo).await?;

    let diff = working_tree_diff(repo, &changes).await?;

    let listing = changes
        .iter()
        .map(|c| format!("  {:<9} {}", c.note(), c.path))
        .collect::<Vec<_>>()
        .join("\n");
    let log = format!("branch: {branch}\n\n{stat}\n\n{listing}\n");
    let untracked: Vec<&git::FileChange> = changes.iter().filter(|c| c.is_untracked()).collect();

    let items = changes
        .iter()
        .map(|c| ProposalItem {
            key: c.path.clone(),
            label: c.path.clone(),
            note: c.note().to_string(),
            included: true,
        })
        .collect();

    let log = match &merged {
        Some(m) => format!(
            "{log}\n{branch} was already merged in #{} — these changes go on a fresh \
             branch from {}.\n",
            m.number,
            state.artifact("base")
        ),
        None => log,
    };
    let mut artifacts = MergedBranch::artifacts(merged.as_ref());
    artifacts.extend([
        ("branch".into(), branch.clone()),
        ("stat".into(), stat),
        ("diff".into(), diff),
        ("commit_paths".into(), paths.join("\n")),
        (
            "file_notes".into(),
            changes
                .iter()
                .map(|c| format!("{}\t{}", c.path, c.note()))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "untracked".into(),
            untracked
                .iter()
                .map(|c| c.path.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "first_commit".into(),
            if first_commit { "true" } else { "" }.into(),
        ),
    ]);

    Ok(StepOutcome {
        items,
        summary: format!("{} file(s) changed on {branch}", changes.len()),
        log,
        artifacts,
        nothing_to_do: false,
    })
}

pub(super) async fn draft_commit(
    cfg: &LlmConfig,
    state: &RunState,
    asker: &mut Asker<'_>,
) -> Result<StepOutcome, StepFailure> {
    let system = "You write git commit messages.\n\
        Rules:\n\
        - `subject`: Conventional Commits (feat/fix/chore/docs/refactor/test/perf), \
          imperative mood, at most 72 characters, no trailing period.\n\
        - `body`: one to three short lines saying WHY the change was made. Empty \
          string if the subject already says everything.\n\
        - `branch`: kebab-case, prefixed with the same type and a slash \
          (e.g. `fix/stale-lockfile`), at most 40 characters.\n\
        Describe only what the diff actually shows. Do not invent motivation.";

    let head = format!("Diffstat:\n{}\n\nDiff:\n", state.artifact("stat"));
    let fitted = git::fit(
        state.artifact("diff"),
        cfg.input_budget().saturating_sub(head.len()),
    );
    let user = format!("{head}{}", fitted.text);

    let schema = json!({
        "type": "object",
        "properties": {
            "branch":  { "type": "string" },
            "subject": { "type": "string" },
            "body":    { "type": "string" }
        },
        "required": ["branch", "subject", "body"]
    });

    let value = complete_json(cfg, system, &user, &schema, asker).await?;

    let subject = value["subject"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let body = value["body"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let branch = sanitise_branch(value["branch"].as_str().unwrap_or_default());

    if subject.is_empty() {
        return Err("model returned an empty commit subject".into());
    }

    Ok(StepOutcome {
        summary: subject.clone(),
        log: format!(
            "branch:  {branch}\nsubject: {subject}\n\n{body}{}",
            fitted.note()
        ),
        artifacts: vec![
            ("branch_name".into(), branch),
            ("commit_subject".into(), subject),
            ("commit_body".into(), body),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}

/// Branch names come from a model, so they get scrubbed before they reach a
/// shell argument: lowercase, only `[a-z0-9._/-]`, no leading or trailing
/// separators, length-capped.
pub fn sanitise_branch(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().to_lowercase().chars() {
        match ch {
            'a'..='z' | '0'..='9' | '/' | '.' | '_' => out.push(ch),
            '-' => out.push('-'),
            ' ' | '\t' => out.push('-'),
            _ => {}
        }
    }
    // Truncate first, then trim. The other order lets truncation put a
    // separator back on the end — git refuses a refname ending in a dot, so
    // a 40-character cut landing on one turned a sanitised name back into an
    // invalid one.
    let out: String = out.chars().take(40).collect();
    let out = out.trim_matches(|c| c == '-' || c == '/' || c == '.');
    // `..` and a `.lock` suffix are refnames git rejects outright, and both
    // survive a character filter that judges one character at a time.
    let out = out.replace("..", ".").replace("//", "/");
    let out = out.strip_suffix(".lock").unwrap_or(&out).to_string();
    let out = out
        .trim_matches(|c| c == '-' || c == '/' || c == '.')
        .to_string();
    if out.is_empty() {
        "gitagent/change".to_string()
    } else {
        out
    }
}

/// The paths this commit node should stage.
///
/// Whether the node offered a choice is the thing to branch on, not whether
/// anything is still checked. Once items were offered, what stayed checked is
/// the whole of the answer — widening back to the full scan because the list
/// came back empty would stage exactly the files that were unchecked, which
/// is the one outcome the approval exists to prevent. Only a node that never
/// offered a list falls back to the scan, and that case is `scan` finding the
/// tree already clean with the branch ahead of `base`.
pub(super) fn paths_to_stage(node_id: &str, state: &RunState) -> Vec<String> {
    if offered_a_choice(node_id, state) {
        return state
            .runs
            .get(node_id)
            .map(|r| r.included_keys())
            .unwrap_or_default();
    }
    state
        .artifact("commit_paths")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Set when the commit step finds, just before committing, that the changes
/// it was approved to commit are no longer there: another run committed them
/// while this one sat at its approval. Says where they went, rather than
/// failing on a branch it would have opened for nothing.
async fn committed_meanwhile(
    node_id: &str,
    repo: &str,
    state: &RunState,
) -> Result<Option<StepOutcome>, StepFailure> {
    // No paths is either every file unchecked, which fails further on as it
    // should, or a scan that found commits already made and nothing to stage.
    let paths = paths_to_stage(node_id, state);
    if paths.is_empty() {
        return Ok(None);
    }
    let live = git::status(repo).await?;
    if live.iter().any(|c| paths.contains(&c.path)) {
        return Ok(None);
    }
    let branch = git::current_branch(repo).await.unwrap_or_default();
    let last = git::run(repo, "git", &["log", "-1", "--format=%h %s"])
        .await
        .unwrap_or_default();
    let pr = match Forge::from_key(state.artifact("forge")) {
        Forge::GitHub if !branch.is_empty() => git::run(
            repo,
            "gh",
            &[
                "pr",
                "view",
                &branch,
                "--json",
                "number,url",
                "-q",
                ".number",
            ],
        )
        .await
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty()),
        _ => None,
    };
    let headline = match &pr {
        Some(n) => format!("Already committed in PR #{n}"),
        None => format!("Already committed on {branch}"),
    };
    let message = format!(
        "{headline} — the changes this run found were committed while it waited, \
         most likely by another run. Nothing left to commit here.\n\n\
         Checked out: {branch}\nLast commit: {}",
        last.trim()
    );
    Ok(Some(StepOutcome {
        summary: headline,
        log: message,
        artifacts: vec![],
        nothing_to_do: true,
        items: vec![],
    }))
}

/// `node_id` is the id of the commit node actually running, not the literal
/// `"commit"`. A flow built in Setup may name it anything, and a flow with two
/// commit steps names the second one `commit_2` — reading a fixed id there
/// would hand the second node the first one's checkboxes, and reading an id
/// that matches nothing would fall through to staging every path the scan
/// found. Both defeat the approval.
pub(super) async fn commit(
    node_id: &str,
    repo: &str,
    state: &RunState,
) -> Result<StepOutcome, StepFailure> {
    // Read again now, not at the scan: an approval can sit for minutes, and
    // whatever happened to the tree meanwhile decides what is safe to do.
    if let Some(done) = committed_meanwhile(node_id, repo, state).await? {
        return Ok(done);
    }
    let started_on = git::current_branch(repo).await?;
    if !is_first_commit(state) && started_on != state.artifact("branch") {
        return Err(format!(
            "The checkout moved from {} to {started_on} after this run looked at the \
             working tree — another run, or someone, switched it. Nothing was committed \
             and no branch was made. Start the run again to commit from where things \
             are now.",
            state.artifact("branch")
        )
        .into());
    }

    let mut log = String::new();
    let (work_branch, branched) = switch_to_work_branch(repo, state, &mut log).await?;

    match commit_on(node_id, repo, state, work_branch, log).await {
        Ok(outcome) => Ok(outcome),
        Err(mut failure) => {
            if let Some(opened) = branched {
                failure.message = format!(
                    "{}\n\n{}",
                    failure.message,
                    put_back(repo, &started_on, &opened).await
                );
            }
            Err(failure)
        }
    }
}

/// A topic branch the commit step opened, and the commit it started at.
pub(super) struct Opened {
    pub name: String,
    pub at: String,
}

/// Undoes the checkout a failed commit made: back to `started_on`, and the
/// branch it opened deleted when nothing was committed on it. Uncommitted
/// changes travel with the checkout both ways, so they are where they were.
/// Says what it did, for the failure's message.
async fn put_back(repo: &str, started_on: &str, opened: &Opened) -> String {
    let name = &opened.name;
    if let Err(e) = git::run(repo, "git", &["checkout", started_on]).await {
        return format!(
            "The checkout moved to {name} before this failed, and could not be put back \
             on {started_on}:\n{e}\nYour changes are on {name}, not on the branch you \
             started from."
        );
    }
    // Deleted only while it still points where it was made, checked by git
    // in the same breath: a branch with any commit of its own is kept.
    let gone = git::run(
        repo,
        "git",
        &[
            "update-ref",
            "-d",
            &format!("refs/heads/{name}"),
            &opened.at,
        ],
    )
    .await
    .is_ok();
    if gone {
        format!(
            "The checkout is back on {started_on}, as it was before this step, and the \
             empty branch {name} it had opened is gone."
        )
    } else {
        format!(
            "The checkout is back on {started_on}, as it was before this step. {name} \
             was kept: it has a commit on it."
        )
    }
}

/// Opens the topic branch a commit needs, if it needs one.
///
/// Returns the branch to commit on, and — separately — the branch opened
/// only when this call is what moved the checkout, which is what the caller
/// needs in order to put it back after a later failure.
pub(super) async fn switch_to_work_branch(
    repo: &str,
    state: &RunState,
    log: &mut String,
) -> Result<(String, Option<Opened>), StepFailure> {
    // Only branch when sitting on the base branch; if the user is already on a
    // topic branch, commit there rather than stacking another one.
    if !must_branch(state) {
        let name = state.artifact("branch").to_string();
        log.push_str(&if is_first_commit(state) {
            format!("first commit in this repository — made on {name} itself\n")
        } else {
            format!("already on {name}, committing there\n")
        });
        return Ok((name, None));
    }

    let mut name = state.artifact("branch_name").to_string();
    // Origin too: a merged pull request's branch often outlives it there, and
    // reusing its name makes the push fail as "behind its remote counterpart".
    if git::branch_exists(repo, &name).await
        || git::branch_exists(repo, &format!("refs/remotes/origin/{name}")).await
    {
        name = format!("{name}-{}", chrono::Local::now().format("%H%M%S"));
    }

    let merged = state.artifact("merged_pr");
    if merged.is_empty() {
        let at = git::run(repo, "git", &["rev-parse", "HEAD"])
            .await?
            .trim()
            .to_string();
        log.push_str(&git::create_branch(repo, &name).await?);
        log.push_str(&format!("created and switched to {name}\n"));
        if moves_commits_off_base(state) {
            log.push_str(&reset_base_to_origin(repo, state.artifact("base"), &name).await);
        }
        return Ok((name.clone(), Some(Opened { name, at })));
    }

    // The branch checked out was merged already: start from the base as it
    // is on origin, not from the merged branch, so the next pull request
    // holds this change and nothing else.
    let base = state.artifact("base");
    let from = format!("origin/{base}");
    git::fetch(repo, &["origin", base]).await?;
    let at = git::run(repo, "git", &["rev-parse", &from])
        .await?
        .trim()
        .to_string();
    let here = state.artifact("branch");
    match git::run(repo, "git", &["checkout", "-b", &name, &from]).await {
        Ok(out) => {
            log.push_str(&out);
            log.push_str(&format!(
                "{here} was already merged in #{merged}: created {name} from {from} and \
                 switched to it, bringing the changes along\n"
            ));
            Ok((name.clone(), Some(Opened { name, at })))
        }
        Err(e) => {
            let script = r#"git stash push --include-untracked && git checkout -b "$1" "origin/$2" && git stash pop"#;
            Err(StepFailure {
                message: format!(
                    "{here} was already merged in #{merged}, so this commit belongs on a \
                     fresh branch from {base}. Git would not move your changes there: \
                     some of the files they touch changed on {base} since. Nothing was \
                     moved or committed.\n\n\
                     Carry them over below — they are set aside, {name} is made from \
                     {from}, and they are put back. If they clash with what {base} has, \
                     both versions are marked in the file and a copy stays in the stash.\n\n{e}"
                ),
                remedies: vec![Remedy {
                    display: format!(
                        "git stash push --include-untracked && git checkout -b {name} {from} && git stash pop"
                    ),
                    ..Remedy::new(
                        &format!("Carry your changes over to {name}, from {base}"),
                        "sh",
                        &["-c", script, "sh", &name, base],
                    )
                    .setting("branch", &name)
                    .setting("merged_pr", "")
                }],
            })
        }
    }
}

/// Whether the commit step is carrying commits already made on `base` itself
/// onto a branch of their own: `scan` found nothing to stage, only commits
/// origin's `base` does not have.
pub(super) fn moves_commits_off_base(state: &RunState) -> bool {
    !is_first_commit(state)
        && state.artifact("merged_pr").is_empty()
        && !state.artifact("base").is_empty()
        && state.artifact("branch") == state.artifact("base")
        && state.artifact("commit_paths").trim().is_empty()
}

/// Puts `base` back where origin has it, once its unpushed commits are on
/// `name`. Left as it was, it would still hold them after the pull request
/// is squash-merged, and could no longer be brought up to date. Nothing is
/// lost — `name` holds every commit — so a refusal is only noted.
async fn reset_base_to_origin(repo: &str, base: &str, name: &str) -> String {
    let origin = format!("origin/{base}");
    match git::run(repo, "git", &["branch", "-f", base, &origin]).await {
        Ok(_) => format!("{base} put back to {origin}: its unpushed commits are on {name} now\n"),
        Err(e) => format!(
            "{base} still has the commits that are on {name} — git would not move it \
             back to {origin}:\n{e}\n"
        ),
    }
}

/// Stages the approved paths and commits them, on a branch already checked
/// out by the caller.
pub(super) async fn commit_on(
    node_id: &str,
    repo: &str,
    state: &RunState,
    work_branch: String,
    mut log: String,
) -> Result<StepOutcome, StepFailure> {
    let items_offered = offered_a_choice(node_id, state);
    let paths = paths_to_stage(node_id, state);
    if paths.is_empty() {
        // Nothing was ever proposed to stage (`items` empty) means `scan`
        // found the tree already clean but the branch ahead of `base` — the
        // commit already exists, made outside GitAgent. Unchecking every
        // proposed file, by contrast, is a real "stop" the human meant.
        if items_offered {
            return Err("no files selected to stage".into());
        }
        let sha = git::head_sha(repo).await?;
        log.push_str(&format!("nothing to stage — {sha} already committed\n"));
        return Ok(StepOutcome {
            summary: format!("{sha} already on {work_branch}"),
            log,
            artifacts: vec![
                ("work_branch".into(), work_branch),
                ("commit_sha".into(), sha),
            ],
            nothing_to_do: false,
            items: vec![],
        });
    }

    // Re-read status rather than trusting the scan: an approval can sit for a
    // while, and what still needs staging may have changed underneath.
    let live = git::status(repo).await.unwrap_or_default();
    let to_stage = git::needs_staging(&live, &paths);

    if !to_stage.is_empty() {
        git::add(repo, &to_stage).await?;
        log.push_str(&format!("staged {} file(s)\n", to_stage.len()));
    }
    if paths.len() > to_stage.len() {
        log.push_str(&format!(
            "{} path(s) already staged\n",
            paths.len() - to_stage.len()
        ));
    }

    // Anything staged that was not approved has to leave the index, or the
    // commit would quietly contain more than the approval showed. Only the
    // index is touched — the files themselves are left exactly as they are.
    let extra = git::staged_but_unapproved(&live, &paths);
    if !extra.is_empty() {
        git::unstage(repo, &extra).await?;
        log.push_str(&format!(
            "unstaged {} path(s) that were not approved (working tree untouched):\n{}\n",
            extra.len(),
            extra
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    // The index now matches the approval, so commit it as it stands — which
    // preserves any hunks staged by hand with `git add -p`.
    let out = git::commit(
        repo,
        state.artifact("commit_subject"),
        state.artifact("commit_body"),
    )
    .await?;
    log.push_str(&out);

    let sha = git::head_sha(repo).await?;

    Ok(StepOutcome {
        summary: format!("{sha} on {work_branch}"),
        log,
        artifacts: vec![
            ("work_branch".into(), work_branch),
            ("commit_sha".into(), sha),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}
