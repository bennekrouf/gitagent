//! The strip of flows above the run: one tab per flow the repository can run.

use super::*;
use crate::services::store::{Repo, RepoFlows};

#[allow(clippy::too_many_arguments)]
pub(super) fn flow_strip(
    _book: Signal<FlowBook>,
    mut selected_flow: Signal<String>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    mut drawer: Signal<Option<String>>,
    mut diff_panel: Signal<Option<String>>,
    mut repo_flows: Signal<RepoFlows>,
    mut confirm_hide: Signal<Option<(String, String)>>,
    mut picker_open: Signal<Option<String>>,
    mut hide_flow: impl FnMut(&str, &str) + Copy + 'static,
    repo_list: Vec<Repo>,
    status_map: BTreeMap<String, RepoStatus>,
    states_snapshot: States,
    flows: FlowBook,
    listed: Vec<(String, String, Vec<String>)>,
    flow_id: String,
    _graph: Graph,
    active: Option<String>,
) -> Element {
    match active.clone() {
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
            // Where each flow's run stands here, so the tabs answer "what is
            // going on" without opening each one.
            let runs: BTreeMap<String, FlowRun> = visible_tabs
                .iter()
                .filter_map(|(id, _, _)| {
                    let graph = flows.get(id)?.to_graph();
                    flow_run(&states_snapshot, &repo, id, &graph).map(|run| (id.clone(), run))
                })
                .collect();
            // The most urgent run in another flow than the one on screen —
            // waiting for you, failed, or under way — pointed to under the
            // tabs, since its tab alone is easy to miss.
            let pointer = runs
                .iter()
                .filter(|(id, run)| {
                    **id != flow_id
                        && matches!(
                            run.phase,
                            Phase::NeedsApproval | Phase::Failed | Phase::Running
                        )
                })
                .min_by_key(|(_, run)| run.phase.priority())
                .map(|(id, run)| {
                    let name = visible_tabs
                        .iter()
                        .find(|(t, _, _)| t == id)
                        .map(|(_, l, _)| l.clone())
                        .unwrap_or_else(|| id.clone());
                    (id.clone(), name, run.clone())
                });
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
                                        diff_panel.set(None);
                                    }
                                },
                                if !problems.is_empty() {
                                    span { class: "flow-tab-warn", "\u{26a0}" }
                                }
                                "{label}"
                                // What this flow would act on right now: files
                                // to commit, pull requests to review, commits
                                // to release. Nothing when there is nothing.
                                if let Some((count, why)) = flows
                                    .get(&id)
                                    .and_then(|f| status_map.get(&repo).and_then(|s| s.count_for(&f.handles)))
                                {
                                    span { class: "flow-tab-count", title: "{why}", "{count}" }
                                }
                                // The name on one line, where its run stands
                                // on the next — dot first, so a narrow tab
                                // never strands the dot on a line of its own.
                                if let Some(run) = runs.get(&id) {
                                    span { class: "flow-tab-note status-{run.phase.css()}", title: "{run.note}",
                                        span { class: "flow-tab-state flow-tab-state-{run.phase.css()}" }
                                        "{run.note}"
                                    }
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
                    // Always there, and always saying what it is —
                    // "0 hidden" rather than a bare "⋯": the × only
                    // appears on hover, so this is how someone learns
                    // the strip is theirs to edit at all.
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
                        } else {
                            "{hidden_count} hidden"
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
                if let Some((flow, name, run)) = pointer {
                    div { class: "flow-elsewhere flow-elsewhere-{run.phase.css()}",
                        span { class: "flow-elsewhere-mark", {run.phase.icon()} }
                        span { class: "flow-elsewhere-text",
                            "{name}"
                            if !run.pr.is_empty() { " #{run.pr}" }
                            " \u{2014} {run.note}"
                        }
                        button {
                            class: "btn flow-elsewhere-go",
                            title: "Switch to this flow, at the step where it is",
                            onclick: move |_| {
                                selected_flow.set(flow.clone());
                                selected_pr.set(run.pr.clone());
                                selected_node.set(run.step.clone());
                                drawer.set(None);
                                diff_panel.set(None);
                            },
                            "Show"
                        }
                    }
                }
            }
        }
    }
}
