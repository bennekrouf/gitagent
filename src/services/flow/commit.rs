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
    if base.is_empty() || base == branch {
        return Ok(None);
    }
    let base_ref = if git::run(
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
    .is_ok()
    {
        format!("origin/{base}")
    } else {
        base.to_string()
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

pub(super) async fn scan(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let branch = git::current_branch(repo).await?;
    let changes = git::status(repo).await?;
    let first_commit = !git::has_commits(repo).await;

    if changes.is_empty() {
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

    Ok(StepOutcome {
        items,
        summary: format!("{} file(s) changed on {branch}", changes.len()),
        log,
        artifacts: vec![
            ("branch".into(), branch),
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
        ],
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
    let mut log = String::new();
    let (work_branch, branched) = switch_to_work_branch(repo, state, &mut log).await?;

    commit_on(node_id, repo, state, work_branch, log)
        .await
        .map_err(|mut failure| {
            // Everything past the checkout runs with the person somewhere
            // they did not ask to be. Leaving that out of the error is how
            // someone ends up wondering why their branch changed. The branch
            // itself is kept: their staged work is on it, and deleting it
            // would be this app discarding local work.
            if let Some(name) = branched {
                failure.message = format!(
                    "{}\n\nThe checkout moved to {name} before this failed, and has been \
                     left there — your changes are on that branch, not on the one you \
                     started from.",
                    failure.message
                );
            }
            failure
        })
}

/// Opens the topic branch a commit needs, if it needs one.
///
/// Returns the branch to commit on, and — separately — the name only when
/// this call is what moved the checkout, which is what the caller needs in
/// order to explain a later failure.
pub(super) async fn switch_to_work_branch(
    repo: &str,
    state: &RunState,
    log: &mut String,
) -> Result<(String, Option<String>), StepFailure> {
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
    log.push_str(&git::create_branch(repo, &name).await?);
    log.push_str(&format!("created and switched to {name}\n"));
    Ok((name.clone(), Some(name)))
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
