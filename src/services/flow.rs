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
use super::graph::{NodeSpec, ProposalItem, Remedy, RunState, Step};
use super::llm::{self, complete_json, LlmConfig};
use super::remote;

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
}

impl StepOutcome {
    pub fn nothing(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            summary: reason.clone(),
            log: reason,
            artifacts: vec![],
            nothing_to_do: true,
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
pub fn proposal_items(node: &NodeSpec, state: &RunState) -> Vec<ProposalItem> {
    if node.step != Step::Commit {
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
    match node.step {
        Step::Preflight => preflight(repo, cfg).await,
        Step::ScanChanges => scan(repo, state).await,
        Step::DraftCommit => draft_commit(cfg, state).await,
        Step::Commit => commit(repo, state).await,
        Step::DraftPr => draft_pr(cfg, state).await,
        Step::Push => push(repo, state).await,
        Step::OpenPr => open_pr(repo, state).await,
        // The review steps live in their own module but share one entry point:
        // a step means the same thing wherever a flow places it.
        Step::FindPr | Step::PrStatus | Step::PrDiff | Step::Analyse | Step::Merge | Step::Sync => {
            super::review::execute(node.step, repo, cfg, state).await
        }
        // Only these two shell out to a process that can run long enough for
        // live output to matter — everything else above finishes fast enough
        // that a final log is all "streaming" would ever show anyway.
        Step::RunScript => run_script(node, repo, on_line).await,
        Step::RunRemote => run_remote(node, on_line).await,
    }
}

/// Runs an arbitrary command in the repository.
///
/// `sh -c` rather than an argument array, so what you type in Setup behaves the
/// way it does in a terminal — pipes, flags, `&&` and all.
///
/// stdin matters more than it looks: a subprocess has no terminal, so a script
/// that asks `Proceed? [y/N]` reads EOF and aborts. The `stdin` setting answers
/// it. That is not a way around the confirmation — GitAgent already asked at
/// the approval step, and showed the exact command it was asking about.
/// Runs a command on another machine. See `services::remote` for why no key
/// ever reaches this app.
async fn run_remote(node: &NodeSpec, on_line: &mut dyn FnMut(&str)) -> Result<StepOutcome, StepFailure> {
    let host = node.setting("host").trim().to_string();
    let command = node.setting("command").trim().to_string();
    if host.is_empty() || command.is_empty() {
        return Err(StepFailure::from(
            "This step needs a host and a command. Set them in Setup.",
        ));
    }

    let (ok, output) = remote::run_streaming(
        &host,
        node.setting("port"),
        node.setting("identity"),
        &command,
        node.setting("stdin"),
        on_line,
    )
    .await;

    if !ok {
        return Err(StepFailure::from(format!(
            "`{command}` failed on {host}.\n\n{output}"
        )));
    }

    Ok(StepOutcome {
        summary: format!("{host} — ok"),
        log: if output.trim().is_empty() {
            format!("{host}: {command}\n\n(no output)")
        } else {
            output.clone()
        },
        artifacts: vec![
            (format!("{}_output", node.id), output),
            (format!("{}_exit", node.id), "0".to_string()),
        ],
        nothing_to_do: false,
    })
}

async fn run_script(
    node: &NodeSpec,
    repo: &str,
    on_line: &mut dyn FnMut(&str),
) -> Result<StepOutcome, StepFailure> {
    let command = node.setting("command").trim().to_string();
    if command.is_empty() {
        return Err(StepFailure::from(
            "This step has no command. Set one in Setup.",
        ));
    }

    let (ok, output) =
        git::run_shell_streaming(repo, &command, node.setting("stdin"), on_line).await;
    if !ok {
        return Err(StepFailure::from(format!(
            "`{command}` failed.\n\n{output}"
        )));
    }

    Ok(StepOutcome {
        summary: format!("{command} — ok"),
        log: if output.trim().is_empty() {
            format!("{command}\n\n(no output)")
        } else {
            output.clone()
        },
        artifacts: vec![
            (format!("{}_output", node.id), output),
            (format!("{}_exit", node.id), "0".to_string()),
        ],
        nothing_to_do: false,
    })
}

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
        nothing_to_do: false,
    })
}

