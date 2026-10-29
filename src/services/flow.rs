//! The commit-and-PR flow: the graph's shape, and what each node does.
//!
//! This is the hardcoded part. Everything it touches — `graph.rs`, `llm.rs`,
//! `git.rs` — is already generic; making the app generic means replacing this
//! file with something loaded from disk, not rewriting the engine.
//!
//! The shape is a diamond, not a chain:
//!
//! ```text
//!                                      ┌──> push ──────┐
//!   scan ──> draft_commit ──> commit ──>┤               ├──> open_pr
//!                                      └──> draft_pr ──┘
//! ```
//!
//! `draft_pr` waits for `commit`, so a run stopped at the commit approval
//! never spends a model call on a description. It does not wait for `push`:
//! neither reads what the other writes, and `open_pr` waits for both.

use serde_json::json;

use super::forge::{self, Forge};
use super::git;
use super::graph::{NodeSpec, ProposalItem, Remedy, RunState, Step};
use super::llm::{self, complete_json, Asker, LlmConfig};
use super::remote;
use super::testsuite;
mod commit;
pub use commit::*;
mod publish;
use publish::*;
mod checks;
pub(crate) use checks::*;

/// What a node produced. `summary` lands on the card, `log` in the detail
/// pane, and `artifacts` crosses the edge to downstream nodes.
#[derive(Clone, Debug, Default)]
pub struct StepOutcome {
    pub summary: String,
    pub log: String,
    pub artifacts: Vec<(String, String)>,
    /// The step ran fine and found there was nothing for it to do. The run
    /// stops here, but it did not go wrong — no red, no retry, no fix to offer.
    pub nothing_to_do: bool,
    /// Files this step produced that a later one will act on, offered for
    /// deselection right here. `scan` fills this so the choice can be made
    /// against the diff, rather than against a list of paths at the approval.
    pub items: Vec<ProposalItem>,
}

impl StepOutcome {
    pub fn nothing(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            summary: reason.clone(),
            log: reason,
            artifacts: vec![],
            nothing_to_do: true,
            items: vec![],
        }
    }
}

/// A failure that may come with known fixes. The message is what the user
/// reads; the remedies are what they can press.
///
/// The `From` impls exist so every other step can keep returning plain strings
/// and let `?` widen them — only the steps that actually know a fix build one.
#[derive(Clone, Debug, Default)]
pub struct StepFailure {
    pub message: String,
    pub remedies: Vec<Remedy>,
}

impl From<String> for StepFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            remedies: vec![],
        }
    }
}

impl From<&str> for StepFailure {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_string(),
            remedies: vec![],
        }
    }
}

/// The shipped commit flow, as a runnable graph.
///
/// Flows live in `flows.toml` now; this exists so tests and anything else
/// needing a known-good graph do not have to touch disk.
#[cfg(test)]
pub fn commit_and_pr_flow() -> super::graph::Graph {
    super::flowdef::FlowBook::defaults()
        .get("commit_and_pr")
        .expect("the shipped book always contains commit_and_pr")
        .to_graph()
}

