//! What the middle of the window shows about the selected repository: its flow,
//! its run, its pull request.

use super::*;
use crate::services::live::LiveRun;
use crate::services::probe::FlowKind;

#[allow(clippy::too_many_arguments)]
pub(super) fn flow_context(
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    running: Signal<BTreeSet<Key>>,
    mut pr_fix: Signal<BTreeMap<String, (bool, String)>>,
    _trusted: Signal<BTreeSet<Key>>,
    run_view: Signal<bool>,
    mut drawer: Signal<Option<String>>,
    mut diff_panel: Signal<Option<String>>,
    mut open_diff: impl FnMut(String, probe::PrBrief) + Copy + 'static,
    mut setup_open: Signal<bool>,
    live_runs: Signal<Vec<LiveRun>>,
    status_map: BTreeMap<String, RepoStatus>,
    flow_id: String,
    flow_problems: Vec<String>,
    flow_kind: FlowKind,
    _graph: Graph,
    active: Option<String>,
) -> Element {
    match active.clone() {
        None => rsx! {},
        Some(repo) => {
            let pr_id = selected_pr.read().clone();
            // The MCP server names a repository by its resolved path.
            let resolved = dunce::canonicalize(&repo)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| repo.clone());
            let live = live_runs
                .read()
                .iter()
                .find(|r| r.repo == resolved || r.repo == repo)
                .cloned();
            rsx! {
            // A run Claude started through GitAgent's MCP server, or one
            // started with `gitagent run`, as it goes. Read-only: its
            // approvals are answered where it was started.
            if let Some(live) = live {
                div { class: "live-run",
                    div { class: "live-run-head",
                        span { class: if live.finished { "live-dot live-dot-done" } else { "live-dot" } }
                        if live.terminal {
                            if live.finished {
                                "Ran {live.flow_label} here"
                            } else {
                                "Running {live.flow_label} here"
                            }
                            span { class: "live-run-via", "from a terminal" }
                        } else {
                            if live.finished {
                                "Claude ran {live.flow_label} here"
                            } else {
                                "Claude is running {live.flow_label} here"
                            }
                            span { class: "live-run-via", "through GitAgent\u{2019}s MCP server" }
                        }
                        if live.trusted && !live.finished {
                            span {
                                class: "live-run-trusted",
                                title: "Approvals are answered by the Trusted run's rules; it stops at anything they would hold",
                                "trusted"
                            }
                        }
                    }
                    div { class: "live-run-steps",
                        for (i, step) in live.steps.iter().cloned().enumerate() {
                            div { key: "{i}", class: "live-step",
                                span { class: "dot dot-{step.status}" }
                                span { class: "live-step-title", "{step.title}" }
                                span { class: "live-step-summary",
                                    if step.summary.is_empty() { "{step.status_label}" } else { "{step.summary}" }
                                }
                            }
                        }
                    }
                    if let Some((title, proposal)) = live.waiting.clone() {
                        div { class: "live-run-wait",
                            if live.terminal {
                                "{title} is waiting for approval \u{2014} answer it in the terminal where the run was started."
                            } else {
                                "{title} is waiting for approval \u{2014} answer it in Claude, where the run was started."
                            }
                        }
                        if !proposal.trim().is_empty() {
                            pre { class: "live-run-proposal", "{proposal}" }
                        }
                    }
                }
            }

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
            if flow_kind == probe::FlowKind::Review {
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
                                                        // Its step belongs to the run of the
                                                        // pull request just left.
                                                        drawer.set(None);
                                                        diff_panel.set(None);
                                                    }
                                                },
                                                PrCard { pr: pr.clone() }
                                                // The one being reviewed can be read in
                                                // full beside the map, in the run view.
                                                if pr.number == pr_id && *run_view.read() {
                                                    button {
                                                        class: "btn pr-diff-btn",
                                                        title: "Show this pull request's diff beside the map",
                                                        onclick: {
                                                            let repo = repo.clone();
                                                            let pr = pr.clone();
                                                            move |e: Event<MouseData>| {
                                                                e.stop_propagation();
                                                                open_diff(repo.clone(), pr.clone());
                                                            }
                                                        },
                                                        "Diff"
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
            } else if let Some(pr) = status_map.get(&repo).and_then(|s| s.pr.clone()) {
                div { class: "pr-solo",
                    PrCard { pr: pr.clone() }
                    if *run_view.read() {
                        button {
                            class: "btn pr-diff-btn",
                            title: "Show this pull request's diff beside the map",
                            onclick: {
                                let repo = repo.clone();
                                move |_| open_diff(repo.clone(), pr.clone())
                            },
                            "Diff"
                        }
                    }
                }
            }            }
        }
    }
}