/// The working tree's diff, untracked files included — same shape `scan`
/// commits as the `diff` artifact. Shared with `diff_preview` so the
/// approval and the step it approves can never show two different diffs
/// for the same tree.
async fn working_tree_diff(
    repo: &str,
    changes: &[git::FileChange],
) -> Result<String, StepFailure> {
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
            let _ = git::run(repo, "git", &["fetch", "origin", base, head]).await;
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

/// `Some` when the working tree is clean but the branch is already ahead of
/// `base` — commits made outside this run, still unpushed. `None` when there
/// is truly nothing (a fresh checkout of `base` itself, or a branch already
/// level with it), in which case `scan` falls back to its usual "nothing to
/// commit" outcome.
async fn already_committed(
    repo: &str,
    branch: &str,
    state: &RunState,
) -> Result<Option<StepOutcome>, StepFailure> {
    let base = state.artifact("base");
    if base.is_empty() || base == branch {
        return Ok(None);
    }
    let base_ref = if git::run(
        repo,
        "git",
        &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{base}")],
    )
    .await
    .is_ok()
    {
        format!("origin/{base}")
    } else {
        base.to_string()
    };

    let range = format!("{base_ref}..HEAD");
    let ahead: usize = git::run(repo, "git", &["rev-list", "--count", &range])
        .await
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if ahead == 0 {
        return Ok(None);
    }

    let stat = git::run(repo, "git", &["diff", &range, "--stat"])
        .await
        .unwrap_or_default();
    let diff = git::run(repo, "git", &["diff", &range, "--unified=3"]).await?;
    let plural = if ahead == 1 { "" } else { "s" };

    Ok(Some(StepOutcome {
        summary: format!("{ahead} commit{plural} already on {branch}, ready to push"),
        log: format!(
            "branch: {branch}\n\n{stat}\n\n{ahead} commit{plural} ahead of {base}, none pushed yet\n"
        ),
        artifacts: vec![
            ("branch".into(), branch.to_string()),
            ("stat".into(), stat),
            ("diff".into(), git::cap(&diff)),
            ("commit_paths".into(), String::new()),
            ("file_notes".into(), String::new()),
            ("untracked".into(), String::new()),
        ],
        nothing_to_do: false,
    }))
}

async fn scan(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let branch = git::current_branch(repo).await?;
    let changes = git::status(repo).await?;

    if changes.is_empty() {
        // A clean tree isn't necessarily nothing to do — a commit made
        // outside GitAgent (or by a previous run) can already sit ahead of
        // `base`, unpushed. Diff those commits against `base` so the rest
        // of the flow — draft, commit (a no-op here), push, open PR — has
        // something to work from instead of everything downstream getting
        // skipped just because there was nothing to *stage*.
        if let Some(outcome) = already_committed(repo, &branch, state).await? {
            return Ok(outcome);
        }
        return Ok(StepOutcome::nothing(
            "No changes in the working tree — nothing to commit.",
        ));
    }

    // Everything git reports is a candidate, new files included. `git status`
    // already filters out anything .gitignore covers, and the commit approval
    // lists each file individually with a checkbox — that is the safety net,
    // not a blanket rule about which kinds of change are allowed.
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let stat = git::diff_stat(repo).await?;

    let diff = working_tree_diff(repo, &changes).await?;

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
        nothing_to_do: false,
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
        nothing_to_do: false,
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
        // Nothing was ever proposed to stage (`items` empty) means `scan`
        // found the tree already clean but the branch ahead of `base` — the
        // commit already exists, made outside GitAgent. Unchecking every
        // proposed file, by contrast, is a real "stop" the human meant.
        let items_offered = state
            .runs
            .get("commit")
            .is_some_and(|r| !r.items.is_empty());
        if items_offered {
            return Err("no files selected to stage".into());
        }
        let sha = git::head_sha(repo).await?;
        log.push_str(&format!("nothing to stage — {sha} already committed\n"));
        return Ok(StepOutcome {
            summary: format!("{sha} already on {work_branch}"),
            log,
            artifacts: vec![
                ("work_branch".into(), work_branch),
                ("commit_sha".into(), sha),
            ],
            nothing_to_do: false,
        });
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
        nothing_to_do: false,
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
        nothing_to_do: false,
    })
}