/// What a model step produces when there is no model.
///
/// Skipping these outright is not enough: `commit` reads `commit_subject`, and
/// a run that skips `draft_commit` would commit an empty message. So a skipped
/// model step still writes its outputs — from the diff, deterministically. The
/// result is a plainer commit message than a model writes and one you see at
/// the approval before it is used, which is the trade a person who has turned
/// AI off has already accepted.
pub fn without_model(node: &NodeSpec, state: &RunState) -> StepOutcome {
    let artifacts = match node.step {
        Step::DraftCommit => {
            let subject = subject_from_stat(state.artifact("stat"));
            vec![
                ("branch_name".to_string(), branch_from(&subject)),
                ("commit_subject".to_string(), subject),
                (
                    "commit_body".to_string(),
                    state.artifact("stat").to_string(),
                ),
            ]
        }
        Step::DraftPr => {
            // The commit subject is already a summary of the same change, and
            // repeating it beats inventing a second worse one.
            let title = match state.artifact("commit_subject") {
                "" => subject_from_stat(state.artifact("stat")),
                subject => subject.to_string(),
            };
            vec![
                ("pr_title".to_string(), title),
                ("pr_body".to_string(), state.artifact("stat").to_string()),
            ]
        }
        // Nothing deterministic stands in for reading a diff for regressions.
        // Leaving `verdict` empty is the honest answer, and it is the one the
        // trusted-run rules already read as "nobody looked".
        _ => vec![],
    };

    StepOutcome {
        summary: "skipped — this install runs without AI".into(),
        log: if artifacts.is_empty() {
            "This step needs a model, and this install is set to run without one. \
             Nothing was written."
                .into()
        } else {
            format!(
                "This step needs a model, and this install is set to run without one.\n\n\
                 Written from the diff instead:\n\n{}",
                artifacts
                    .iter()
                    .map(|(k, v)| format!("{k}:\n  {v}"))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )
        },
        artifacts,
        nothing_to_do: false,
        items: vec![],
    }
}

/// A Conventional Commits subject from `git diff --stat`, naming the deepest
/// directory every changed file shares.
///
/// `chore: update 4 files in src/services` says the true amount a machine can
/// know about a change without reading it. Anything more confident would be a
/// guess dressed up as a summary.
fn subject_from_stat(stat: &str) -> String {
    let paths: Vec<&str> = stat
        .lines()
        .filter_map(|line| line.split('|').next())
        .map(str::trim)
        .filter(|p| !p.is_empty() && p.contains('/') || p.contains('.'))
        .collect();

    match paths.len() {
        0 => "chore: update".to_string(),
        1 => format!("chore: update {}", paths[0]),
        n => match common_dir(&paths) {
            Some(dir) => format!("chore: update {n} files in {dir}"),
            None => format!("chore: update {n} files"),
        },
    }
}

/// The longest directory prefix every path shares, if there is one.
fn common_dir(paths: &[&str]) -> Option<String> {
    let first: Vec<&str> = paths[0].split('/').collect();
    let mut shared = first.len().saturating_sub(1);
    for path in &paths[1..] {
        let parts: Vec<&str> = path.split('/').collect();
        shared = shared.min(parts.len().saturating_sub(1));
        while shared > 0 && first[..shared] != parts[..shared] {
            shared -= 1;
        }
    }
    (shared > 0).then(|| first[..shared].join("/"))
}

/// A branch name from a commit subject: lowercase, punctuation to dashes.
fn branch_from(subject: &str) -> String {
    let slug: String = subject
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    format!("work/{slug}")
}

/// Whether the commit node must open a topic branch first.
///
/// True when the working branch *is* the base, and also whenever the branch is
/// a protected name regardless of what base detection concluded. Those two can
/// disagree — a stale local `main` in a `master` repository is enough — and
/// when they do, the protected-name rule wins.
pub fn must_branch(state: &RunState) -> bool {
    // Nothing exists yet to branch from or open a pull request into: the
    // first commit goes on the branch itself, and pushing it creates it.
    if is_first_commit(state) {
        return false;
    }
    let branch = state.artifact("branch");
    branch == state.artifact("base")
        || git::is_protected(branch)
        || !state.artifact("merged_pr").is_empty()
}

/// Set by `scan` in a repository with no commits yet.
pub fn is_first_commit(state: &RunState) -> bool {
    state.artifact("first_commit") == "true"
}

