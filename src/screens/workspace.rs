//! The workspace screen: repositories on the left, the selected flow in the
//! middle, the selected node on the right.
//!
//! Run state is keyed by `(repository, flow, pull request)`. That is what
//! lets you leave one repository parked at an approval, look at another, come
//! back and find it where it was; what lets a repository hold a half-finished
//! commit flow and a review flow at the same time without them treading on
//! each other; and what lets two different pull requests on the same
//! repository each sit mid-review independently, rather than the second
//! overwriting the first's progress. The PR slot is empty for anything that
//! isn't PR-scoped review.

use dioxus::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::components::branches_panel::BranchesPanel;
use crate::components::detail_pane::DetailPane;
use crate::components::forge_icon::ForgeIcon;
use crate::components::licence_panel::LicencePanel;
use crate::components::node_card::NodeCard;
use crate::components::pr_card::PrCard;
use crate::components::repo_sidebar::{phase_of, Phase, RepoEntry, RepoSidebar};
use crate::components::run_view::{Gates, RunView};
use crate::components::settings_panel::SettingsPanel;
use crate::screens::setup::Setup;
use crate::services::flow;
use crate::services::flowdef::{self, FlowBook};
use crate::services::graph::{Graph, NodeKind, NodeRun, NodeStatus, Remedy, RunState, Step};
use crate::services::licence;
use crate::services::llm::LlmConfig;
use crate::services::notify;
use crate::services::probe::{self, Need, RepoStatus, Wants};
use crate::services::store::Layout;
use crate::services::trusted;
use crate::services::{forge, git, store};
use crate::telemetry;

/// One run per repository, per flow, per pull request — the third slot is
/// empty for anything that isn't PR-scoped review. That is what lets you
/// review PR #7 and PR #5 on the same repository at once without one
/// overwriting the other's progress, the same way two different repositories
/// already don't tread on each other.
type Key = (String, String, String);
type States = BTreeMap<Key, RunState>;

/// The run already going in `repo`, other than `key` itself. A repository has
/// one working tree, so it gets one run at a time: a commit switching to a new
/// branch while a merge pulls the base underneath it leaves each one acting on
/// a tree the other has just changed.
fn other_run_in<'a>(running: &'a BTreeSet<Key>, repo: &str, key: &Key) -> Option<&'a Key> {
    running.iter().find(|k| k.0 == repo && *k != key)
}

/// Why a start is refused while `other` runs, naming it so you know which tab
/// to go and finish.
/// Why a start is refused while the Branches panel is working in the same
/// repository.
fn branch_busy_note(branch: &str) -> String {
    format!(
        "The Branches panel is still working on {branch} in this repository \u{2014} wait for it to finish."
    )
}

fn busy_note(book: &FlowBook, other: &Key) -> String {
    let (_, flow, pr) = other;
    let label = book
        .get(flow)
        .map(|f| f.label.clone())
        .unwrap_or_else(|| flow.clone());
    let what = if pr.is_empty() {
        format!("\u{201c}{label}\u{201d}")
    } else {
        format!("\u{201c}{label}\u{201d} for pull request #{pr}")
    };
    format!("{what} is already running in this repository \u{2014} finish or cancel it first.")
}

#[derive(Props, Clone, PartialEq)]
pub struct WorkspaceProps {
    pub workspace: String,
    pub llm_config: Signal<LlmConfig>,
    pub is_light: Signal<bool>,
    pub theme_overridden: Signal<bool>,
    pub on_change_workspace: EventHandler<()>,
}

/// The base branch to use for the Branches panel — the per-repo override if
/// one is set, else whatever auto-detection finds, same fallback preflight
/// itself uses.
async fn resolved_base(repo: &str, override_base: Option<String>) -> String {
    probe::base_branch(repo, override_base).await.0
}

/// Raises an OS notification when a run stops for a person and the window is
/// not in front of them.
///
/// The repository is named by its folder rather than its full path — a
/// notification is two short lines, and `/Users/…/code/ais-runner` spends all
/// of them saying nothing.
fn announce(status: NodeStatus, repo_path: &str, node_title: &str, detail: &str) {
    if !notify::should_notify(status, notify::window_focused()) {
        return;
    }
    let repo = std::path::Path::new(repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| repo_path.to_string());

    let (summary, body) = notify::message(status, &repo, node_title, detail);
    notify::raise(summary, body);
}

fn snapshot(states: &Signal<States>, key: &Key) -> RunState {
    states.read().get(key).cloned().unwrap_or_default()
}

/// Where to land when a repository is picked: the first step, in the first
/// runnable flow, that is waiting on the person looking at the screen — or,
/// if nothing is, the first runnable flow's first step.
fn default_selection(
    book: &FlowBook,
    states: &States,
    repo: &str,
    wants: Option<Wants>,
    open_prs: &[probe::PrBrief],
    hidden: &[String],
) -> (String, String, String) {
    // Landing on a flow hidden here would open a tab the strip does not show.
    let runnable = book.runnable_for(hidden);

    // A person being waited on outranks everything else, same precedence as
    // the sidebar's dot — check every flow for one before falling back.
    // Same order the sidebar's dot uses, and it must include Failed: the
    // status is now read across every flow, so a failure in one the user is
    // not looking at would otherwise report "failed" with no way to reach it.
    for status in [
        NodeStatus::AwaitingApproval,
        NodeStatus::Failed,
        NodeStatus::Running,
    ] {
        for flow in &runnable {
            // Any PR-scoped run for this repo+flow, not just a no-PR one — a
            // review under a specific PR must not be missed just because it
            // isn't the "current branch" slot.
            let matching_run = states
                .iter()
                .filter(|((r, f, _), _)| r == repo && f == &flow.id)
                .find_map(|((_, _, pr), run)| {
                    flowdef::topological_order(flow)
                        .into_iter()
                        .find(|id| run.status(id) == status)
                        .map(|node_id| (node_id, pr.clone()))
                });
            if let Some((node_id, pr_id)) = matching_run {
                return (flow.id.clone(), node_id, pr_id);
            }
        }
    }
    // Nothing in flight, so open on the flow that handles whatever the
    // repository actually needs — landing on "Commit → PR" for a repository
    // whose only outstanding work is a release is how the first task to do
    // ends up hidden.
    let need = wants.and_then(|w| w.need());
    let hinted = need.and_then(|need| runnable.iter().find(|f| f.answers(need)));

    // For a review, pick the pull request too. "Which one?" is a question the
    // app can already answer, and leaving the slot empty means arriving at a
    // review flow with nothing selected to review.
    let pr_id = match need {
        Some(Need::OpenPullRequest) => open_prs.first().map(|pr| pr.number.clone()),
        _ => None,
    }
    .unwrap_or_default();

    hinted
        .or_else(|| runnable.first())
        .map(|f| (f.id.clone(), f.first_node(), pr_id))
        .unwrap_or_default()
}

/// Woken whenever an approval decision is recorded, or a run's trusted flag
/// changes — the two things a node parked in `AwaitingApproval` is waiting to
/// hear about.
///
/// Before this, the wait was a 120 ms poll: a run left on an approval
/// overnight woke eight times a second, taking a borrow of every run's state
/// each time, and never went idle. One notifier for the whole process rather
/// than one per run, because a wake only costs a re-read of state the loop
/// looks at anyway — routing them precisely would be more bookkeeping than
/// the spurious wakes are worth.
fn approvals() -> &'static tokio::sync::Notify {
    static APPROVALS: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    APPROVALS.get_or_init(tokio::sync::Notify::new)
}

/// How many flows one trusted run will chain through before stopping.
///
/// Commit, review, release is three; the cap is not a limit anyone should
/// reach, it is there so a pair of flows that keep handing work to each other
/// cannot spin forever without a person noticing.
const MOST_FLOWS_IN_A_TRUSTED_RUN: usize = 6;

