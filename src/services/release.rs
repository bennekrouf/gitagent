//! Whether a repository has shipped what it has merged.
//!
//! The question "does this need a release?" has an exact answer in git: are
//! there commits on the default branch that the last tag does not reach?
//! `release.sh` bumps the version and tags that same commit, so anything after
//! the tag is merged-but-unreleased work.
//!
//! Squash merges put the pull request number in the subject — `(#15)` — so the
//! merged pull requests can be named without asking the forge, which keeps this
//! free of an extra network round-trip.
//!
//! Local refs only: no fetch happens here. The probe already makes one network
//! call per repository and a fetch per repository on every refresh would be
//! slower than it is worth. After merging through the review flow the `sync`
//! step pulls, and the release steps themselves fetch and catch up before they
//! read anything (`flow::catch_up`), so a merge made on the forge's website is
//! picked up there even when this count missed it.
//!
//! A deploy flow ships without tagging, so for it the last tag is the wrong
//! bound: it never moves, and the count only grows. Each successful run of a
//! flow that handles releases therefore leaves the base-branch commit it
//! shipped behind (`record_shipped`), and the count starts from whichever of
//! that and the last tag is further along.

use super::flowdef::FlowBook;
use super::git;
use super::graph::{Graph, NodeStatus, RunState};
use super::probe::{self, Need};
use super::store;

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ReleaseState {
    /// `None` when the repository has never been tagged.
    pub last_tag: Option<String>,
    /// Commits on the default branch the last tag does not reach.
    pub commits: usize,
    /// Pull request numbers among them, newest first.
    pub prs: Vec<String>,
    /// Whether this repository releases at all.
    ///
    /// Without this, any repository that has simply never been tagged reports
    /// its entire history as pending — a 349-commit Node project with no
    /// release tooling claiming "release due" is noise, not a signal.
    pub releases: bool,
    /// The count starts at the last successful run of a flow that handles
    /// releases rather than at `last_tag` — a deploy that tags nothing has
    /// shipped since.
    pub since_shipped: bool,
}

impl ReleaseState {
    pub fn due(&self) -> bool {
        self.releases && self.commits > 0
    }

    /// One line for the detail panes.
    pub fn summary(&self) -> String {
        if !self.releases {
            return "not released from here".into();
        }
        if !self.due() {
            if self.since_shipped {
                return "nothing new since the last deploy".into();
            }
            return match &self.last_tag {
                Some(tag) => format!("released at {tag}"),
                None => "never released".into(),
            };
        }

        let what = match &self.last_tag {
            _ if self.since_shipped => format!(
                "{} commit{} since the last deploy",
                self.commits,
                if self.commits == 1 { "" } else { "s" }
            ),
            Some(tag) => format!(
                "{} commit{} since {tag}",
                self.commits,
                if self.commits == 1 { "" } else { "s" }
            ),
            None => format!(
                "{} commit{}, never tagged",
                self.commits,
                if self.commits == 1 { "" } else { "s" }
            ),
        };

        if self.prs.is_empty() {
            what
        } else {
            format!("{what} ({})", self.prs.join(", "))
        }
    }

    /// What the count is measured from, for "N commits since …".
    pub fn since(&self) -> String {
        if self.since_shipped {
            return "the last deploy".into();
        }
        self.last_tag
            .clone()
            .unwrap_or_else(|| "the first commit".into())
    }
}

/// Pulls `#15` out of squash-merge subjects like
/// `feat: adopt login shell's PATH at startup (#15)`.
pub fn pr_numbers(subjects: &[String]) -> Vec<String> {
    let mut found = vec![];
    for subject in subjects {
        // The number is the last parenthesised group, so a subject that itself
        // contains brackets does not confuse it.
        let Some(open) = subject.rfind("(#") else {
            continue;
        };
        let rest = &subject[open + 2..];
        let Some(close) = rest.find(')') else {
            continue;
        };
        let digits = &rest[..close];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let tag = format!("#{digits}");
            if !found.contains(&tag) {
                found.push(tag);
            }
        }
    }
    found
}

/// Files that mean "this repository is released from here".
///
/// Deliberately release-*specific* names rather than any CI definition at
/// all. `.github/workflows/release.yml` says what it is for; a bare
/// `azure-pipelines.yml` does not — it is just as likely to be build-and-
/// test, and treating every Azure DevOps repo that has CI as a releasing one
/// would put back exactly the noise this check exists to remove. So the
/// Azure entries match the same shape as the GitHub one: a pipeline that
/// names itself a release.
const RELEASE_MARKERS: &[&str] = &[
    "scripts/release.sh",
    ".github/workflows/release.yml",
    ".github/workflows/release.yaml",
    "azure-pipelines-release.yml",
    "azure-pipelines-release.yaml",
    ".azuredevops/release.yml",
    ".azuredevops/release.yaml",
    ".pipelines/release.yml",
    ".pipelines/release.yaml",
];

