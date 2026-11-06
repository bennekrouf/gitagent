//! What the middle of the window shows about the selected repository: its flow,
//! its run, its pull request.

use super::*;
use crate::services::live::{self, LiveRun};
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
    on_graph: bool,
    status_map: BTreeMap<String, RepoStatus>,
    flow_id: String,
    flow_problems: Vec<String>,
    flow_kind: FlowKind,
    _graph: Graph,
    active: Option<String>,
    selected_flow: Signal<String>,
    begin: impl FnMut(bool) + Copy + 'static,
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
            // Drawn on the graph below already: what the waiting step would
            // do is there, so only who runs it is said.
            let on_graph = on_graph && live.as_ref().is_some_and(|l| l.flow == flow_id);
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
                    if let Some((title, proposal)) = live.waiting.clone() {
                        div { class: "live-run-wait",
                            if live.terminal {
                                "{title} is waiting for approval \u{2014} answer it in the terminal where the run was started."
                            } else {
                                "{title} is waiting for approval \u{2014} answer it here or in Claude."
                            }
                        }
                        if !on_graph && !proposal.trim().is_empty() {
                            pre { class: "live-run-proposal", "{proposal}" }
                        }
                        // Claude's server answers what the window writes, so
                        // its run never needs the Claude session to move on —
                        // and Take over hands the repository to this window
                        // altogether, for when that session is not coming
                        // back to it.
                        if let Some(step) = waiting_step(&live) {
                            div { class: "live-run-actions",
                                button {
                                    class: "btn btn-primary",
                                    title: "Approve this step here. Claude\u{2019}s run carries on from it.",
                                    onclick: {
                                        let (pid, run, step) = (live.pid, live.run, step.clone());
                                        move |_| live::answer(live::Answer { pid, run, step: step.clone(), approved: true })
                                    },
                                    "Approve"
                                }
                                button {
                                    class: "btn",
                                    title: "Reject this step here. Nothing is run, and Claude\u{2019}s run ends.",
                                    onclick: {
                                        let (pid, run, step) = (live.pid, live.run, step.clone());
                                        move |_| live::answer(live::Answer { pid, run, step: step.clone(), approved: false })
                                    },
                                    "Reject"
                                }
                                button {
                                    class: "btn",
                                    title: "End Claude\u{2019}s run and start {live.flow_label} again in this window, where you answer it from now on. Steps it already did are run again.",
                                    onclick: {
                                        let live = live.clone();
                                        let repo = repo.clone();
                                        move |_| take_over(live.clone(), step.clone(), repo.clone(), selected_flow, selected_pr, selected_node, begin)
                                    },
                                    "Take over"
                                }
                            }
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
                                                PrCard { pr: pr.clone(), repo: repo.clone() }
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
                    PrCard { pr: pr.clone(), repo: repo.clone() }
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

/// The id of the step a run Claude is driving waits on, when the window can
/// answer it: not a terminal run's, and not from a server too old to name its
/// steps, whose answer could not be matched to one.
pub(super) fn waiting_step(live: &LiveRun) -> Option<String> {
    if live.terminal || live.finished {
        return None;
    }
    live.steps
        .iter()
        .find(|s| s.status == "awaiting" && !s.id.is_empty())
        .map(|s| s.id.clone())
}

/// How long a take-over waits for Claude's server to let the repository go.
/// Its server reads the window's answers every second.
const TAKE_OVER_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// Ends Claude's run and starts its flow in this window instead.
///
/// The run lives in Claude's server, a process of its own that the window
/// cannot drive, so it cannot be moved across as it stands. What can be done
/// is to turn its waiting step down — which ends it and lets the repository
/// go — and run the same flow here, from the top. The steps already done are
/// done again; for a release that means the notes are drafted again, which
/// is the price of not depending on that Claude session any longer.
fn take_over(
    live: LiveRun,
    step: String,
    repo: String,
    mut selected_flow: Signal<String>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    mut begin: impl FnMut(bool) + Copy + 'static,
) {
    live::answer(live::Answer {
        pid: live.pid,
        run: live.run,
        step,
        approved: false,
    });
    spawn(async move {
        let since = std::time::Instant::now();
        while crate::services::busy::held_elsewhere(&repo) && since.elapsed() < TAKE_OVER_WAIT {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        selected_flow.set(live.flow.clone());
        // The pull request a review was about is not in what the server
        // writes; empty falls back to the checked-out branch's own.
        selected_pr.set(String::new());
        selected_node.set(String::new());
        // Refused, if Claude's server still has not let go: the start says
        // so, and why, as any other start would.
        begin(false);
    });
}
