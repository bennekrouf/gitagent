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

/// One `az repos pr list` object into a `PrBrief`. Azure's list gives no
/// size and no web address, which left every Azure card at "0 files +0 −0"
/// with a link to `/pullrequest/N`: the size is read from git, against the
/// refs already fetched (nothing is fetched for it, so it is as fresh as the
/// last fetch, and 0 when the refs are not here), the link from `origin`.
pub(super) async fn azure_brief(
    repo: &str,
    remote: &str,
    pr: &serde_json::Value,
) -> Option<PrBrief> {
    let number = pr["pullRequestId"].as_i64()?.to_string();
    let web = pr["repository"]["webUrl"].as_str().unwrap_or_default();
    let base = azure_branch(&pr["targetRefName"]);
    let head = azure_branch(&pr["sourceRefName"]);
    let (files, additions, deletions) = if base.is_empty() || head.is_empty() {
        (0, 0, 0)
    } else {
        let range = format!("origin/{base}...origin/{head}");
        git::run(repo, "git", &["diff", "--shortstat", &range])
            .await
            .map(|out| shortstat(&out))
            .unwrap_or_default()
    };
    let commits = if base.is_empty() || head.is_empty() {
        0
    } else {
        git::run(
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
        .unwrap_or(0)
    };
    Some(PrBrief {
        title: pr["title"].as_str().unwrap_or_default().to_string(),
        url: forge::azure_pr_link(remote, web, &number),
        number,
        // Azure policy evaluation is a different shape; saying we did
        // not look beats reporting a green build nobody checked.
        checks: Checks::Unknown,
        files,
        additions,
        deletions,
        commits,
        base,
        head,
        failed: vec![],
    })
}

/// `(files, insertions, deletions)` out of `git diff --shortstat`
/// (` 3 files changed, 45 insertions(+), 3 deletions(-)`; either count is
/// left out when it is zero).
pub(crate) fn shortstat(out: &str) -> (usize, usize, usize) {
    let (mut files, mut add, mut del) = (0, 0, 0);
    for part in out.trim().split(',') {
        let mut words = part.split_whitespace();
        let (Some(n), Some(what)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(n) = n.parse::<usize>() else { continue };
        if what.starts_with("file") {
            files = n;
        } else if what.starts_with("insertion") {
            add = n;
        } else if what.starts_with("deletion") {
            del = n;
        }
    }
    (files, add, del)
}

/// Once GitHub says the hourly GraphQL budget is spent, every further query
/// fails the same way and keeps the budget at zero — for this app and every
/// other `gh` use on the account. So the first such answer stops the checks
/// for a while, and each repository gets that answer back without asking.
const HOLD_OFF: std::time::Duration = std::time::Duration::from_secs(10 * 60);

fn held() -> &'static std::sync::Mutex<Option<(std::time::Instant, String)>> {
    static HELD: std::sync::OnceLock<std::sync::Mutex<Option<(std::time::Instant, String)>>> =
        std::sync::OnceLock::new();
    HELD.get_or_init(|| std::sync::Mutex::new(None))
}

pub(super) fn is_rate_limit(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    err.contains("rate limit") || err.contains("secondary rate")
}

fn hold_off(err: &str) {
    if let Ok(mut held) = held().lock() {
        *held = Some((std::time::Instant::now() + HOLD_OFF, err.to_string()));
    }
}

fn rate_limited() -> Option<String> {
    let mut held = held().lock().ok()?;
    match held.as_ref() {
        Some((until, err)) if std::time::Instant::now() < *until => Some(format!(
            "{err} (paused GitHub checks for a few minutes to let the budget recover)"
        )),
        Some(_) => {
            *held = None;
            None
        }
        None => None,
    }
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
            if let Some(err) = rate_limited() {
                return Err(err);
            }
            let out = git::run(
                repo,
                "gh",
                &["pr", "list", "--state", "open", "--json", PR_FIELDS],
            )
            .await
            .inspect_err(|e| {
                if is_rate_limit(e) {
                    hold_off(e);
                }
            })?;
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
            let remote = git::run(repo, "git", &["remote", "get-url", "origin"])
                .await
                .unwrap_or_default();
            let mut briefs = vec![];
            for pr in list.as_array().into_iter().flatten() {
                if let Some(brief) = azure_brief(repo, &remote, pr).await {
                    briefs.push(brief);
                }
            }
            Ok(briefs)
        }
        _ => Ok(vec![]),
    }
}

/// How many open issues are read per repository. The badge says "50+" past
/// it rather than paging for an exact count nobody acts on.
pub const ISSUES_READ: usize = 50;

/// Open issues on a GitHub repository, newest first. Number, title and URL
/// only: about one point of the hourly GraphQL budget per repository. Any
/// failure — rate limit, signed out, issues turned off — is an empty list.
pub(super) async fn list_open_issues(repo: &str, forge: &Forge) -> Vec<IssueBrief> {
    if *forge != Forge::GitHub || rate_limited().is_some() {
        return vec![];
    }
    let limit = ISSUES_READ.to_string();
    let Ok(out) = git::run(
        repo,
        "gh",
        &[
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title,url",
        ],
    )
    .await
    .inspect_err(|e| {
        if is_rate_limit(e) {
            hold_off(e);
        }
    }) else {
        return vec![];
    };
    issues_from_json(&out)
}

pub(super) fn issues_from_json(out: &str) -> Vec<IssueBrief> {
    serde_json::from_str::<serde_json::Value>(out)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|issue| {
            Some(IssueBrief {
                number: issue["number"].as_i64()?.to_string(),
                title: issue["title"].as_str().unwrap_or_default().to_string(),
                url: issue["url"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
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
            let remote = git::run(repo, "git", &["remote", "get-url", "origin"])
                .await
                .unwrap_or_default();
            azure_brief(repo, &remote, pr).await
        }
        _ => None,
    }
}

/// A failing check outranks a pending one: red is worth surfacing immediately,
/// and a run still going could turn red anyway.
pub(crate) fn rollup(value: &serde_json::Value) -> Checks {
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
pub(crate) fn failed_checks(value: &serde_json::Value) -> Vec<FailedCheck> {
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
