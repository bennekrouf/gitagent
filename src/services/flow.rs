//! The commit-and-PR flow: the graph's shape, and what each node does.
//!
//! This is the hardcoded part. Everything it touches — `graph.rs`, `llm.rs`,
//! `git.rs` — is already generic; making the app generic means replacing this
//! file with something loaded from disk, not rewriting the engine.
//!
//! The shape is a diamond, not a chain:
//!
//! ```text
//!                            ┌──> commit ──> push ──┐
//!   scan ──> draft_commit ──>┤                      ├──> open_pr
//!                            └──> draft_pr ─────────┘
//! ```
//!
//! `draft_pr` reads the diff and the commit message; it never reads anything
//! `commit` or `push` produce, so there is no edge between them. Today the
//! executor still runs one node at a time, but the dependency is honest, and
//! that is what makes running the ready set concurrently a scheduler change
//! rather than a redesign.

use serde_json::json;

use super::forge::{self, Forge};
use super::git;
use super::graph::{Graph, NodeKind, NodeSpec, ProposalItem, Remedy, RunState, Step};
use super::llm::{self, complete_json, LlmConfig};

/// What a node produced. `summary` lands on the card, `log` in the detail
/// pane, and `artifacts` crosses the edge to downstream nodes.
#[derive(Clone, Debug, Default)]
pub struct StepOutcome {
    pub summary: String,
    pub log: String,
    pub artifacts: Vec<(String, String)>,
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

/// Small builder so the flow below reads as a declaration rather than as a
/// wall of positional arguments. `after`/`reads`/`writes` are the node's
/// contract; anything not stated is empty.
pub struct Build(NodeSpec);

pub fn node(id: &str, title: &str, subtitle: &str, step: Step, kind: NodeKind) -> Build {
    Build(NodeSpec {
        id: id.into(),
        title: title.into(),
        subtitle: subtitle.into(),
        step,
        kind,
        deps: vec![],
        reads: vec![],
        writes: vec![],
        requires_approval: false,
    })
}

fn keys(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

impl Build {
    pub fn after(mut self, deps: &[&str]) -> Self {
        self.0.deps = keys(deps);
        self
    }

    pub fn reads(mut self, k: &[&str]) -> Self {
        self.0.reads = keys(k);
        self
    }

    pub fn writes(mut self, k: &[&str]) -> Self {
        self.0.writes = keys(k);
        self
    }

    /// Touches git history or the remote: park and wait for a human.
    pub fn gated(mut self) -> Self {
        self.0.requires_approval = true;
        self
    }

    pub fn done(self) -> NodeSpec {
        self.0
    }
}

pub fn commit_and_pr_flow() -> Graph {
    Graph {
        nodes: vec![
            node(
                "preflight",
                "Preflight",
                "Remote, forge, credentials, model — before anything is touched",
                Step::Preflight,
                NodeKind::Deterministic,
            )
            .writes(&["remote_url", "forge", "base"])
            .done(),
            node(
                "scan",
                "Scan changes",
                "Read the working tree",
                Step::ScanChanges,
                NodeKind::Deterministic,
            )
            .after(&["preflight"])
            .reads(&["base"])
            .writes(&[
                "branch",
                "stat",
                "diff",
                "commit_paths",
                "file_notes",
                "untracked",
            ])
            .done(),
            node(
                "draft_commit",
                "Draft commit message",
                "Model call — subject, body, branch name",
                Step::DraftCommit,
                NodeKind::Model,
            )
            .after(&["scan"])
            .reads(&["stat", "diff"])
            .writes(&["branch_name", "commit_subject", "commit_body"])
            .done(),
            node(
                "commit",
                "Commit",
                "Branch if needed, stage the listed files, commit",
                Step::Commit,
                NodeKind::Deterministic,
            )
            .after(&["draft_commit"])
            .reads(&[
                "branch_name",
                "commit_subject",
                "commit_body",
                "commit_paths",
            ])
            .writes(&["work_branch", "commit_sha"])
            .gated()
            .done(),
            node(
                "draft_pr",
                "Draft PR description",
                "Model call — runs off the diff, not off the push",
                Step::DraftPr,
                NodeKind::Model,
            )
            .after(&["draft_commit"])
            .reads(&["stat", "diff", "commit_subject"])
            .writes(&["pr_title", "pr_body"])
            .done(),
            node(
                "push",
                "Push branch",
                "git push -u origin <branch>",
                Step::Push,
                NodeKind::Deterministic,
            )
            .after(&["commit"])
            .reads(&["work_branch"])
            .writes(&["push_output"])
            .gated()
            .done(),
            node(
                "open_pr",
                "Open pull request",
                "gh pr create",
                Step::OpenPr,
                NodeKind::Deterministic,
            )
            .after(&["push", "draft_pr"])
            .reads(&["work_branch", "base", "pr_title", "pr_body"])
            .writes(&["pr_url"])
            .gated()
            .done(),
        ],
    }
}

/// Whether the commit node must open a topic branch first.
///
/// True when the working branch *is* the base, and also whenever the branch is
/// a protected name regardless of what base detection concluded. Those two can
/// disagree — a stale local `main` in a `master` repository is enough — and
/// when they do, the protected-name rule wins.
pub fn must_branch(state: &RunState) -> bool {
    let branch = state.artifact("branch");
    branch == state.artifact("base") || git::is_protected(branch)
}

/// Exactly what will happen if the human approves this node. Rendered in the
/// approval pane, so it must describe the real command, not a paraphrase.
pub fn proposal(step: Step, state: &RunState) -> String {
    match step {
        Step::Commit => {
            let branching = if must_branch(state) {
                format!("git checkout -b {}\n", state.artifact("branch_name"))
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

/// The items a gated node lets the human pick through before approving.
/// Only `commit` offers any today.
pub fn proposal_items(step: Step, state: &RunState) -> Vec<ProposalItem> {
    if step != Step::Commit {
        return vec![];
    }
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
            included: true,
        })
        .collect()
}

pub async fn execute(
    step: Step,
    repo: &str,
    cfg: &LlmConfig,
    state: &RunState,
) -> Result<StepOutcome, StepFailure> {
    match step {
        Step::Preflight => preflight(repo, cfg).await,
        Step::ScanChanges => scan(repo).await,
        Step::DraftCommit => draft_commit(cfg, state).await,
        Step::Commit => commit(repo, state).await,
        Step::DraftPr => draft_pr(cfg, state).await,
        Step::Push => push(repo, state).await,
        Step::OpenPr => open_pr(repo, state).await,
        _ => Err(StepFailure::from("step does not belong to this flow")),
    }
}

/// The flows this app knows how to run.
///
/// This enum is the seam. Today both arms are Rust functions; the generic
/// version replaces them with definitions loaded from disk and everything
/// above this line — the engine, the executor, the UI — stays as it is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum FlowKind {
    CommitAndPr,
    ReviewAndMerge,
}

impl FlowKind {
    pub const ALL: [FlowKind; 2] = [FlowKind::CommitAndPr, FlowKind::ReviewAndMerge];

    pub fn label(self) -> &'static str {
        match self {
            FlowKind::CommitAndPr => "Commit → PR",
            FlowKind::ReviewAndMerge => "Review → Merge",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            FlowKind::CommitAndPr => "commit_and_pr",
            FlowKind::ReviewAndMerge => "review_and_merge",
        }
    }

    pub fn first_node(self) -> &'static str {
        match self {
            FlowKind::CommitAndPr => "preflight",
            FlowKind::ReviewAndMerge => "find_pr",
        }
    }

    pub fn graph(self) -> Graph {
        match self {
            FlowKind::CommitAndPr => commit_and_pr_flow(),
            FlowKind::ReviewAndMerge => super::review::review_and_merge_flow(),
        }
    }

    pub fn proposal(self, step: Step, state: &RunState) -> String {
        match self {
            FlowKind::CommitAndPr => proposal(step, state),
            FlowKind::ReviewAndMerge => super::review::proposal(step, state),
        }
    }

    pub fn proposal_items(self, step: Step, state: &RunState) -> Vec<ProposalItem> {
        match self {
            FlowKind::CommitAndPr => proposal_items(step, state),
            FlowKind::ReviewAndMerge => vec![],
        }
    }

    pub async fn execute(
        self,
        step: Step,
        repo: &str,
        cfg: &LlmConfig,
        state: &RunState,
    ) -> Result<StepOutcome, StepFailure> {
        match self {
            FlowKind::CommitAndPr => execute(step, repo, cfg, state).await,
            FlowKind::ReviewAndMerge => super::review::execute(step, repo, cfg, state).await,
        }
    }
}

/// Everything that has to be true before the run may touch anything.
///
/// This node exists because the alternative is discovering at `open_pr` — after
/// a commit and a push have already happened — that `gh` was never
/// authenticated. It reads; it never writes.
async fn preflight(repo: &str, cfg: &LlmConfig) -> Result<StepOutcome, StepFailure> {
    let mut log = String::new();
    let mut failures: Vec<String> = vec![];

    let url = git::remote_url(repo).await;
    let forge = url.as_deref().map(forge::detect).unwrap_or(Forge::None);
    log.push_str(&format!(
        "remote  {}\nforge   {}\n",
        url.as_deref().unwrap_or("(none)"),
        forge.label()
    ));

    let (base, how) = git::default_remote_branch(repo).await;
    let branch = git::current_branch(repo).await?;
    log.push_str(&format!("base    {base}  ({how})\nbranch  {branch}\n"));
    if git::is_protected(&branch) {
        log.push_str(&format!(
            "        {branch} is protected — the commit node will branch off it\n"
        ));
    }
    log.push('\n');

    let mut remedies: Vec<Remedy> = vec![];

    for check in forge::check_credentials(&forge).await {
        log.push_str(&format!(
            "{}  {:<24} {}\n",
            if check.ok { "ok  " } else { "FAIL" },
            check.name,
            check.detail
        ));
        if !check.ok {
            failures.push(format!("{}: {}", check.name, check.detail));
            if let Some(fix) = check.fix {
                remedies.push(fix);
            }
        }
    }

    match llm::probe(cfg).await {
        Ok(detail) => log.push_str(&format!("ok    {:<24} {detail}\n", cfg.active_model())),
        Err(detail) => {
            log.push_str(&format!("FAIL  {:<24} {detail}\n", cfg.active_model()));
            failures.push(format!("model: {detail}"));
            // A model that is simply not pulled yet is worth a button; an
            // unreachable ollama is not something this app can start for you.
            if detail.contains("not pulled") {
                remedies.push(Remedy::new(
                    &format!("Pull {}", cfg.ollama_model),
                    "ollama",
                    &["pull", &cfg.ollama_model],
                ));
            }
        }
    }

    if !failures.is_empty() {
        return Err(StepFailure {
            message: format!(
                "Preflight failed. Nothing in the repository has been touched.\n\n{}\n\n{log}",
                failures.join("\n")
            ),
            remedies,
        });
    }

    Ok(StepOutcome {
        summary: format!("{} · base {base}", forge.label()),
        log,
        artifacts: vec![
            ("remote_url".into(), url.unwrap_or_default()),
            ("forge".into(), forge.as_key()),
            ("base".into(), base),
        ],
    })
}

async fn scan(repo: &str) -> Result<StepOutcome, StepFailure> {
    let branch = git::current_branch(repo).await?;
    let changes = git::status(repo).await?;

    if changes.is_empty() {
        return Err("No changes in the working tree — nothing to commit.".into());
    }

    // Everything git reports is a candidate, new files included. `git status`
    // already filters out anything .gitignore covers, and the commit approval
    // lists each file individually with a checkbox — that is the safety net,
    // not a blanket rule about which kinds of change are allowed.
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let stat = git::diff_stat(repo).await?;

    let mut diff = git::diff(repo).await?;
    for change in changes.iter().filter(|c| c.is_untracked()) {
        if diff.len() >= git::DIFF_CAP {
            break;
        }
        diff.push_str(&git::untracked_diff(repo, &change.path).await);
    }
    let diff = git::cap(&diff);

    let listing = changes
        .iter()
        .map(|c| format!("  {:<9} {}", c.note(), c.path))
        .collect::<Vec<_>>()
        .join("\n");
    let log = format!("branch: {branch}\n\n{stat}\n\n{listing}\n");
    let untracked: Vec<&git::FileChange> = changes.iter().filter(|c| c.is_untracked()).collect();

    Ok(StepOutcome {
        summary: format!("{} file(s) changed on {branch}", changes.len()),
        log,
        artifacts: vec![
            ("branch".into(), branch),
            ("stat".into(), stat),
            ("diff".into(), diff),
            ("commit_paths".into(), paths.join("\n")),
            (
                "file_notes".into(),
                changes
                    .iter()
                    .map(|c| format!("{}\t{}", c.path, c.note()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (
                "untracked".into(),
                untracked
                    .iter()
                    .map(|c| c.path.clone())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ],
    })
}

async fn draft_commit(cfg: &LlmConfig, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let system = "You write git commit messages for a Rust desktop application.\n\
        Rules:\n\
        - `subject`: Conventional Commits (feat/fix/chore/docs/refactor/test/perf), \
          imperative mood, at most 72 characters, no trailing period.\n\
        - `body`: one to three short lines saying WHY the change was made. Empty \
          string if the subject already says everything.\n\
        - `branch`: kebab-case, prefixed with the same type and a slash \
          (e.g. `fix/stale-lockfile`), at most 40 characters.\n\
        Describe only what the diff actually shows. Do not invent motivation.";

    let user = format!(
        "Diffstat:\n{}\n\nDiff:\n{}",
        state.artifact("stat"),
        state.artifact("diff")
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "branch":  { "type": "string" },
            "subject": { "type": "string" },
            "body":    { "type": "string" }
        },
        "required": ["branch", "subject", "body"]
    });

    let value = complete_json(cfg, system, &user, &schema).await?;

    let subject = value["subject"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let body = value["body"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let branch = sanitise_branch(value["branch"].as_str().unwrap_or_default());

    if subject.is_empty() {
        return Err("model returned an empty commit subject".into());
    }

    Ok(StepOutcome {
        summary: subject.clone(),
        log: format!("branch:  {branch}\nsubject: {subject}\n\n{body}"),
        artifacts: vec![
            ("branch_name".into(), branch),
            ("commit_subject".into(), subject),
            ("commit_body".into(), body),
        ],
    })
}

/// Branch names come from a model, so they get scrubbed before they reach a
/// shell argument: lowercase, only `[a-z0-9._/-]`, no leading or trailing
/// separators, length-capped.
pub fn sanitise_branch(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().to_lowercase().chars() {
        match ch {
            'a'..='z' | '0'..='9' | '/' | '.' | '_' => out.push(ch),
            '-' => out.push('-'),
            ' ' | '\t' => out.push('-'),
            _ => {}
        }
    }
    let out = out
        .trim_matches(|c| c == '-' || c == '/' || c == '.')
        .to_string();
    let out: String = out.chars().take(40).collect();
    if out.is_empty() {
        "gitagent/change".to_string()
    } else {
        out
    }
}

async fn commit(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let mut log = String::new();

    // Only branch when sitting on the base branch; if the user is already on a
    // topic branch, commit there rather than stacking another one.
    let work_branch = if must_branch(state) {
        let mut name = state.artifact("branch_name").to_string();
        if git::branch_exists(repo, &name).await {
            name = format!("{name}-{}", chrono::Local::now().format("%H%M%S"));
        }
        log.push_str(&git::create_branch(repo, &name).await?);
        log.push_str(&format!("created and switched to {name}\n"));
        name
    } else {
        let name = state.artifact("branch").to_string();
        log.push_str(&format!("already on {name}, committing there\n"));
        name
    };

    // What the human left checked at the approval step, falling back to the
    // full scan if the node offered no items.
    let selected = state
        .runs
        .get("commit")
        .map(|r| r.included_keys())
        .unwrap_or_default();
    let paths: Vec<String> = if selected.is_empty() {
        state
            .artifact("commit_paths")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    } else {
        selected
    };
    if paths.is_empty() {
        return Err("no files selected to stage".into());
    }

    git::add(repo, &paths).await?;
    log.push_str(&format!("staged {} file(s)\n", paths.len()));

    let out = git::commit(
        repo,
        state.artifact("commit_subject"),
        state.artifact("commit_body"),
    )
    .await?;
    log.push_str(&out);

    let sha = git::head_sha(repo).await?;

    Ok(StepOutcome {
        summary: format!("{sha} on {work_branch}"),
        log,
        artifacts: vec![
            ("work_branch".into(), work_branch),
            ("commit_sha".into(), sha),
        ],
    })
}

async fn draft_pr(cfg: &LlmConfig, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let system = "You write pull request descriptions for a Rust desktop application.\n\
        Rules:\n\
        - `title`: one line, at most 72 characters, plain language, no type prefix.\n\
        - `body`: GitHub markdown with exactly three sections: `## What changed`, \
          `## Why`, `## How to test`.\n\
        - `## How to test` must name the specific screens, commands or cases this \
          diff touches. Never write a generic checklist.\n\
        Describe only what the diff shows.";

    let user = format!(
        "Commit subject: {}\n\nDiffstat:\n{}\n\nDiff:\n{}",
        state.artifact("commit_subject"),
        state.artifact("stat"),
        state.artifact("diff")
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "body":  { "type": "string" }
        },
        "required": ["title", "body"]
    });

    let value = complete_json(cfg, system, &user, &schema).await?;
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
        log: format!("{title}\n\n{body}"),
        artifacts: vec![("pr_title".into(), title), ("pr_body".into(), body)],
    })
}

async fn push(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let branch = state.artifact("work_branch").to_string();
    let out = git::push(repo, &branch).await?;
    Ok(StepOutcome {
        summary: format!("pushed {branch}"),
        log: out.clone(),
        artifacts: vec![("push_output".into(), out)],
    })
}

async fn open_pr(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::graph::NodeStatus;

    #[test]
    fn every_dependency_names_a_node_that_exists() {
        let g = commit_and_pr_flow();
        for n in &g.nodes {
            for d in &n.deps {
                assert!(g.get(d).is_some(), "{} depends on unknown node {d}", n.id);
            }
        }
    }

    #[test]
    fn every_artifact_a_node_reads_is_written_upstream() {
        let g = commit_and_pr_flow();
        for n in &g.nodes {
            for key in &n.reads {
                let produced = g
                    .nodes
                    .iter()
                    .any(|other| other.id != n.id && other.writes.contains(key));
                assert!(produced, "{} reads {key}, which nothing writes", n.id);
            }
        }
    }

    #[test]
    fn drafting_the_pr_does_not_wait_on_the_push() {
        // The diamond: if this edge ever appears, the flow has silently become
        // a chain and the PR text cannot be prepared while git works.
        let g = commit_and_pr_flow();
        let draft_pr = g.get("draft_pr").unwrap();
        assert!(!draft_pr.deps.contains(&"push".to_string()));
        assert!(!draft_pr.deps.contains(&"commit".to_string()));
    }

    #[test]
    fn every_node_that_touches_git_history_or_the_remote_asks_first() {
        let g = commit_and_pr_flow();
        for id in ["commit", "push", "open_pr"] {
            assert!(g.get(id).unwrap().requires_approval, "{id} must be gated");
        }
        for id in ["preflight", "scan", "draft_commit", "draft_pr"] {
            assert!(!g.get(id).unwrap().requires_approval, "{id} is read-only");
        }
    }

    #[test]
    fn credentials_are_checked_before_anything_is_written() {
        // The bug this guards: gh auth failing at open_pr, after the commit and
        // the push had already happened.
        let g = commit_and_pr_flow();
        assert!(
            g.get("preflight").unwrap().deps.is_empty(),
            "preflight is the root"
        );
        for id in ["scan", "commit", "push", "open_pr"] {
            let mut seen = vec![id.to_string()];
            let mut i = 0;
            while i < seen.len() {
                let node = g.get(&seen[i]).unwrap().clone();
                for d in node.deps {
                    if !seen.contains(&d) {
                        seen.push(d);
                    }
                }
                i += 1;
            }
            assert!(
                seen.contains(&"preflight".to_string()),
                "{id} must sit downstream of preflight"
            );
        }
    }

    #[test]
    fn a_protected_branch_forces_a_topic_branch_even_when_base_disagrees() {
        // ais-analytics: a stale local `main`, a working branch of `master`.
        // Comparing branch to base alone said "already on a topic branch" and
        // committed straight to master.
        let mut s = RunState::default();
        s.artifacts.insert("branch".into(), "master".into());
        s.artifacts.insert("base".into(), "main".into());
        assert!(must_branch(&s));
    }

    #[test]
    fn sitting_on_the_base_branch_forces_a_topic_branch() {
        let mut s = RunState::default();
        s.artifacts.insert("branch".into(), "master".into());
        s.artifacts.insert("base".into(), "master".into());
        assert!(must_branch(&s));
    }

    #[test]
    fn a_real_topic_branch_is_committed_to_directly() {
        let mut s = RunState::default();
        s.artifacts
            .insert("branch".into(), "fix/stale-lockfile".into());
        s.artifacts.insert("base".into(), "master".into());
        assert!(!must_branch(&s));
    }

    #[test]
    fn the_flow_reaches_the_pr_when_every_node_succeeds() {
        let g = commit_and_pr_flow();
        let mut s = RunState::fresh(&g);
        let mut order = vec![];
        while let Some(n) = s.next_ready(&g) {
            order.push(n.id.clone());
            s.set_status(&n.id, NodeStatus::Done);
        }
        assert!(s.is_finished(&g));
        assert_eq!(order.last().unwrap(), "open_pr");
        assert_eq!(order.first().unwrap(), "preflight");
    }

    #[test]
    fn rejecting_the_commit_blocks_the_push_and_the_pr() {
        let g = commit_and_pr_flow();
        let mut s = RunState::fresh(&g);
        for id in ["preflight", "scan", "draft_commit", "draft_pr"] {
            s.set_status(id, NodeStatus::Done);
        }
        s.set_status("commit", NodeStatus::Rejected);
        s.propagate_block(&g);
        assert_eq!(s.status("push"), NodeStatus::Blocked);
        assert_eq!(s.status("open_pr"), NodeStatus::Blocked);
        assert!(s.is_finished(&g));
    }

    #[test]
    fn each_flow_starts_at_the_node_it_claims_to() {
        for kind in FlowKind::ALL {
            let g = kind.graph();
            let s = RunState::fresh(&g);
            assert_eq!(
                s.next_ready(&g).unwrap().id,
                kind.first_node(),
                "{:?}",
                kind
            );
        }
    }

    #[test]
    fn no_two_flows_share_a_key() {
        let keys: Vec<&str> = FlowKind::ALL.iter().map(|f| f.key()).collect();
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(keys.len(), unique.len());
    }

    #[test]
    fn a_new_file_is_offered_for_commit_rather_than_refused() {
        // ais-monitor: the only change was an untracked ci.yml, and the scan
        // used to fail outright.
        let mut s = RunState::default();
        s.artifacts
            .insert("commit_paths".into(), ".github/workflows/ci.yml".into());
        s.artifacts
            .insert("file_notes".into(), ".github/workflows/ci.yml\tnew".into());
        let items = proposal_items(Step::Commit, &s);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].note, "new");
        assert!(items[0].included, "checked by default");
    }

    #[test]
    fn only_the_commit_node_offers_a_file_list() {
        let s = RunState::default();
        for step in [Step::Push, Step::OpenPr, Step::Preflight] {
            assert!(proposal_items(step, &s).is_empty());
        }
    }

    #[test]
    fn a_path_with_no_recorded_status_still_appears() {
        let mut s = RunState::default();
        s.artifacts
            .insert("commit_paths".into(), "src/lib.rs".into());
        let items = proposal_items(Step::Commit, &s);
        assert_eq!(items[0].note, "modified");
    }

    #[test]
    fn branch_names_from_a_model_are_scrubbed_before_they_reach_a_shell() {
        assert_eq!(sanitise_branch("Feat/Add Thing"), "feat/add-thing");
        assert_eq!(sanitise_branch("fix: `rm -rf /`"), "fix-rm--rf");
        assert_eq!(sanitise_branch("  --weird--  "), "weird");
        assert_eq!(sanitise_branch("$(whoami)"), "whoami");
        assert_eq!(sanitise_branch(""), "gitagent/change");
    }

    #[test]
    fn a_long_branch_name_is_capped() {
        assert!(sanitise_branch(&"a".repeat(200)).len() <= 40);
    }
}