/// Walks one flow, for one repository, to completion.
///
/// Returns `false` when the run was cancelled under it — or cancelled and
/// started afresh — so the caller knows the run, and the `running` entry,
/// belong to someone else now.
///
/// One node at a time, in dependency order. The graph already permits running
/// the whole ready set together — `next_ready` returns the first of a set, not
/// the next link in a chain — so making this concurrent is a change to this
/// function alone. Across repositories it already is concurrent: each call gets
/// its own task and writes only its own key.
#[allow(clippy::too_many_arguments)]
async fn drive(
    graph: Graph,
    key: Key,
    cfg: Signal<LlmConfig>,
    mut states: Signal<States>,
    mut selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    mut trusted: Signal<BTreeSet<Key>>,
) -> bool {
    let repo = key.0.clone();
    // Cancel run drops the run's state; a new Start puts a fresh one in its
    // place. Either way the run this call was driving is gone, and carrying
    // on would run its steps — model calls included — for nobody, or worse,
    // into the new run.
    let run = snapshot(&states, &key).run;
    let gone = {
        let key = key.clone();
        move || states.read().get(&key).map(|s| s.run) != Some(run)
    };

    loop {
        if gone() {
            return false;
        }
        let state = snapshot(&states, &key);
        let Some(node) = state.next_ready(&graph) else {
            break;
        };

        // Only steer the selection when this run is the one on screen;
        // otherwise a background run would yank the view around.
        let viewing = selected_repo.read().as_deref() == Some(repo.as_str())
            && *selected_flow.read() == key.1
            && *selected_pr.read() == key.2;
        if viewing {
            selected_node.set(node.id.clone());
        }

        if node.requires_approval && !flow::nothing_to_approve(&node, &state.resolved_for(&node)) {
            // The approval describes what will run, so it has to read the same
            // resolved state the step will — otherwise the proposal quotes one
            // node's `commit_subject` and the commit uses another's.
            let state = state.resolved_for(&node);
            let proposal = flow::proposal(&node, &state);
            let items = flow::proposal_items(&node, &state);
            let preview_diff = flow::diff_preview(&node, &repo, &state)
                .await
                .unwrap_or_default();
            {
                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                let run = entry.runs.entry(node.id.clone()).or_default();
                run.proposal = proposal;
                run.items = items;
                run.preview_diff = preview_diff;
                entry.set_status(&node.id, NodeStatus::AwaitingApproval);
            }

            // The run has stopped and cannot continue without a person. If they
            // are not looking at the window, say so.
            announce(NodeStatus::AwaitingApproval, &key.0, &node.title, "");

            // A trusted run answers the approval by writing the same decision
            // the button writes, after a beat long enough to read the proposal
            // it is agreeing to. Going through `decisions` rather than short-
            // circuiting the wait is deliberate: there is exactly one path an
            // approval can take, so what you watch happen on screen is what
            // actually happened.
            let mut auto: Option<trusted::Verdict> = None;
            let approved = loop {
                // Registered before the state is read, not after. `enable`
                // puts this future in the waiter list now rather than on
                // first poll, so a decision written in the window between
                // the read below and the park at the bottom still wakes it
                // — the lost-wakeup race every condition-variable wait has.
                let waiter = approvals().notified();
                tokio::pin!(waiter);
                waiter.as_mut().enable();

                // `None` breaks out as "bypassed": somebody pressed Skip
                // while this sat here, so there is no decision coming and
                // nothing to run. Reading the status rather than inventing a
                // third decision value is what makes Skip behave identically
                // whether the run is still waiting here or has already
                // finished and failed.
                let (bypassed, decision) = {
                    let snapshot = states.read();
                    let entry = snapshot.get(&key);
                    (
                        entry.map(|s| s.status(&node.id)) == Some(NodeStatus::Bypassed),
                        entry.and_then(|s| s.decisions.get(&node.id).copied()),
                    )
                };
                if bypassed {
                    break None;
                }
                if gone() {
                    return false;
                }
                if let Some(decision) = decision {
                    break Some(decision);
                }

                if auto.is_none() && trusted.read().contains(&key) {
                    let verdict = trusted::decide(&node, &state);
                    if let Some(why) = verdict.reason() {
                        // It stopped, so it is no longer trusting: the button
                        // goes back to offering it, and the rest of the run is
                        // the person's again. Resuming on their approval and
                        // carrying on clicking would be the one behaviour that
                        // makes "it stopped for you" untrue.
                        states
                            .write()
                            .entry(key.clone())
                            .or_default()
                            .runs
                            .entry(node.id.clone())
                            .or_default()
                            .held = why.to_string();
                        trusted.write().remove(&key);
                        // No second notification: becoming `AwaitingApproval`
                        // already sent one, and the reason is on the node.
                    }
                    auto = Some(verdict);
                }

                if auto == Some(trusted::Verdict::Approve) {
                    // Long enough to see the node light up and read what it is
                    // about to do, short enough that a whole flow still feels
                    // like one action.
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    // Still trusted, and still nobody else's decision: a person
                    // who hit Stop during that pause gets their run back.
                    if !trusted.read().contains(&key) {
                        auto = None;
                        continue;
                    }
                    let mut w = states.write();
                    let entry = w.entry(key.clone()).or_default();
                    entry.decisions.entry(node.id.clone()).or_insert(true);
                    continue;
                }

                // Nothing left but to wait for a person. The timeout is a
                // safety net rather than the mechanism: if a wake is ever
                // missed the run resumes late instead of never.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), waiter).await;
            };

            let Some(approved) = approved else {
                // `bypass` already set the status and freed whatever was
                // blocked behind it, so there is nothing to do here but move
                // on to the next ready node.
                continue;
            };
            telemetry::record(telemetry::Event::ApprovalDecided {
                approved,
                trusted: auto == Some(trusted::Verdict::Approve),
            });
            if !approved {
                states
                    .write()
                    .entry(key.clone())
                    .or_default()
                    .reject(&node.id, &graph);
                continue;
            }
        }

        states
            .write()
            .entry(key.clone())
            .or_default()
            .set_status(&node.id, NodeStatus::Running);

        // A model step on an install with no model never calls one: it writes
        // what can be derived from the diff and is marked skipped. Done here,
        // once, rather than in each of the three model steps — and before the
        // approval, so what you approve is what will actually be used.
        if node.kind == NodeKind::Model && !cfg.read().uses_model() {
            let stand_in = flow::without_model(&node, &state);
            let mut w = states.write();
            let entry = w.entry(key.clone()).or_default();
            for (k, v) in stand_in.artifacts {
                entry
                    .artifacts
                    .insert(crate::services::graph::qualified(&node.id, &k), v.clone());
                entry.artifacts.insert(k, v);
            }
            entry.set_status(&node.id, NodeStatus::Bypassed);
            let run = entry.runs.entry(node.id.clone()).or_default();
            run.summary = stand_in.summary;
            run.log = stand_in.log;
            continue;
        }

        // Focused reviews with no lens turned on, likewise.
        if node.step == Step::Lenses && !cfg.read().lenses.any() {
            let mut w = states.write();
            let entry = w.entry(key.clone()).or_default();
            entry.set_status(&node.id, NodeStatus::Bypassed);
            let run = entry.runs.entry(node.id.clone()).or_default();
            run.summary = "skipped — no focused review turned on".into();
            run.log = "No focused review is turned on in Settings, so none ran. Turn on \
                       alignment, security or architecture under Model provider \u{2192} \
                       Focused reviews to have every pull request checked for it."
                .into();
            continue;
        }

        // A second opinion with no second model chosen is skipped the same
        // way, and for the same reason it must not fail: the merge waits on
        // it, and an optional reviewer nobody set up must not stop a merge.
        if node.step == Step::SecondOpinion && !cfg.read().second_config().uses_model() {
            let mut w = states.write();
            let entry = w.entry(key.clone()).or_default();
            entry.set_status(&node.id, NodeStatus::Bypassed);
            let run = entry.runs.entry(node.id.clone()).or_default();
            run.summary = "skipped — no second reviewer chosen".into();
            run.log = "No second reviewer is chosen in Settings, so nothing gave a second \
                       opinion. Pick one under Model provider \u{2192} Second reviewer to have \
                       every pull request reviewed twice."
                .into();
            continue;
        }

        // As this node sees it: bound inputs already resolved to the
        // producer each one names, so the step reads its own literal keys.
        let state = snapshot(&states, &key).resolved_for(&node);
        let cfg_snapshot = cfg.read().clone();

        // Fills in the node's log as its command's output arrives, rather
        // than only once the whole thing finishes — the point of streaming
        // at all. `result`'s own `outcome.log`/`failure.message` still wins
        // once the step settles, so formatting (a placeholder for empty
        // output, the failing command echoed back) stays exactly as before.
        // The live log is a preview, and only a preview: whatever the step
        // finally returns replaces `run.log` wholesale below, on both the
        // success and the failure path. That is what makes coalescing the
        // writes and capping the growth here free at the end and worth a lot
        // in the middle — one signal write per line meant one full Dioxus
        // render per line, each cloning the entire run map and re-running
        // syntect over the diff. `cargo test` on a real project emits
        // thousands of lines, so the cost was quadratic in output length.
        const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(100);
        /// Enough to watch a command work. The whole output still arrives
        /// when the step settles.
        const LIVE_LOG_CAP: usize = 200_000;

        let mut pending = String::new();
        // Starts "due", so a step's first line shows at once. A model step
        // says what it is waiting on and then nothing for a while; held back
        // until a second line, that one line is the one you never saw.
        let mut last_flush = std::time::Instant::now()
            .checked_sub(FLUSH_EVERY)
            .unwrap_or_else(std::time::Instant::now);
        let mut push_line = {
            let mut states = states;
            let key = key.clone();
            let node_id = node.id.clone();
            move |line: &str| {
                if !pending.is_empty() {
                    pending.push('\n');
                }
                pending.push_str(line);
                if last_flush.elapsed() < FLUSH_EVERY {
                    return;
                }
                last_flush = std::time::Instant::now();

                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                let run = entry.runs.entry(node_id.clone()).or_default();
                if !run.log.is_empty() {
                    run.log.push('\n');
                }
                run.log.push_str(&pending);
                pending.clear();

                // Keep the tail: the end of a log is the part worth watching,
                // and an unbounded one is a step away from a looping script
                // eating the heap.
                if run.log.len() > LIVE_LOG_CAP {
                    let mut cut = run.log.len() - LIVE_LOG_CAP;
                    while cut < run.log.len() && !run.log.is_char_boundary(cut) {
                        cut += 1;
                    }
                    run.log.drain(..cut);
                }
            }
        };
        // Racing the step against a skip, rather than only awaiting it, is what
        // makes Skip work on a step that is *already running* — which is the
        // case that matters, because a model call that will take fifteen
        // minutes is exactly the one you want to abandon. Dropping the future
        // cancels it: an in-flight request is dropped, and `git` children are
        // spawned `kill_on_drop`.
        let result = {
            let step = flow::execute(&node, &repo, &cfg_snapshot, &state, &mut push_line);
            tokio::pin!(step);
            loop {
                tokio::select! {
                    // Biased so a step that has finished is never discarded in
                    // favour of a skip that arrived in the same breath.
                    biased;
                    settled = &mut step => break Some(settled),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                        let bypassed = states
                            .read()
                            .get(&key)
                            .map(|s| s.status(&node.id))
                            == Some(NodeStatus::Bypassed);
                        if bypassed || gone() {
                            break None;
                        }
                    }
                }
            }
        };
        if gone() {
            return false;
        }
        let Some(result) = result else {
            // Skipped mid-flight. `bypass` has already set the status and freed
            // whatever was blocked behind it, and the log it wrote so far is
            // kept as the account of how far it got.
            continue;
        };

        let mut w = states.write();
        let entry = w.entry(key.clone()).or_default();
        match result {
            Ok(outcome) => {
                let nothing = outcome.nothing_to_do;
                for (k, v) in outcome.artifacts {
                    entry.artifacts.insert(k, v);
                }
                entry.set_status(
                    &node.id,
                    if nothing {
                        NodeStatus::Skipped
                    } else {
                        NodeStatus::Done
                    },
                );
                {
                    let run = entry.runs.entry(node.id.clone()).or_default();
                    run.summary = outcome.summary;
                    run.log = outcome.log;
                    // A step may offer files for deselection (scan does), and
                    // a re-run must not silently restore ones already dropped.
                    if !outcome.items.is_empty() {
                        run.items = outcome.items;
                    }
                }
                // Nothing to do is still nothing downstream can build on.
                if nothing {
                    entry.propagate_block(&graph);
                }
            }
            Err(failure) => {
                announce(NodeStatus::Failed, &key.0, &node.title, &failure.message);
                entry.set_status(&node.id, NodeStatus::Failed);
                {
                    let run = entry.runs.entry(node.id.clone()).or_default();
                    run.summary = "failed".into();
                    run.log = failure.message;
                    run.remedies = failure.remedies;
                }
                // A failure stays local: only what depends on this node is blocked.
                entry.propagate_block(&graph);
            }
        }
    }

    report_finished(&graph, &key, cfg, &states).await;
    true
}

/// Tells the usage statistics how this drive ended, if it ended — a run that
/// stopped with work still pending (nothing ready, nothing finished) has not
/// produced an outcome yet.
///
/// Gated on `telemetry::active()` before doing anything, including the git
/// call that names the forge: with statistics off this costs nothing.
async fn report_finished(
    graph: &Graph,
    key: &Key,
    cfg: Signal<LlmConfig>,
    states: &Signal<States>,
) {
    if !telemetry::active() {
        return;
    }
    let state = snapshot(states, key);
    if !state.is_finished(graph) {
        return;
    }
    let statuses: Vec<NodeStatus> = graph.nodes.iter().map(|n| state.status(&n.id)).collect();
    let forge = match git::remote_url(&key.0).await {
        Some(url) => forge::detect(&url),
        None => forge::Forge::None,
    };
    let provider = cfg.read().kind;
    telemetry::record(telemetry::Event::flow_finished(
        &key.1,
        telemetry::outcome_of(&statuses),
        &forge,
        provider,
    ));
    // A finished run is the moment worth getting to the server promptly;
    // detached so the run never waits on the network.
    tokio::spawn(telemetry::flush());
}

fn set_remedy(
    mut states: Signal<States>,
    key: &Key,
    node: &str,
    index: usize,
    edit: impl FnOnce(&mut Remedy),
) {
    let mut w = states.write();
    let entry = w.entry(key.clone()).or_default();
    if let Some(run) = entry.runs.get_mut(node) {
        if let Some(remedy) = run.remedies.get_mut(index) {
            edit(remedy);
        }
    }
}

/// Re-queues a settled node and restarts the executor if it had stopped. Work
/// already done upstream is kept — this resumes, it does not start over.
#[allow(clippy::too_many_arguments)]
fn retry_node(
    mut states: Signal<States>,
    mut running: Signal<BTreeSet<Key>>,
    selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    cfg: Signal<LlmConfig>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    graph: Graph,
    key: Key,
    node: &str,
    trusted: Signal<BTreeSet<Key>>,
) {
    states
        .write()
        .entry(key.clone())
        .or_default()
        .retry_from(node, &graph);

    if running.read().contains(&key) {
        return;
    }
    running.write().insert(key.clone());
    spawn(async move {
        if !drive(
            graph,
            key.clone(),
            cfg,
            states,
            selected_node,
            selected_repo,
            selected_flow,
            selected_pr,
            trusted,
        )
        .await
        {
            return;
        }
        running.write().remove(&key);
        reprobe(key.0.clone(), statuses);
    });
}

/// Marks a node bypassed and lets the run carry on without it.
///
/// Shares its shape with `retry_node` because it answers the same shape of
/// question — a settled node and what to do about it — and differs in one
/// place: the node ends `Bypassed` rather than back in the queue.
///
/// Both entry points land here. A node still sitting at its approval has a
/// driver waiting on it, which sees the new status and moves on, so the
/// `running` guard below is what stops a second one being started. A node that
/// failed has no driver left, so this starts one.
#[allow(clippy::too_many_arguments)]
fn skip_node(
    mut states: Signal<States>,
    mut running: Signal<BTreeSet<Key>>,
    selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    cfg: Signal<LlmConfig>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    graph: Graph,
    key: Key,
    node: &str,
    trusted: Signal<BTreeSet<Key>>,
) {
    states
        .write()
        .entry(key.clone())
        .or_default()
        .bypass(node, &graph);
    // Wake the waiting driver the same way an approval does, so a skip takes
    // effect immediately rather than on the next timeout tick.
    approvals().notify_waiters();

    if running.read().contains(&key) {
        return;
    }
    running.write().insert(key.clone());
    spawn(async move {
        if !drive(
            graph,
            key.clone(),
            cfg,
            states,
            selected_node,
            selected_repo,
            selected_flow,
            selected_pr,
            trusted,
        )
        .await
        {
            return;
        }
        running.write().remove(&key);
        reprobe(key.0.clone(), statuses);
    });
}

/// Re-reads one repository. Called after a run settles, so the Start button
/// reflects what the run just did — committing empties the tree, opening a pull
/// request gives the review flow something to work on.
fn reprobe(path: String, mut statuses: Signal<BTreeMap<String, RepoStatus>>) {
    spawn(async move {
        let status = probe::probe(&path).await;
        statuses.write().insert(path, status);
    });
}

