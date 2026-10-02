//! Local branches, and whether their pull request still gives a reason to
//! keep them around.
//!
//! Git never deletes a branch just because the pull request it fed is merged
//! or closed — a repository worked on for a while accumulates branches
//! nobody needs any more. This answers, per branch, what became of its pull
//! request, so a panel can offer to clean up the ones that are done with.

use super::forge::Forge;
use super::git;

/// What happened to a branch's pull request. Separate from "no pull request
/// was ever opened" (`None`) and from "this forge isn't checked yet"
/// (`Unchecked`) — collapsing either into the other would either claim a
/// branch is safe to keep when it might not be, or safe to delete when
/// nothing was actually confirmed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrState {
    Open,
    Merged,
    Closed,
    None,
    Unchecked,
}

impl PrState {
    pub fn label(&self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
            PrState::None => "no PR",
            PrState::Unchecked => "not checked",
        }
    }

    /// Whether the branch's work is confirmed to live somewhere else, so
    /// losing the branch itself loses nothing. Only a merge earns this — a
    /// closed PR's commits exist only on the branch, and "no PR" or
    /// "unchecked" promise nothing either way.
    pub fn merged(&self) -> bool {
        matches!(self, PrState::Merged)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    pub protected: bool,
    pub pr_number: Option<String>,
    pub pr_title: String,
    pub pr_state: PrState,
    /// Commits on this branch not on `base` — the case this exists for is a
    /// branch with real, un-shipped work and no live pull request (`None` or
    /// `Closed`): 0 here means there is nothing a PR would even carry, so
    /// deleting is the only sensible option, not a choice.
    pub ahead: usize,
    /// Merging this branch into `base` would change no file. Its commits are
    /// old merges, a release bump already superseded, or work that reached
    /// the base another way (a squash merge) — the count above is real, but
    /// there is nothing in it to ship.
    pub changes_nothing: bool,
    /// The branch still points at exactly the commit its merged pull request
    /// merged — nothing has been added since. Squash merges leave such a
    /// branch looking ahead of the base forever.
    pub merged_as_is: bool,
    pub is_base: bool,
}

impl BranchInfo {
    /// Whether there's real, un-shipped work here that a person should be
    /// offered a pull request for instead of only a delete button — commits
    /// that exist nowhere else, and no live PR already carrying them.
    pub fn worth_a_pr(&self) -> bool {
        self.ahead > 0
            && !self.changes_nothing
            && matches!(self.pr_state, PrState::None | PrState::Closed)
    }

    /// Nothing here that the base does not already have, so cleaning it up
    /// loses nothing. Never the base itself, whatever it is called, and never
    /// a branch whose pull request is still open — that one is for merging.
    pub fn leftover(&self) -> bool {
        !self.is_base
            && self.pr_state != PrState::Open
            && (self.ahead == 0 || self.changes_nothing || self.merged_as_is)
    }

    /// Why `leftover` said so, for the row's tooltip.
    pub fn leftover_reason(&self) -> &'static str {
        if self.ahead == 0 {
            "No commits that the base branch does not already have."
        } else if self.merged_as_is {
            "Still exactly what its pull request merged; nothing has been added since."
        } else {
            "Its commits would change no file if merged."
        }
    }
}

