//! The sidebar's All button: every row's button pressed, carefully.
//!
//! The rows are labelled from the last read of each repository, which can be
//! minutes old, and a coding agent may still be writing files in any of them.
//! So All reads every repository again, and where there are uncommitted files
//! it waits for them to stop changing, reads that repository once more, and
//! only then decides what to press. A repository whose files never settle is
//! left alone, and says so.

use std::sync::Arc;

use super::*;
use crate::components::repo_sidebar::Settle;
use crate::services::quiet::{self, Quiet};
use crate::services::store::{Repo, RepoFlows};

/// How many repositories are read from the forge at once — the same bound,
/// and for the same reason, as a Refresh.
const AT_ONCE: usize = 6;

/// The selection, to put back after a start: All works through the list in
/// the background and must not leave you somewhere you were not looking.
type Selection = (Option<String>, String, String, String);

#[allow(clippy::too_many_arguments)]
pub(super) fn press_all(
    repos: Signal<Vec<Repo>>,
    mut statuses: Signal<BTreeMap<String, RepoStatus>>,
    states: Signal<States>,
    book: Signal<FlowBook>,
    repo_flows: Signal<RepoFlows>,
    running: Signal<BTreeSet<Key>>,
    mut settling: Signal<BTreeMap<String, Settle>>,
    mut all_left: Signal<usize>,
    mut selected_repo: Signal<Option<String>>,
    mut selected_flow: Signal<String>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    drawer: Signal<Option<String>>,
    begin: impl FnMut(bool) + Copy + 'static,
) {
    if *all_left.peek() > 0 {
        return;
    }
    // A repository given up on last time gets another chance.
    settling.write().clear();
    let paths: Vec<String> = repos.peek().iter().map(|r| r.path.clone()).collect();
    all_left.set(paths.len());
    let forge = Arc::new(tokio::sync::Semaphore::new(AT_ONCE));

    for path in paths {
        let forge = forge.clone();
        spawn(async move {
            let mut begin = begin;
            let read = |path: String| {
                let forge = forge.clone();
                async move {
                    let _turn = forge.acquire().await;
                    let status = probe::probe(&path).await;
                    statuses.write().insert(path, status.clone());
                    status
                }
            };

            let act = async {
                // The window is already running something here; its own
                // buttons are the ones that apply.
                if running.peek().iter().any(|k| k.0 == path) {
                    return;
                }
                let mut status = read(path.clone()).await;
                if status.changes > 0 {
                    settling
                        .write()
                        .insert(path.clone(), Settle::Waiting(quiet::WINDOW.as_secs()));
                    let verdict = quiet::wait_until_quiet(&path, |left| {
                        settling.write().insert(path.clone(), Settle::Waiting(left));
                    })
                    .await;
                    if verdict == Quiet::StillWriting {
                        settling.write().insert(path.clone(), Settle::StillWriting);
                        return;
                    }
                    // What it needs may have changed while it was written.
                    status = read(path.clone()).await;
                }
                settling.write().remove(&path);

                // Bound first: a read of `states` still alive across the
                // await below would make every write to it fail meanwhile.
                let approval = approval_for(&states.peek(), &book.peek(), &path);
                if let Some(approval) = approval {
                    let posted = posted_at(&states.peek(), &approval);
                    if let Some(why) = changed_since(posted, &path).await {
                        held_back(states, &approval, why);
                    } else {
                        approve(
                            states,
                            approval,
                            (selected_repo, selected_flow, selected_pr),
                            drawer,
                        );
                    }
                    return;
                }
                let failed = {
                    let wants = book
                        .peek()
                        .wants_here(&status, &repo_flows.peek().hidden_for(&path));
                    rows::failed_step(&states.peek(), &book.peek(), &path, Some(wants))
                };
                let live = states
                    .peek()
                    .iter()
                    .any(|((r, _, _), s)| *r == path && phase_of(s).is_live());
                if failed.is_some() || live || running.peek().iter().any(|k| k.0 == path) {
                    return;
                }
                let Some((flow_id, _, pr)) =
                    row_start(&book.peek(), &status, &repo_flows.peek().hidden_for(&path))
                else {
                    return;
                };
                let first = book
                    .peek()
                    .get(&flow_id)
                    .map(|f| f.first_node())
                    .unwrap_or_default();
                let before: Selection = (
                    selected_repo.peek().clone(),
                    selected_flow.peek().clone(),
                    selected_pr.peek().clone(),
                    selected_node.peek().clone(),
                );
                selected_repo.set(Some(path.clone()));
                selected_flow.set(flow_id);
                selected_node.set(first);
                selected_pr.set(pr);
                begin(false);
                // The run is keyed by repository, flow and pull request, so
                // it carries on whatever is on screen.
                if before.0.as_ref().is_some_and(|was| *was != path) {
                    selected_repo.set(before.0);
                    selected_flow.set(before.1);
                    selected_pr.set(before.2);
                    selected_node.set(before.3);
                }
            };
            act.await;
            let left = *all_left.peek();
            all_left.set(left.saturating_sub(1));
        });
    }
}

fn posted_at(states: &States, approval: &Approval) -> Option<std::time::SystemTime> {
    states
        .get(&approval.run)
        .and_then(|s| s.runs.get(&approval.node))
        .and_then(|r| r.posted_at)
}

/// Why an approval posted at `posted` must not be clicked through any more,
/// if it must not: a file here changed after it was put in front of you, so
/// what it would do is not what it said.
async fn changed_since(posted: Option<std::time::SystemTime>, repo: &str) -> Option<String> {
    let posted = posted?;
    let newest = quiet::snapshot(repo).await.ok()?.newest()?;
    (newest > posted).then(|| {
        "Files changed after this was proposed, so All left it for you: what it lists may \
         not be what is on disk now. Check it, or Retry the scan to read the files again."
            .to_string()
    })
}

/// Leaves an approval for the person, with the reason at the top of its
/// button's tooltip and on the step.
fn held_back(mut states: Signal<States>, approval: &Approval, why: String) {
    if let Some(state) = states.write().get_mut(&approval.run) {
        if state.status(&approval.node) == NodeStatus::AwaitingApproval {
            state.runs.entry(approval.node.clone()).or_default().held = why;
        }
    }
}

/// Says yes to a step from the list — what the detail pane's button writes,
/// and only if the step is still the one waiting: the list can be a render
/// behind a run that moved on.
pub(super) fn approve(
    mut states: Signal<States>,
    approval: Approval,
    shown: (Signal<Option<String>>, Signal<String>, Signal<String>),
    mut drawer: Signal<Option<String>>,
) {
    let mut w = states.write();
    let Some(state) = w.get_mut(&approval.run) else {
        return;
    };
    if state.status(&approval.node) != NodeStatus::AwaitingApproval {
        return;
    }
    // As the pane's own button does: a panel left open on the step just
    // approved closes with it.
    let on_screen: Key = (
        shown.0.peek().clone().unwrap_or_default(),
        shown.1.peek().clone(),
        shown.2.peek().clone(),
    );
    if on_screen == approval.run && drawer.peek().as_deref() == Some(approval.node.as_str()) {
        drawer.set(None);
    }
    state.decisions.insert(approval.node, true);
    approvals().notify_waiters();
}