/// Re-reads the workspace folder, re-probes every repository in it
/// concurrently, and opens on whichever one wants a person if nothing has been
/// chosen yet.
#[allow(clippy::too_many_arguments)]
fn refresh_all(
    workspace: &str,
    mut repos: Signal<Vec<store::Repo>>,
    mut statuses: Signal<BTreeMap<String, RepoStatus>>,
    mut probing: Signal<usize>,
    mut picked: Signal<bool>,
    mut selected_repo: Signal<Option<String>>,
    mut selected_flow: Signal<String>,
    book: Signal<FlowBook>,
    mut free_slots: Signal<licence::Slots>,
) {
    if *probing.read() > 0 {
        return;
    }
    // The folder, not the list read when the workspace opened: a repository
    // cloned since then belongs in it, and one deleted since does not.
    let list = store::discover_repos(workspace);
    if list != *repos.read() {
        let kept: std::collections::HashSet<&str> = list.iter().map(|r| r.path.as_str()).collect();
        statuses
            .write()
            .retain(|path, _| kept.contains(path.as_str()));
        let gone = selected_repo
            .read()
            .as_deref()
            .is_some_and(|path| !kept.contains(path));
        if gone {
            selected_repo.set(None);
        }
        repos.set(list.clone());
    }

    // Without Pro, only the free version's repositories are checked; the rest
    // are listed locked. The slots are re-read here, not kept per window,
    // because every window shares them.
    let licence_now = licence::current();
    let paths: Vec<String> = list.iter().map(|r| r.path.clone()).collect();
    let slots = licence::slots_for(&licence_now, &paths);
    let locked = |path: &str| licence::is_locked(&licence_now, &slots, path);
    statuses.write().retain(|path, _| !locked(path));
    if selected_repo.read().as_deref().is_some_and(locked) {
        selected_repo.set(None);
    }
    let list: Vec<store::Repo> = list.into_iter().filter(|r| !locked(&r.path)).collect();
    free_slots.set(slots.clone());

    if list.is_empty() {
        return;
    }
    probing.set(list.len());

    // One task for the whole sweep rather than one per repository, with the
    // concurrency bounded.
    //
    // Two things were wrong with a task each. `probe` runs several
    // subprocesses, two of them network calls to the forge, so a workspace of
    // forty repositories opened forty concurrent `gh` calls — enough to trip
    // GitHub's secondary rate limiter — alongside a few hundred process
    // spawns at once. And the "are we done" counter was decremented inside
    // each task, so a single task that never got there left `probing` above
    // zero and wedged every later refresh for the life of the window. There
    // is one completion point now, and it does not depend on arithmetic.
    const AT_ONCE: usize = 6;

    spawn(async move {
        use futures_util::stream::StreamExt;

        // Each result lands as it arrives, so the sidebar fills in rather
        // than appearing all at once at the end.
        futures_util::stream::iter(list.clone())
            .for_each_concurrent(AT_ONCE, |repo| {
                let mut statuses = statuses;
                async move {
                    let status = probe::probe(&repo.path).await;
                    statuses.write().insert(repo.path.clone(), status);
                }
            })
            .await;
        probing.set(0);

        if *picked.read() {
            return;
        }
        picked.set(true);
        let map = statuses.read().clone();
        let best = first_with_work(
            list.iter()
                .filter_map(|r| map.get(&r.path).map(|s| (r.path.clone(), s.wants()))),
        );

        if let Some((path, wants)) = best {
            // Open on whichever flow says it answers this, whatever its name.
            let hidden_here = store::load_repo_flows().hidden_for(&path).to_vec();
            let answering = wants.need().and_then(|need| {
                book.read()
                    .runnable_for(&hidden_here)
                    .iter()
                    .find(|f| f.answers(need))
                    .map(|f| f.id.clone())
            });
            if let Some(id) = answering {
                selected_flow.set(id);
            }
            selected_repo.set(Some(path));
        }
    });
}

/// The signals a step's panel acts on. Copy, like the signals themselves, so
/// one value can be handed to every place that shows a step.
#[derive(Clone, Copy)]
struct Wiring {
    states: Signal<States>,
    running: Signal<BTreeSet<Key>>,
    selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    llm_config: Signal<LlmConfig>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    trusted: Signal<BTreeSet<Key>>,
}

/// One step of one run, with everything you can do to it: approve or reject,
/// pick files, run a fix, retry, skip, cancel. The list view shows it in its
/// right-hand column and the run view in a panel over the map — the same
/// panel, built here once, so the two can never drift apart.
///
/// `drawer` is the run view's open panel, closed once the step is approved or
/// skipped: the run moves on, and so does your attention. Reject leaves it
/// open, since that is where you would retry. The list view passes `None`.
fn step_detail(
    wiring: Wiring,
    key: Key,
    graph: Graph,
    state: RunState,
    node_id: String,
    is_light: bool,
    drawer: Option<Signal<Option<String>>>,
) -> Element {
    let Wiring {
        mut states,
        mut running,
        selected_node,
        selected_repo,
        selected_flow,
        selected_pr,
        llm_config,
        statuses,
        trusted,
    } = wiring;
    rsx! {
        DetailPane {
            spec: graph.get(&node_id).cloned(),
            run: state.runs.get(&node_id).cloned().unwrap_or_else(NodeRun::default),
            diff: graph.get(&node_id).and_then(|spec| {
                spec.writes.iter()
                    .find(|w| w.as_str() == "diff" || w.as_str() == "pr_diff")
                    .and_then(|key| state.artifacts.get(key).cloned())
            }).or_else(|| {
                // `merge` writes no diff of its own, but the one
                // `pr_diff` already fetched earlier in this same
                // run is exactly the code a conflict — or a
                // decision to abandon — is about.
                if node_id == "merge" {
                    state.artifacts.get("pr_diff").cloned()
                } else {
                    None
                }
            }),
            is_light,
            run_started: state.started,
            on_approve: {
                let key = key.clone();
                move |id: String| {
                    states.write().entry(key.clone()).or_default()
                        .decisions.insert(id, true);
                    approvals().notify_waiters();
                    if let Some(mut drawer) = drawer {
                        drawer.set(None);
                    }
                }
            },
            on_reject: {
                let key = key.clone();
                move |id: String| {
                    states.write().entry(key.clone()).or_default()
                        .decisions.insert(id, false);
                    approvals().notify_waiters();
                }
            },
            on_toggle: {
                let key = key.clone();
                move |(node, item): (String, String)| {
                    let mut w = states.write();
                    let entry = w.entry(key.clone()).or_default();
                    if let Some(run) = entry.runs.get_mut(&node) {
                        if let Some(found) =
                            run.items.iter_mut().find(|i| i.key == item)
                        {
                            found.included = !found.included;
                        }
                    }
                }
            },
            on_remedy: {
                let key = key.clone();
                let retry_graph = graph.clone();
                move |(node, index): (String, usize)| {
                    let key = key.clone();
                    let retry_graph = retry_graph.clone();
                    let found = states.read().get(&key)
                        .and_then(|s| s.runs.get(&node))
                        .and_then(|r| r.remedies.get(index).cloned());
                    let Some(remedy) = found else { return };

                    set_remedy(states, &key, &node, index, |r| {
                        r.running = true;
                        r.output.clear();
                    });

                    spawn(async move {
                        // In the repo: `gh pr close 11` from anywhere
                        // else closes #11 of whichever repo that is.
                        let (ok, output) = git::run_streaming(
                            &remedy.program,
                            &remedy.args,
                            Some(&key.0),
                            "",
                            &mut |_| {},
                        )
                        .await;
                        set_remedy(states, &key, &node, index, |r| {
                            r.running = false;
                            r.done = ok;
                            r.output = if output.is_empty() && ok {
                                "done".into()
                            } else {
                                output.clone()
                            };
                        });
                        if ok && !remedy.sets.is_empty() {
                            let mut w = states.write();
                            let entry = w.entry(key.clone()).or_default();
                            for (k, v) in &remedy.sets {
                                entry.artifacts.insert(k.clone(), v.clone());
                            }
                        }
                        if ok && remedy.retry_after {
                            // A fix that unblocked this step is only
                            // useful if the run moves on, so re-queue it.
                            retry_node(
                                states, running, selected_node,
                                selected_repo, selected_flow, selected_pr,
                                llm_config, statuses, retry_graph,
                                key, &node, trusted,
                            );
                        } else if ok {
                            // A terminal remedy resolves the failure by
                            // abandoning the step, not by unblocking it —
                            // retrying would just fail again differently.
                            states.write().remove(&key);
                            running.write().remove(&key);
                            reprobe(key.0.clone(), statuses);
                        }
                    });
                }
            },
            on_retry: {
                let key = key.clone();
                let retry_graph = graph.clone();
                move |node: String| {
                    retry_node(
                        states, running, selected_node,
                        selected_repo, selected_flow, selected_pr,
                        llm_config, statuses, retry_graph.clone(),
                        key.clone(), &node, trusted,
                    );
                }
            },
            on_skip: {
                let key = key.clone();
                let skip_graph = graph.clone();
                move |node: String| {
                    skip_node(
                        states, running, selected_node,
                        selected_repo, selected_flow, selected_pr,
                        llm_config, statuses, skip_graph.clone(),
                        key.clone(), &node, trusted,
                    );
                    if let Some(mut drawer) = drawer {
                        drawer.set(None);
                    }
                }
            },
            on_cancel: {
                let key = key.clone();
                move |_| {
                    states.write().remove(&key);
                    running.write().remove(&key);
                    // A run parked at an approval
                    // hears about it now, not on its
                    // next timeout.
                    approvals().notify_waiters();
                    reprobe(key.0.clone(), statuses);
                }
            },
        }
    }
}

/// The repository to open on: the first in the list, top to bottom as the
/// sidebar shows it, that has something left to do. Not the most urgent one
/// further down — the list is the order you read in, and opening halfway down
/// it reads as a jump.
fn first_with_work(
    repos: impl IntoIterator<Item = (String, probe::Wants)>,
) -> Option<(String, probe::Wants)> {
    repos.into_iter().find(|(_, wants)| wants.needs_a_person())
}

