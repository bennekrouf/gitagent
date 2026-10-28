//! Open pull requests on the forge, and the state of their checks.

use super::*;

/// One GitHub PR JSON object (from either `pr view` or `pr list`, same
/// field names either way) into a `PrBrief`.
pub(super) fn github_pr_from_json(value: &serde_json::Value) -> Option<PrBrief> {
    Some(PrBrief {
        number: value["number"].as_i64()?.to_string(),
        title: value["title"].as_str().unwrap_or_default().to_string(),
        url: value["url"].as_str().unwrap_or_default().to_string(),
        checks: rollup(&value["statusCheckRollup"]),
        failed: failed_checks(&value["statusCheckRollup"]),
        files: value["changedFiles"].as_u64().unwrap_or(0) as usize,
        additions: value["additions"].as_u64().unwrap_or(0) as usize,
        deletions: value["deletions"].as_u64().unwrap_or(0) as usize,
        commits: value["commits"].as_array().map(|a| a.len()).unwrap_or(0),
        base: value["baseRefName"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        head: value["headRefName"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
}

/// Every open pull request on the repository — not scoped to the checked-out
/// branch, unlike `open_pr` below. What lets the sidebar offer a choice of
/// which PR to review instead of only ever "whichever one HEAD happens to
/// point at".
/// `Err` only for an actual failed call (rate limit, network, auth) — a
/// repository with genuinely zero open PRs is `Ok(vec![])`. Collapsing those
/// two into one silently showed "clean" for a check that never happened, no
/// different from a repo that really was clean.
pub(super) async fn list_open_prs(repo: &str, forge: &Forge) -> Result<Vec<PrBrief>, String> {
    match forge {
        Forge::GitHub => {
            let out = git::run(
                repo,
                "gh",
                &["pr", "list", "--state", "open", "--json", PR_FIELDS],
            )
            .await?;
            let value: serde_json::Value =
                serde_json::from_str(&out).map_err(|e| format!("could not read gh output: {e}"))?;
            Ok(value
                .as_array()
                .map(|prs| prs.iter().filter_map(github_pr_from_json).collect())
                .unwrap_or_default())
        }
        Forge::AzureDevOps => {
            let scope = forge::azure_scope(repo, forge::Scope::Repository).await;
            let mut args = vec![
                "repos", "pr", "list", "--status", "active", "--output", "json",
            ];
            args.extend(scope.iter().map(String::as_str));
            let out = git::run(repo, "az", &args).await?;
            let list: serde_json::Value =
                serde_json::from_str(&out).map_err(|e| format!("could not read az output: {e}"))?;
            Ok(list
                .as_array()
                .map(|prs| {
                    prs.iter()
                        .filter_map(|pr| {
                            let number = pr["pullRequestId"].as_i64()?.to_string();
                            let web = pr["repository"]["webUrl"].as_str().unwrap_or_default();
                            Some(PrBrief {
                                number: number.clone(),
                                title: pr["title"].as_str().unwrap_or_default().to_string(),
                                url: format!("{web}/pullrequest/{number}"),
                                checks: Checks::Unknown,
                                files: 0,
                                additions: 0,
                                deletions: 0,
                                commits: 0,
                                base: azure_branch(&pr["targetRefName"]),
                                head: azure_branch(&pr["sourceRefName"]),
                                failed: vec![],
                            })
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
        _ => Ok(vec![]),
    }
}

pub(super) async fn open_pr(repo: &str, forge: &Forge) -> Option<PrBrief> {
    match forge {
        Forge::GitHub => {
            let out = git::run(repo, "gh", &["pr", "view", "--json", PR_FIELDS])
                .await
                .ok()?;
            let value: serde_json::Value = serde_json::from_str(&out).ok()?;
            // `gh pr view` answers for the checked-out branch whatever the
            // pull request's state, so a merged one comes back looking open.
            // Reporting that as "ready to merge" sends the review flow at a
            // branch the remote deleted on merge.
            if !is_open(&value) {
                return None;
            }
            github_pr_from_json(&value)
        }
        Forge::AzureDevOps => {
            let branch = git::current_branch(repo).await.ok()?;
            let scope = forge::azure_scope(repo, forge::Scope::Repository).await;
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
            let out = git::run(repo, "az", &args).await.ok()?;
            let list: serde_json::Value = serde_json::from_str(&out).ok()?;
            let pr = list.as_array()?.first()?;
            let number = pr["pullRequestId"].as_i64()?.to_string();
            let web = pr["repository"]["webUrl"].as_str().unwrap_or_default();
            Some(PrBrief {
                number: number.clone(),
                title: pr["title"].as_str().unwrap_or_default().to_string(),
                url: format!("{web}/pullrequest/{number}"),
                // Azure policy evaluation is a different shape; saying we did
                // not look beats reporting a green build nobody checked.
                checks: Checks::Unknown,
                files: 0,
                additions: 0,
                deletions: 0,
                commits: 0,
                base: azure_branch(&pr["targetRefName"]),
                head: azure_branch(&pr["sourceRefName"]),
                failed: vec![],
            })
        }
        _ => None,
    }
}

/// A failing check outranks a pending one: red is worth surfacing immediately,
/// and a run still going could turn red anyway.
pub(super) fn rollup(value: &serde_json::Value) -> Checks {
    let Some(checks) = value.as_array() else {
        return Checks::Unknown;
    };
    if checks.is_empty() {
        return Checks::Unknown;
    }

    let (mut failing, mut pending) = (false, false);
    for check in checks {
        if is_pending(check) {
            pending = true;
        } else if has_failed(check) {
            failing = true;
        }
    }

    match (failing, pending) {
        (true, _) => Checks::Failing,
        (false, true) => Checks::Pending,
        _ => Checks::Passing,
    }
}

/// The checks `rollup` counted as failing, by name, with their log pages.
///
/// The rollup holds two shapes: check runs (`name`, `detailsUrl`) from
/// Actions and apps, and commit statuses (`context`, `targetUrl`) from older
/// integrations. A name reported twice — a re-run, a matrix leg — is listed
/// once, with the first link.
pub(super) fn failed_checks(value: &serde_json::Value) -> Vec<FailedCheck> {
    let mut out: Vec<FailedCheck> = vec![];
    for check in value.as_array().into_iter().flatten() {
        if !has_failed(check) {
            continue;
        }
        let name = check["name"]
            .as_str()
            .or_else(|| check["context"].as_str())
            .unwrap_or("check")
            .to_string();
        if out.iter().any(|f| f.name == name) {
            continue;
        }
        let url = check["detailsUrl"]
            .as_str()
            .or_else(|| check["targetUrl"].as_str())
            .unwrap_or("")
            .to_string();
        out.push(FailedCheck { name, url });
    }
    out
}