/// Evidence that this repository is one that gets released: it has been
/// tagged before, or it carries the tooling to tag itself.
fn releases_from_here(repo: &str, has_tag: bool) -> bool {
    if has_tag {
        return true;
    }
    let root = std::path::Path::new(repo);
    RELEASE_MARKERS
        .iter()
        .any(|marker| root.join(marker).exists())
}

pub async fn status(repo: &str, base: &str) -> ReleaseState {
    let remote_base = format!("origin/{base}");
    let last_tag = git::run(
        repo,
        "git",
        &["describe", "--tags", "--abbrev=0", &remote_base],
    )
    .await
    .ok()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty());

    // Only while the base branch still contains it: after a force push or a
    // switch of base branch it bounds nothing.
    let shipped = match store::shipped_commit(repo) {
        Some(commit) if is_ancestor(repo, &commit, &remote_base).await => Some(commit),
        _ => None,
    };

    // Short-circuit before walking the log: with no tag and no tooling the
    // range would be the whole history, which is both meaningless and slow.
    if !releases_from_here(repo, last_tag.is_some() || shipped.is_some()) {
        return ReleaseState::default();
    }

    // The shipped commit names the count only when it is past the tag; a
    // release flow that tags records the tagged commit itself, and then the
    // tag is the better name for the same place.
    let since_shipped = match (&last_tag, &shipped) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(tag), Some(commit)) => git::run(
            repo,
            "git",
            &["rev-list", "--count", &format!("{tag}..{commit}")],
        )
        .await
        .ok()
        .and_then(|n| n.trim().parse::<usize>().ok())
        .is_some_and(|n| n > 0),
    };

    let mut args = vec!["log", remote_base.as_str(), "--format=%s", "--not"];
    args.extend(last_tag.as_deref());
    args.extend(shipped.as_deref());

    let subjects: Vec<String> = git::run(repo, "git", &args)
        .await
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    ReleaseState {
        last_tag,
        commits: subjects.len(),
        prs: pr_numbers(&subjects),
        releases: true,
        since_shipped,
    }
}

async fn is_ancestor(repo: &str, commit: &str, of: &str) -> bool {
    git::run(repo, "git", &["merge-base", "--is-ancestor", commit, of])
        .await
        .is_ok()
}

/// Whether a finished run went all the way through: something was done and
/// nothing failed, was declined, or stopped short of running.
fn went_through(statuses: &[NodeStatus]) -> bool {
    statuses.contains(&NodeStatus::Done)
        && statuses
            .iter()
            .all(|s| matches!(s, NodeStatus::Done | NodeStatus::Bypassed))
}

