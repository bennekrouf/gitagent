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
use crate::components::repo_sidebar::{phase_of, Phase, RepoEntry, RepoSidebar};
use crate::components::settings_panel::SettingsPanel;
use crate::services::flow::FlowKind;
use crate::services::forge::{self, Forge};
use crate::services::graph::{Graph, NodeRun, NodeStatus, Remedy, RunState};
use crate::services::llm::LlmConfig;
use crate::services::{git, store};

/// One run per repository per flow.
type Key = (String, FlowKind);
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
    kind: FlowKind,
    repo: String,
    cfg: Signal<LlmConfig>,
    mut states: Signal<States>,
    mut selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<FlowKind>,
) {
    let graph = kind.graph();
    let key: Key = (repo.clone(), kind);

    loop {
        let state = snapshot(&states, &key);
        let Some(node) = state.next_ready(&graph) else {
            break;
        };

        // Only steer the selection when this run is the one on screen;
        // otherwise a background run would yank the view around.
        let viewing =
            selected_repo.read().as_deref() == Some(repo.as_str()) && *selected_flow.read() == kind;
        if viewing {
            selected_node.set(node.id.clone());
        }

        if node.requires_approval {
            let proposal = kind.proposal(node.step, &state);
            let items = kind.proposal_items(node.step, &state);
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
        let result = kind.execute(node.step, &repo, &cfg_snapshot, &state).await;

        let mut w = states.write();
        let entry = w.entry(key.clone()).or_default();
        match result {
            Ok(outcome) => {
                for (k, v) in outcome.artifacts {
                    entry.artifacts.insert(k, v);
                }
                let run = entry.runs.entry(node.id.clone()).or_default();
                run.status = NodeStatus::Done;
                run.summary = outcome.summary;
                run.log = outcome.log;
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
    selected_flow: Signal<FlowKind>,
    cfg: Signal<LlmConfig>,
    key: Key,
    node: &str,
) {
    let graph = key.1.graph();
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
            key.1,
            key.0.clone(),
            cfg,
            states,
            selected_node,
            selected_repo,
            selected_flow,
        )
        .await;
        running.write().remove(&key);
    });
}

#[component]
pub fn Workspace(props: WorkspaceProps) -> Element {
    let workspace = props.workspace.clone();
    let repos = use_signal(|| store::discover_repos(&workspace));
    let mut changes = use_signal(BTreeMap::<String, usize>::new);
    let mut forges = use_signal(BTreeMap::<String, Forge>::new);
    let mut states = use_signal(States::new);
    let mut selected_repo = use_signal(|| Option::<String>::None);
    let mut selected_flow = use_signal(|| FlowKind::CommitAndPr);
    let mut selected_node = use_signal(|| FlowKind::CommitAndPr.first_node().to_string());
    let mut running = use_signal(BTreeSet::<Key>::new);
    let mut settings_open = use_signal(|| false);

    let mut is_light = props.is_light;
    let mut theme_overridden = props.theme_overridden;

    // One pass over the workspace: uncommitted counts, and which forge each
    // repository lives on. Both feed the sidebar.
    use_coroutine(move |_rx: UnboundedReceiver<()>| async move {
        let list = repos.read().clone();
        for repo in list {
            if let Ok(entries) = git::status(&repo.path).await {
                changes.write().insert(repo.path.clone(), entries.len());
            }
            let detected = git::remote_url(&repo.path)
                .await
                .map(|url| forge::detect(&url))
                .unwrap_or(Forge::None);
            forges.write().insert(repo.path.clone(), detected);
        }
    });

    let llm_config = props.llm_config;
    let repo_list = repos.read().clone();
    let change_counts = changes.read().clone();
    let forge_map = forges.read().clone();
    let states_snapshot = states.read().clone();
    let flow = *selected_flow.read();
    let graph: Graph = flow.graph();

    let entries: Vec<RepoEntry> = repo_list
        .iter()
        .map(|repo| RepoEntry {
            path: repo.path.clone(),
            label: repo.label.clone(),
            changes: change_counts.get(&repo.path).copied(),
            forge: forge_map.get(&repo.path).cloned(),
            phase: states_snapshot
                .get(&(repo.path.clone(), flow))
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
        let kind = *selected_flow.read();
        let key: Key = (repo.clone(), kind);
        if running.read().contains(&key) {
            return;
        }
        let mut fresh = RunState::fresh(&kind.graph());
        fresh.started = true;
        states.write().insert(key.clone(), fresh);
        selected_node.set(kind.first_node().to_string());
        running.write().insert(key.clone());

        spawn(async move {
            drive(
                kind,
                key.0.clone(),
                llm_config,
                states,
                selected_node,
                selected_repo,
                selected_flow,
            )
            .await;
            running.write().remove(&key);
        });
    };

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

            div { class: "body",
                RepoSidebar {
                    entries,
                    selected: active.clone(),
                    workspace: props.workspace.clone(),
                    on_select: move |path: String| {
                        selected_repo.set(Some(path));
                        selected_node.set(selected_flow.read().first_node().to_string());
                    },
                    on_change_workspace: move |_| props.on_change_workspace.call(()),
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
                        let key: Key = (repo.clone(), flow);
                        let state = states_snapshot.get(&key).cloned().unwrap_or_default();
                        let node_id = selected_node.read().clone();
                        let label = repo_list.iter()
                            .find(|r| r.path == repo)
                            .map(|r| r.label.clone())
                            .unwrap_or_else(|| repo.clone());
                        let is_running = running.read().contains(&key);
                        // Both flows end with a pull request worth linking to:
                        // the one just opened, or the one just merged.
                        let pr_url = state.artifact("pr_url").to_string();
                        let finished = state.started && state.is_finished(&graph);

                        rsx! {
                            div { class: "graph-col",
                                div { class: "col-head",
                                    match forge_map.get(&repo).cloned() {
                                        Some(forge) => rsx! { ForgeIcon { forge, size: 16 } },
                                        None => rsx! {},
                                    }
                                    div { class: "col-head-main",
                                        div { class: "col-title", "{label}" }
                                        div { class: "col-sub", "{repo}" }
                                    }
                                    button {
                                        class: "btn btn-primary",
                                        disabled: is_running,
                                        onclick: start,
                                        if is_running { "Running…" }
                                        else if state.started { "Run again" }
                                        else { "Start run" }
                                    }
                                }

                                div { class: "flow-tabs",
                                    for kind in FlowKind::ALL {
                                        button {
                                            key: "{kind.key()}",
                                            class: if kind == flow { "flow-tab flow-tab-on" } else { "flow-tab" },
                                            onclick: move |_| {
                                                selected_flow.set(kind);
                                                selected_node.set(kind.first_node().to_string());
                                            },
                                            "{kind.label()}"
                                            if running.read().contains(&(repo.clone(), kind)) {
                                                span { class: "flow-tab-dot" }
                                            }
                                        }
                                    }
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
                                        move |(node, index): (String, usize)| {
                                            let key = key.clone();
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
                                                        llm_config, key, &node,
                                                    );
                                                }
                                            });
                                        }
                                    },
                                    on_retry: {
                                        let key = key.clone();
                                        move |node: String| {
                                            retry_node(
                                                states, running, selected_node,
                                                selected_repo, selected_flow,
                                                llm_config, key.clone(), &node,
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
