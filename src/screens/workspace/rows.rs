//! What the workspace shows that is worked out from its state rather than
//! kept: the sidebar's rows, and a run started outside the window.

use super::*;
use crate::services::live::LiveRun;
use crate::services::store::{Repo, RepoFlows};

/// One sidebar row per repository, from its last probe and its runs.
pub(super) fn repo_entries(
    repo_list: &[Repo],
    status_map: &BTreeMap<String, RepoStatus>,
    states: &States,
    book: &FlowBook,
    repo_flows: &RepoFlows,
    settling: &BTreeMap<String, crate::components::repo_sidebar::Settle>,
) -> Vec<RepoEntry> {
    repo_list
        .iter()
        .map(|repo| {
            let status = status_map.get(&repo.path);
            RepoEntry {
                path: repo.path.clone(),
                label: repo.label.clone(),
                wants: status.map(|s| s.wants()),
                branch: status.map(|s| s.branch.clone()).unwrap_or_default(),
                detail: status.map(|s| s.summary()).unwrap_or_default(),
                forge: status.map(|s| s.forge.clone()),
                // Several PR-scoped runs can be in flight for this repo+flow at
                // once — the sidebar shows whichever most needs a look, the same
                // way `phase_of` already picks the most urgent status within one.
                // Every flow, not just the one on screen: a review parked at an
                // approval must still show while the commit flow is selected.
                phase: states
                    .iter()
                    .filter(|((r, _, _), _)| *r == repo.path)
                    .map(|(_, s)| phase_of(s))
                    .min_by_key(|p| p.priority())
                    .unwrap_or(Phase::Idle),
                ahead: status.map(|s| s.ahead).unwrap_or(0),
                behind: status.map(|s| s.behind).unwrap_or(0),
                open_pr_count: status.map(|s| s.prs.len()).unwrap_or(0),
                prs_error: status.and_then(|s| s.prs_error.clone()),
                approval: approval_for(states, book, &repo.path),
                start: status.and_then(|s| {
                    row_start(book, s, &repo_flows.hidden_for(&repo.path))
                        .map(|(_, label, _)| label)
                }),
                failed_step: failed_step(states, book, &repo.path, status.map(|s| s.wants())),
                settle: settling.get(&repo.path).copied(),
            }
        })
        .collect()
}

/// The title of the step a failed run here stopped on, when that run is still
/// the answer to what the repository needs — its commit failed and the files
/// are still uncommitted, say.
///
/// Once the repository has moved on (committed by hand, a release now due),
/// the failure is history: the row goes back to what is needed now and only
/// keeps the mark beside the name.
pub(super) fn failed_step(
    states: &States,
    book: &FlowBook,
    repo: &str,
    wants: Option<Wants>,
) -> Option<String> {
    let need = wants?.need()?;
    states
        .iter()
        .filter(|((r, _, _), s)| r == repo && phase_of(s) == Phase::Failed)
        .find_map(|((_, flow, _), state)| {
            let def = book.get(flow).filter(|def| def.answers(need))?;
            let (step, _) = state.newest_for_a_person()?;
            let graph = def.to_graph();
            Some(
                graph
                    .get(&step)
                    .map(|spec| spec.title.clone())
                    .unwrap_or(step),
            )
        })
}

/// A run Claude (or a terminal) is driving on the flow on screen, drawn on
/// its graph as if it were the window's own. A terminal run is read-only; an
/// MCP run's approvals can be answered here as well as in Claude. Not while
/// the window runs something in this repository itself: that run is the one
/// on screen.
pub(super) fn watched_run(
    repo: &str,
    flow_id: &str,
    graph: &Graph,
    running: &BTreeSet<Key>,
    live_runs: &[LiveRun],
) -> Option<Watched> {
    if running.iter().any(|k| k.0 == repo) {
        return None;
    }
    let resolved = dunce::canonicalize(repo)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo.to_string());
    let run = live_runs
        .iter()
        .find(|r| (r.repo == resolved || r.repo == repo) && r.flow == flow_id)?;
    run.as_state(graph).map(|state| Watched {
        state,
        finished: run.finished,
        note: if run.terminal {
            "This run was started in a terminal. Answer it there.".into()
        } else {
            "Claude started this run through GitAgent\u{2019}s MCP server. \
             Answer it here or in Claude."
                .into()
        },
        remote: (!run.terminal).then_some((run.pid, run.run)),
    })
}

/// The run Claude is driving in `repo` that is stopped on an approval, and
/// the id of the step it waits on — what keeps a start here refused for as
/// long as nobody answers it in Claude. The window can answer it instead,
/// which is the way out the refusal offers. Not a terminal run, whose
/// approvals are answered only in its terminal, and not from a server too
/// old to name its steps, whose answer could not be matched to one.
pub(super) fn waiting_blocker(
    repo: &str,
    running: &BTreeSet<Key>,
    live_runs: &[LiveRun],
) -> Option<(LiveRun, String)> {
    if running.iter().any(|k| k.0 == repo) {
        return None;
    }
    let resolved = dunce::canonicalize(repo)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo.to_string());
    live_runs
        .iter()
        .filter(|r| (r.repo == resolved || r.repo == repo) && !r.finished && !r.terminal)
        .find_map(|r| {
            let step = r.steps.iter().find(|s| s.status == "awaiting")?;
            (!step.id.is_empty()).then(|| (r.clone(), step.id.clone()))
        })
}
