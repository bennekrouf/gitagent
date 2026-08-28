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
//! step pulls, so the refs are fresh exactly when it matters.

use super::git;

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
            return match &self.last_tag {
                Some(tag) => format!("released at {tag}"),
                None => "never released".into(),
            };
        }

        let what = match &self.last_tag {
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
    RELEASE_MARKERS.iter().any(|marker| root.join(marker).exists())
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

    // Short-circuit before walking the log: with no tag and no tooling the
    // range would be the whole history, which is both meaningless and slow.
    if !releases_from_here(repo, last_tag.is_some()) {
        return ReleaseState::default();
    }

    let range = match &last_tag {
        Some(tag) => format!("{tag}..{remote_base}"),
        None => remote_base.clone(),
    };

    let subjects: Vec<String> = git::run(repo, "git", &["log", &range, "--format=%s"])
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
        };
        assert!(!never.due(), "no commits at all is not a pending release");
        assert_eq!(never.summary(), "never released");
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
