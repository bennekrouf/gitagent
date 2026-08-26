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

use crate::components::detail_pane::DetailPane;
use crate::components::forge_icon::ForgeIcon;
use crate::components::node_card::NodeCard;
use crate::components::pr_card::PrCard;
use crate::components::repo_sidebar::{phase_of, Phase, RepoEntry, RepoSidebar};
use crate::components::settings_panel::SettingsPanel;
use crate::screens::setup::Setup;
use crate::services::flow;
use crate::services::flowdef::FlowBook;
use crate::services::graph::{Graph, NodeRun, NodeStatus, Remedy, RunState};
use crate::services::llm::LlmConfig;
use crate::services::probe::{self, RepoStatus};
use crate::services::store::Layout;
use crate::services::{git, store};

/// One run per repository, per flow, per pull request — the third slot is
/// empty for anything that isn't PR-scoped review. That is what lets you
/// review PR #7 and PR #5 on the same repository at once without one
/// overwriting the other's progress, the same way two different repositories
/// already don't tread on each other.
type Key = (String, String, String);
type States = BTreeMap<Key, RunState>;

#[derive(Props, Clone, PartialEq)]
pub struct WorkspaceProps {
    pub workspace: String,
    pub llm_config: Signal<LlmConfig>,
    pub is_light: Signal<bool>,
    pub theme_overridden: Signal<bool>,
    pub on_change_workspace: EventHandler<()>,
}

fn snapshot(states: &Signal<States>, key: &Key) -> RunState {
    states.read().get(key).cloned().unwrap_or_default()
}

