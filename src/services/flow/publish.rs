//! Pushing a branch and opening its pull request.

use super::*;

pub(super) async fn draft_pr(
    cfg: &LlmConfig,
    state: &RunState,
    asker: &mut Asker<'_>,
) -> Result<StepOutcome, StepFailure> {
    let system = "You write pull request descriptions.\n\
        Rules:\n\
        - `title`: one line, at most 72 characters, plain language, no type prefix.\n\
        - `body`: GitHub markdown with exactly three sections: `## What changed`, \
          `## Why`, `## How to test`.\n\
        - `## How to test` must name the specific screens, commands or cases this \
          diff touches. Never write a generic checklist.\n\
        Describe only what the diff shows.";

    let head = format!(
        "Commit subject: {}\n\nDiffstat:\n{}\n\nDiff:\n",
        state.artifact("commit_subject"),
        state.artifact("stat"),
    );
    let fitted = git::fit(
        state.artifact("diff"),
        cfg.input_budget().saturating_sub(head.len()),
    );
    let user = format!("{head}{}", fitted.text);

    let schema = json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "body":  { "type": "string" }
        },
        "required": ["title", "body"]
    });

    let value = complete_json(cfg, system, &user, &schema, asker).await?;
    let title = value["title"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let body = value["body"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();

    if title.is_empty() {
        return Err("model returned an empty PR title".into());
    }

    Ok(StepOutcome {
        summary: title.clone(),
        log: format!("{title}\n\n{body}{}", fitted.note()),
        artifacts: vec![("pr_title".into(), title), ("pr_body".into(), body)],
        nothing_to_do: false,
        items: vec![],
    })
}

/// A push git refused because the branch is behind its remote.
///
/// Universal enough to detect from output alone, and the fix is exactly the
/// one git itself suggests — so offer it rather than printing a hint and
/// sending the person to a terminal. Rebase rather than merge: it keeps the
/// linear history these repositories already have.
pub(super) fn pull_remedy(output: &str) -> Vec<Remedy> {
    if !push_rejected(output) {
        return vec![];
    }
    vec![Remedy::new(
        "Pull, and replay your commits on top",
        "git",
        &["pull", "--rebase"],
    )]
}

/// Why the branch of the same name on origin is somebody else's finished
/// work rather than newer commits of this one — `None` when it may well be
/// the latter, and pulling is the right fix after all.
///
/// The forge first, because squash merges leave nothing in git to connect a
/// branch to what it became on the base. Git second, for a plain merge.
pub(super) async fn leftover_on_origin(
    repo: &str,
    state: &RunState,
    branch: &str,
) -> Option<String> {
    let base = state.artifact("base");
    if branch == base {
        return None;
    }
    if Forge::from_key(state.artifact("forge")) == Forge::GitHub {
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
                "number,state",
            ],
        )
        .await
        .ok()?;
        let prs: serde_json::Value = serde_json::from_str(&out).ok()?;
        let prs = prs.as_array()?;
        if prs.iter().any(|p| p["state"] == "OPEN") {
            return None;
        }
        if let Some(pr) = prs.first() {
            return Some(format!(
                "it belongs to #{}, which is {}",
                pr["number"],
                pr["state"].as_str().unwrap_or("closed").to_lowercase()
            ));
        }
    }
    git::fetch(repo, &["origin", branch, base]).await.ok()?;
    git::run(
        repo,
        "git",
        &[
            "merge-base",
            "--is-ancestor",
            &format!("origin/{branch}"),
            &format!("origin/{base}"),
        ],
    )
    .await
    .ok()
    .map(|_| format!("everything on it is already in {base}"))
}

pub(super) async fn push(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let branch = state.artifact("work_branch").to_string();
    // Preflight lets a repository without a remote through, for the steps
    // before this one; git's own words for it ("'origin' does not appear to
    // be a git repository") would read as a broken repository.
    if git::remote_url(repo).await.is_none() {
        return Err(StepFailure {
            message: format!(
                "this repository has no `origin` remote, so {branch} was committed but not \
                 pushed. Add one with `git remote add origin <url>`, then run the flow again."
            ),
            remedies: vec![],
        });
    }
    let out = match git::push(repo, &branch).await {
        Ok(out) => out,
        Err(e) if push_rejected(&e) => {
            let Some(why) = leftover_on_origin(repo, state, &branch).await else {
                return Err(StepFailure {
                    remedies: pull_remedy(&e),
                    message: e,
                });
            };
            let new = free_branch_name(repo, &branch).await;
            return Err(StepFailure {
                message: format!(
                    "origin already has a branch called {branch}, and {why}: it is left over, \
                     not newer work of yours. Pulling it in would mix its old commits into \
                     this change.\n\n\
                     Rename yours to {new} and the push, and the pull request after it, go \
                     ahead under that name. Your commits are untouched.\n\n{e}"
                ),
                remedies: vec![rename_remedy(&branch, &new)],
            });
        }
        Err(e) => return Err(e.into()),
    };
    Ok(StepOutcome {
        summary: format!("pushed {branch}"),
        log: out.clone(),
        artifacts: vec![("push_output".into(), out)],
        nothing_to_do: false,
        items: vec![],
    })
}

pub(super) async fn open_pr(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    if is_first_commit(state) {
        return Ok(StepOutcome::nothing(first_commit_note(state)));
    }
    let forge = Forge::from_key(state.artifact("forge"));
    let url = forge::create_pr(
        &forge,
        repo,
        state.artifact("base"),
        state.artifact("work_branch"),
        state.artifact("pr_title"),
        state.artifact("pr_body"),
    )
    .await?;
    Ok(StepOutcome {
        summary: url.clone(),
        log: url.clone(),
        artifacts: vec![("pr_url".into(), url)],
        nothing_to_do: false,
        items: vec![],
    })
}
