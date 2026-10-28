//! The middle of the window: the selected repository's run, or what it can do
//! next.

use super::*;
use crate::services::flowdef::FlowDef;
use crate::services::forge::Forge;
use crate::services::probe::FlowKind;
use crate::services::store::Repo;

#[allow(clippy::too_many_arguments)]
pub(super) fn middle_pane(
    probing: Signal<usize>,
    selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    running: Signal<BTreeSet<Key>>,
    mut chain_note: Signal<String>,
    run_view: Signal<bool>,
    mut drawer: Signal<Option<String>>,
    panel_step: Signal<Option<String>>,
    panel_folded: Signal<bool>,
    mut set_folded: impl FnMut(bool) + Copy + 'static,
    mut diff_panel: Signal<Option<String>>,
    pr_diffs: Signal<PrDiffs>,
    close_when_done: Signal<Option<(Key, String)>>,
    mut setup_open: Signal<bool>,
    branches_busy: Signal<Option<(String, String)>>,
    middle_w: Signal<f64>,
    mut panel_w: Signal<Option<f64>>,
    mut dragging: Signal<u8>,
    mut drag_from: Signal<(f64, f64)>,
    is_light: Signal<bool>,
    _llm_config: Signal<LlmConfig>,
    wiring: Wiring,
    repo_list: Vec<Repo>,
    status_map: BTreeMap<String, RepoStatus>,
    forge_map: BTreeMap<String, Forge>,
    states_snapshot: States,
    flows: FlowBook,
    flow_id: String,
    flow_problems: Vec<String>,
    current: Option<FlowDef>,
    flow_kind: FlowKind,
    graph: Graph,
    flow_first_node: String,
    active: Option<String>,
    mut begin: impl FnMut(bool) + Copy + 'static,
    start: impl FnMut(Event<MouseData>) + Copy + 'static,
    repo_actions: impl Fn(&str, &Key, &probe::Affordance, &Option<String>) -> Element,
    flow_context: Result<VNode, RenderError>,
    flow_strip: Result<VNode, RenderError>,
    props: WorkspaceProps,
) -> Element {
    rsx! {
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
                    flow_kind,
                    status_map.get(&repo),
                    *probing.read() > 0,
                    run_state.started,
                    &key.2,
                    &flow_problems,
                );
                let can_start = !running.read().contains(&key)
                    && other_run.is_none()
                    && can_run.enabled;
                // What starting it would do, in the list view's own
                // words: "Commit 5 files again", "Review #104".
                let start_label = can_run.label.clone();
                let (can_run_actions, other_run_actions) = (can_run.clone(), other_run.clone());
                let start_note = other_run.unwrap_or(can_run.reason);
                // The step whose panel is open, if it is still a step
                // of the flow on screen.
                let open_step = drawer.read().clone().filter(|id| graph.get(id).is_some());
                // What the panel holds: the open step, or while it
                // slides away, the one it was showing.
                let shown_step = open_step
                    .clone()
                    .or_else(|| panel_step.read().clone())
                    .filter(|id| graph.get(id).is_some());
                // A pull request's diff, when that is what the panel
                // holds instead of a step.
                let diff_open = diff_panel.read().clone();
                let diff_pr = diff_open.as_ref().and_then(|n| {
                    status_map
                        .get(&repo)
                        .and_then(|s| s.prs.iter().chain(s.pr.iter()).find(|p| &p.number == n).cloned())
                });
                let diff_state = diff_open
                    .as_ref()
                    .and_then(|n| pr_diffs.read().get(&(repo.clone(), n.clone())).cloned());
                let panel_open = open_step.is_some() || diff_open.is_some();
                // Folded only means anything while it is open.
                let folded = *panel_folded.read() && panel_open;
                let strip_title = match &diff_open {
                    Some(n) => format!("#{n} diff"),
                    None => shown_step
                        .as_ref()
                        .and_then(|id| graph.get(id))
                        .map(|spec| spec.title.clone())
                        .unwrap_or_default(),
                };
                let strip_status = shown_step
                    .as_deref()
                    .map(|id| run_state.status(id))
                    .unwrap_or_default();
                rsx! {
                    RunView {
                        can_start,
                        start_note,
                        start_label,
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
                        panel_open,
                        panel_folded: folded,
                        panel_width: *panel_w.read(),
                        on_panel_width: move |w: Option<f64>| {
                            panel_w.set(w);
                            store::save_layout(&Layout {
                                panel: w,
                                ..store::load_layout()
                            });
                        },
                        flows: flow_strip,
                        context: flow_context,
                        actions: repo_actions(&repo, &key, &can_run_actions, &other_run_actions),
                        // Always there, open or not: a panel that is
                        // only removed cannot slide away, and one that
                        // is only added cannot slide in from where it
                        // last was.
                        div {
                            class: match (panel_open, folded) {
                                (true, true) => "run-drawer run-drawer-open run-drawer-folded",
                                (true, false) => "run-drawer run-drawer-open",
                                _ => "run-drawer",
                            },
                            "aria-hidden": "{!panel_open}",
                            if folded {
                                // The whole strip unfolds it: name and
                                // state, read sideways, are enough to
                                // say which step is waiting there.
                                button {
                                    class: "run-drawer-strip",
                                    title: "Unfold",
                                    onclick: move |_| set_folded(false),
                                    span { class: "run-drawer-unfold", "\u{2039}" }
                                    span { class: "dot dot-{strip_status.css()}" }
                                    span { class: "run-drawer-strip-name", "{strip_title}" }
                                }
                            } else {
                                button {
                                    class: "run-drawer-fold",
                                    title: "Fold to the edge, to see more of the map",
                                    onclick: move |_| set_folded(true),
                                    "\u{203a}"
                                }
                                button {
                                    class: "run-drawer-close",
                                    title: "Close (Esc)",
                                    onclick: move |_| {
                                        drawer.set(None);
                                        diff_panel.set(None);
                                    },
                                    "\u{2715}"
                                }
                                if let Some(number) = diff_open {
                                    div { class: "detail", div { class: "detail-body",
                                        div { class: "detail-head",
                                            span { class: "detail-title",
                                                "#{number} "
                                                {diff_pr.as_ref().map(|p| p.title.clone()).unwrap_or_default()}
                                            }
                                            if let Some(p) = diff_pr.as_ref() {
                                                span { class: "detail-status", "{p.head} \u{2192} {p.base}" }
                                            }
                                        }
                                        match diff_state {
                                            Some(Some(Ok((stat, diff)))) if diff.trim().is_empty() => rsx! {
                                                div { class: "pr-diff-note", "No changes between these branches." }
                                                pre { class: "pr-diff-stat", "{stat}" }
                                            },
                                            Some(Some(Ok((stat, diff)))) => rsx! {
                                                pre { class: "pr-diff-stat", "{stat}" }
                                                DiffView { diff, is_light: *is_light.read() }
                                            },
                                            Some(Some(Err(why))) => rsx! {
                                                pre { class: "log log-error", "{why}" }
                                            },
                                            _ => rsx! {
                                                div { class: "pr-diff-note", "Fetching the diff\u{2026}" }
                                            },
                                        }
                                    }
                                    }
                                } else if let Some(node_id) = shown_step {
                                    {step_detail(wiring, key.clone(), graph.clone(), run_state.clone(), node_id.clone(), *is_light.read(), Some(Panel { drawer, close_when_done }))}
                                }
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
                    flow_kind,
                    status_map.get(&repo),
                    *probing.read() > 0,
                    state.started,
                    &pr_id,
                    &flow_problems,
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
                            {repo_actions(&repo, &key, &can_run, &other_run)}
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

                        {flow_context}

                        div { class: "col-scroll",
                            for node in graph.nodes.iter().cloned() {
                                NodeCard {
                                    key: "{node.id}",
                                    // Its real branches and pull request in
                                    // its description once the run knows
                                    // them, as on the run view's map.
                                    spec: NodeSpec {
                                        subtitle: live_subtitle(node.step, &node.subtitle, &state),
                                        ..node.clone()
                                    },
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