/// Exactly what will happen if the human approves this node. Rendered in the
/// approval pane, so it must describe the real command, not a paraphrase.
pub fn proposal(node: &NodeSpec, state: &RunState) -> String {
    match node.step {
        Step::Merge => super::review::proposal(node.step, state),
        Step::ScanChanges => "git status --porcelain\ngit diff\n\n\
            Reads the working tree and produces the diff every later step \
            works from. Nothing is written to git — this only looks."
            .to_string(),
        Step::PrDiff => format!(
            "git fetch origin {} {}\ngit diff origin/{}...origin/{}\n\n\
            Reads the pull request's diff from git — nothing is written, \
            this only looks. Worth a look before the model analyses it.",
            state.artifact("pr_base"),
            state.artifact("pr_head"),
            state.artifact("pr_base"),
            state.artifact("pr_head"),
        ),
        Step::RunRemote => format!(
            "ssh {} '{}'\n\nThis runs on another machine.{}",
            node.setting("host"),
            node.setting("command"),
            if node.setting("stdin").is_empty() {
                String::new()
            } else {
                format!("\n\nAnswering prompts with: {:?}", node.setting("stdin"))
            }
        ),
        Step::RunTests => match node.setting("command").trim() {
            "" => "The project's test suite, worked out from the repository — \
                   `cargo test`, `npm test`, `pytest` and so on. The node log \
                   names the file it decided from.\n\n\
                   A failure stops the flow here; a repository with no suite \
                   this recognises is not a failure."
                .to_string(),
            command => format!("sh -c '{command}'\n\nA failure stops the flow here."),
        },
        Step::RunScript => format!(
            "sh -c '{}'\n\nin {}{}",
            node.setting("command"),
            state.artifact("branch"),
            if node.setting("stdin").is_empty() {
                String::new()
            } else {
                format!("\n\nAnswering prompts with: {:?}", node.setting("stdin"))
            }
        ),
        Step::Commit => {
            let branching = if must_branch(state) && !state.artifact("merged_pr").is_empty() {
                format!(
                    "git checkout -b {} origin/{}\n\n\
                     {} was already merged in #{}, so this goes on a fresh branch from \
                     {}, not on top of it. Your changes come along.\n\n",
                    state.artifact("branch_name"),
                    state.artifact("base"),
                    state.artifact("branch"),
                    state.artifact("merged_pr"),
                    state.artifact("base"),
                )
            } else if must_branch(state) {
                format!("git checkout -b {}\n", state.artifact("branch_name"))
            } else if is_first_commit(state) {
                format!(
                    "stay on branch {} — the repository's first commit, so there is \
                     nothing to branch from yet\n",
                    state.artifact("branch")
                )
            } else {
                format!("stay on branch {}\n", state.artifact("branch"))
            };
            format!(
                "{branching}git add -- <the files below>\ngit commit -m \"{}\"{}",
                state.artifact("commit_subject"),
                if state.artifact("commit_body").trim().is_empty() {
                    String::new()
                } else {
                    format!(" -m \"{}\"", state.artifact("commit_body"))
                },
            )
        }
        Step::WriteNotes => super::notes::proposal(state.artifact("release_notes")),
        Step::Push => format!(
            "git push -u origin {}\n\nThis publishes the branch to origin.",
            state.artifact("work_branch")
        ),
        Step::OpenPr => format!(
            "gh pr create --base {} --head {}\n\nTitle:\n  {}\n\nBody:\n{}",
            state.artifact("base"),
            state.artifact("work_branch"),
            state.artifact("pr_title"),
            state.artifact("pr_body"),
        ),
        _ => String::new(),
    }
}

/// A gated node that would do nothing if approved. Stopping to ask would be
/// a question with no content: release notes that were already written leave
/// `write_notes` nothing to write, and that is the usual case, not the rare one.
pub fn nothing_to_approve(node: &NodeSpec, state: &RunState) -> bool {
    match node.step {
        Step::WriteNotes => state.artifact("release_notes").trim().is_empty(),
        Step::OpenPr => is_first_commit(state),
        _ => false,
    }
}

fn first_commit_note(state: &RunState) -> String {
    format!(
        "No pull request: this was the repository's first commit, pushed straight to {} \
         — origin had no branch to open one against. Work from here on goes through \
         pull requests as usual.",
        state.artifact("work_branch")
    )
}