/// Every local branch, each paired with what became of its pull request.
/// `Err` only for an actual failed call — an empty repository (no branches)
/// is `Ok(vec![])`.
pub async fn list(repo: &str, forge: &Forge, base: &str) -> Result<Vec<BranchInfo>, String> {
    let current = git::current_branch(repo).await.unwrap_or_default();
    let out = git::run(repo, "git", &["branch", "--format=%(refname:short)"]).await?;
    let names: Vec<String> = out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    let prs: Vec<PrRow> = match forge {
        Forge::GitHub => {
            let out = git::run(
                repo,
                "gh",
                &[
                    "pr",
                    "list",
                    "--state",
                    "all",
                    "--limit",
                    "200",
                    "--json",
                    "number,title,state,headRefName,headRefOid",
                ],
            )
            .await?;
            let value: serde_json::Value =
                serde_json::from_str(&out).map_err(|e| format!("could not read gh output: {e}"))?;
            value
                .as_array()
                .map(|prs| prs.iter().filter_map(github_pr_row).collect())
                .unwrap_or_default()
        }
        _ => vec![],
    };

    let unchecked = !matches!(forge, Forge::GitHub);
    let base_ref = resolve_ref(repo, base).await;

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let found = prs.iter().find(|row| row.head == name);
        let (pr_number, pr_title, pr_state) = match found {
            Some(row) => (Some(row.number.clone()), row.title.clone(), row.state),
            None if unchecked => (None, String::new(), PrState::Unchecked),
            None => (None, String::new(), PrState::None),
        };
        let merged_as_is = match found {
            Some(row) if row.state == PrState::Merged && !row.head_oid.is_empty() => {
                git::run(repo, "git", &["rev-parse", &format!("refs/heads/{name}")])
                    .await
                    .is_ok_and(|tip| tip.trim() == row.head_oid)
            }
            _ => false,
        };
        let ahead = if name == base {
            0
        } else {
            ahead_count(repo, &base_ref, &name).await
        };
        let changes_nothing = ahead > 0 && merges_to_nothing(repo, &base_ref, &name).await;
        out.push(BranchInfo {
            changes_nothing,
            merged_as_is,
            is_base: name == base,
            is_current: name == current,
            protected: git::is_protected(&name),
            pr_number,
            pr_title,
            pr_state,
            ahead,
            name,
        });
    }
    Ok(out)
}

/// `origin/<base>` when that remote-tracking ref exists, else the bare local
/// name — mirrors `already_committed`'s reasoning in `flow.rs`: comparing
/// against a local `base` that has drifted from `origin/base` would either
/// miscount or fail outright.
async fn resolve_ref(repo: &str, base: &str) -> String {
    let remote = format!("refs/remotes/origin/{base}");
    if git::branch_exists(repo, &remote).await {
        format!("origin/{base}")
    } else {
        base.to_string()
    }
}

/// Whether merging `branch` into `base_ref` leaves the base's tree exactly
/// as it is. Asked of git's merge machinery rather than a diff: a two-dot
/// diff is non-empty whenever the base has moved on, and a three-dot one
/// shows a squash-merged branch's changes even though the base already has
/// them. A conflict, or a git too old for `merge-tree --write-tree`, answers
/// no — the branch then keeps its ordinary "decide per branch" treatment.
async fn merges_to_nothing(repo: &str, base_ref: &str, branch: &str) -> bool {
    let Ok(merged) = git::run(
        repo,
        "git",
        &["merge-tree", "--write-tree", base_ref, branch],
    )
    .await
    else {
        return false;
    };
    let base_tree = git::run(repo, "git", &["rev-parse", &format!("{base_ref}^{{tree}}")])
        .await
        .unwrap_or_default();
    let merged_tree = merged.lines().next().unwrap_or_default().trim();
    !merged_tree.is_empty() && merged_tree == base_tree.trim()
}

async fn ahead_count(repo: &str, base_ref: &str, branch: &str) -> usize {
    git::run(
        repo,
        "git",
        &["rev-list", "--count", &format!("{base_ref}..{branch}")],
    )
    .await
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

struct PrRow {
    head: String,
    number: String,
    title: String,
    state: PrState,
    /// The commit the pull request pointed at — for a merged one, what was
    /// merged.
    head_oid: String,
}

fn github_pr_row(pr: &serde_json::Value) -> Option<PrRow> {
    let head = pr["headRefName"].as_str()?.to_string();
    let head_oid = pr["headRefOid"].as_str().unwrap_or_default().to_string();
    let number = pr["number"].as_i64().unwrap_or_default().to_string();
    let title = pr["title"].as_str().unwrap_or_default().to_string();
    let state = match pr["state"].as_str().unwrap_or_default() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        "OPEN" => PrState::Open,
        _ => PrState::None,
    };
    Some(PrRow {
        head,
        number,
        title,
        state,
        head_oid,
    })
}

/// Pushes a branch and opens a pull request for it — the other option
/// alongside deleting, for a branch that has real work and no live PR.
/// Title and body come from that branch's own commit log rather than an
/// extra model call: the point is a fast, obvious way to not lose work
/// found while cleaning up, not a polished description.
pub async fn create_pr(
    repo: &str,
    branch: &str,
    base: &str,
    forge: &Forge,
) -> Result<String, String> {
    git::run(repo, "git", &["push", "-u", "origin", branch]).await?;

    let base_ref = resolve_ref(repo, base).await;
    let log = git::run(
        repo,
        "git",
        &["log", &format!("{base_ref}..{branch}"), "--format=%s"],
    )
    .await
    .unwrap_or_default();
    let (title, body) = title_and_body(&log, branch);

    super::forge::create_pr(forge, repo, base, branch, &title, &body).await
}

