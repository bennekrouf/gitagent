//! What each repository is waiting for, worked out before you click anything.
//!
//! Opening a workspace used to tell you only how many files were dirty. That is
//! the least interesting half: a repository with a green pull request wants a
//! merge, one with a red one wants a look, and one with neither wants a commit.
//! The probe answers that per repository so the list can rank itself, and so
//! the app can open on the thing that actually needs a person.
//!
//! Every query here is read-only.

use super::forge::{self, Forge};
use super::git;
use super::release::{self, ReleaseState};
use super::store;

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

/// What the repository wants from you, most urgent first.
///
/// The ordering is the whole point: it decides which repository the app opens
/// on, so it is stated once, here, and tested.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Wants {
    /// A rebase, merge or cherry-pick stopped part-way. Nothing else in the
    /// repository can move until it is finished or abandoned, so it outranks
    /// everything — including a decision on a pull request.
    Resolve,
    /// A pull request whose checks are green — a decision is waiting.
    Merge,
    /// A pull request whose checks are red.
    Attention,
    /// Uncommitted work.
    Commit,
    /// Committed work on a topic branch with no pull request behind it.
    OpenPr,
    /// Merged work that has not been tagged.
    Release,
    /// A pull request whose checks are still running. Nothing to do yet.
    Wait,
    /// Clean tree, no pull request.
    Nothing,
}

impl Wants {
    /// Whether a person is actually expected to do something.
    pub fn needs_a_person(self) -> bool {
        matches!(
            self,
            Wants::Resolve
                | Wants::Merge
                | Wants::Attention
                | Wants::Commit
                | Wants::OpenPr
                | Wants::Release
        )
    }