/// The items a gated node lets the human pick through before approving.
/// Only `commit` offers any today.
///
/// A file deselected earlier in the run stays deselected here. `scan` offers
/// the same list against the actual diff, which is the better place to decide,
/// and the approval must not quietly re-check what was already dropped.
pub fn proposal_items(node: &NodeSpec, state: &RunState) -> Vec<ProposalItem> {
    if node.step != Step::Commit {
        return vec![];
    }

    // Read from every node rather than a named one, so this keeps working when
    // a flow built in Setup calls its scanning step something else.
    let dropped: std::collections::HashSet<&str> = state
        .runs
        .values()
        .flat_map(|run| run.items.iter())
        .filter(|item| !item.included)
        .map(|item| item.key.as_str())
        .collect();

    state
        .artifact("commit_paths")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|path| ProposalItem {
            key: path.to_string(),
            label: path.to_string(),
            note: state
                .artifact("file_notes")
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{path}\t")))
                .unwrap_or("modified")
                .to_string(),
            included: !dropped.contains(path),
        })
        .collect()
}

/// Runs one node.
///
/// Takes the whole `NodeSpec` rather than just its `Step`, because a
/// configurable step — a script, say — is only fully described by the node it
/// sits in: the command lives there, not in the code.
pub async fn execute(
    node: &NodeSpec,
    repo: &str,
    cfg: &LlmConfig,
    state: &RunState,
    on_line: &mut dyn FnMut(&str),
) -> Result<StepOutcome, StepFailure> {
    // What a model step is called to any other step waiting for the model
    // behind it — likely in another repository, so the repository is named.
    let mut asker = Asker {
        what: format!(
            "{} in {}",
            node.title,
            std::path::Path::new(repo)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| repo.to_string())
        ),
        on_line,
    };
    let asker = &mut asker;
    match node.step {
        Step::Preflight => preflight(repo, cfg).await,
        Step::ScanChanges => scan(repo, state).await,
        Step::DraftCommit => draft_commit(cfg, state, asker).await,
        Step::Commit => commit(&node.id, repo, state).await,
        Step::DraftPr => draft_pr(cfg, state, asker).await,
        Step::Push => push(repo, state).await,
        Step::OpenPr => open_pr(repo, state).await,
        // The review steps live in their own module but share one entry point:
        // a step means the same thing wherever a flow places it.
        Step::FindPr
        | Step::PrStatus
        | Step::PrDiff
        | Step::Analyse
        | Step::SecondOpinion
        | Step::Lenses
        | Step::Merge
        | Step::Sync => super::review::execute(node.step, repo, cfg, state, asker).await,
        // Only these two shell out to a process that can run long enough for
        // live output to matter — everything else above finishes fast enough
        // that a final log is all "streaming" would ever show anyway.
        Step::DraftNotes => super::notes::draft(repo, cfg, asker).await,
        Step::WriteNotes => super::notes::write(repo, state.artifact("release_notes")).await,
        Step::RunTests => run_tests(node, repo, asker.on_line).await,
        Step::RunScript => run_script(node, repo, asker.on_line).await,
        Step::RunRemote => run_remote(node, asker.on_line).await,
    }
}

