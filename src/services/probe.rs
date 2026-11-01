//! What each repository is waiting for, worked out before you click anything.
//!
//! Opening a workspace used to tell you only how many files were dirty. That is
//! the least interesting half: a repository with a green pull request wants a
//! merge, one with a red one wants a look, and one with neither wants a commit.
//! The probe answers that per repository so the list can rank itself, and so
//! the app can open on the thing that actually needs a person.
//!
//! Every query here is read-only.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::forge::{self, Forge};
use super::git;
use super::release::{self, ReleaseState};
use super::store;
mod prs;
use prs::*;
mod status;
pub use status::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Checks {
    Passing,
    Failing,
    Pending,
    /// No checks configured, or a forge we do not read them from.
    Unknown,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PrBrief {
    pub number: String,
    pub title: String,
    pub url: String,
    pub checks: Checks,
    /// Size, which is what actually tells two pull requests apart when their
    /// titles and check states are identical.
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
    pub commits: usize,
    /// The branch it merges into, and the branch it brings — what its diff is
    /// worked out between, locally, without asking the forge for it.
    pub base: String,
    pub head: String,
    /// The checks behind a `Checks::Failing`, each with the page its log is
    /// on — so "checks failing" can say which one and take you to why.
    /// Empty for any other state, and on Azure, where checks are not read.
    pub failed: Vec<FailedCheck>,
}

/// One check that did not pass.
#[derive(Clone, PartialEq, Debug)]
pub struct FailedCheck {
    pub name: String,
    /// Its run on the forge, where the log is. Empty when GitHub gave none.
    pub url: String,
}

impl PrBrief {
    /// "115 files  +9297  −4567"
    pub fn size(&self) -> String {
        format!(
            "{} file{}  +{}  −{}",
            self.files,
            if self.files == 1 { "" } else { "s" },
            self.additions,
            self.deletions
        )
    }
}

/// Whether starting the selected flow would actually do anything, and what the
/// button should say.
///
/// The rules are per flow because the precondition is: the commit flow needs a
/// dirty tree, the review flow needs an open pull request. A flow the app does
/// not ship — anything built in Setup — is always offered, because only its
/// author knows when it makes sense to run. A flow that does not validate is
/// refused whichever it is.
/// What a flow is for. A flow declares which of these it answers, and the app
/// opens on it when a repository is in that state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    Uncommitted,
    UnpushedBranch,
    OpenPullRequest,
    Release,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Affordance {
    pub enabled: bool,
    pub label: String,
    /// Tooltip: why it is disabled, when it is.
    pub reason: String,
}

impl Affordance {
    fn go(label: impl Into<String>) -> Self {
        Self {
            enabled: true,
            label: label.into(),
            reason: String::new(),
        }
    }

    fn stop(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            enabled: false,
            label: label.into(),
            reason: reason.into(),
        }
    }
}

/// What a flow does, as far as the Start button is concerned. Decided from
/// the flow's contents rather than its id, so a flow copied in Setup behaves
/// like the shipped one it was copied from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlowKind {
    Commit,
    Review,
    /// A flow from Setup: its author knows the precondition, not this code.
    Other,
}

/// GitHub reports `OPEN`, `MERGED` or `CLOSED`; anything else is not
/// reviewable. A missing field is treated as open so an older `gh` that does
/// not report it still works.
pub fn is_open(value: &serde_json::Value) -> bool {
    match value.get("state").and_then(|s| s.as_str()) {
        None => true,
        Some(state) => state.eq_ignore_ascii_case("OPEN"),
    }
}

/// How `base_branch` reports a base that came from the per-repo override
/// rather than auto-detection. Preflight keys an extra check off this, so it
/// is a constant rather than the same string spelled out in two modules.
pub const OVERRIDDEN: &str = "set for this repository";

/// The base branch this repository's pull requests target, and how that was
/// decided.
///
/// The override wins over auto-detection, and every caller resolves it the
/// same way. They did not used to: preflight and the Branches panel read the
/// override while `probe` went straight to detection, so a repository whose
/// pull requests target `develop` had its unmerged count measured against
/// `origin/HEAD` — `main`, 1536 commits back — and a clean tree offered
/// "Push 1536 commits & open PR".
///
/// Takes the override rather than loading it, because the workspace holds it
/// in a signal the Base button writes to; a fresh read from disk there would
/// lag a click behind.
pub async fn base_branch(repo: &str, override_base: Option<String>) -> (String, String) {
    match override_base {
        Some(base) => (base, OVERRIDDEN.to_string()),
        None => git::default_remote_branch(repo).await,
    }
}