/// A pull request title and body straight from a branch's own commit
/// subjects — the latest becomes the title (git log lists newest first),
/// and the rest (if any) become a bulleted body. `branch` is the fallback
/// title for a branch whose log came back empty.
fn title_and_body(log: &str, branch: &str) -> (String, String) {
    let subjects: Vec<&str> = log
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let title = subjects.first().copied().unwrap_or(branch).to_string();
    let body = if subjects.len() > 1 {
        subjects
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };
    (title, body)
}

/// Deletes a local branch. `force` bypasses git's own merged-into-current-
/// branch check — needed for a squash-merged branch, whose commit is folded
/// into one on the base branch and so never looks like an ancestor locally,
/// even though GitHub confirms the work landed.
pub async fn delete(repo: &str, branch: &str, force: bool) -> Result<String, String> {
    let flag = if force { "-D" } else { "-d" };
    git::run(repo, "git", &["branch", flag, branch]).await
}

/// Removes a leftover branch everywhere it lives: closes its open pull
/// request, deletes it on origin, then locally. Each step is skipped when
/// there is nothing for it to do, and the log says what was done.
///
/// Force-deleting locally is right here and only here: the branch was just
/// shown to change nothing, which is exactly the case git's own "not fully
/// merged" check cannot see.
pub async fn clean_up(repo: &str, forge: &Forge, branch: &BranchInfo) -> Result<String, String> {
    let mut log = vec![];
    if let (Forge::GitHub, PrState::Open, Some(number)) =
        (forge, branch.pr_state, branch.pr_number.as_deref())
    {
        git::run(
            repo,
            "gh",
            &[
                "pr",
                "close",
                number,
                "--comment",
                "Closing — this branch changes no files compared with the base branch.",
            ],
        )
        .await?;
        log.push(format!("closed #{number}"));
    }
    if git::branch_exists(repo, &format!("refs/remotes/origin/{}", branch.name)).await {
        git::run(repo, "git", &["push", "origin", "--delete", &branch.name]).await?;
        log.push("deleted on origin".to_string());
    }
    git::run(repo, "git", &["branch", "-D", &branch.name]).await?;
    log.push("deleted locally".to_string());
    Ok(log.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_confirmed_merge_says_the_branch_is_safe_to_lose() {
        assert!(PrState::Merged.merged());
        for state in [
            PrState::Open,
            PrState::Closed,
            PrState::None,
            PrState::Unchecked,
        ] {
            assert!(!state.merged(), "{state:?}");
        }
    }

    #[test]
    fn a_pull_request_row_with_no_head_branch_is_skipped_rather_than_panicking() {
        let row = serde_json::json!({"number": 3, "title": "x", "state": "MERGED"});
        assert!(github_pr_row(&row).is_none());
    }

    #[test]
    fn a_pull_request_row_parses_its_state() {
        let row = serde_json::json!({
            "number": 7, "title": "Add diff view", "state": "MERGED", "headRefName": "feat/diff-view"
        });
        let row = github_pr_row(&row).unwrap();
        let (head, number, title, state) = (row.head, row.number, row.title, row.state);
        assert_eq!(head, "feat/diff-view");
        assert_eq!(number, "7");
        assert_eq!(title, "Add diff view");
        assert_eq!(state, PrState::Merged);
    }

    fn branch(ahead: usize, pr_state: PrState) -> BranchInfo {
        BranchInfo {
            name: "feat/x".into(),
            is_current: false,
            protected: false,
            pr_number: None,
            pr_title: String::new(),
            pr_state,
            ahead,
            changes_nothing: false,
            merged_as_is: false,
            is_base: false,
        }
    }

    #[test]
    fn nothing_ahead_or_merged_as_is_is_a_leftover_but_an_open_pr_never_is() {
        assert!(
            branch(0, PrState::None).leftover(),
            "nothing the base lacks"
        );
        let mut merged = branch(1, PrState::Merged);
        assert!(
            !merged.leftover(),
            "a commit added after the merge is new work"
        );
        merged.merged_as_is = true;
        assert!(
            merged.leftover(),
            "still exactly what the pull request merged"
        );
        let mut open = branch(0, PrState::Open);
        open.changes_nothing = true;
        assert!(
            !open.leftover(),
            "an open pull request is for merging, not cleaning up"
        );
    }

    #[test]
    fn a_branch_that_changes_nothing_is_a_leftover_not_a_pull_request() {
        let mut b = branch(3, PrState::None);
        assert!(b.worth_a_pr() && !b.leftover());
        b.changes_nothing = true;
        assert!(
            !b.worth_a_pr(),
            "no pull request for commits that add nothing"
        );
        assert!(b.leftover());
        b.is_base = true;
        assert!(
            !b.leftover(),
            "the base is never cleaned up, whatever it holds"
        );
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[tokio::test]
    async fn merging_tells_a_leftover_from_real_work() {
        let dir = std::env::temp_dir().join(format!("gitagent-leftover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "trunk"]);
        std::fs::write(dir.join("a"), "1").unwrap();
        git(&dir, &["add", "a"]);
        git(&dir, &["commit", "-qm", "one"]);

        // Merge commits only: ahead, but nothing new — splitter's stale `main`.
        git(&dir, &["checkout", "-qb", "stale"]);
        git(
            &dir,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "Merge branch 'trunk'",
            ],
        );
        // Work the trunk then got by a squash merge.
        git(&dir, &["checkout", "-q", "trunk"]);
        git(&dir, &["checkout", "-qb", "squashed"]);
        std::fs::write(dir.join("b"), "2").unwrap();
        git(&dir, &["add", "b"]);
        git(&dir, &["commit", "-qm", "add b"]);
        git(&dir, &["checkout", "-q", "trunk"]);
        std::fs::write(dir.join("b"), "2").unwrap();
        git(&dir, &["add", "b"]);
        git(&dir, &["commit", "-qm", "add b (#1)"]);
        // Real, unshipped work.
        git(&dir, &["checkout", "-qb", "real"]);
        std::fs::write(dir.join("c"), "3").unwrap();
        git(&dir, &["add", "c"]);
        git(&dir, &["commit", "-qm", "add c"]);
        git(&dir, &["checkout", "-q", "trunk"]);

        let repo = dir.to_string_lossy().to_string();
        let list = list(&repo, &Forge::None, "trunk").await.unwrap();
        let get = |n: &str| list.iter().find(|b| b.name == n).unwrap().clone();
        assert!(get("stale").leftover(), "merge commits that add nothing");
        assert!(
            get("squashed").leftover(),
            "squash-merged: the trunk already has it"
        );
        assert!(!get("real").leftover() && get("real").ahead == 1);
        assert!(get("trunk").is_base && !get("trunk").leftover());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_branch_is_only_worth_a_pr_with_real_work_and_no_live_pr() {
        assert!(branch(3, PrState::None).worth_a_pr());
        assert!(branch(3, PrState::Closed).worth_a_pr());
        assert!(
            !branch(0, PrState::None).worth_a_pr(),
            "nothing for a PR to carry"
        );
        assert!(
            !branch(3, PrState::Open).worth_a_pr(),
            "already has a live PR"
        );
        assert!(!branch(3, PrState::Merged).worth_a_pr(), "already shipped");
        assert!(
            !branch(3, PrState::Unchecked).worth_a_pr(),
            "not confirmed either way"
        );
    }

    #[test]
    fn a_single_commit_becomes_the_title_with_no_body() {
        let (title, body) = title_and_body("fix: stale lockfile\n", "feat/x");
        assert_eq!(title, "fix: stale lockfile");
        assert_eq!(body, "");
    }

    #[test]
    fn several_commits_keep_the_newest_as_title_and_list_the_rest() {
        let (title, body) = title_and_body(
            "feat: add retry\nfix: typo\nfeat: start feature\n",
            "feat/x",
        );
        assert_eq!(title, "feat: add retry");
        assert_eq!(
            body,
            "- feat: add retry\n- fix: typo\n- feat: start feature"
        );
    }

    #[test]
    fn an_empty_log_falls_back_to_the_branch_name() {
        let (title, body) = title_and_body("", "feat/git-staging-improvements");
        assert_eq!(title, "feat/git-staging-improvements");
        assert_eq!(body, "");
    }
}