fn is_release(command: &str) -> bool {
    command.contains("release.sh")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// What a failed release script left behind, gathered after it stopped so the
/// failure can name the actual fix rather than echo the script's best guess.
#[derive(Clone, Debug, Default)]
struct ReleaseFindings {
    upstream: git::Upstream,
    /// See `git::unpushed_release_tag`.
    unpushed_tag: Option<String>,
}

fn unpushed_release_message(tag: &str, tracking: &str, behind: usize) -> String {
    format!(
        "The release committed and tagged {tag} here, but {tracking} has moved on — \
         {} this copy did not have — so git refused the push. Nothing reached origin: \
         {tag} exists only on this machine.\n\n\
         Undo the local release commit and tag, pull, and the release runs again on \
         top of what origin has, picking up any notes that came with it. Commits of \
         your own before the release commit are kept and replayed on top.",
        plural(behind, "commit")
    )
}

/// Drops the release commit and its tag, then pulls.
///
/// `--keep`, not `--hard`: it refuses rather than discard a local change, and
/// the only thing it should ever remove is the version bump the script made.
fn undo_release_remedy(tag: &str) -> Remedy {
    let script = format!("git tag -d {tag} && git reset --keep HEAD~1 && git pull --rebase");
    Remedy::new(
        &format!("Undo the local {tag}, pull, and release again"),
        "sh",
        &["-c", &script],
    )
}

/// The working tree's diff, untracked files included — same shape `scan`
/// commits as the `diff` artifact. Shared with `diff_preview` so the
/// approval and the step it approves can never show two different diffs
/// for the same tree.
async fn working_tree_diff(repo: &str, changes: &[git::FileChange]) -> Result<String, StepFailure> {
    let mut diff = git::diff(repo).await?;
    for change in changes.iter().filter(|c| c.is_untracked()) {
        if diff.len() >= git::DIFF_CAP {
            break;
        }
        diff.push_str(&git::untracked_diff(repo, &change.path).await);
    }
    Ok(git::cap(&diff))
}

/// A live look at what `scan_changes` or `pr_diff` would produce, read fresh
/// at approval time so the panel shows the actual diff rather than a
/// paraphrase of it. `None` for any other step, or once there is nothing to
/// show — the approval falls back to the plain text proposal in that case.
pub async fn diff_preview(node: &NodeSpec, repo: &str, state: &RunState) -> Option<String> {
    match node.step {
        Step::ScanChanges => {
            let changes = git::status(repo).await.ok()?;
            if changes.is_empty() {
                return None;
            }
            working_tree_diff(repo, &changes).await.ok()
        }
        Step::PrDiff => {
            let base = state.artifact("pr_base");
            let head = state.artifact("pr_head");
            if base.is_empty() || head.is_empty() {
                return None;
            }
            let _ = git::fetch(repo, &["origin", base, head]).await;
            let range = format!("origin/{base}...origin/{head}");
            let diff = git::run(repo, "git", &["diff", &range, "--unified=3"])
                .await
                .ok()?;
            if diff.trim().is_empty() {
                return None;
            }
            Some(git::cap(&diff))
        }
        _ => None,
    }
}

/// Whether this commit node put a list of files in front of the human at all.
///
/// The distinction the staging decision turns on. A node that offered items
/// got an answer, however empty; a node that offered none never asked.
fn offered_a_choice(node_id: &str, state: &RunState) -> bool {
    state.runs.get(node_id).is_some_and(|r| !r.items.is_empty())
}

fn push_rejected(output: &str) -> bool {
    output.contains("non-fast-forward")
        || output.contains("tip of your current branch is behind")
        || output.contains("Updates were rejected")
}

/// `branch-2`, `branch-3`… — the first not taken locally or on origin.
pub(crate) async fn free_branch_name(repo: &str, branch: &str) -> String {
    for n in 2.. {
        let name = format!("{branch}-{n}");
        if !git::branch_exists(repo, &name).await
            && !git::branch_exists(repo, &format!("refs/remotes/origin/{name}")).await
        {
            return name;
        }
    }
    unreachable!()
}

/// A fast-forward git refused because uncommitted edits are in the way.
///
/// `--autostash` sets them aside, moves the branch, and puts them back. If
/// putting them back clashes with what came in, git leaves both versions
/// marked in the files and keeps a copy in the stash — nothing is lost, and
/// the person resolves it in their editor.
pub(crate) fn autostash_remedy(target: &str) -> Remedy {
    Remedy::new(
        "Set your changes aside, update, and put them back",
        "git",
        &["merge", "--ff-only", "--autostash", target],
    )
}

fn rename_remedy(branch: &str, new: &str) -> Remedy {
    Remedy::new(
        &format!("Push under a new name, {new}"),
        "git",
        &["branch", "-m", branch, new],
    )
    .setting("work_branch", new)
}

#[cfg(test)]
mod tests;