/// `base_branch` for callers with no signal to read — the override comes from
/// disk.
pub async fn stored_base_branch(repo: &str) -> (String, String) {
    let override_base = store::load_repo_bases().get(repo).map(str::to_string);
    base_branch(repo, override_base).await
}

/// How long a repository's check is good enough to show again without asking
/// git and GitHub a second time — long enough to cover the home screen
/// checking every repository at launch and the folder opened from it straight
/// after, short enough that nothing shown is stale in a way that matters.
pub const RECENT: Duration = Duration::from_secs(120);

/// The last check of each repository, from any window: they share a process.
fn recent() -> &'static Mutex<HashMap<String, (Instant, RepoStatus)>> {
    static RECENT_CHECKS: OnceLock<Mutex<HashMap<String, (Instant, RepoStatus)>>> = OnceLock::new();
    RECENT_CHECKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn still_recent(checked: Instant, now: Instant) -> bool {
    now.saturating_duration_since(checked) < RECENT
}

/// Checks a repository now, and keeps the answer for `probe_recent`. What a
/// Refresh, a finished run or a fix calls: those are asking for news.
pub async fn probe(repo: &str) -> RepoStatus {
    let status = probe_now(repo).await;
    if let Ok(mut map) = recent().lock() {
        map.insert(repo.to_string(), (Instant::now(), status.clone()));
    }
    status
}

/// A check from the last two minutes if there is one, else a fresh one. For
/// opening a list of repositories — the home screen at launch, a folder
/// opened from it — so the same few dozen repositories are not asked about
/// twice within seconds, at a cost to the hourly GitHub budget each time.
pub async fn probe_recent(repo: &str) -> RepoStatus {
    let kept = recent().lock().ok().and_then(|map| {
        map.get(repo)
            .filter(|(at, _)| still_recent(*at, Instant::now()))
            .map(|(_, status)| status.clone())
    });
    match kept {
        Some(status) => status,
        None => probe(repo).await,
    }
}

/// Everything the sidebar needs to know about one repository.
///
/// Run in two waves rather than as ten sequential awaits. Each of these
/// shells out to `git` or `gh`, so awaiting them one after another spent the
/// whole latency of every subprocess in series — and it was what made the
/// caller fan out across repositories hard enough to trip the forge's rate
/// limiter. Only two things are genuine dependencies: the forge has to be
/// known before the pull requests can be listed, and the base branch before
/// the release and unmerged-commit counts. Everything else goes at once.
async fn probe_now(repo: &str) -> RepoStatus {
    let (branch, changes, url, (ahead, behind), (base, _), in_progress) = tokio::join!(
        async { git::current_branch(repo).await.unwrap_or_default() },
        async { git::status(repo).await.map(|c| c.len()).unwrap_or(0) },
        git::remote_url(repo),
        ahead_behind(repo),
        stored_base_branch(repo),
        git::in_progress(repo),
    );
    let forge = url.map(|url| forge::detect(&url)).unwrap_or(Forge::None);

    let (pr, listed, release, unmerged) = tokio::join!(
        open_pr(repo, &forge),
        list_open_prs(repo, &forge),
        release::status(repo, &base),
        unmerged_commits(repo, &base),
    );
    let (prs, prs_error) = match listed {
        Ok(list) => (list, None),
        Err(e) => (vec![], Some(e)),
    };

    RepoStatus {
        branch,
        changes,
        forge,
        pr,
        prs,
        prs_error,
        ahead,
        behind,
        unmerged,
        release,
        in_progress,
    }
}

/// Commits the checked-out branch has that the base branch does not.
///
/// Prefers the remote-tracking base, since that is what a pull request would
/// actually target; falls back to the local one for a repository that has not
/// been fetched.
async fn unmerged_commits(repo: &str, base: &str) -> usize {
    for reference in [format!("origin/{base}"), base.to_string()] {
        let range = format!("{reference}..HEAD");
        if let Ok(out) = git::run(repo, "git", &["rev-list", "--count", &range]).await {
            if let Ok(n) = out.trim().parse::<usize>() {
                return n;
            }
        }
    }
    0
}

/// How far the checked-out branch and its upstream have diverged. `(0, 0)`
/// when there is no upstream to compare against — a branch that has never
/// been pushed reports no changes here, not a false "3 behind".
async fn ahead_behind(repo: &str) -> (usize, usize) {
    let Ok(out) = git::run(
        repo,
        "git",
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
    )
    .await
    else {
        return (0, 0);
    };
    parse_ahead_behind(&out)
}

