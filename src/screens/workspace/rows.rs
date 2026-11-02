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
            }
        })
        .collect()
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
