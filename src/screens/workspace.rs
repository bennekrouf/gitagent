//! The workspace screen: repositories on the left, the selected flow in the
//! middle, the selected node on the right.
//!
//! Run state is keyed by `(repository, flow)`. That is what lets you leave one
//! repository parked at an approval, look at another, come back and find it
//! where it was — and what lets a repository hold a half-finished commit flow
//! and a review flow at the same time without them treading on each other.

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

/// One run per repository per flow, the flow named by its id in the book.
type Key = (String, String);
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
async fn drive(
    graph: Graph,
    key: Key,
    cfg: Signal<LlmConfig>,
    mut states: Signal<States>,
    mut selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
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
            && *selected_flow.read() == key.1;
        if viewing {
            selected_node.set(node.id.clone());
        }

        if node.requires_approval {
            let proposal = flow::proposal(&node, &state);
            let items = flow::proposal_items(&node, &state);
            {
                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                let run = entry.runs.entry(node.id.clone()).or_default();
                run.proposal = proposal;
                run.items = items;
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
        let result = flow::execute(&node, &repo, &cfg_snapshot, &state).await;

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
    let mut selected_node = use_signal(String::new);
    let mut running = use_signal(BTreeSet::<Key>::new);
    let mut settings_open = use_signal(|| false);
    let mut setup_open = use_signal(|| false);

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
            phase: states_snapshot
                .get(&(repo.path.clone(), flow_id.clone()))
                .map(phase_of)
                .unwrap_or(Phase::Idle),
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
        let key: Key = (repo.clone(), id);
        if running.read().contains(&key) {
            return;
        }
        let graph = def.to_graph();
        let mut fresh = RunState::fresh(&graph);
        fresh.started = true;
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
                        let key: Key = (repo.clone(), flow_id.clone());
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
                        );
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
                                    for (id, label) in runnable.iter().cloned() {
                                        button {
                                            key: "{id}",
                                            class: if id == flow_id { "flow-tab flow-tab-on" } else { "flow-tab" },
                                            onclick: {
                                                let id = id.clone();
                                                let first = flows.get(&id)
                                                    .map(|f| f.first_node())
                                                    .unwrap_or_default();
                                                move |_| {
                                                    selected_flow.set(id.clone());
                                                    selected_node.set(first.clone());
                                                }
                                            },
                                            "{label}"
                                            if running.read().contains(&(repo.clone(), id.clone())) {
                                                span { class: "flow-tab-dot" }
                                            }
                                        }
                                    }
                                }

                                // What this repository's pull request actually
                                // contains, before committing to a run.
                                if let Some(pr) = status_map.get(&repo).and_then(|s| s.pr.clone()) {
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
                                                        selected_repo, selected_flow,
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
                                                selected_repo, selected_flow,
                                                llm_config, statuses, retry_graph.clone(),
                                                key.clone(), &node,
                                            );
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
    }
}