    pub fn note(self) -> &'static str {
        match self {
            Wants::Resolve => "unfinished rebase",
            Wants::Merge => "ready to merge",
            Wants::Attention => "checks failing",
            Wants::Commit => "uncommitted",
            Wants::OpenPr => "needs a PR",
            Wants::Release => "release due",
            Wants::Wait => "checks running",
            Wants::Nothing => "clean",
        }
    }

    /// Whether this state is worth any words in a list of repositories.
    ///
    /// The two resting states get none. A column where every clean repository
    /// says CLEAN is a column you have to read to find the one that does not —
    /// which is exactly backwards. The dot still carries the state for anyone
    /// who wants it.
    pub fn is_worth_saying(self) -> bool {
        !matches!(self, Wants::Nothing | Wants::Wait)
    }

    /// A glyph for the states worth saying something about, so a row can be
    /// sorted by eye before it is read.
    pub fn icon(self) -> &'static str {
        match self {
            Wants::Resolve => "\u{26a0}",
            Wants::Merge => "\u{2713}",
            Wants::Attention => "\u{2715}",
            Wants::Commit => "\u{270e}",
            Wants::OpenPr => "\u{2197}",
            Wants::Release => "\u{2191}",
            Wants::Wait | Wants::Nothing => "",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            Wants::Resolve => "failed",
            Wants::Merge => "done",
            Wants::Attention => "failed",
            Wants::Commit => "awaiting",
            Wants::OpenPr => "awaiting",
            Wants::Release => "running",
            Wants::Wait => "running",
            Wants::Nothing => "skipped",
        }
    }

    /// Which flow to open on when this repository is chosen.
    /// The kind of work this state represents, which a flow declares it
    /// answers. Names a *need*, not a flow — so "Deploy VPS" can answer the
    /// release need without being called `release`.
    ///
    /// `None` for states nobody acts on.
    pub fn need(self) -> Option<Need> {
        match self {
            Wants::Commit => Some(Need::Uncommitted),
            Wants::OpenPr => Some(Need::UnpushedBranch),
            // No flow answers this: it is finished in preflight, which offers
            // continue and abort where the person is already looking.
            Wants::Resolve => None,
            Wants::Merge | Wants::Attention | Wants::Wait => Some(Need::OpenPullRequest),
            Wants::Release => Some(Need::Release),
            Wants::Nothing => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RepoStatus {
    pub branch: String,
    pub changes: usize,
    pub forge: Forge,
    /// The PR for the checked-out branch specifically — what the "Commit /
    /// Review" affordance and the top-of-column `PrCard` react to.
    pub pr: Option<PrBrief>,
    /// Every open PR on the repository, branch-independent — what lets the
    /// sidebar offer a choice of which one to review, not just the one HEAD
    /// happens to point at right now.
    pub prs: Vec<PrBrief>,
    /// Set when the call behind `prs` actually failed (rate limit, network,
    /// auth) rather than the repository genuinely having none — an empty
    /// `prs` alone cannot tell those two apart, and showing "clean" for a
    /// check that never happened is worse than showing nothing.
    pub prs_error: Option<String>,
    /// Commits on this branch not yet on its upstream, and vice versa. Both
    /// `0` whenever there's nothing to report — no upstream configured, or
    /// the branch is caught up — not just "unpushed work exists".
    pub ahead: usize,
    pub behind: usize,
    /// Commits on the checked-out branch that the base branch does not have.
    ///
    /// Distinct from `ahead`, which compares against the *upstream* — a branch
    /// that was never pushed has no upstream, so `ahead` is 0 while the work
    /// plainly exists. That gap is why a branch with a commit and no pull
    /// request used to report "nothing to do".
    pub unmerged: usize,
    /// Merged work the last tag does not reach.
    pub release: ReleaseState,
    /// A git operation that stopped part-way, if any.
    pub in_progress: Option<git::InProgress>,
}

impl RepoStatus {
    /// Uncommitted work outranks a pull request that is merely running its
    /// checks, but never outranks one that is ready for a decision — finishing
    /// something beats starting something.
    /// The most urgent thing this repository needs, across everything it could
    /// need — not just whatever the flow on screen happens to cover.
    ///
    /// Every candidate is collected and the ranking picks the winner, rather
    /// than the first arm of a match short-circuiting the rest. That
    /// distinction is the bug this replaced: an open pull request whose checks
    /// were still running reported "waiting", and a release sitting behind it
    /// was never even considered.
    pub fn wants(&self) -> Wants {
        // Nothing else is worth reporting while git is mid-operation: every
        // other answer would be advice you cannot act on.
        if self.in_progress.is_some() {
            return Wants::Resolve;
        }

        let from_pr = self.pr.as_ref().and_then(|pr| match pr.checks {
            Checks::Passing | Checks::Unknown => Some(Wants::Merge),
            Checks::Failing => Some(Wants::Attention),
            // Nothing to decide yet — but that is not the same as nothing to
            // do, so this contributes no candidate rather than winning.
            Checks::Pending => None,
        });

        [
            from_pr,
            (self.changes > 0).then_some(Wants::Commit),
            // Only without a pull request: with one, the commits are already
            // proposed and the decision is the pull request's.
            (self.pr.is_none() && self.unmerged > 0).then_some(Wants::OpenPr),
            self.release.due().then_some(Wants::Release),
            self.pr.is_some().then_some(Wants::Wait),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(Wants::Nothing)
    }

    /// The pull request to open a review on, when nothing else has said.
    ///
    /// `""` only when the answer is genuinely a question. One open pull
    /// request is not a question, and neither is a branch with its own — those
    /// are the two cases where making the person pick from a list of one, or
    /// from a list where only one is theirs, is just a click that says nothing.
    pub fn default_pr(&self) -> String {
        if let Some(pr) = &self.pr {
            // The checked-out branch's own. Whatever else is open, this is
            // the one being worked on.
            return pr.number.clone();
        }
        match self.prs.as_slice() {
            [only] => only.number.clone(),
            // Several, none of them this branch's: picking one for you would
            // be a guess, and the wrong guess merges somebody else's work.
            _ => String::new(),
        }
    }

    /// One line for the sidebar.
    pub fn summary(&self) -> String {
        match (&self.pr, self.changes) {
            (Some(pr), _) if pr.files > 0 => format!("#{} · {}f", pr.number, pr.files),
            (Some(pr), _) => format!("#{}", pr.number),
            (None, 0) if self.unmerged > 0 => format!("{} ahead", self.unmerged),
            (None, 0) => String::new(),
            (None, n) => n.to_string(),
        }
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

impl Need {
    pub const ALL: [Need; 4] = [
        Need::Uncommitted,
        Need::UnpushedBranch,
        Need::OpenPullRequest,
        Need::Release,
    ];

    /// Stable identifier for the flow file.
    pub fn key(self) -> &'static str {
        match self {
            Need::Uncommitted => "uncommitted",
            Need::UnpushedBranch => "unpushed_branch",
            Need::OpenPullRequest => "open_pull_request",
            Need::Release => "release",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Need::Uncommitted => "Uncommitted changes",
            Need::UnpushedBranch => "A branch with no pull request",
            Need::OpenPullRequest => "An open pull request",
            Need::Release => "Merged work not yet released",
        }
    }

    /// Used by the round-trip test that keeps `key` and `ALL` in step.
    #[cfg(test)]
    pub fn from_key(key: &str) -> Option<Need> {
        Need::ALL.into_iter().find(|n| n.key() == key)
    }
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

pub const COMMIT_FLOW: &str = "commit_and_pr";
pub const REVIEW_FLOW: &str = "review_and_merge";

pub fn affordance(
    flow_id: &str,
    status: Option<&RepoStatus>,
    probing: bool,
    already_ran: bool,
    selected_pr: &str,
    problems: &[String],
) -> Affordance {
    // A flow that does not validate is never handed to the executor, so this
    // outranks everything below it: whatever the repository has waiting, this
    // flow cannot be the thing that acts on it. Said here rather than at the
    // button so it is one rule with the rest of them, and tested with them.
    if let Some(first) = problems.first() {
        let reason = match problems.len() {
            1 => format!("{first} Fix it in Setup."),
            n => format!("{first} And {} more. Fix them in Setup.", n - 1),
        };
        return Affordance::stop("Flow is broken", reason);
    }

    let Some(status) = status else {
        return if probing {
            Affordance::stop("Checking…", "Still reading this repository")
        } else {
            // The probe failed or has not run; do not stand in the way.
            Affordance::go("Start run")
        };
    };

    let again = |label: String| {
        if already_ran {
            format!("{label} again")
        } else {
            label
        }
    };

    match flow_id {
        COMMIT_FLOW => match (status.changes, status.unmerged) {
            // Clean tree, but commits sitting on a branch with no pull
            // request: the flow can still push them and propose them.
            (0, 0) => Affordance::stop(
                "Nothing to commit",
                "The working tree is clean and the branch has nothing the base \
                 branch does not — there is nothing for this flow to do",
            ),
            (0, 1) => Affordance::go(again("Push 1 commit & open PR".into())),
            (0, n) => Affordance::go(again(format!("Push {n} commits & open PR"))),
            (1, _) => Affordance::go(again("Commit 1 file".into())),
            (n, _) => Affordance::go(again(format!("Commit {n} files"))),
        },

        // A PR picked from the sidebar's list always enables the run — it
        // does not have to be the one for the checked-out branch. Only fall
        // back to "does the checked-out branch have one" when nothing was
        // explicitly picked, which keeps the old single-PR behaviour intact.
        REVIEW_FLOW if !selected_pr.is_empty() => {
            Affordance::go(again(format!("Review #{selected_pr}")))
        }
        REVIEW_FLOW => match &status.pr {
            None => Affordance::stop(
                "No pull request",
                format!("No open pull request for `{}`", status.branch),
            ),
            Some(pr) => Affordance::go(again(format!("Review #{}", pr.number))),
        },
        // A flow from Setup: its author knows the precondition, not this code.
        _ => Affordance::go(again("Start run".into())),
    }
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

pub async fn probe(repo: &str) -> RepoStatus {
    let branch = git::current_branch(repo).await.unwrap_or_default();
    let changes = git::status(repo).await.map(|c| c.len()).unwrap_or(0);
    let forge = git::remote_url(repo)
        .await
        .map(|url| forge::detect(&url))
        .unwrap_or(Forge::None);
    let pr = open_pr(repo, &forge).await;
    let (prs, prs_error) = match list_open_prs(repo, &forge).await {
        Ok(list) => (list, None),
        Err(e) => (vec![], Some(e)),
    };
    let (ahead, behind) = ahead_behind(repo).await;
    let (base, _) = stored_base_branch(repo).await;
    let release = release::status(repo, &base).await;
    let unmerged = unmerged_commits(repo, &base).await;
    let in_progress = git::in_progress(repo).await;

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

/// One GitHub PR JSON object (from either `pr view` or `pr list`, same
/// field names either way) into a `PrBrief`.
fn github_pr_from_json(value: &serde_json::Value) -> Option<PrBrief> {
    Some(PrBrief {
        number: value["number"].as_i64()?.to_string(),
        title: value["title"].as_str().unwrap_or_default().to_string(),
        url: value["url"].as_str().unwrap_or_default().to_string(),
        checks: rollup(&value["statusCheckRollup"]),
        files: value["changedFiles"].as_u64().unwrap_or(0) as usize,
        additions: value["additions"].as_u64().unwrap_or(0) as usize,
        deletions: value["deletions"].as_u64().unwrap_or(0) as usize,
        commits: value["commits"].as_array().map(|a| a.len()).unwrap_or(0),
    })
}

const PR_FIELDS: &str =
    "number,title,url,state,statusCheckRollup,changedFiles,additions,deletions,commits";

/// Every open pull request on the repository — not scoped to the checked-out
/// branch, unlike `open_pr` below. What lets the sidebar offer a choice of
/// which PR to review instead of only ever "whichever one HEAD happens to
/// point at".
/// `Err` only for an actual failed call (rate limit, network, auth) — a
/// repository with genuinely zero open PRs is `Ok(vec![])`. Collapsing those
/// two into one silently showed "clean" for a check that never happened, no
/// different from a repo that really was clean.
async fn list_open_prs(repo: &str, forge: &Forge) -> Result<Vec<PrBrief>, String> {
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
            let out = git::run(
                repo,
                "az",
                &[
                    "repos", "pr", "list", "--status", "active", "--output", "json",
                ],
            )
            .await?;
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
                            })
                        })
                        .collect()
                })
                .unwrap_or_default())
        }
        _ => Ok(vec![]),
    }
}