/// `git rev-list --left-right --count upstream...HEAD` prints
/// "<behind>\t<ahead>" — split out so the parsing has no subprocess to mock.
fn parse_ahead_behind(out: &str) -> (usize, usize) {
    let mut parts = out.split_whitespace();
    let behind = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let ahead = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (ahead, behind)
}

/// `refs/heads/feature/x` as Azure DevOps names a branch, as `feature/x`.
fn azure_branch(value: &serde_json::Value) -> String {
    let full = value.as_str().unwrap_or_default();
    full.strip_prefix("refs/heads/").unwrap_or(full).to_string()
}

/// What every repository check asks GitHub for, per pull request.
///
/// Not `commits`. `gh` answers it with up to 100 commits per pull request and
/// up to 100 authors on each, and GitHub prices a query by what it could
/// return rather than what it does: about 30 of GraphQL's 5,000 points an
/// hour for every repository checked, open pull requests or not, against
/// about 2 without it. Checking a few dozen repositories at launch spent the
/// whole hour's budget in a handful of launches, and every `gh` call after
/// that failed with "API rate limit already exceeded" — for this app and
/// anything else on the account. The commit count it bought was one line on
/// a pull request card.
const PR_FIELDS: &str =
    "number,title,url,state,statusCheckRollup,changedFiles,additions,deletions,baseRefName,headRefName";

/// One rollup entry is still going. Check runs say so in `status`; the older
/// commit statuses put `PENDING` in `state`, which `has_failed` reads.
fn is_pending(check: &serde_json::Value) -> bool {
    matches!(
        check["status"].as_str().unwrap_or(""),
        "IN_PROGRESS" | "QUEUED" | "PENDING"
    )
}

fn has_failed(check: &serde_json::Value) -> bool {
    let conclusion = check["conclusion"]
        .as_str()
        .or_else(|| check["state"].as_str())
        .unwrap_or("");
    !is_pending(check) && !matches!(conclusion, "SUCCESS" | "NEUTRAL" | "SKIPPED")
}

#[cfg(test)]
mod failed_check_tests {
    use super::*;

    #[test]
    fn each_failing_check_is_named_once_with_its_log() {
        let rollup = serde_json::json!([
            { "name": "fmt", "status": "COMPLETED", "conclusion": "FAILURE",
              "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2" },
            { "name": "changelog", "status": "COMPLETED", "conclusion": "SUCCESS",
              "detailsUrl": "https://x/3" },
            { "name": "test", "status": "IN_PROGRESS", "conclusion": "",
              "detailsUrl": "https://x/4" },
            // A re-run of the same check: one entry, not two.
            { "name": "fmt", "status": "COMPLETED", "conclusion": "FAILURE",
              "detailsUrl": "https://x/5" },
            // The older commit-status shape.
            { "context": "ci/legacy", "state": "ERROR", "targetUrl": "https://x/6" },
        ]);
        assert_eq!(
            failed_checks(&rollup),
            vec![
                FailedCheck {
                    name: "fmt".into(),
                    url: "https://github.com/o/r/actions/runs/1/job/2".into(),
                },
                FailedCheck {
                    name: "ci/legacy".into(),
                    url: "https://x/6".into(),
                },
            ]
        );
        // The same reading as the overall verdict: red, even with one running.
        assert_eq!(super::rollup(&rollup), Checks::Failing);
    }

    #[test]
    fn nothing_failing_lists_nothing() {
        let green = serde_json::json!([
            { "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS" },
            { "name": "lint", "status": "COMPLETED", "conclusion": "SKIPPED" },
        ]);
        assert!(failed_checks(&green).is_empty());
        assert!(failed_checks(&serde_json::Value::Null).is_empty());
    }
}

#[cfg(test)]
mod default_pr_tests {
    use super::*;
    use crate::services::forge::Forge;
    use crate::services::release::ReleaseState;

    fn brief(number: &str) -> PrBrief {
        PrBrief {
            number: number.into(),
            title: "t".into(),
            url: "u".into(),
            checks: Checks::Passing,
            files: 1,
            additions: 1,
            deletions: 0,
            commits: 1,
            base: String::new(),
            head: String::new(),
            failed: vec![],
        }
    }

    fn status(pr: Option<&str>, prs: &[&str]) -> RepoStatus {
        RepoStatus {
            branch: "feat/x".into(),
            changes: 0,
            forge: Forge::GitHub,
            pr: pr.map(brief),
            prs: prs.iter().map(|n| brief(n)).collect(),
            prs_error: None,
            ahead: 0,
            behind: 0,
            unmerged: 0,
            release: ReleaseState::default(),
            in_progress: None,
        }
    }

