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
    /// A pull request whose checks are green — a decision is waiting.
    Merge,
    /// A pull request whose checks are red.
    Attention,
    /// Uncommitted work.
    Commit,
    /// A pull request whose checks are still running. Nothing to do yet.
    Wait,
    /// Clean tree, no pull request.
    Nothing,
}

impl Wants {
    /// Whether a person is actually expected to do something.
    pub fn needs_a_person(self) -> bool {
        matches!(self, Wants::Merge | Wants::Attention | Wants::Commit)
    }

    pub fn note(self) -> &'static str {
        match self {
            Wants::Merge => "ready to merge",
            Wants::Attention => "checks failing",
            Wants::Commit => "uncommitted",
            Wants::Wait => "checks running",
            Wants::Nothing => "clean",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            Wants::Merge => "done",
            Wants::Attention => "failed",
            Wants::Commit => "awaiting",
            Wants::Wait => "running",
            Wants::Nothing => "skipped",
        }
    }

    /// Which flow to open on when this repository is chosen.
    pub fn flow_hint(self) -> &'static str {
        match self {
            Wants::Merge | Wants::Attention | Wants::Wait => "review_and_merge",
            _ => "commit_and_pr",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RepoStatus {
    pub branch: String,
    pub changes: usize,
    pub forge: Forge,
    pub pr: Option<PrBrief>,
}

impl RepoStatus {
    /// Uncommitted work outranks a pull request that is merely running its
    /// checks, but never outranks one that is ready for a decision — finishing
    /// something beats starting something.
    pub fn wants(&self) -> Wants {
        match &self.pr {
            Some(pr) => match pr.checks {
                Checks::Passing | Checks::Unknown => Wants::Merge,
                Checks::Failing => Wants::Attention,
                Checks::Pending if self.changes > 0 => Wants::Commit,
                Checks::Pending => Wants::Wait,
            },
            None if self.changes > 0 => Wants::Commit,
            None => Wants::Nothing,
        }
    }

    /// One line for the sidebar.
    pub fn summary(&self) -> String {
        match (&self.pr, self.changes) {
            (Some(pr), _) if pr.files > 0 => format!("#{} · {}f", pr.number, pr.files),
            (Some(pr), _) => format!("#{}", pr.number),
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
/// author knows when it makes sense to run.
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
) -> Affordance {
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
        COMMIT_FLOW => match status.changes {
            0 => Affordance::stop(
                "Nothing to commit",
                "The working tree is clean — there is nothing for this flow to do",
            ),
            1 => Affordance::go(again("Commit 1 file".into())),
            n => Affordance::go(again(format!("Commit {n} files"))),
        },
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

pub async fn probe(repo: &str) -> RepoStatus {
    let branch = git::current_branch(repo).await.unwrap_or_default();
    let changes = git::status(repo).await.map(|c| c.len()).unwrap_or(0);
    let forge = git::remote_url(repo)
        .await
        .map(|url| forge::detect(&url))
        .unwrap_or(Forge::None);
    let pr = open_pr(repo, &forge).await;

    RepoStatus {
        branch,
        changes,
        forge,
        pr,
    }
}

async fn open_pr(repo: &str, forge: &Forge) -> Option<PrBrief> {
    match forge {
        Forge::GitHub => {
            let out = git::run(
                repo,
                "gh",
                &["pr", "view", "--json", "number,title,url,statusCheckRollup"],
            )
            .await
            .ok()?;
            let value: serde_json::Value = serde_json::from_str(&out).ok()?;
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
mod tests {
    use super::*;
    use serde_json::json;

    fn status(changes: usize, pr: Option<Checks>) -> RepoStatus {
        RepoStatus {
            branch: "master".into(),
            changes,
            forge: Forge::GitHub,
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
    fn the_ranking_puts_actionable_repositories_first() {
        let mut all = vec![
            Wants::Nothing,
            Wants::Wait,
            Wants::Commit,
            Wants::Merge,
            Wants::Attention,
        ];
        all.sort();
        assert_eq!(
            all,
            vec![
                Wants::Merge,
                Wants::Attention,
                Wants::Commit,
                Wants::Wait,
                Wants::Nothing
            ]
        );
    }

    #[test]
    fn a_pull_request_opens_the_review_flow_and_a_dirty_tree_the_commit_flow() {
        assert_eq!(Wants::Merge.flow_hint(), "review_and_merge");
        assert_eq!(Wants::Attention.flow_hint(), "review_and_merge");
        assert_eq!(Wants::Commit.flow_hint(), "commit_and_pr");
    }

    #[test]
    fn committing_is_offered_only_when_there_is_something_to_commit() {
        let clean = status(0, None);
        assert!(!affordance(COMMIT_FLOW, Some(&clean), false, false).enabled);

        let dirty = status(3, None);
        let a = affordance(COMMIT_FLOW, Some(&dirty), false, false);
        assert!(a.enabled);
        assert_eq!(a.label, "Commit 3 files");
    }

    #[test]
    fn the_button_counts_one_file_in_the_singular() {
        let one = status(1, None);
        assert_eq!(
            affordance(COMMIT_FLOW, Some(&one), false, false).label,
            "Commit 1 file"
        );
    }

    #[test]
    fn reviewing_is_offered_only_when_a_pull_request_exists() {
        let none = status(0, None);
        let a = affordance(REVIEW_FLOW, Some(&none), false, false);
        assert!(!a.enabled);
        assert!(a.reason.contains("master"), "says which branch");

        let open = status(0, Some(Checks::Passing));
        let b = affordance(REVIEW_FLOW, Some(&open), false, false);
        assert!(b.enabled);
        assert_eq!(b.label, "Review #2");
    }

    #[test]
    fn the_same_repository_can_offer_one_flow_and_refuse_the_other() {
        // Committed and pushed: nothing left to commit, a PR now to review.
        let after = status(0, Some(Checks::Passing));
        assert!(!affordance(COMMIT_FLOW, Some(&after), false, false).enabled);
        assert!(affordance(REVIEW_FLOW, Some(&after), false, false).enabled);

        // And the other way round, before any of that happened.
        let before = status(4, None);
        assert!(affordance(COMMIT_FLOW, Some(&before), false, false).enabled);
        assert!(!affordance(REVIEW_FLOW, Some(&before), false, false).enabled);
    }

    #[test]
    fn a_second_run_says_again() {
        let dirty = status(2, None);
        assert_eq!(
            affordance(COMMIT_FLOW, Some(&dirty), false, true).label,
            "Commit 2 files again"
        );
    }

    #[test]
    fn a_flow_built_in_setup_is_never_second_guessed() {
        let clean = status(0, None);
        let a = affordance("release", Some(&clean), false, false);
        assert!(
            a.enabled,
            "only its author knows when a custom flow applies"
        );
    }

    #[test]
    fn an_unprobed_repository_waits_rather_than_guessing() {
        assert!(!affordance(COMMIT_FLOW, None, true, false).enabled);
        // But a probe that never returned must not lock the button forever.
        assert!(affordance(COMMIT_FLOW, None, false, false).enabled);
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