#[component]
pub fn Workspace(props: WorkspaceProps) -> Element {
    let workspace = props.workspace.clone();
    let repos = use_signal(|| store::discover_repos(&workspace));
    let mut statuses = use_signal(BTreeMap::<String, RepoStatus>::new);
    let probing = use_signal(|| 0usize);
    // Auto-selection happens once, on the first probe: after that the choice is
    // yours and a refresh must not move it.
    let picked = use_signal(|| false);
    let mut states = use_signal(States::new);
    let mut selected_repo = use_signal(|| Option::<String>::None);
    // Loaded once per mount, so returning from Setup picks up any edits.
    let mut book = use_signal(FlowBook::load);
    // A runnable flow by preference — never open on a broken one while a
    // working one exists. But if every flow is broken, select the first anyway:
    // an empty column explains nothing, whereas the selected tab's banner says
    // exactly what to fix.
    //
    // Repo-blind on purpose, unlike every other flow choice in this file: no
    // repository is selected yet, so there is nothing for "hidden here" to be
    // relative to. Picking one replaces this via `default_selection`.
    let first_flow = {
        let book = book.read();
        book.runnable()
            .first()
            .or(book.flows.first().as_ref())
            .map(|f| f.id.clone())
            .unwrap_or_default()
    };
    let mut selected_flow = use_signal(|| first_flow);
    // Which open PR a review run is scoped to. Empty means "whatever the
    // checked-out branch has open" — the same default behaviour as before
    // this existed. Set by picking a specific PR from the sidebar's list.
    let mut selected_pr = use_signal(String::new);
    let mut selected_node = use_signal(String::new);
    let mut running = use_signal(BTreeSet::<Key>::new);
    // A fix being run for "couldn't list pull requests", by repository:
    // whether it is still running, and what it printed.
    let mut pr_fix = use_signal(BTreeMap::<String, (bool, String)>::new);
    // Runs whose approvals are being clicked through for us. A key is in here
    // only while that is wanted: taking it out mid-run hands the next approval
    // straight back to the person, without disturbing the run itself.
    let mut trusted = use_signal(BTreeSet::<Key>::new);
    // Every repository, every flow, no asking. Set from the "Trust all" button
    // in the top bar rather than per repository — for someone who wants
    // GitAgent to just run, not for the default. Turning it on also trusts
    // whatever is already running, the same way adopting a single run does;
    // turning it off does not untrust anything already in flight, so a run
    // that is mid-chain still finishes the leg it is on before the next
    // approval asks a person again.
    let mut global_trust = use_signal(|| false);
    // Why the last trusted run stopped, when the reason was not "there is
    // nothing left". Cleared when the next one starts.
    let mut chain_note = use_signal(String::new);
    // The run map instead of the step list and detail pane: one switch for
    // the whole window, so it stays on while you move between repositories.
    // On by default — where a run is is the first thing worth seeing; the
    // list is one click away for approving and reading logs.
    let mut run_view = use_signal(|| true);
    // The step whose panel is open over the run map, if any.
    let mut drawer = use_signal(|| Option::<String>::None);
    let mut settings_open = use_signal(|| false);
    let mut setup_open = use_signal(|| false);
    // GitAgent Pro. `licence_open` holds the repository a refused run was
    // for, when that is why the window opened.
    let licence_status = use_signal(licence::current);
    let mut free_slots = use_signal(licence::load_slots);
    let mut licence_open = use_signal(|| Option::<Option<String>>::None);
    // Which flows the *current* repository has chosen not to see — a filter
    // on top of the shared flow list, not a copy of it. `flows.toml` never
    // changes when a flow is hidden here.
    let mut repo_flows = use_signal(store::load_repo_flows);
    let mut confirm_hide = use_signal(|| Option::<(String, String)>::None);
    let mut branches_open = use_signal(|| Option::<String>::None);
    let mut branches_data =
        use_signal(|| Option::<Result<Vec<crate::services::branches::BranchInfo>, String>>::None);
    let mut repo_bases = use_signal(store::load_repo_bases);
    let mut branches_action_error = use_signal(|| Option::<String>::None);
    // Which repository and branch a delete, clean-up or create-PR is currently
    // running against, so the panel can show that row working instead of
    // looking like the click did nothing. It also holds the repository the
    // way a run does: no flow starts there until it is done, and the other
    // way round.
    let mut branches_busy = use_signal(|| Option::<(String, String)>::None);
    let mut base_editor_open = use_signal(|| Option::<String>::None);
    let mut base_editor_value = use_signal(String::new);
    // Which repository's flow picker is open, rather than a single flag for
    // all of them: opening it on one repository used to leave it open on the
    // next one you selected, which reads as a panel that will not close.
    let mut picker_open = use_signal(|| Option::<String>::None);

    // A step that stops to ask for approval opens its panel over the run map
    // on its own, once per approval: close it while the step still waits and
    // it stays closed. Not for a trusted run, which answers on its own a beat
    // later — the panel would only flash open. Not away from another step
    // that is waiting too, which would have two approvals fight for it.
    let mut auto_opened = use_signal(|| Option::<(u64, String, usize)>::None);
    use_effect(move || {
        if !*run_view.read() {
            return;
        }
        let Some(repo) = selected_repo.read().clone() else {
            return;
        };
        let key: Key = (
            repo,
            selected_flow.read().clone(),
            selected_pr.read().clone(),
        );
        let states = states.read();
        let Some(state) = states.get(&key) else {
            return;
        };
        let Some((step, at)) = state.newest_approval() else {
            return;
        };
        let this = (state.run, step.clone(), at);
        if auto_opened.peek().as_ref() == Some(&this) {
            return;
        }
        if *global_trust.read() || trusted.read().contains(&key) {
            return;
        }
        auto_opened.set(Some(this));
        let showing_another_approval = drawer.peek().as_ref().is_some_and(|open| {
            open != &step && state.status(open) == NodeStatus::AwaitingApproval
        });
        if !showing_another_approval {
            selected_node.set(step.clone());
            drawer.set(Some(step));
        }
    });

    // Escape closes a step's panel over the run map. Listened for on the
    // window, not the panel, because nothing in the panel has focus after
    // a click on the map. The handler is kept on `window` and replaced on
    // each mount, so a workspace opened twice never stacks two of them.
    use_future(move || async move {
        let mut keys = document::eval(
            "if (window._gaEscape) window.removeEventListener('keydown', window._gaEscape);\
             window._gaEscape = (e) => { if (e.key === 'Escape') dioxus.send(true); };\
             window.addEventListener('keydown', window._gaEscape);",
        );
        while keys.recv::<bool>().await.is_ok() {
            // A dialog in front owns the key: closing the panel hidden
            // behind it would be a change nobody could see happen.
            let dialog_open = *settings_open.peek()
                || licence_open.peek().is_some()
                || branches_open.peek().is_some()
                || base_editor_open.peek().is_some()
                || confirm_hide.peek().is_some()
                || picker_open.peek().is_some();
            if !dialog_open {
                drawer.set(None);
            }
        }
    });

    // Hiding a flow is one operation whether it comes from a tab's × or from
    // the picker's checkbox, including the part that is easy to forget: the
    // graph column must not keep showing a tab the strip no longer does.
    let mut hide_flow = move |repo: &str, id: &str| {
        repo_flows.write().hide(repo, id);
        store::save_repo_flows(&repo_flows.read());
        if selected_repo.read().as_deref() == Some(repo) && *selected_flow.read() == id {
            let next = {
                let still_hidden = repo_flows.read();
                book.read()
                    .runnable()
                    .iter()
                    .find(|f| !still_hidden.is_hidden(repo, &f.id))
                    .map(|f| f.id.clone())
                    .unwrap_or_default()
            };
            let first_node = book
                .read()
                .get(&next)
                .map(|f| f.first_node())
                .unwrap_or_default();
            selected_flow.set(next);
            selected_node.set(first_node);
        }
    };

    // Pane widths, dragged by the dividers and remembered on disk.
    let saved = use_signal(store::load_layout);
    let mut sidebar_w = use_signal(|| saved.read().sidebar);
    let mut middle_w = use_signal(|| saved.read().middle);
    // 0 = not dragging, 1 = the sidebar edge, 2 = the flow-column edge.
    let mut dragging = use_signal(|| 0u8);
    let mut drag_from = use_signal(|| (0.0f64, 0.0f64));

    let mut is_light = props.is_light;
    let mut theme_overridden = props.theme_overridden;

    // Probe every repository at once rather than one after another: eight
    // repositories each needing a `gh` round-trip is seconds sequentially and
    // barely one concurrently. Results land as they arrive, so the list fills
    // in rather than appearing all at once.
    let opened = workspace.clone();
    use_coroutine(move |_rx: UnboundedReceiver<()>| {
        let opened = opened.clone();
        async move {
            refresh_all(
                &opened,
                repos,
                statuses,
                probing,
                picked,
                selected_repo,
                selected_flow,
                book,
                free_slots,
            );
        }
    });

    let llm_config = props.llm_config;
    let wiring = Wiring {
        states,
        running,
        selected_node,
        selected_repo,
        selected_flow,
        selected_pr,
        llm_config,
        statuses,
        trusted,
    };
    let repo_list = repos.read().clone();
    let status_map = statuses.read().clone();
    let forge_map: BTreeMap<String, crate::services::forge::Forge> = status_map
        .iter()
        .map(|(path, s)| (path.clone(), s.forge.clone()))
        .collect();
    let states_snapshot = states.read().clone();
    let flows = book.read().clone();
    // Every flow, not just the runnable ones: a broken flow is shown, marked,
    // and refused, rather than quietly disappearing from the strip.
    let listed: Vec<(String, String, Vec<String>)> = flows
        .listed()
        .iter()
        .map(|(f, problems)| {
            (
                f.id.clone(),
                f.label.clone(),
                problems.iter().map(|p| p.message()).collect(),
            )
        })
        .collect();
    let flow_id = selected_flow.read().clone();
    let flow_problems: Vec<String> = listed
        .iter()
        .find(|(id, _, _)| id == &flow_id)
        .map(|(_, _, problems)| problems.clone())
        .unwrap_or_default();
    let current = flows.get(&flow_id).cloned();
    let graph: Graph = current
        .as_ref()
        .map(|f| f.to_graph())
        .unwrap_or(Graph { nodes: vec![] });
    let flow_first_node = current.as_ref().map(|f| f.first_node()).unwrap_or_default();

    let entries: Vec<RepoEntry> = repo_list
        .iter()
        .map(|repo| RepoEntry {
            path: repo.path.clone(),
            label: repo.label.clone(),
            wants: status_map.get(&repo.path).map(|s| s.wants()),
            branch: status_map
                .get(&repo.path)
                .map(|s| s.branch.clone())
                .unwrap_or_default(),
            detail: status_map
                .get(&repo.path)
                .map(|s| s.summary())
                .unwrap_or_default(),
            forge: forge_map.get(&repo.path).cloned(),
            // Several PR-scoped runs can be in flight for this repo+flow at
            // once — the sidebar shows whichever most needs a look, the same
            // way `phase_of` already picks the most urgent status within one.
            // Every flow, not just the one on screen: a review parked at an
            // approval must still show while the commit flow is selected.
            phase: states_snapshot
                .iter()
                .filter(|((r, _, _), _)| *r == repo.path)
                .map(|(_, s)| phase_of(s))
                .min_by_key(|p| p.priority())
                .unwrap_or(Phase::Idle),
            ahead: status_map.get(&repo.path).map(|s| s.ahead).unwrap_or(0),
            behind: status_map.get(&repo.path).map(|s| s.behind).unwrap_or(0),
            open_pr_count: status_map.get(&repo.path).map(|s| s.prs.len()).unwrap_or(0),
            prs_error: status_map.get(&repo.path).and_then(|s| s.prs_error.clone()),
            locked: licence::is_locked(&licence_status.read(), &free_slots.read(), &repo.path),
        })
        .collect();

    let active = selected_repo.read().clone();
    let cfg = props.llm_config.read().clone();

    let mut llm_config_mut = props.llm_config;
    let mut begin = move |trust: bool| {
        let Some(repo) = selected_repo.read().clone() else {
            return;
        };
        // The buttons are disabled while another run holds this repository,
        // but a click can still land between that run starting and the
        // re-render, so the rule is enforced here too. The disabled button's
        // tooltip already says which run is in the way.
        if running.read().iter().any(|k| k.0 == repo)
            || branches_busy
                .read()
                .as_ref()
                .is_some_and(|(r, _)| r == &repo)
        {
            return;
        }
        // The free version's five repositories: the first run in one takes a
        // slot; with none left, say so instead of starting.
        if !licence::may_run(&licence_status.read(), &repo) {
            telemetry::record(telemetry::Event::LicenceWallHit);
            licence_open.set(Some(Some(repo)));
            return;
        }
        free_slots.set(licence::load_slots());
        // "Trust all" overrides the button that was actually clicked — even a
        // plain Start runs trusted once it's on, since the whole point is not
        // having to remember which button to press per repository.
        let trust = trust || *global_trust.read();
        // Settings live in a per-window signal but one file on disk. Re-reading
        // here is what stops a second window running against a stale provider.
        llm_config_mut.set(store::load_settings());
        chain_note.set(String::new());

        // An ordinary Start runs the flow on screen. A trusted run is for the
        // repository, not for one flow, so it starts on whichever flow answers
        // what the repository actually needs — which is how "Trusted run" on
        // the Commit → PR tab does the release a repository is waiting for
        // instead of refusing because there is nothing to commit.
        //
        // Falling back to the flow on screen matters as much as the preference
        // does. `next_flow` answers "what does this repository most need",
        // which is `None` for a repository that needs nothing in particular —
        // and that used to disable the button on every tab at once, including
        // a "Deploy VPS" flow whose whole point is that you run it when you
        // decide to, not when a probe says so.
        let opening = if trust {
            statuses
                .read()
                .get(&repo)
                .and_then(|status| {
                    trusted::next_flow(&book.read(), status, &repo_flows.read().hidden_for(&repo))
                })
                .or_else(|| Some((selected_flow.read().clone(), selected_pr.read().clone())))
        } else {
            Some((selected_flow.read().clone(), selected_pr.read().clone()))
        };
        let Some((mut id, mut pr)) = opening else {
            // Nothing to start at all. Silently doing nothing is what made
            // this button feel broken, so say why.
            if trust {
                if let Some(status) = statuses.read().get(&repo) {
                    chain_note.set(
                        trusted::why_stopped(
                            &book.read(),
                            status,
                            &repo_flows.read().hidden_for(&repo),
                        )
                        .unwrap_or_default(),
                    );
                }
            }
            return;
        };

        spawn(async move {
            // Whether the run is *currently* trusted, which is not the same as
            // the button that started it. A run started with plain Start and
            // adopted part-way — "stop asking me" at an approval — has to chain
            // like any other trusted run from that point, so this is re-read
            // from the trusted set after every leg rather than captured once.
            let mut trusting = trust;

            // Each turn of this loop is one flow, start to finish. Only a
            // trusted run goes round twice.
            for _ in 0..MOST_FLOWS_IN_A_TRUSTED_RUN {
                let Some(def) = book.read().get(&id).cloned() else {
                    break;
                };
                let key: Key = (repo.clone(), id.clone(), pr.clone());
                // Repository-wide, not just this key: the next leg of a chain
                // must not start while something else holds the working tree.
                if running.read().iter().any(|k| k.0 == repo)
                    || branches_busy
                        .read()
                        .as_ref()
                        .is_some_and(|(r, _)| r == &repo)
                {
                    break;
                }

                let graph = def.to_graph();
                let mut fresh = RunState::fresh(&graph);
                fresh.started = true;
                if !pr.is_empty() {
                    // Read by `find_pr`, so this run reviews the PR that was
                    // actually picked rather than falling back to "whatever the
                    // checked-out branch has open".
                    fresh
                        .artifacts
                        .insert("selected_pr_number".into(), pr.clone());
                }
                states.write().insert(key.clone(), fresh);
                running.write().insert(key.clone());
                if trusting {
                    trusted.write().insert(key.clone());
                } else {
                    trusted.write().remove(&key);
                }

                // Follow the run on screen, so a chain that moves to another
                // flow does not leave you watching the tab it has left.
                if selected_repo.read().as_deref() == Some(repo.as_str()) {
                    selected_flow.set(id.clone());
                    selected_pr.set(pr.clone());
                    selected_node.set(def.first_node());
                }

                if !drive(
                    graph.clone(),
                    key.clone(),
                    llm_config,
                    states,
                    selected_node,
                    selected_repo,
                    selected_flow,
                    selected_pr,
                    trusted,
                )
                .await
                {
                    // Cancelled: the run is over, and nothing chains from it.
                    return;
                }
                running.write().remove(&key);

                // `drive` takes the key back out when it stopped at something
                // it would not approve, so this is also how a hold ends the
                // chain rather than only the leg it happened on. It is equally
                // how adoption gets picked up: the key is in the set because a
                // person put it there mid-flight.
                trusting = trusted.read().contains(&key);
                trusted.write().remove(&key);

                let status = probe::probe(&repo).await;
                let finished = snapshot(&states, &key);
                statuses.write().insert(repo.clone(), status.clone());

                if !trusting || !trusted::may_continue(&finished, &graph) {
                    break;
                }
                let hidden_here = repo_flows.read().hidden_for(&repo).to_vec();
                let Some(next) = trusted::next_flow(&book.read(), &status, &hidden_here) else {
                    // The commonest end of a chain, and until now the most
                    // silent: a release is due and the release flow never
                    // declared that it handles releases.
                    chain_note.set(
                        trusted::why_stopped(&book.read(), &status, &hidden_here)
                            .unwrap_or_default(),
                    );
                    break;
                };
                // The same flow again means the last one did not move the
                // repository on. Whatever is left needs a person, not another
                // identical lap.
                if next == (id.clone(), pr.clone()) {
                    break;
                }
                (id, pr) = next;
            }
        });
    };
    let start = move |_: Event<MouseData>| begin(false);
    let start_trusted = move |_: Event<MouseData>| begin(true);

    // The flow tabs — pick a flow, hide one, or choose which are shown — for
    // the selected repository. One strip, placed above the list view's steps
    // and under the run view's header alike.
    let flow_strip: Element = match active.clone() {
        None => rsx! {},
        Some(repo) => {
            let label = repo_list
                .iter()
                .find(|r| r.path == repo)
                .map(|r| r.label.clone())
                .unwrap_or_else(|| repo.clone());
            let hidden_here = repo_flows.read().hidden_for(&repo).to_vec();
            let visible_tabs: Vec<(String, String, Vec<String>)> = listed
                .iter()
                .filter(|(id, _, _)| !hidden_here.contains(id))
                .cloned()
                .collect();
            let showing_picker = picker_open.read().as_deref() == Some(repo.as_str());
            // Counted against the book rather than the stored list: an id
            // left behind by a flow deleted in Setup must not advertise
            // "1 hidden" with nothing to show. Flows made for other
            // repositories are offers, not something this one hid, so they
            // are counted apart.
            let elsewhere: Vec<String> = listed
                .iter()
                .filter(|(id, _, _)| repo_flows.read().elsewhere_only(&repo, id))
                .map(|(id, _, _)| id.clone())
                .collect();
            let hidden_count = listed
                .iter()
                .filter(|(id, _, _)| hidden_here.contains(id) && !elsewhere.contains(id))
                .count();
            let offered = elsewhere.len();
            rsx! {
                div { class: "flow-tabs",
                    for (id, label, problems) in visible_tabs.iter().cloned() {
                        div {
                            key: "{id}",
                            class: match (id == flow_id, problems.is_empty()) {
                                (true, true) => "flow-tab flow-tab-on",
                                (true, false) => "flow-tab flow-tab-on flow-tab-broken",
                                (false, true) => "flow-tab",
                                (false, false) => "flow-tab flow-tab-broken",
                            },
                            button {
                                class: "flow-tab-main",
                                // The tab still selects: seeing why a flow is
                                // broken is the point of showing it.
                                title: if problems.is_empty() {
                                    String::new()
                                } else {
                                    problems.join("\n")
                                },
                                onclick: {
                                    let id = id.clone();
                                    let first = flows.get(&id)
                                        .map(|f| f.first_node())
                                        .unwrap_or_default();
                                    // A tab is a flow, not one particular PR review
                                    // within it, so the previous tab's selection must
                                    // not carry over and silently scope the next
                                    // "Start" to it. Clearing it outright was the
                                    // over-correction: arriving at a review flow with
                                    // one open pull request and nothing selected makes
                                    // you click a list of one to say the only thing it
                                    // could have said.
                                    let obvious = status_map
                                        .get(&repo)
                                        .map(|s| s.default_pr())
                                        .unwrap_or_default();
                                    move |_| {
                                        selected_flow.set(id.clone());
                                        selected_node.set(first.clone());
                                        selected_pr.set(obvious.clone());
                                        // Its step belongs to the flow just left.
                                        drawer.set(None);
                                    }
                                },
                                if !problems.is_empty() {
                                    span { class: "flow-tab-warn", "\u{26a0}" }
                                }
                                "{label}"
                                if running.read().iter().any(|(r, f, _)| r == &repo && f == &id) {
                                    span { class: "flow-tab-dot" }
                                }
                            }
                            button {
                                class: "flow-tab-hide",
                                title: "Hide \"{label}\" for {repo_list.iter().find(|r| r.path == repo).map(|r| r.label.clone()).unwrap_or_else(|| repo.clone())}",
                                onclick: {
                                    let repo = repo.clone();
                                    let id = id.clone();
                                    move |e: Event<MouseData>| {
                                        e.stop_propagation();
                                        confirm_hide.set(Some((repo.clone(), id.clone())));
                                    }
                                },
                                "×"
                            }
                        }
                    }
                    // Always there, even with nothing hidden:
                    // the × only appears on hover, so this is
                    // how someone learns the strip is theirs
                    // to edit at all.
                    button {
                        class: if showing_picker {
                            "flow-tab-picker flow-tab-picker-on"
                        } else {
                            "flow-tab-picker"
                        },
                        title: "Choose which flows {label} shows",
                        onclick: {
                            let repo = repo.clone();
                            move |_| {
                                let open = picker_open.read().as_deref() == Some(repo.as_str());
                                picker_open.set(if open { None } else { Some(repo.clone()) });
                            }
                        },
                        if hidden_count > 0 && offered > 0 {
                            "{hidden_count} hidden · {offered} more"
                        } else if offered > 0 {
                            "{offered} more"
                        } else if hidden_count > 0 {
                            "{hidden_count} hidden"
                        } else {
                            "\u{22ef}"
                        }
                    }
                    if showing_picker {
                        // Clicking anywhere else closes it; a
                        // transparent backdrop is what makes
                        // "anywhere else" mean the whole window.
                        div {
                            class: "flow-picker-backdrop",
                            onclick: move |_| picker_open.set(None),
                        }
                        div {
                            class: "flow-picker",
                            onclick: move |e: Event<MouseData>| e.stop_propagation(),
                            div { class: "flow-picker-head", "Flows shown for {label}" }
                            // Every flow in the book, checked or
                            // not, so the choice is made against
                            // the full list rather than by
                            // remembering what was taken away.
                            for (id, flow_label, problems) in listed.iter().cloned() {
                                {
                                    let shown = !hidden_here.contains(&id);
                                    rsx! {
                                        label {
                                            key: "{id}",
                                            class: if shown { "flow-picker-row" } else { "flow-picker-row flow-picker-row-off" },
                                            title: if problems.is_empty() { String::new() } else { problems.join("\n") },
                                            input {
                                                r#type: "checkbox",
                                                checked: shown,
                                                onchange: {
                                                    let repo = repo.clone();
                                                    let id = id.clone();
                                                    move |_| {
                                                        if repo_flows.read().is_hidden(&repo, &id) {
                                                            repo_flows.write().show(&repo, &id);
                                                            store::save_repo_flows(&repo_flows.read());
                                                        } else {
                                                            hide_flow(&repo, &id);
                                                        }
                                                    }
                                                },
                                            }
                                            if !problems.is_empty() {
                                                span { class: "flow-tab-warn", "\u{26a0}" }
                                            }
                                            span { class: "flow-picker-label", "{flow_label}" }
                                            if elsewhere.contains(&id) {
                                                span {
                                                    class: "flow-picker-hint",
                                                    "only on other repositories — tick to add here"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            div { class: "flow-picker-note",
                                "Only this repository is affected. Flows themselves are edited in Setup."
                            }
                        }
                    }
                }
            }
        }
    };

    if *setup_open.read() {
        return rsx! {
            Setup {
                repo: selected_repo.read().clone(),
                on_close: move |_| {
                    // Pick up whatever Setup wrote, without disturbing any run.
                    book.set(FlowBook::load());
                    repo_flows.set(store::load_repo_flows());
                    setup_open.set(false);
                },
            }
        };
    }

    rsx! {
        div { class: "screen",
            div { class: "topbar",
                span { class: "topbar-brand", "GitAgent" }
                div { class: "topbar-title",
                    span { class: "topbar-path", "{props.workspace}" }
                }
                div { class: "topbar-right",
                    button {
                        class: if *global_trust.read() { "btn btn-trusted btn-trusted-on" } else { "btn btn-ghost" },
                        title: if *global_trust.read() {
                            "Every repository and every flow is running trusted. Click to go back to approving each one yourself."
                        } else {
                            "Trust every repository and every flow: runs answer their own approvals instead of asking, the same as clicking \u{201c}Trusted run\u{201d} everywhere at once."
                        },
                        onclick: move |_| {
                            let on = !*global_trust.read();
                            global_trust.set(on);
                            if on {
                                trusted.write().extend(running.read().iter().cloned());
                                approvals().notify_waiters();
                            }
                        },
                        if *global_trust.read() { "Trusting everything…" } else { "Trust all" }
                    }
                    div { class: "view-switch",
                        button {
                            class: if *run_view.read() { "view-switch-opt" } else { "view-switch-opt view-switch-on" },
                            title: "Each step as a card, with the selected one's detail beside it",
                            onclick: move |_| run_view.set(false),
                            "List"
                        }
                        button {
                            class: if *run_view.read() { "view-switch-opt view-switch-on" } else { "view-switch-opt" },
                            title: "The selected repository's flow as a line that fills in while it runs",
                            onclick: move |_| run_view.set(true),
                            "Run"
                        }
                    }
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| setup_open.set(true),
                        "Setup"
                    }
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| settings_open.set(true),
                        "{cfg.active_model()}"
                    }
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| {
                            theme_overridden.set(true);
                            let now = *is_light.read();
                            is_light.set(!now);
                        },
                        if *props.is_light.read() { "Dark" } else { "Light" }
                    }
                    // Last, so it sits in the corner: the one element in
                    // the bar that is about you rather than the work.
                    match licence_status.read().clone() {
                        licence::Status::Pro(l) => rsx! {
                            button {
                                class: "pro-badge",
                                title: "GitAgent Pro · licensed to {l.email} · updates until {l.updates_until}",
                                onclick: move |_| licence_open.set(Some(None)),
                                span { class: "pro-badge-star", "\u{2726}" }
                                span { class: "pro-badge-text", "PRO" }
                            }
                        },
                        licence::Status::Renew(l) => rsx! {
                            button {
                                class: "pro-badge pro-badge-renew",
                                title: "Your Pro updates ended on {l.updates_until}, before this version. Renew, or paste a new key.",
                                onclick: move |_| licence_open.set(Some(None)),
                                span { class: "pro-badge-star", "\u{2726}" }
                                span { class: "pro-badge-text", "RENEW PRO" }
                            }
                        },
                        licence::Status::Free => rsx! {
                            button {
                                class: "btn btn-ghost btn-pro",
                                title: "Buy GitAgent Pro, paste your licence key, or choose the free version's repositories",
                                onclick: move |_| licence_open.set(Some(None)),
                                "Get Pro\u{2026}"
                            }
                        },
                        licence::Status::Unavailable => rsx! {
                            button {
                                class: "btn btn-ghost",
                                title: "A build that cannot check licences, such as one built from source: nothing is limited.",
                                onclick: move |_| licence_open.set(Some(None)),
                                "Pro"
                            }
                        },
                    }
                }
            }

            div {
                class: if *dragging.read() > 0 { "body body-dragging" } else { "body" },
                onmousemove: move |e| {
                    let which = *dragging.read();
                    if which == 0 {
                        return;
                    }
                    let (start_x, start_w) = *drag_from.read();
                    let delta = e.client_coordinates().x - start_x;
                    if which == 1 {
                        sidebar_w.set(Layout::clamp_sidebar(start_w + delta));
                    } else {
                        middle_w.set(Layout::clamp_middle(start_w + delta));
                    }
                },
                onmouseup: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *middle_w.read(),
                        });
                    }
                },
                // A pointer that leaves the window mid-drag would otherwise
                // leave the divider stuck to the cursor.
                onmouseleave: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *middle_w.read(),
                        });
                    }
                },

                RepoSidebar {
                    entries,
                    selected: active.clone(),
                    workspace: props.workspace.clone(),
                    probing: *probing.read(),
                    on_refresh: {
                        let workspace = workspace.clone();
                        move |_| {
                            refresh_all(&workspace, repos, statuses, probing, picked, selected_repo, selected_flow, book, free_slots);
                        }
                    },
                    on_reprobe: move |path: String| {
                        if !licence::is_locked(&licence_status.read(), &free_slots.read(), &path) {
                            reprobe(path, statuses);
                        }
                    },
                    on_select: move |path: String| {
                        // A locked repository can't be selected, so nothing —
                        // no flow, no branch action — can act on it. With a
                        // slot free, selecting it takes the slot.
                        if !licence_status.read().unlimited() {
                            let mut slots = licence::load_slots();
                            if !slots.has(&path) {
                                if !slots.claim(&path) {
                                    free_slots.set(slots);
                                    licence_open.set(Some(Some(path)));
                                    return;
                                }
                                licence::save_slots(&slots);
                                free_slots.set(slots);
                                reprobe(path.clone(), statuses);
                            }
                        }
                        let (flow_id, node_id, pr_id) =
                            default_selection(
                                &book.read(),
                                &states.read(),
                                &path,
                                statuses.read().get(&path).map(|s| s.wants()),
                                statuses
                                    .read()
                                    .get(&path)
                                    .map(|s| s.prs.clone())
                                    .unwrap_or_default()
                                    .as_slice(),
                                &repo_flows.read().hidden_for(&path),
                            );
                        selected_repo.set(Some(path));
                        if !flow_id.is_empty() {
                            selected_flow.set(flow_id);
                        }
                        selected_node.set(node_id);
                        selected_pr.set(pr_id);
                        // A panel left open would show a step of the
                        // repository you just left.
                        drawer.set(None);
                    },
                    on_change_workspace: move |_| props.on_change_workspace.call(()),
                    width: *sidebar_w.read(),
                }

                div {
                    class: "divider",
                    onmousedown: move |e| {
                        drag_from.set((e.client_coordinates().x, *sidebar_w.read()));
                        dragging.set(1);
                    },
                }

                match active.clone() {
                    None => rsx! {
                        div { class: "placeholder",
                            div { class: "placeholder-title", "Pick a repository" }
                            div { class: "placeholder-sub",
                                "{repo_list.len()} found in this folder. Selecting one shows the \
                                 flow it will run."
                            }
                        }
                    },
                    Some(repo) if *run_view.read() => {
                        let key: Key = (repo.clone(), flow_id.clone(), selected_pr.read().clone());
                        let repo_label = repo_list.iter()
                            .find(|r| r.path == repo)
                            .map(|r| r.label.clone())
                            .unwrap_or_else(|| repo.clone());
                        let flow_label = current.as_ref().map(|f| f.label.clone()).unwrap_or_default();
                        // Whether Play may start the flow, by the same rules
                        // as the list view's Start button.
                        let run_state = states_snapshot.get(&key).cloned().unwrap_or_default();
                        let other_run = other_run_in(&running.read(), &repo, &key)
                            .map(|other| busy_note(&flows, other))
                            .or_else(|| {
                                branches_busy
                                    .read()
                                    .as_ref()
                                    .filter(|(r, _)| r == &repo)
                                    .map(|(_, branch)| branch_busy_note(branch))
                            });
                        let can_run = probe::affordance(
                            &flow_id,
                            status_map.get(&repo),
                            *probing.read() > 0,
                            run_state.started,
                            &key.2,
                            &flow_problems,
                        );
                        let can_start = !running.read().contains(&key)
                            && other_run.is_none()
                            && can_run.enabled;
                        let start_note = other_run.unwrap_or(can_run.reason);
                        // The step whose panel is open, if it is still a step
                        // of the flow on screen.
                        let open_step = drawer.read().clone().filter(|id| graph.get(id).is_some());
                        rsx! {
                            RunView {
                                can_start,
                                start_note,
                                on_start: move |_| begin(false),
                                graph: graph.clone(),
                                state: run_state.clone(),
                                repo_label,
                                flow_label,
                                selected: selected_node.read().clone(),
                                lenses: props.llm_config.read().lenses.clone(),
                                gates: {
                                    let status = status_map.get(&repo);
                                    Gates::for_run(
                                        &graph,
                                        &run_state,
                                        &selected_pr.read(),
                                        status.map(|s| s.prs.as_slice()).unwrap_or_default(),
                                        status.and_then(|s| s.pr.as_ref()),
                                    )
                                },
                                // A step clicked on the map — or its pill, or
                                // its reviewer spoke — opens its panel over
                                // the map, where it is approved, retried or
                                // read, without leaving the run.
                                on_select: move |id: String| {
                                    selected_node.set(id.clone());
                                    drawer.set(Some(id));
                                },
                                panel_open: open_step.is_some(),
                                flows: flow_strip,
                                if let Some(node_id) = open_step {
                                    div { class: "run-drawer", key: "{node_id}",
                                        button {
                                            class: "run-drawer-close",
                                            title: "Close (Esc)",
                                            onclick: move |_| drawer.set(None),
                                            "\u{2715}"
                                        }
                                        {step_detail(wiring, key.clone(), graph.clone(), run_state.clone(), node_id.clone(), *is_light.read(), Some(drawer))}
                                    }
                                }
                            }
                        }
                    }
                    Some(repo) => {
                        let pr_id = selected_pr.read().clone();
                        let key: Key = (repo.clone(), flow_id.clone(), pr_id.clone());
                        let state = states_snapshot.get(&key).cloned().unwrap_or_default();
                        let node_id = {
                            let chosen = selected_node.read().clone();
                            if graph.get(&chosen).is_some() { chosen } else { flow_first_node.clone() }
                        };
                        let label = repo_list.iter()
                            .find(|r| r.path == repo)
                            .map(|r| r.label.clone())
                            .unwrap_or_else(|| repo.clone());
                        let is_running = running.read().contains(&key);
                        // Trust is granted per leg, but the chain moves between
                        // flows, so the button has to look at the repository
                        // rather than at this tab's key alone.
                        let is_trusted = trusted.read().iter().any(|(r, _, _)| r == &repo);
                        // Flows this repository has hidden. Needed both for the
                        // tab strip below and for the trusted-run hint just
                        // under here, which must not offer a flow the strip
                        // does not even show.
                        let hidden_here = repo_flows.read().hidden_for(&repo).to_vec();
                        // What a trusted run would take on, which is not
                        // necessarily the flow on screen: a clean tree with a
                        // release due offers one from the Commit → PR tab.
                        let trusted_next = status_map
                            .get(&repo)
                            .and_then(|status| trusted::next_flow(&flows, status, &hidden_here));
                        // One run per repository: committing on one tab while
                        // merging a pull request on another, or reviewing #7
                        // while #5 is under way, would have two runs racing
                        // each other's checkout and fetch in the same tree.
                        let other_run = other_run_in(&running.read(), &repo, &key)
                            .map(|other| busy_note(&flows, other))
                            .or_else(|| {
                                branches_busy
                                    .read()
                                    .as_ref()
                                    .filter(|(r, _)| r == &repo)
                                    .map(|(_, branch)| branch_busy_note(branch))
                            });
                        let other_running = other_run.is_some();
                        let can_run = probe::affordance(
                            &flow_id,
                            status_map.get(&repo),
                            *probing.read() > 0,
                            state.started,
                            &pr_id,
                            &flow_problems,
                        );
                        // What the trusted button will actually start. The
                        // repository's most urgent need by preference — that
                        // is the whole point of the button — but the flow on
                        // screen when there is no such need and this flow can
                        // run anyway. Requiring a need meant the button was
                        // dead on every tab whenever the probe said "nothing
                        // in particular", which reads as "trusted runs only
                        // work on Commit → PR".
                        let trusted_start = trusted_next
                            .clone()
                            .or_else(|| can_run.enabled.then(|| (flow_id.clone(), pr_id.clone())));

                        // Both flows end with a pull request worth linking to:
                        // the one just opened, or the one just merged.
                        let pr_url = state.artifact("pr_url").to_string();
                        let finished = state.started && state.is_finished(&graph);

                        rsx! {
                            div { class: "graph-col", style: "width: {middle_w}px;",
                                div { class: "col-head",
                                    match forge_map.get(&repo).cloned() {
                                        Some(forge) => rsx! { ForgeIcon { forge, size: 16 } },
                                        None => rsx! {},
                                    }
                                    div { class: "col-head-main",
                                        // Elided when long, so the full name
                                        // stays reachable on hover.
                                        div { class: "col-title", title: "{label}", "{label}" }
                                        div { class: "col-sub", title: "{repo}",
                                            match status_map.get(&repo).map(|s| s.branch.clone()) {
                                                Some(branch) if !branch.is_empty() => rsx! {
                                                    span { class: "col-branch", "⑂ {branch}" }
                                                },
                                                _ => rsx! { span { "{repo}" } },
                                            }
                                            // Merged work that has not shipped, named by the
                                            // pull requests that are sitting in it.
                                            if let Some(rel) = status_map
                                                .get(&repo)
                                                .map(|s| s.release.clone())
                                                .filter(|r| r.due())
                                            {
                                                span { class: "col-release", "⬆ {rel.summary()}" }
                                            }
                                        }
                                    }
                                    button {
                                        class: "btn btn-ghost",
                                        title: "Local branches, and whether their pull request landed",
                                        onclick: {
                                            let repo = repo.clone();
                                            let forge = forge_map.get(&repo).cloned().unwrap_or(crate::services::forge::Forge::None);
                                            move |_| {
                                                let repo = repo.clone();
                                                let forge = forge.clone();
                                                let override_base = repo_bases.read().get(&repo).map(str::to_string);
                                                branches_open.set(Some(repo.clone()));
                                                branches_data.set(None);
                                                branches_action_error.set(None);
                                                spawn(async move {
                                                    let base = resolved_base(&repo, override_base).await;
                                                    let result = crate::services::branches::list(&repo, &forge, &base).await;
                                                    branches_data.set(Some(result));
                                                });
                                            }
                                        },
                                        "Branches"
                                    }
                                    button {
                                        class: "btn btn-ghost",
                                        title: "Which branch this repository's pull requests target",
                                        onclick: {
                                            let repo = repo.clone();
                                            move |_| {
                                                base_editor_value.set(
                                                    repo_bases.read().get(&repo).unwrap_or("").to_string()
                                                );
                                                base_editor_open.set(Some(repo.clone()));
                                            }
                                        },
                                        match repo_bases.read().get(&repo) {
                                            Some(base) => format!("Base: {base}"),
                                            None => "Base: auto".to_string(),
                                        }
                                    }
                                    // Two ways to start the same run. The
                                    // trusted one answers the approvals for
                                    // you, in the open, and stops itself at
                                    // anything `services::trusted` refuses.
                                    if is_trusted {
                                        button {
                                            class: "btn btn-trusted btn-trusted-on",
                                            title: "Stop clicking through the approvals. The run keeps going and asks you at the next one, and no further flow is started.",
                                            onclick: {
                                                let repo = repo.clone();
                                                move |_| {
                                                    // Every leg for this repository, not just the
                                                    // one on screen — the chain may already have
                                                    // moved on to another flow's key.
                                                    trusted.write().retain(|(r, _, _)| r != &repo);
                                                    approvals().notify_waiters();
                                                }
                                            },
                                            span { class: "flow-tab-dot" }
                                            "Trusting…"
                                        }
                                    } else if is_running {
                                        // A run is already going, and you are
                                        // most likely reading this because it
                                        // has stopped to ask you something.
                                        // Adopting it is a different action
                                        // from starting one: nothing is reset,
                                        // the approval in front of you is
                                        // answered, and every one after it in
                                        // this repository is too. `drive` is
                                        // already watching the trusted set on
                                        // its approval wait, so putting the key
                                        // in is the whole mechanism.
                                        button {
                                            class: "btn btn-trusted",
                                            title: "Approve this step, and every step after it in this repository, without asking again. Still stops at a merge the analysis or CI is unhappy about.",
                                            onclick: {
                                                let key = key.clone();
                                                let mut trusted = trusted;
                                                move |_| {
                                                    trusted.write().insert(key.clone());
                                                    approvals().notify_waiters();
                                                }
                                            },
                                            "Stop asking me"
                                        }
                                    } else {
                                        button {
                                            class: "btn btn-trusted",
                                            disabled: other_running || trusted_start.is_none(),
                                            title: match (&trusted_start, other_running) {
                                                (_, true) => other_run.clone().unwrap_or_default(),
                                                (None, _) => format!(
                                                    "Nothing here needs a run, and this flow cannot start — {}.",
                                                    if can_run.reason.is_empty() {
                                                        status_map
                                                            .get(&repo)
                                                            .map(|s| s.wants().note())
                                                            .unwrap_or("still reading this repository")
                                                            .to_string()
                                                    } else {
                                                        can_run.reason.clone()
                                                    }
                                                ),
                                                (Some((id, _)), _) => format!(
                                                    "Work through what this repository needs, starting with \u{201c}{}\u{201d}, \
                                                     approving each step for you as you watch. Moves on to the next flow \
                                                     when one finishes, and stops for you at a merge the analysis or CI \
                                                     is unhappy about.",
                                                    flows.get(id).map(|f| f.label.clone()).unwrap_or_else(|| id.clone()),
                                                ),
                                            },
                                            onclick: start_trusted,
                                            "Trusted run"
                                        }
                                    }
                                    button {
                                        class: "btn btn-primary",
                                        disabled: is_running || other_running || !can_run.enabled,
                                        title: if let Some(note) = &other_run {
                                            note.clone()
                                        } else {
                                            can_run.reason.clone()
                                        },
                                        onclick: start,
                                        if is_running { "Running…" } else { "{can_run.label}" }
                                    }
                                }

                                // Where the chain stopped, when it stopped for
                                // a reason you can act on.
                                if !chain_note.read().is_empty() {
                                    div { class: "chain-note",
                                        span { class: "chain-note-icon", "\u{26a0}" }
                                        span { class: "chain-note-text", "{chain_note}" }
                                        button {
                                            class: "chain-note-open",
                                            onclick: move |_| setup_open.set(true),
                                            "Open Setup"
                                        }
                                        button {
                                            class: "chain-note-close",
                                            onclick: move |_| chain_note.set(String::new()),
                                            "\u{2715}"
                                        }
                                    }
                                }

                                {flow_strip}

                                // A tooltip on the tab is not enough once the
                                // broken flow is the one you are looking at:
                                // the graph below is drawn from a definition
                                // that will not run, and nothing else on screen
                                // would say why.
                                if !flow_problems.is_empty() {
                                    div { class: "flow-broken",
                                        div { class: "flow-broken-head",
                                            span { class: "flow-broken-mark", "\u{26a0}" }
                                            "This flow cannot run"
                                        }
                                        ul { class: "flow-broken-list",
                                            for problem in flow_problems.iter().cloned() {
                                                li { key: "{problem}", "{problem}" }
                                            }
                                        }
                                        button {
                                            class: "btn",
                                            onclick: move |_| setup_open.set(true),
                                            "Fix in Setup"
                                        }
                                    }
                                }

                                // Every open pull request on this repository —
                                // not just the one for whatever branch happens
                                // to be checked out — so reviewing #7 today and
                                // #5 tomorrow needs no `git checkout` between.
                                if flow_id == probe::REVIEW_FLOW {
                                    {
                                        let prs = status_map.get(&repo).map(|s| s.prs.clone()).unwrap_or_default();
                                        let prs_error = status_map.get(&repo).and_then(|s| s.prs_error.clone());
                                        if let Some(err) = prs_error {
                                            // A CLI that is missing or signed out has a known
                                            // fix; offer it here rather than only naming it.
                                            let fix = status_map
                                                .get(&repo)
                                                .and_then(|s| forge::pr_list_remedy(&s.forge, &err));
                                            let (fixing, fix_output) =
                                                pr_fix.read().get(&repo).cloned().unwrap_or_default();
                                            rsx! {
                                                div { class: "pr-list-error",
                                                    "Couldn't check for open pull requests: {err}"
                                                    if let Some(fix) = fix {
                                                        div { class: "remedy pr-list-fix",
                                                            div { class: "remedy-main",
                                                                div { class: "remedy-label", "{fix.label}" }
                                                                code { class: "remedy-cmd", "{fix.display}" }
                                                            }
                                                            button {
                                                                class: "btn btn-primary",
                                                                disabled: fixing,
                                                                onclick: {
                                                                    let repo = repo.clone();
                                                                    move |_| {
                                                                        let repo = repo.clone();
                                                                        let fix = fix.clone();
                                                                        pr_fix.write().insert(repo.clone(), (true, String::new()));
                                                                        spawn(async move {
                                                                            let (ok, output) = git::run_streaming(
                                                                                &fix.program,
                                                                                &fix.args,
                                                                                Some(&repo),
                                                                                "",
                                                                                &mut |_| {},
                                                                            )
                                                                            .await;
                                                                            pr_fix.write().insert(repo.clone(), (false, output));
                                                                            if ok {
                                                                                reprobe(repo, statuses);
                                                                            }
                                                                        });
                                                                    }
                                                                },
                                                                if fixing { "Running…" } else { "Run" }
                                                            }
                                                        }
                                                    }
                                                    if !fix_output.is_empty() {
                                                        pre { class: "remedy-out pr-list-fix-out", "{fix_output}" }
                                                    }
                                                }
                                            }
                                        } else if prs.is_empty() {
                                            rsx! {}
                                        } else {
                                            // Once one PR on this repo is running, the
                                            // rest are unpickable — switching to another
                                            // would leave that run's git state (checkout,
                                            // fetch) racing against this one's.
                                            let running_pr = running
                                                .read()
                                                .iter()
                                                .find(|(r, f, p)| r == &repo && f == &flow_id && !p.is_empty())
                                                .map(|(_, _, p)| p.clone());
                                            rsx! {
                                                div { class: "pr-list-head",
                                                    "{prs.len()} open pull request" if prs.len() != 1 { "s" }
                                                }
                                                div { class: "pr-list",
                                                    for pr in prs.iter().cloned() {
                                                        {
                                                            let locked = running_pr.as_deref()
                                                                .is_some_and(|running| running != pr.number);
                                                            let class = if pr.number == pr_id {
                                                                "pr-list-item pr-list-item-on"
                                                            } else if locked {
                                                                "pr-list-item pr-list-item-locked"
                                                            } else {
                                                                "pr-list-item"
                                                            };
                                                            rsx! {
                                                                div {
                                                                    key: "{pr.number}",
                                                                    class,
                                                                    title: if locked { "Another pull request review is already running for this repository." } else { "" },
                                                                    onclick: {
                                                                        let number = pr.number.clone();
                                                                        move |_| {
                                                                            if locked {
                                                                                return;
                                                                            }
                                                                            selected_pr.set(number.clone());
                                                                            selected_node.set(String::new());
                                                                        }
                                                                    },
                                                                    PrCard { pr: pr.clone() }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else if let Some(pr) = status_map.get(&repo).and_then(|s| s.pr.clone()) {
                                    PrCard { pr }
                                }

                                div { class: "col-scroll",
                                    for node in graph.nodes.iter().cloned() {
                                        NodeCard {
                                            key: "{node.id}",
                                            spec: node.clone(),
                                            run: state.runs.get(&node.id).cloned().unwrap_or_default(),
                                            selected: node.id == node_id,
                                            on_select: move |id: String| selected_node.set(id),
                                        }
                                    }
                                }

                                if finished && !pr_url.is_empty() {
                                    div { class: "footer footer-ok",
                                        a { class: "footer-link", href: "{pr_url}", target: "_blank", "{pr_url}" }
                                    }
                                }
                            }

                            div {
                                class: "divider",
                                onmousedown: move |e| {
                                    drag_from.set((e.client_coordinates().x, *middle_w.read()));
                                    dragging.set(2);
                                },
                            }

                            div { class: "detail-col",
                                {step_detail(wiring, key.clone(), graph.clone(), state.clone(), node_id.clone(), *is_light.read(), None)}
                            }
                        }
                    }
                }
            }
        }

        if let Some(wanted) = licence_open.read().clone() {
            LicencePanel {
                status: licence_status,
                slots: free_slots,
                repos: repos.read().clone(),
                wanted,
                on_close: {
                    let workspace = workspace.clone();
                    move |_| {
                        licence_open.set(None);
                        crate::refresh_window_title();
                        // A licence activated, or a slot given back: re-read
                        // what is locked, and check what just opened up.
                        refresh_all(&workspace, repos, statuses, probing, picked, selected_repo, selected_flow, book, free_slots);
                    }
                },
            }
        }

        if *settings_open.read() {
            SettingsPanel {
                llm_config: props.llm_config,
                on_close: move |_| settings_open.set(false),
            }
        }

        if let Some(repo) = branches_open.read().clone() {
            {
                let repo_label = repos.read().iter()
                    .find(|r| r.path == repo)
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| repo.clone());
                let forge = forge_map.get(&repo).cloned().unwrap_or(crate::services::forge::Forge::None);
                let override_base = repo_bases.read().get(&repo).map(str::to_string);
                let reload = {
                    let repo = repo.clone();
                    let forge = forge.clone();
                    let override_base = override_base.clone();
                    move || {
                        let repo = repo.clone();
                        let forge = forge.clone();
                        let override_base = override_base.clone();
                        branches_data.set(None);
                        spawn(async move {
                            let base = resolved_base(&repo, override_base).await;
                            let result = crate::services::branches::list(&repo, &forge, &base).await;
                            branches_data.set(Some(result));
                        });
                    }
                };
                // A flow running here holds the working tree; deleting or
                // pushing a branch underneath it is the same race as two flows.
                let held_by = running
                    .read()
                    .iter()
                    .find(|k| k.0 == repo)
                    .map(|k| busy_note(&book.read(), k));
                // Checked again when a click lands, not just when the panel
                // last rendered: the run may have started in between.
                let holds = {
                    let repo = repo.clone();
                    move || {
                        running.read().iter().any(|k| k.0 == repo)
                            || branches_busy.read().as_ref().is_some_and(|(r, _)| r == &repo)
                    }
                };
                rsx! {
                    BranchesPanel {
                        repo_label,
                        branches: branches_data.read().clone(),
                        action_error: branches_action_error.read().clone(),
                        busy: branches_busy
                            .read()
                            .as_ref()
                            .filter(|(r, _)| r == &repo)
                            .map(|(_, branch)| branch.clone()),
                        held_by: held_by.clone(),
                        on_close: move |_| branches_open.set(None),
                        on_refresh: {
                            let mut reload = reload.clone();
                            move |_| reload()
                        },
                        on_delete: {
                            let repo = repo.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |(branch, force): (String, bool)| {
                                if holds() {
                                    return;
                                }
                                let repo = repo.clone();
                                let mut reload = reload.clone();
                                branches_busy.set(Some((repo.clone(), branch.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    if let Err(e) = crate::services::branches::delete(&repo, &branch, force).await {
                                        branches_action_error.set(Some(format!("Couldn't delete {branch}: {e}")));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                        on_delete_merged: {
                            let repo = repo.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |names: Vec<String>| {
                                if holds() {
                                    return;
                                }
                                let repo = repo.clone();
                                let mut reload = reload.clone();
                                // One after another, as a single action: the
                                // repository stays held from the first delete
                                // to the last, and git never has two of them
                                // writing its refs at once.
                                branches_busy.set(names.first().map(|b| (repo.clone(), b.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    let mut failed = vec![];
                                    for branch in names {
                                        branches_busy.set(Some((repo.clone(), branch.clone())));
                                        if let Err(e) = crate::services::branches::delete(&repo, &branch, true).await {
                                            failed.push(format!("{branch}: {e}"));
                                        }
                                    }
                                    if !failed.is_empty() {
                                        branches_action_error.set(Some(format!("Couldn't delete {}", failed.join("; "))));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                        on_review: {
                            let repo = repo.clone();
                            move |number: String| {
                                // Whichever flow says it handles an open pull
                                // request here, whatever it is called.
                                let hidden = repo_flows.read().hidden_for(&repo);
                                let flow = book
                                    .read()
                                    .runnable_for(&hidden)
                                    .iter()
                                    .find(|f| f.answers(Need::OpenPullRequest))
                                    .map(|f| f.id.clone());
                                let Some(flow) = flow else {
                                    branches_action_error.set(Some(
                                        "No flow shown on this repository handles an open pull request. \
                                         In Setup, tick \u{201c}an open pull request\u{201d} on the review flow."
                                            .into(),
                                    ));
                                    return;
                                };
                                branches_open.set(None);
                                selected_repo.set(Some(repo.clone()));
                                selected_flow.set(flow);
                                selected_pr.set(number);
                                selected_node.set(String::new());
                            }
                        },
                        on_clean_up: {
                            let repo = repo.clone();
                            let forge = forge.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |branch: String| {
                                let repo = repo.clone();
                                let forge = forge.clone();
                                let mut reload = reload.clone();
                                let found = branches_data
                                    .read()
                                    .as_ref()
                                    .and_then(|r| r.as_ref().ok())
                                    .and_then(|list| list.iter().find(|b| b.name == branch).cloned());
                                let Some(info) = found else { return };
                                if holds() {
                                    return;
                                }
                                branches_busy.set(Some((repo.clone(), branch.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    if let Err(e) = crate::services::branches::clean_up(&repo, &forge, &info).await {
                                        branches_action_error.set(Some(format!("Couldn't clean up {branch}: {e}")));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                        on_create_pr: {
                            let repo = repo.clone();
                            let forge = forge.clone();
                            let override_base = override_base.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |branch: String| {
                                if holds() {
                                    return;
                                }
                                let repo = repo.clone();
                                let forge = forge.clone();
                                let override_base = override_base.clone();
                                let mut reload = reload.clone();
                                branches_busy.set(Some((repo.clone(), branch.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    let base = resolved_base(&repo, override_base).await;
                                    if let Err(e) = crate::services::branches::create_pr(&repo, &branch, &base, &forge).await {
                                        branches_action_error.set(Some(format!("Couldn't open a pull request for {branch}: {e}")));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                    }
                }
            }
        }

        if let Some(repo) = base_editor_open.read().clone() {
            {
                let repo_label = repos.read().iter()
                    .find(|r| r.path == repo)
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| repo.clone());
                let close = move |_| base_editor_open.set(None);
                rsx! {
                    div { class: "modal-backdrop", onclick: close,
                        div {
                            class: "modal",
                            onclick: move |e: Event<MouseData>| e.stop_propagation(),
                            div { class: "modal-head",
                                span { "Base branch — {repo_label}" }
                                button { class: "modal-close", onclick: close, "×" }
                            }
                            div { class: "modal-body",
                                label { class: "field",
                                    span { "Target branch" }
                                    input {
                                        value: "{base_editor_value.read()}",
                                        placeholder: "auto-detected (origin/HEAD, then main, then master)",
                                        oninput: move |e| base_editor_value.set(e.value()),
                                    }
                                }
                                p { class: "field-note",
                                    "Where this repository's pull requests go. Leave empty to let \
                                     GitAgent detect it — set this only when a repository's pull \
                                     requests target something other than its default branch, e.g. \
                                     \"develop\"."
                                }
                                div { class: "field-row",
                                    button {
                                        class: "btn",
                                        onclick: {
                                            let repo = repo.clone();
                                            move |_| {
                                                base_editor_value.set(String::new());
                                                repo_bases.write().set(&repo, "");
                                                store::save_repo_bases(&repo_bases.read());
                                                base_editor_open.set(None);
                                            }
                                        },
                                        "Clear (use auto-detection)"
                                    }
                                    button {
                                        class: "btn btn-primary",
                                        onclick: {
                                            let repo = repo.clone();
                                            move |_| {
                                                repo_bases.write().set(&repo, &base_editor_value.read());
                                                store::save_repo_bases(&repo_bases.read());
                                                base_editor_open.set(None);
                                            }
                                        },
                                        "Save"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some((repo, id)) = confirm_hide.read().clone() {
            {
                let label = book.read().get(&id).map(|f| f.label.clone()).unwrap_or(id.clone());
                let repo_label = repos.read().iter()
                    .find(|r| r.path == repo)
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| repo.clone());
                rsx! {
                    div { class: "modal-backdrop", onclick: move |_| confirm_hide.set(None),
                        div {
                            class: "modal",
                            onclick: move |e: Event<MouseData>| e.stop_propagation(),
                            div { class: "modal-head",
                                span { "Hide this flow?" }
                                button {
                                    class: "modal-close",
                                    onclick: move |_| confirm_hide.set(None),
                                    "×"
                                }
                            }
                            div { class: "modal-body",
                                p { class: "field-note",
                                    "\"{label}\" will no longer show as a tab for {repo_label}. \
                                     It still exists — every other repository keeps seeing it, \
                                     and you can bring it back from the picker at the end of \
                                     the tab strip."
                                }
                                div { class: "approval-actions",
                                    button {
                                        class: "btn btn-danger",
                                        onclick: {
                                            let repo = repo.clone();
                                            let id = id.clone();
                                            move |_| {
                                                hide_flow(&repo, &id);
                                                confirm_hide.set(None);
                                            }
                                        },
                                        "Hide"
                                    }
                                    button {
                                        class: "btn",
                                        onclick: move |_| confirm_hide.set(None),
                                        "Cancel"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_on_the_first_repository_in_the_list_with_work_left() {
        let list = vec![
            ("a".to_string(), probe::Wants::Nothing),
            ("b".to_string(), probe::Wants::Release),
            ("c".to_string(), probe::Wants::Resolve),
        ];
        // c is more urgent, but b comes first in the list.
        assert_eq!(first_with_work(list).map(|(p, _)| p), Some("b".to_string()));
    }

    #[test]
    fn checks_still_running_is_not_work_left() {
        let list = vec![
            ("a".to_string(), probe::Wants::Wait),
            ("b".to_string(), probe::Wants::Commit),
        ];
        assert_eq!(first_with_work(list).map(|(p, _)| p), Some("b".to_string()));
        assert_eq!(
            first_with_work(vec![("a".to_string(), probe::Wants::Nothing)]),
            None
        );
    }

    fn commit_and_pr() -> flowdef::FlowDef {
        FlowBook::defaults().get("commit_and_pr").unwrap().clone()
    }

    fn brief(number: &str) -> probe::PrBrief {
        probe::PrBrief {
            number: number.into(),
            title: "t".into(),
            url: "u".into(),
            checks: probe::Checks::Passing,
            files: 1,
            additions: 1,
            deletions: 0,
            commits: 1,
        }
    }

    #[test]
    fn arriving_at_a_review_picks_the_pull_request_too() {
        // Landing on the review flow with nothing selected to review leaves
        // the person to answer a question the app already knows.
        let book = FlowBook::defaults();
        let states = States::new();
        let prs = [brief("7"), brief("9")];

        let (flow_id, _, pr_id) =
            default_selection(&book, &states, "/repo", Some(Wants::Merge), &prs, &[]);
        assert_eq!(flow_id, "review_and_merge");
        assert_eq!(pr_id, "7", "the first one, matching the order shown");
    }

    #[test]
    fn a_run_in_a_repository_holds_every_other_flow_there() {
        let key = |r: &str, f: &str, p: &str| (r.to_string(), f.to_string(), p.to_string());
        let running: BTreeSet<Key> = [key("/a", "commit_and_pr", "")].into();
        // Merging a pull request while a commit is under way is refused…
        let merge = key("/a", probe::REVIEW_FLOW, "7");
        assert_eq!(
            other_run_in(&running, "/a", &merge),
            Some(&key("/a", "commit_and_pr", ""))
        );
        // …the run itself is not in its own way…
        assert_eq!(
            other_run_in(&running, "/a", &key("/a", "commit_and_pr", "")),
            None
        );
        // …and another repository is unaffected.
        assert_eq!(
            other_run_in(&running, "/b", &key("/b", probe::REVIEW_FLOW, "7")),
            None
        );
    }

    #[test]
    fn a_commit_flow_selects_no_pull_request() {
        let book = FlowBook::defaults();
        let states = States::new();
        let (_, _, pr_id) = default_selection(
            &book,
            &states,
            "/repo",
            Some(Wants::Commit),
            &[brief("7")],
            &[],
        );
        assert!(pr_id.is_empty(), "nothing to scope a commit run to");
    }

    #[test]
    fn a_review_with_no_pull_requests_listed_selects_none() {
        let book = FlowBook::defaults();
        let states = States::new();
        let (_, _, pr_id) =
            default_selection(&book, &states, "/repo", Some(Wants::Merge), &[], &[]);
        assert!(pr_id.is_empty());
    }

    #[test]
    fn with_nothing_running_the_flow_follows_what_the_repository_needs() {
        // Clicking a repository whose only outstanding work is a release must
        // not land on "Commit → PR" and show nothing to do.
        let book = FlowBook::defaults();
        let states = States::new();

        let (flow_id, node_id, _) =
            default_selection(&book, &states, "/repo", Some(Wants::Merge), &[], &[]);
        assert_eq!(flow_id, "review_and_merge");
        assert_eq!(node_id, book.get("review_and_merge").unwrap().first_node());

        let (flow_id, _, _) =
            default_selection(&book, &states, "/repo", Some(Wants::Commit), &[], &[]);
        assert_eq!(flow_id, "commit_and_pr");
    }

    #[test]
    fn a_hint_naming_a_flow_that_does_not_exist_falls_back() {
        // Wants::Release points at a flow id nobody has built yet.
        let book = FlowBook::defaults();
        let states = States::new();
        let (flow_id, _, _) =
            default_selection(&book, &states, "/repo", Some(Wants::Release), &[], &[]);
        assert_eq!(flow_id, book.runnable().first().unwrap().id);
    }

    #[test]
    fn a_running_task_is_selected_over_an_idle_flow() {
        let book = FlowBook::defaults();
        let flow = commit_and_pr();
        let mut run = RunState::fresh(&flow.to_graph());
        run.started = true;
        run.set_status("preflight", NodeStatus::Done);
        run.set_status("scan", NodeStatus::Running);

        let mut states = States::new();
        states.insert(("/repo".into(), flow.id.clone(), String::new()), run);

        let (flow_id, node_id, pr_id) = default_selection(&book, &states, "/repo", None, &[], &[]);
        assert_eq!(flow_id, flow.id);
        assert_eq!(node_id, "scan");
        assert_eq!(pr_id, "");
    }

    #[test]
    fn a_running_task_only_wins_for_its_own_repository() {
        let book = FlowBook::defaults();
        let flow = commit_and_pr();
        let mut run = RunState::fresh(&flow.to_graph());
        run.started = true;
        run.set_status("scan", NodeStatus::Running);

        let mut states = States::new();
        states.insert(("/other-repo".into(), flow.id.clone(), String::new()), run);

        let (flow_id, node_id, _) = default_selection(&book, &states, "/repo", None, &[], &[]);
        // Nothing running here — falls back to the first runnable flow's
        // first node, same as an untouched repository.
        assert_eq!(flow_id, book.runnable().first().unwrap().id);
        assert_eq!(node_id, book.runnable().first().unwrap().first_node());
    }

    #[test]
    fn someone_awaiting_approval_still_outranks_a_running_task() {
        let book = FlowBook::defaults();
        let flow = commit_and_pr();
        let mut run = RunState::fresh(&flow.to_graph());
        run.started = true;
        run.set_status("scan", NodeStatus::Done);
        run.set_status("draft_commit", NodeStatus::Running);
        run.set_status("commit", NodeStatus::AwaitingApproval);

        let mut states = States::new();
        states.insert(("/repo".into(), flow.id.clone(), String::new()), run);

        let (_, node_id, _) = default_selection(&book, &states, "/repo", None, &[], &[]);
        assert_eq!(node_id, "commit");
    }
}