    #[test]
    fn the_only_open_pull_request_needs_no_choosing() {
        // The reported case: open Review -> Merge, one pull request, and
        // nothing selected. The list had exactly one entry and clicking it
        // said nothing that was not already known.
        assert_eq!(status(None, &["7"]).default_pr(), "7");
    }

    #[test]
    fn the_checked_out_branchs_own_wins_over_the_rest() {
        assert_eq!(status(Some("7"), &["3", "7", "9"]).default_pr(), "7");
    }

    #[test]
    fn a_pull_request_the_checked_out_branch_does_not_own_is_still_work() {
        // This is the one that killed every trusted run. The commit flow ends
        // in `sync`, which is `git checkout <base>` — so seconds after opening
        // a pull request, HEAD is on `develop`, `pr` is None, and a repository
        // with two open pull requests reported `Nothing`. The chain asked what
        // to do next, was told nothing, and stopped. Every time, on every
        // repository, whatever else had been fixed.
        let synced = status(None, &["3", "9"]);
        assert_eq!(synced.wants(), Wants::Merge, "not Nothing");
        assert_eq!(synced.default_pr(), "3");
    }

    #[test]
    fn someone_elses_pull_request_does_not_hide_your_uncommitted_work() {
        // ais_tom_platform: on develop with 14 changed files, and two
        // colleagues' pull requests open against it. The repository read as
        // ready to merge and opened on Review -> Merge; the commit was never
        // offered.
        let mut shared = status(None, &["3", "9"]);
        shared.changes = 14;
        assert_eq!(shared.wants(), Wants::Commit);
        assert_eq!(shared.default_pr(), "3", "still there to review");

        // The checked-out branch's own pull request is a different matter:
        // finishing it still beats starting something new.
        let mut own = status(Some("7"), &["7"]);
        own.changes = 2;
        assert_eq!(own.wants(), Wants::Merge);
    }

    #[test]
    fn the_most_actionable_open_pull_request_is_the_one_chosen() {
        let mut mixed = status(None, &["3", "9"]);
        mixed.prs[0].checks = Checks::Pending;
        mixed.prs[1].checks = Checks::Passing;
        assert_eq!(mixed.default_pr(), "9", "decidable beats still-running");

        mixed.prs[0].checks = Checks::Failing;
        mixed.prs[1].checks = Checks::Pending;
        assert_eq!(mixed.default_pr(), "3", "red beats nothing-known-yet");
    }

    #[test]
    fn a_merge_finished_locally_but_not_pushed_asks_for_the_push() {
        // polishcast: Review -> Merge stopped on a conflict with main, the
        // merge was finished in Preflight — a commit, no push — and #50 kept
        // saying it conflicts. The repository read as ready to merge, about a
        // pull request that did not have the fix.
        let mut finished = status(Some("50"), &["50"]);
        finished.ahead = 2;
        assert_eq!(finished.wants(), Wants::Push);
        assert_eq!(finished.wants().need(), Some(Need::UnpushedBranch));
        assert_eq!(
            finished
                .count_for(&["unpushed_branch".into()])
                .map(|(tab, _)| tab),
            Some("2 commits".into())
        );

        finished.prs[0].checks = Checks::Failing;
        finished.pr.as_mut().unwrap().checks = Checks::Failing;
        assert_eq!(
            finished.wants(),
            Wants::Push,
            "red checks are the old branch's"
        );

        let pushed = status(Some("50"), &["50"]);
        assert_eq!(pushed.wants(), Wants::Merge);
    }

    #[test]
    fn unpushed_commits_on_a_branch_with_a_pull_request_are_offered_as_a_push() {
        let mut finished = status(Some("50"), &["50"]);
        finished.ahead = 2;
        finished.unmerged = 3;
        let start = affordance(FlowKind::Commit, Some(&finished), false, false, "", &[]);
        assert_eq!(start, Affordance::go("Push 2 commits to #50"));

        // Files to commit come first: the flow commits them, then pushes all.
        finished.changes = 1;
        let start = affordance(FlowKind::Commit, Some(&finished), false, false, "", &[]);
        assert_eq!(start.label, "Commit 1 file");
    }

    #[test]
    fn a_repository_with_no_open_pull_requests_is_unaffected() {
        let clean = status(None, &[]);
        assert_eq!(clean.wants(), Wants::Nothing);
        assert_eq!(clean.default_pr(), "");
    }

    #[test]
    fn no_pull_requests_selects_nothing() {
        assert_eq!(status(None, &[]).default_pr(), "");
    }
}

#[cfg(test)]
mod tests;