/// Walks one flow, for one repository, to completion.
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
) {
    let repo = key.0.clone();

    loop {
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

        if node.requires_approval {
            let proposal = flow::proposal(&node, &state);
            let items = flow::proposal_items(&node, &state);
            let preview_diff = flow::diff_preview(&node, &repo).await.unwrap_or_default();
            {
                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                let run = entry.runs.entry(node.id.clone()).or_default();
                run.proposal = proposal;
                run.items = items;
                run.preview_diff = preview_diff;
                run.status = NodeStatus::AwaitingApproval;
            }

            let approved = loop {
                let decision = states
                    .read()
                    .get(&key)
                    .and_then(|s| s.decisions.get(&node.id).copied());
                if let Some(decision) = decision {
                    break decision;
                }
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            };

            if !approved {
                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                entry.set_status(&node.id, NodeStatus::Rejected);
                entry.runs.entry(node.id.clone()).or_default().summary =
                    "declined — nothing was run".into();
                entry.propagate_block(&graph);
                continue;
            }
        }

        states
            .write()
            .entry(key.clone())
            .or_default()
            .set_status(&node.id, NodeStatus::Running);

        let state = snapshot(&states, &key);
        let cfg_snapshot = cfg.read().clone();

        // Fills in the node's log as its command's output arrives, rather
        // than only once the whole thing finishes — the point of streaming
        // at all. `result`'s own `outcome.log`/`failure.message` still wins
        // once the step settles, so formatting (a placeholder for empty
        // output, the failing command echoed back) stays exactly as before.
        let mut push_line = {
            let mut states = states;
            let key = key.clone();
            let node_id = node.id.clone();
            move |line: &str| {
                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                let run = entry.runs.entry(node_id.clone()).or_default();
                if !run.log.is_empty() {
                    run.log.push('\n');
                }
                run.log.push_str(line);
            }
        };
        let result = flow::execute(&node, &repo, &cfg_snapshot, &state, &mut push_line).await;

        let mut w = states.write();
        let entry = w.entry(key.clone()).or_default();
        match result {
            Ok(outcome) => {
                let nothing = outcome.nothing_to_do;
                for (k, v) in outcome.artifacts {
                    entry.artifacts.insert(k, v);
                }
                {
                    let run = entry.runs.entry(node.id.clone()).or_default();
                    run.status = if nothing {
                        NodeStatus::Skipped
                    } else {
                        NodeStatus::Done
                    };
                    run.summary = outcome.summary;
                    run.log = outcome.log;
                }
                // Nothing to do is still nothing downstream can build on.
                if nothing {
                    entry.propagate_block(&graph);
                }
            }
            Err(failure) => {
                {
                    let run = entry.runs.entry(node.id.clone()).or_default();
                    run.status = NodeStatus::Failed;
                    run.summary = "failed".into();
                    run.log = failure.message;
                    run.remedies = failure.remedies;
                }
                // A failure stays local: only what depends on this node is blocked.
                entry.propagate_block(&graph);
            }
        }
    }
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
        drive(
            graph,
            key.clone(),
            cfg,
            states,
            selected_node,
            selected_repo,
            selected_flow,
            selected_pr,
        )
        .await;
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

/// Re-probes every repository, concurrently, and opens on whichever one wants
/// a person if nothing has been chosen yet.
#[allow(clippy::too_many_arguments)]
fn refresh_all(
    repos: Signal<Vec<store::Repo>>,
    mut statuses: Signal<BTreeMap<String, RepoStatus>>,
    mut probing: Signal<usize>,
    mut picked: Signal<bool>,
    mut selected_repo: Signal<Option<String>>,
    mut selected_flow: Signal<String>,
    book: Signal<FlowBook>,
) {
    if *probing.read() > 0 {
        return;
    }
    let list = repos.read().clone();
    probing.set(list.len());

    for repo in list.clone() {
        let all = list.clone();
        spawn(async move {
            let status = probe::probe(&repo.path).await;
            statuses.write().insert(repo.path.clone(), status);
            let left = probing.read().saturating_sub(1);
            probing.set(left);

            if left == 0 && !*picked.read() {
                picked.set(true);
                let map = statuses.read().clone();
                // Most urgent first; ties broken by the order on disk.
                let best = all
                    .iter()
                    .filter_map(|r| map.get(&r.path).map(|s| (r.path.clone(), s.wants())))
                    .filter(|(_, wants)| wants.needs_a_person())
                    .min_by_key(|(_, wants)| *wants);

                if let Some((path, wants)) = best {
                    let hint = wants.flow_hint().to_string();
                    if book.read().get(&hint).is_some() {
                        selected_flow.set(hint);
                    }
                    selected_repo.set(Some(path));
                }
            }
        });
    }
}

#[component]
pub fn Workspace(props: WorkspaceProps) -> Element {
    let workspace = props.workspace.clone();
    let repos = use_signal(|| store::discover_repos(&workspace));
    let statuses = use_signal(BTreeMap::<String, RepoStatus>::new);
    let probing = use_signal(|| 0usize);
    // Auto-selection happens once, on the first probe: after that the choice is
    // yours and a refresh must not move it.
    let picked = use_signal(|| false);
    let mut states = use_signal(States::new);
    let mut selected_repo = use_signal(|| Option::<String>::None);
    // Loaded once per mount, so returning from Setup picks up any edits.
    let mut book = use_signal(FlowBook::load);
    let first_flow = book
        .read()
        .runnable()
        .first()
        .map(|f| f.id.clone())
        .unwrap_or_default();
    let mut selected_flow = use_signal(|| first_flow);
    // Which open PR a review run is scoped to. Empty means "whatever the
    // checked-out branch has open" — the same default behaviour as before
    // this existed. Set by picking a specific PR from the sidebar's list.
    let mut selected_pr = use_signal(String::new);
    let mut selected_node = use_signal(String::new);
    let mut running = use_signal(BTreeSet::<Key>::new);
    let mut settings_open = use_signal(|| false);
    let mut setup_open = use_signal(|| false);
    // Which flows the *current* repository has chosen not to see — a filter
    // on top of the shared flow list, not a copy of it. `flows.toml` never
    // changes when a flow is hidden here.
    let mut repo_flows = use_signal(store::load_repo_flows);
    let mut confirm_hide = use_signal(|| Option::<(String, String)>::None);
    let mut hidden_open = use_signal(|| false);

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
    use_coroutine(move |_rx: UnboundedReceiver<()>| async move {
        refresh_all(
            repos,
            statuses,
            probing,
            picked,
            selected_repo,
            selected_flow,
            book,
        );
    });

    let llm_config = props.llm_config;
    let repo_list = repos.read().clone();
    let status_map = statuses.read().clone();
    let forge_map: BTreeMap<String, crate::services::forge::Forge> = status_map
        .iter()
        .map(|(path, s)| (path.clone(), s.forge.clone()))
        .collect();
    let states_snapshot = states.read().clone();
    let flows = book.read().clone();
    let runnable: Vec<(String, String)> = flows
        .runnable()
        .iter()
        .map(|f| (f.id.clone(), f.label.clone()))
        .collect();
    let flow_id = selected_flow.read().clone();
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
            phase: states_snapshot
                .iter()
                .filter(|((r, f, _), _)| *r == repo.path && *f == flow_id)
                .map(|(_, s)| phase_of(s))
                .min_by_key(|p| p.priority())
                .unwrap_or(Phase::Idle),
            ahead: status_map.get(&repo.path).map(|s| s.ahead).unwrap_or(0),
            behind: status_map.get(&repo.path).map(|s| s.behind).unwrap_or(0),
        })
        .collect();

    let active = selected_repo.read().clone();
    let cfg = props.llm_config.read().clone();

    let mut llm_config_mut = props.llm_config;
    let start = move |_| {
        let Some(repo) = selected_repo.read().clone() else {
            return;
        };
        // Settings live in a per-window signal but one file on disk. Re-reading
        // here is what stops a second window running against a stale provider.
        llm_config_mut.set(store::load_settings());
        let id = selected_flow.read().clone();
        let Some(def) = book.read().get(&id).cloned() else {
            return;
        };
        let pr = selected_pr.read().clone();
        let key: Key = (repo.clone(), id, pr.clone());
        if running.read().contains(&key) {
            return;
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
        selected_node.set(def.first_node());
        running.write().insert(key.clone());

        spawn(async move {
            drive(
                graph,
                key.clone(),
                llm_config,
                states,
                selected_node,
                selected_repo,
                selected_flow,
                selected_pr,
            )
            .await;
            running.write().remove(&key);
            reprobe(key.0.clone(), statuses);
        });
    };

    if *setup_open.read() {
        return rsx! {
            Setup {
                on_close: move |_| {
                    // Pick up whatever Setup wrote, without disturbing any run.
                    book.set(FlowBook::load());
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
                    on_refresh: move |_| {
                        refresh_all(repos, statuses, probing, picked, selected_repo, selected_flow, book);
                    },
                    on_select: move |path: String| {
                        selected_repo.set(Some(path));
                        selected_node.set(String::new());
                        selected_pr.set(String::new());
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
                        let can_run = probe::affordance(
                            &flow_id,
                            status_map.get(&repo),
                            *probing.read() > 0,
                            state.started,
                            &pr_id,
                        );
                        // Both flows end with a pull request worth linking to:
                        // the one just opened, or the one just merged.
                        let pr_url = state.artifact("pr_url").to_string();
                        let finished = state.started && state.is_finished(&graph);

                        let hidden_here = repo_flows.read().hidden_for(&repo).to_vec();
                        let visible_tabs: Vec<(String, String)> = runnable
                            .iter()
                            .filter(|(id, _)| !hidden_here.contains(id))
                            .cloned()
                            .collect();
                        // A label for a hidden flow can vanish from `runnable`
                        // entirely (deleted in Setup, or now invalid) — fall
                        // back to the id so the restore list never disappears
                        // silently for a flow that still exists in flows.toml
                        // but currently fails validation.
                        let hidden_tabs: Vec<(String, String)> = hidden_here
                            .iter()
                            .map(|id| {
                                let label = flows
                                    .get(id)
                                    .map(|f| f.label.clone())
                                    .unwrap_or_else(|| id.clone());
                                (id.clone(), label)
                            })
                            .collect();

                        rsx! {
                            div { class: "graph-col", style: "width: {middle_w}px;",
                                div { class: "col-head",
                                    match forge_map.get(&repo).cloned() {
                                        Some(forge) => rsx! { ForgeIcon { forge, size: 16 } },
                                        None => rsx! {},
                                    }
                                    div { class: "col-head-main",
                                        div { class: "col-title", "{label}" }
                                        div { class: "col-sub", title: "{repo}",
                                            match status_map.get(&repo).map(|s| s.branch.clone()) {
                                                Some(branch) if !branch.is_empty() => rsx! {
                                                    span { class: "col-branch", "⑂ {branch}" }
                                                },
                                                _ => rsx! { span { "{repo}" } },
                                            }
                                        }
                                    }
                                    button {
                                        class: "btn btn-primary",
                                        disabled: is_running || !can_run.enabled,
                                        title: "{can_run.reason}",
                                        onclick: start,
                                        if is_running { "Running…" } else { "{can_run.label}" }
                                    }
                                }

                                div { class: "flow-tabs",
                                    for (id, label) in visible_tabs.iter().cloned() {
                                        div {
                                            key: "{id}",
                                            class: if id == flow_id { "flow-tab flow-tab-on" } else { "flow-tab" },
                                            button {
                                                class: "flow-tab-main",
                                                onclick: {
                                                    let id = id.clone();
                                                    let first = flows.get(&id)
                                                        .map(|f| f.first_node())
                                                        .unwrap_or_default();
                                                    move |_| {
                                                        selected_flow.set(id.clone());
                                                        selected_node.set(first.clone());
                                                        // A tab is a flow, not one particular PR review
                                                        // within it — leaving a PR selected here would
                                                        // silently scope the next "Start" to it.
                                                        selected_pr.set(String::new());
                                                    }
                                                },
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
                                    if !hidden_tabs.is_empty() {
                                        button {
                                            class: "flow-tab-hidden-count",
                                            title: "Flows hidden for this repository",
                                            onclick: move |_| {
                                                let now = *hidden_open.read();
                                                hidden_open.set(!now);
                                            },
                                            "{hidden_tabs.len()} hidden"
                                        }
                                    }
                                }

                                if *hidden_open.read() && !hidden_tabs.is_empty() {
                                    div { class: "hidden-flows",
                                        for (id, label) in hidden_tabs.iter().cloned() {
                                            div { key: "{id}", class: "hidden-flow",
                                                span { class: "hidden-flow-label", "{label}" }
                                                button {
                                                    class: "btn",
                                                    onclick: {
                                                        let repo = repo.clone();
                                                        let id = id.clone();
                                                        move |_| {
                                                            repo_flows.write().show(&repo, &id);
                                                            store::save_repo_flows(&repo_flows.read());
                                                        }
                                                    },
                                                    "Show"
                                                }
                                            }
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
                                        if prs.is_empty() {
                                            rsx! {}
                                        } else {
                                            rsx! {
                                                div { class: "pr-list-head",
                                                    "{prs.len()} open pull request" if prs.len() != 1 { "s" }
                                                }
                                                div { class: "pr-list",
                                                    for pr in prs.iter().cloned() {
                                                        div {
                                                            key: "{pr.number}",
                                                            class: if pr.number == pr_id { "pr-list-item pr-list-item-on" } else { "pr-list-item" },
                                                            onclick: {
                                                                let number = pr.number.clone();
                                                                move |_| {
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
                                DetailPane {
                                    spec: graph.get(&node_id).cloned(),
                                    run: state.runs.get(&node_id).cloned().unwrap_or_else(NodeRun::default),
                                    diff: graph.get(&node_id).and_then(|spec| {
                                        spec.writes.iter()
                                            .find(|w| w.as_str() == "diff" || w.as_str() == "pr_diff")
                                            .and_then(|key| state.artifacts.get(key).cloned())
                                    }),
                                    is_light: *is_light.read(),
                                    on_approve: {
                                        let key = key.clone();
                                        move |id: String| {
                                            states.write().entry(key.clone()).or_default()
                                                .decisions.insert(id, true);
                                        }
                                    },
                                    on_reject: {
                                        let key = key.clone();
                                        move |id: String| {
                                            states.write().entry(key.clone()).or_default()
                                                .decisions.insert(id, false);
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
                                                let (ok, output) =
                                                    git::run_command(&remedy.program, &remedy.args).await;
                                                set_remedy(states, &key, &node, index, |r| {
                                                    r.running = false;
                                                    r.done = ok;
                                                    r.output = if output.is_empty() && ok {
                                                        "done".into()
                                                    } else {
                                                        output.clone()
                                                    };
                                                });
                                                // A fix that worked is only useful if the
                                                // run moves on, so re-queue what it unblocked.
                                                if ok {
                                                    retry_node(
                                                        states, running, selected_node,
                                                        selected_repo, selected_flow, selected_pr,
                                                        llm_config, statuses, retry_graph,
                                                        key, &node,
                                                    );
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
                                                key.clone(), &node,
                                            );
                                        }
                                    },
                                    on_cancel: {
                                        let key = key.clone();
                                        move |_| {
                                            states.write().remove(&key);
                                            running.write().remove(&key);
                                            reprobe(key.0.clone(), statuses);
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
        }

        if *settings_open.read() {
            SettingsPanel {
                llm_config: props.llm_config,
                on_close: move |_| settings_open.set(false),
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
                                     and you can bring it back from the \"hidden\" list next to \
                                     the tabs."
                                }
                                div { class: "approval-actions",
                                    button {
                                        class: "btn btn-danger",
                                        onclick: {
                                            let repo = repo.clone();
                                            let id = id.clone();
                                            move |_| {
                                                repo_flows.write().hide(&repo, &id);
                                                store::save_repo_flows(&repo_flows.read());
                                                // Hiding the flow on screen must not leave the
                                                // graph column showing a tab that no longer exists.
                                                if selected_repo.read().as_deref() == Some(repo.as_str())
                                                    && *selected_flow.read() == id
                                                {
                                                    let still_hidden = repo_flows.read();
                                                    let next = book.read().runnable().iter()
                                                        .find(|f| !still_hidden.is_hidden(&repo, &f.id))
                                                        .map(|f| f.id.clone())
                                                        .unwrap_or_default();
                                                    drop(still_hidden);
                                                    let first_node = book.read().get(&next)
                                                        .map(|f| f.first_node())
                                                        .unwrap_or_default();
                                                    selected_flow.set(next);
                                                    selected_node.set(first_node);
                                                }
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