async fn open_pr(repo: &str, forge: &Forge) -> Option<PrBrief> {
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
            let out = git::run(
                repo,
                "az",
                &[
                    "repos",
                    "pr",
                    "list",
                    "--source-branch",
                    &branch,
                    "--status",
                    "active",
                    "--output",
                    "json",
                ],
            )
            .await
            .ok()?;
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
            })
        }
        _ => None,
    }
}

/// A failing check outranks a pending one: red is worth surfacing immediately,
/// and a run still going could turn red anyway.
fn rollup(value: &serde_json::Value) -> Checks {
    let Some(checks) = value.as_array() else {
        return Checks::Unknown;
    };
    if checks.is_empty() {
        return Checks::Unknown;
    }

    let (mut failing, mut pending) = (false, false);
    for check in checks {
        let status = check["status"].as_str().unwrap_or("");
        let conclusion = check["conclusion"]
            .as_str()
            .or_else(|| check["state"].as_str())
            .unwrap_or("");

        if matches!(status, "IN_PROGRESS" | "QUEUED" | "PENDING") {
            pending = true;
        } else if !matches!(conclusion, "SUCCESS" | "NEUTRAL" | "SKIPPED") {
            failing = true;
        }
    }

    match (failing, pending) {
        (true, _) => Checks::Failing,
        (false, true) => Checks::Pending,
        _ => Checks::Passing,
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
    fn several_that_are_none_of_them_this_branchs_stays_a_question() {
        // Guessing here picks a pull request to merge on someone's behalf.
        assert_eq!(status(None, &["3", "9"]).default_pr(), "");
    }

    #[test]
    fn no_pull_requests_selects_nothing() {
        assert_eq!(status(None, &[]).default_pr(), "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(changes: usize, pr: Option<Checks>) -> RepoStatus {
        RepoStatus {
            branch: "master".into(),
            changes,
            forge: Forge::GitHub,
            unmerged: 0,
            in_progress: None,
            release: ReleaseState::default(),
            pr: pr.map(|checks| PrBrief {
                number: "2".into(),
                title: "t".into(),
                url: "u".into(),
                checks,
                files: 3,
                additions: 10,
                deletions: 4,
                commits: 1,
            }),
            prs: vec![],
            prs_error: None,
            ahead: 0,
            behind: 0,
        }
    }

    #[test]
    fn a_green_pull_request_is_the_most_urgent_thing() {
        assert_eq!(status(0, Some(Checks::Passing)).wants(), Wants::Merge);
    }

    #[test]
    fn finishing_beats_starting() {
        // Dirty tree *and* a green PR: the decision comes first.
        assert_eq!(status(5, Some(Checks::Passing)).wants(), Wants::Merge);
    }

    #[test]
    fn a_red_pull_request_asks_for_attention() {
        assert_eq!(status(0, Some(Checks::Failing)).wants(), Wants::Attention);
    }

    #[test]
    fn while_ci_runs_there_is_still_uncommitted_work_to_do() {
        assert_eq!(status(3, Some(Checks::Pending)).wants(), Wants::Commit);
        assert_eq!(status(0, Some(Checks::Pending)).wants(), Wants::Wait);
    }

    #[test]
    fn no_pull_request_and_a_dirty_tree_wants_a_commit() {
        assert_eq!(status(1, None).wants(), Wants::Commit);
    }

    #[test]
    fn a_clean_repository_wants_nothing() {
        assert_eq!(status(0, None).wants(), Wants::Nothing);
        assert!(!Wants::Nothing.needs_a_person());
        assert!(!Wants::Wait.needs_a_person());
    }

    #[test]
    fn an_unfinished_rebase_outranks_everything_else() {
        // Mid-rebase, every other answer is advice you cannot act on.
        let mut s = status(4, Some(Checks::Passing));
        s.unmerged = 2;
        s.in_progress = Some(git::InProgress::Rebase);
        assert_eq!(s.wants(), Wants::Resolve);
        assert!(s.wants().needs_a_person());
    }

    #[test]
    fn no_flow_claims_the_unfinished_state() {
        // It is settled in preflight, not by picking a flow.
        assert_eq!(Wants::Resolve.need(), None);
    }

    #[test]
    fn a_release_is_not_hidden_behind_a_pull_request_still_building() {
        // The reported bug: "Clean" (or "checks running") while a release was
        // plainly due. A pending PR is not a decision, so it must not mask one.
        let mut s = status(0, Some(Checks::Pending));
        s.release = ReleaseState {
            last_tag: Some("v0.3.17".into()),
            commits: 3,
            prs: vec!["#6".into()],
            releases: true,
        };
        assert_eq!(s.wants(), Wants::Release);
    }

    #[test]
    fn a_pending_pull_request_with_nothing_else_is_still_a_wait() {
        let s = status(0, Some(Checks::Pending));
        assert_eq!(s.wants(), Wants::Wait);
    }

    #[test]
    fn a_decision_on_a_pull_request_still_outranks_a_release() {
        let mut s = status(0, Some(Checks::Passing));
        s.release = ReleaseState {
            last_tag: Some("v1".into()),
            commits: 9,
            prs: vec![],
            releases: true,
        };
        assert_eq!(s.wants(), Wants::Merge);
    }

    #[test]
    fn a_red_check_outranks_everything_behind_it() {
        let mut s = status(4, Some(Checks::Failing));
        s.release = ReleaseState {
            last_tag: Some("v1".into()),
            commits: 2,
            prs: vec![],
            releases: true,
        };
        assert_eq!(s.wants(), Wants::Attention);
    }

    #[test]
    fn a_branch_with_commits_and_no_pull_request_asks_for_one() {
        // ais-tracing: on feat/azure-error-handling, 1 ahead of master, clean
        // tree, no PR. This used to report "nothing to do" from every angle.
        let mut s = status(0, None);
        s.unmerged = 1;
        assert_eq!(s.wants(), Wants::OpenPr);
        assert!(s.wants().needs_a_person());
        assert_eq!(s.summary(), "1 ahead");
    }

    #[test]
    fn that_branch_can_actually_be_acted_on() {
        let mut s = status(0, None);
        s.unmerged = 1;
        let a = affordance(COMMIT_FLOW, Some(&s), false, false, "", &[]);
        assert!(a.enabled, "the button must not be dead");
        assert_eq!(a.label, "Push 1 commit & open PR");
    }

    #[test]
    fn uncommitted_work_is_still_the_first_thing_offered() {
        let mut s = status(3, None);
        s.unmerged = 1;
        assert_eq!(s.wants(), Wants::Commit);
        assert_eq!(
            affordance(COMMIT_FLOW, Some(&s), false, false, "", &[]).label,
            "Commit 3 files"
        );
    }

    #[test]
    fn a_branch_already_on_the_base_is_still_nothing_to_do() {
        let s = status(0, None);
        assert_eq!(s.wants(), Wants::Nothing);
        assert!(!affordance(COMMIT_FLOW, Some(&s), false, false, "", &[]).enabled);
    }

    #[test]
    fn an_open_pull_request_outranks_an_unmerged_branch() {
        let mut s = status(0, Some(Checks::Passing));
        s.unmerged = 2;
        assert_eq!(s.wants(), Wants::Merge);
    }

    #[test]
    fn the_ranking_puts_actionable_repositories_first() {
        let mut all = vec![
            Wants::Resolve,
            Wants::Nothing,
            Wants::Wait,
            Wants::Release,
            Wants::OpenPr,
            Wants::Commit,
            Wants::Merge,
            Wants::Attention,
        ];
        all.sort();
        assert_eq!(
            all,
            vec![
                Wants::Resolve,
                Wants::Merge,
                Wants::Attention,
                Wants::Commit,
                Wants::OpenPr,
                Wants::Release,
                Wants::Wait,
                Wants::Nothing
            ]
        );
    }

    #[test]
    fn a_github_pr_json_object_carries_its_size_and_checks() {
        let value = json!({
            "number": 7,
            "title": "Add diff view",
            "url": "https://github.com/o/r/pull/7",
            "changedFiles": 5,
            "additions": 120,
            "deletions": 30,
            "commits": [{}, {}],
            "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SUCCESS"}],
        });
        let pr = github_pr_from_json(&value).unwrap();
        assert_eq!(pr.number, "7");
        assert_eq!(pr.title, "Add diff view");
        assert_eq!(pr.files, 5);
        assert_eq!(pr.additions, 120);
        assert_eq!(pr.deletions, 30);
        assert_eq!(pr.commits, 2);
        assert_eq!(pr.checks, Checks::Passing);
    }

    #[test]
    fn a_pr_json_object_missing_a_number_is_rejected_rather_than_faked() {
        assert!(github_pr_from_json(&json!({"title": "no number"})).is_none());
    }

    #[test]
    fn ahead_and_behind_are_parsed_in_the_order_git_prints_them() {
        // rev-list prints "<behind>\t<ahead>" for `upstream...HEAD` — behind
        // first, which is easy to swap by accident.
        assert_eq!(parse_ahead_behind("2\t3"), (3, 2));
    }

    #[test]
    fn a_branch_caught_up_with_its_upstream_reports_nothing() {
        assert_eq!(parse_ahead_behind("0\t0\n"), (0, 0));
    }

    #[test]
    fn unparsable_output_defaults_to_nothing_rather_than_panicking() {
        assert_eq!(parse_ahead_behind(""), (0, 0));
        assert_eq!(parse_ahead_behind("garbage"), (0, 0));
    }

    #[test]
    fn every_state_worth_acting_on_names_the_need_behind_it() {
        assert_eq!(Wants::Merge.need(), Some(Need::OpenPullRequest));
        assert_eq!(Wants::Attention.need(), Some(Need::OpenPullRequest));
        assert_eq!(Wants::Wait.need(), Some(Need::OpenPullRequest));
        assert_eq!(Wants::Commit.need(), Some(Need::Uncommitted));
        assert_eq!(Wants::OpenPr.need(), Some(Need::UnpushedBranch));
        assert_eq!(Wants::Release.need(), Some(Need::Release));
        assert_eq!(Wants::Nothing.need(), None);
    }

    #[test]
    fn need_keys_survive_a_round_trip_through_the_flow_file() {
        for need in Need::ALL {
            assert_eq!(Need::from_key(need.key()), Some(need));
        }
        assert_eq!(Need::from_key("deploy_vps"), None);
    }

    #[test]
    fn committing_is_offered_only_when_there_is_something_to_commit() {
        let clean = status(0, None);
        assert!(!affordance(COMMIT_FLOW, Some(&clean), false, false, "", &[]).enabled);

        let dirty = status(3, None);
        let a = affordance(COMMIT_FLOW, Some(&dirty), false, false, "", &[]);
        assert!(a.enabled);
        assert_eq!(a.label, "Commit 3 files");
    }

    #[test]
    fn the_button_counts_one_file_in_the_singular() {
        let one = status(1, None);
        assert_eq!(
            affordance(COMMIT_FLOW, Some(&one), false, false, "", &[]).label,
            "Commit 1 file"
        );
    }

    #[test]
    fn reviewing_is_offered_only_when_a_pull_request_exists() {
        let none = status(0, None);
        let a = affordance(REVIEW_FLOW, Some(&none), false, false, "", &[]);
        assert!(!a.enabled);
        assert!(a.reason.contains("master"), "says which branch");

        let open = status(0, Some(Checks::Passing));
        let b = affordance(REVIEW_FLOW, Some(&open), false, false, "", &[]);
        assert!(b.enabled);
        assert_eq!(b.label, "Review #2");
    }

    #[test]
    fn picking_a_pr_from_the_list_enables_review_even_off_its_branch() {
        // The checked-out branch has nothing open (`none`), but a PR was
        // explicitly picked from the sidebar's list — Start must not stay
        // disabled just because HEAD points somewhere else.
        let none = status(0, None);
        let a = affordance(REVIEW_FLOW, Some(&none), false, false, "9", &[]);
        assert!(a.enabled);
        assert_eq!(a.label, "Review #9");
    }

    #[test]
    fn the_same_repository_can_offer_one_flow_and_refuse_the_other() {
        // Committed and pushed: nothing left to commit, a PR now to review.
        let after = status(0, Some(Checks::Passing));
        assert!(!affordance(COMMIT_FLOW, Some(&after), false, false, "", &[]).enabled);
        assert!(affordance(REVIEW_FLOW, Some(&after), false, false, "", &[]).enabled);

        // And the other way round, before any of that happened.
        let before = status(4, None);
        assert!(affordance(COMMIT_FLOW, Some(&before), false, false, "", &[]).enabled);
        assert!(!affordance(REVIEW_FLOW, Some(&before), false, false, "", &[]).enabled);
    }

    #[test]
    fn a_broken_flow_is_refused_however_much_work_is_waiting() {
        // The tab is now shown rather than hidden, so the button is what has
        // to say no — and it must say no even for a repository that is
        // otherwise perfectly ready to run this flow.
        let dirty = status(3, None);
        let problems = vec!["`push` depends on `commit`, which is not in this flow.".to_string()];
        let a = affordance(COMMIT_FLOW, Some(&dirty), false, false, "", &problems);
        assert!(!a.enabled);
        assert_eq!(a.label, "Flow is broken");
        assert!(
            a.reason.contains("`push` depends on `commit`"),
            "says what is wrong"
        );
        assert!(a.reason.contains("Setup"), "says where to fix it");
    }

    #[test]
    fn several_problems_name_the_first_and_count_the_rest() {
        let problems = vec!["One is wrong.".to_string(), "Two is wrong.".to_string()];
        let a = affordance(
            COMMIT_FLOW,
            Some(&status(3, None)),
            false,
            false,
            "",
            &problems,
        );
        assert_eq!(a.reason, "One is wrong. And 1 more. Fix them in Setup.");
    }

    #[test]
    fn a_broken_flow_outranks_even_a_pull_request_waiting_to_be_reviewed() {
        let open = status(0, Some(Checks::Passing));
        let problems = vec!["This flow has no steps.".to_string()];
        assert!(!affordance(REVIEW_FLOW, Some(&open), false, false, "7", &problems).enabled);
        // And without the problems it is offered as before.
        assert!(affordance(REVIEW_FLOW, Some(&open), false, false, "7", &[]).enabled);
    }

    #[test]
    fn a_second_run_says_again() {
        let dirty = status(2, None);
        assert_eq!(
            affordance(COMMIT_FLOW, Some(&dirty), false, true, "", &[]).label,
            "Commit 2 files again"
        );
    }

    #[test]
    fn a_flow_built_in_setup_is_never_second_guessed() {
        let clean = status(0, None);
        let a = affordance("release", Some(&clean), false, false, "", &[]);
        assert!(
            a.enabled,
            "only its author knows when a custom flow applies"
        );
    }

    #[test]
    fn an_unprobed_repository_waits_rather_than_guessing() {
        assert!(!affordance(COMMIT_FLOW, None, true, false, "", &[]).enabled);
        // But a probe that never returned must not lock the button forever.
        assert!(affordance(COMMIT_FLOW, None, false, false, "", &[]).enabled);
    }

    #[test]
    fn the_sidebar_says_how_big_a_pull_request_is() {
        // Four pull requests all reading "ready to merge" are indistinguishable;
        // their size is what tells them apart.
        let s = status(0, Some(Checks::Passing));
        assert_eq!(s.summary(), "#2 · 3f");
    }

    #[test]
    fn a_pull_request_of_unknown_size_still_shows_its_number() {
        let mut s = status(0, Some(Checks::Passing));
        s.pr.as_mut().unwrap().files = 0;
        assert_eq!(s.summary(), "#2");
    }

    #[test]
    fn the_size_line_gets_the_plural_right() {
        let mut pr = PrBrief {
            number: "1".into(),
            title: "t".into(),
            url: "u".into(),
            checks: Checks::Passing,
            files: 1,
            additions: 2,
            deletions: 0,
            commits: 1,
        };
        assert_eq!(pr.size(), "1 file  +2  −0");
        pr.files = 115;
        pr.additions = 9297;
        pr.deletions = 4567;
        assert_eq!(pr.size(), "115 files  +9297  −4567");
    }

    #[tokio::test]
    async fn an_override_decides_the_base_without_asking_git() {
        // ais_tom_platform: PRs target `develop`, but origin/HEAD in that
        // clone still points at `main`, 1536 commits back. Detection would
        // answer `main`; the override must win, and must say it did so
        // preflight can check the branch actually exists on origin.
        let (base, how) = base_branch("/no/such/repo", Some("develop".into())).await;
        assert_eq!(base, "develop");
        assert_eq!(how, OVERRIDDEN);
    }

    #[test]
    fn a_base_that_is_1536_commits_behind_is_what_produced_the_bad_button() {
        // The symptom the override exists to prevent: clean tree, no PR, and
        // an unmerged count measured against the wrong base.
        let mut s = status(0, None);
        s.unmerged = 1536;
        assert_eq!(s.wants(), Wants::OpenPr);
        assert_eq!(
            affordance(COMMIT_FLOW, Some(&s), false, false, "", &[]).label,
            "Push 1536 commits & open PR"
        );

        // Measured against the base the repository actually targets, there is
        // nothing to offer.
        s.unmerged = 0;
        assert_eq!(s.wants(), Wants::Nothing);
        assert!(!affordance(COMMIT_FLOW, Some(&s), false, false, "", &[]).enabled);
    }

    #[test]
    fn a_merged_pull_request_is_not_offered_for_review() {
        // The exact failure: merge with --delete-branch, stay on the branch,
        // and gh still answers with the merged PR.
        assert!(!is_open(&json!({ "number": 5, "state": "MERGED" })));
        assert!(!is_open(&json!({ "number": 5, "state": "CLOSED" })));
        assert!(is_open(&json!({ "number": 5, "state": "OPEN" })));
    }

    #[test]
    fn a_gh_that_does_not_report_state_is_still_usable() {
        assert!(is_open(&json!({ "number": 5 })));
    }

    #[test]
    fn the_state_check_is_not_case_sensitive() {
        assert!(is_open(&json!({ "state": "open" })));
    }

    #[test]
    fn one_red_check_makes_the_rollup_red() {
        // Their own case: fmt fails on purpose, test passes.
        let value = json!([
            { "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS" },
            { "name": "fmt",  "status": "COMPLETED", "conclusion": "FAILURE" },
        ]);
        assert_eq!(rollup(&value), Checks::Failing);
    }

    #[test]
    fn a_running_check_reads_as_pending() {
        let value = json!([{ "name": "test", "status": "IN_PROGRESS" }]);
        assert_eq!(rollup(&value), Checks::Pending);
    }

    #[test]
    fn skipped_and_neutral_checks_do_not_count_against_a_build() {
        let value = json!([
            { "name": "a", "status": "COMPLETED", "conclusion": "SKIPPED" },
            { "name": "b", "status": "COMPLETED", "conclusion": "NEUTRAL" },
        ]);
        assert_eq!(rollup(&value), Checks::Passing);
    }

    #[test]
    fn no_checks_at_all_is_unknown_not_green() {
        assert_eq!(rollup(&json!([])), Checks::Unknown);
        assert_eq!(rollup(&json!(null)), Checks::Unknown);
    }
}