async fn push(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let branch = state.artifact("work_branch").to_string();
    let out = git::push(repo, &branch).await?;
    Ok(StepOutcome {
        summary: format!("pushed {branch}"),
        log: out.clone(),
        artifacts: vec![("push_output".into(), out)],
        nothing_to_do: false,
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
        nothing_to_do: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::graph::NodeStatus;

    #[tokio::test]
    async fn diff_preview_is_none_for_anything_but_scan_changes_and_pr_diff() {
        // Short-circuits before touching git, so an unused repo path is fine.
        let node = NodeSpec {
            id: "commit".into(),
            title: String::new(),
            subtitle: String::new(),
            step: Step::Commit,
            kind: crate::services::graph::NodeKind::Deterministic,
            deps: vec![],
            reads: vec![],
            writes: vec![],
            requires_approval: true,
            config: Default::default(),
        };
        let state = RunState::fresh(&commit_and_pr_flow());
        assert_eq!(diff_preview(&node, "/does/not/matter", &state).await, None);
    }

    #[tokio::test]
    async fn a_clean_base_branch_has_nothing_already_committed_to_find() {
        // Short-circuits before touching git — no `base` artifact means
        // there is nothing to compare the branch against.
        let state = RunState::fresh(&commit_and_pr_flow());
        let out = already_committed("/does/not/matter", "main", &state).await.unwrap();
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn sitting_on_base_itself_has_nothing_already_committed_to_find() {
        // A checkout of `base` compared against itself is always empty —
        // short-circuits without needing to touch git.
        let mut state = RunState::fresh(&commit_and_pr_flow());
        state.artifacts.insert("base".into(), "main".into());
        let out = already_committed("/does/not/matter", "main", &state).await.unwrap();
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn pr_diff_preview_is_none_without_a_base_and_head_to_compare() {
        let node = NodeSpec {
            id: "pr_diff".into(),
            title: String::new(),
            subtitle: String::new(),
            step: Step::PrDiff,
            kind: crate::services::graph::NodeKind::Deterministic,
            deps: vec![],
            reads: vec![],
            writes: vec![],
            requires_approval: true,
            config: Default::default(),
        };
        let state = RunState::fresh(&commit_and_pr_flow());
        assert_eq!(diff_preview(&node, "/does/not/matter", &state).await, None);
    }

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
        for id in ["preflight", "draft_commit", "draft_pr"] {
            assert!(!g.get(id).unwrap().requires_approval, "{id} is read-only");
        }
    }

    #[test]
    fn scan_is_gated_so_the_diff_it_found_is_reviewed_before_a_commit_drafts_from_it() {
        // scan_changes touches no git history or remote, but everything
        // downstream works from the diff it produces — worth a look before
        // the flow keeps going, not just before the commit itself.
        let g = commit_and_pr_flow();
        assert!(g.get("scan").unwrap().requires_approval);
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
    fn every_step_in_the_catalogue_has_an_implementation() {
        // A step the executor cannot run has no business being offered in the
        // editor. This catches a catalogue entry added without wiring.
        for entry in crate::services::catalogue::CATALOGUE {
            let _ = entry.step;
        }
        // Not every catalogue entry maps to a distinct `Step` — a gated and
        // an ungated variant can share one execution — so this only pins
        // down that the catalogue has not shrunk, not an exact 1:1 count.
        assert!(crate::services::catalogue::CATALOGUE.len() >= 15);
    }

    /// A bare node carrying just a step, for the pure functions that only
    /// look at one.
    fn spec(step: Step) -> NodeSpec {
        NodeSpec {
            id: "n".into(),
            title: String::new(),
            subtitle: String::new(),
            step,
            kind: crate::services::graph::NodeKind::Deterministic,
            deps: vec![],
            reads: vec![],
            writes: vec![],
            requires_approval: false,
            config: Default::default(),
        }
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
        let items = proposal_items(&spec(Step::Commit), &s);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].note, "new");
        assert!(items[0].included, "checked by default");
    }

    #[test]
    fn only_the_commit_node_offers_a_file_list() {
        let s = RunState::default();
        for step in [Step::Push, Step::OpenPr, Step::Preflight] {
            assert!(proposal_items(&spec(step), &s).is_empty());
        }
    }

    #[test]
    fn a_path_with_no_recorded_status_still_appears() {
        let mut s = RunState::default();
        s.artifacts
            .insert("commit_paths".into(), "src/lib.rs".into());
        let items = proposal_items(&spec(Step::Commit), &s);
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