/// After a run of a flow that handles releases goes all the way through,
/// remembers the base-branch commit it shipped, so the release count starts
/// there next time — see the module doc.
pub async fn record_shipped(repo: &str, flow_id: &str, graph: &Graph, state: &RunState) {
    let statuses: Vec<NodeStatus> = graph.nodes.iter().map(|n| state.status(&n.id)).collect();
    if !went_through(&statuses) {
        return;
    }
    let releases = FlowBook::load()
        .get(flow_id)
        .is_some_and(|flow| flow.answers(Need::Release));
    if !releases {
        return;
    }
    let (base, _) = probe::stored_base_branch(repo).await;
    let Ok(commit) = git::run(
        repo,
        "git",
        &[
            "rev-parse",
            "--verify",
            &format!("origin/{base}^{{commit}}"),
        ],
    )
    .await
    else {
        return;
    };
    let commit = commit.trim();
    if !commit.is_empty() {
        store::save_shipped_commit(repo, commit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subjects(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_squash_merge_subject_yields_its_pull_request() {
        let found = pr_numbers(&subjects(&[
            "feat: adopt login shell's PATH at startup (#15)",
            "fix: update gitagent version in Cargo.lock (#14)",
        ]));
        assert_eq!(found, vec!["#15", "#14"]);
    }

    #[test]
    fn a_plain_commit_contributes_no_pull_request() {
        assert!(pr_numbers(&subjects(&["chore: release v0.1.9"])).is_empty());
    }

    #[test]
    fn only_the_trailing_group_counts() {
        // A subject that mentions an issue mid-sentence must not be mistaken
        // for the merge's own number.
        let found = pr_numbers(&subjects(&["fix: handle (#3) edge case properly (#42)"]));
        assert_eq!(found, vec!["#42"]);
    }

    #[test]
    fn something_that_is_not_a_number_is_ignored() {
        assert!(pr_numbers(&subjects(&["feat: thing (#abc)", "feat: other (#)"])).is_empty());
    }

    #[test]
    fn the_same_pull_request_is_never_listed_twice() {
        let found = pr_numbers(&subjects(&["a (#7)", "b (#7)"]));
        assert_eq!(found, vec!["#7"]);
    }

    #[test]
    fn nothing_since_the_tag_means_nothing_to_ship() {
        let state = ReleaseState {
            last_tag: Some("v0.1.9".into()),
            commits: 0,
            prs: vec![],
            releases: true,
            since_shipped: false,
        };
        assert!(!state.due());
        assert_eq!(state.summary(), "released at v0.1.9");
    }

    #[test]
    fn work_after_the_tag_is_a_release_waiting_to_happen() {
        let state = ReleaseState {
            last_tag: Some("v0.1.9".into()),
            commits: 1,
            prs: vec!["#16".into()],
            releases: true,
            since_shipped: false,
        };
        assert!(state.due());
        assert_eq!(state.summary(), "1 commit since v0.1.9 (#16)");
    }

    #[test]
    fn the_count_reads_correctly_in_the_plural() {
        let state = ReleaseState {
            last_tag: Some("v1.0.0".into()),
            commits: 3,
            prs: vec!["#3".into(), "#2".into()],
            releases: true,
            since_shipped: false,
        };
        assert_eq!(state.summary(), "3 commits since v1.0.0 (#3, #2)");
    }

    #[test]
    fn a_repository_that_does_not_release_is_never_due() {
        // mayorana: a Node project, no tags, no release tooling. Reporting its
        // 349-commit history as a pending release is noise, not a signal.
        let other = ReleaseState {
            last_tag: None,
            commits: 349,
            prs: vec![],
            releases: false,
            since_shipped: false,
        };
        assert!(!other.due());
        assert_eq!(other.summary(), "not released from here");
    }

    #[test]
    fn a_first_release_still_counts_when_the_tooling_is_there() {
        let unshipped = ReleaseState {
            last_tag: None,
            commits: 4,
            prs: vec![],
            releases: true,
            since_shipped: false,
        };
        assert!(unshipped.due());
        assert_eq!(unshipped.summary(), "4 commits, never tagged");
    }

    #[test]
    fn a_tagged_repository_with_nothing_new_is_not_due() {
        let never = ReleaseState {
            last_tag: None,
            commits: 0,
            prs: vec![],
            releases: true,
            since_shipped: false,
        };
        assert!(!never.due(), "no commits at all is not a pending release");
        assert_eq!(never.summary(), "never released");
    }

    #[test]
    fn a_deploy_since_the_tag_is_what_the_count_is_measured_from() {
        // backend-cvenom: deployed on every merge, last tagged a year ago.
        let deployed = ReleaseState {
            last_tag: Some("2.0".into()),
            commits: 2,
            prs: vec!["#4".into(), "#3".into()],
            releases: true,
            since_shipped: true,
        };
        assert_eq!(deployed.since(), "the last deploy");
        assert_eq!(
            deployed.summary(),
            "2 commits since the last deploy (#4, #3)"
        );

        let caught_up = ReleaseState {
            commits: 0,
            prs: vec![],
            ..deployed
        };
        assert!(!caught_up.due());
        assert_eq!(caught_up.summary(), "nothing new since the last deploy");
    }

    #[test]
    fn only_a_run_that_went_all_the_way_through_has_shipped() {
        use NodeStatus::*;
        assert!(went_through(&[Done, Done]));
        assert!(went_through(&[Done, Bypassed]));
        assert!(!went_through(&[Done, Failed]), "the deploy failed");
        assert!(!went_through(&[Done, Rejected]), "the deploy was declined");
        assert!(!went_through(&[Done, Skipped, Blocked]), "nothing to ship");
        assert!(!went_through(&[Done, AwaitingApproval]), "still waiting");
        assert!(!went_through(&[Bypassed]), "nothing ran at all");
    }

    #[test]
    fn a_previous_tag_is_evidence_enough_on_its_own() {
        assert!(releases_from_here("/nonexistent", true));
        assert!(!releases_from_here("/nonexistent", false));
    }

    fn temp_repo(marker: Option<&str>) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-release-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(marker) = marker {
            let path = dir.join(marker);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        dir
    }

    /// An Azure DevOps repository on its first release has no tag and none of
    /// the GitHub files, and used to be written off as "not released from
    /// here" — the same wrong answer this check was added to remove.
    #[test]
    fn an_untagged_azure_devops_repository_is_recognised() {
        for marker in [
            "azure-pipelines-release.yml",
            ".azuredevops/release.yml",
            ".pipelines/release.yaml",
        ] {
            let dir = temp_repo(Some(marker));
            assert!(
                releases_from_here(dir.to_str().unwrap(), false),
                "{marker} should count as release tooling"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A pipeline that does not name itself a release is not evidence of one.
    /// Most `azure-pipelines.yml` files are build-and-test, and counting them
    /// would report every CI-enabled repo as having a release pending.
    #[test]
    fn a_plain_ci_pipeline_is_not_release_tooling() {
        let dir = temp_repo(Some("azure-pipelines.yml"));
        assert!(!releases_from_here(dir.to_str().unwrap(), false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repository_with_no_markers_at_all_is_not_released_from_here() {
        let dir = temp_repo(None);
        assert!(!releases_from_here(dir.to_str().unwrap(), false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
