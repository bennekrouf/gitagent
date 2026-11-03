//! The repositories down the left.

use super::*;
use crate::components::repo_sidebar::Settle;
use crate::services::store::{Repo, RepoFlows};

#[allow(clippy::too_many_arguments)]
pub(super) fn sidebar(
    workspace: String,
    repos: Signal<Vec<Repo>>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    probing: Signal<usize>,
    picked: Signal<bool>,
    mut states: Signal<States>,
    mut selected_repo: Signal<Option<String>>,
    book: Signal<FlowBook>,
    mut selected_flow: Signal<String>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    running: Signal<BTreeSet<Key>>,
    mut drawer: Signal<Option<String>>,
    mut diff_panel: Signal<Option<String>>,
    repo_flows: Signal<RepoFlows>,
    sidebar_w: Signal<f64>,
    mut settling: Signal<BTreeMap<String, Settle>>,
    all_left: Signal<usize>,
    _flow_id: String,
    entries: Vec<RepoEntry>,
    active: Option<String>,
    mut begin: impl FnMut(bool) + Copy + 'static,
    props: WorkspaceProps,
) -> Element {
    rsx! {
        RepoSidebar {
            entries,
            selected: active.clone(),
            workspace: props.workspace.clone(),
            probing: *probing.read(),
            all_busy: *all_left.read() > 0,
            on_all: move |_| {
                all::press_all(
                    repos, statuses, states, book, repo_flows, running, settling, all_left,
                    selected_repo, selected_flow, selected_pr, selected_node, drawer, begin,
                )
            },
            on_refresh: {
                let workspace = workspace.clone();
                move |_| {
                    // A repository All gave up on is read afresh like the rest.
                    settling.write().retain(|_, s| *s != Settle::StillWriting);
                    forget_finished(&mut states.write(), &running.read(), None);
                    refresh_all(&workspace, repos, statuses, probing, picked, selected_repo, selected_flow, book, true);
                }
            },
            on_approve: move |approval: Approval| {
                all::approve(states, approval, (selected_repo, selected_flow, selected_pr), drawer);
            },
            on_start: move |path: String| {
                let Some((flow_id, _, pr)) = statuses.read().get(&path).and_then(|s| {
                    row_start(&book.read(), s, &repo_flows.read().hidden_for(&path))
                }) else {
                    return;
                };
                let first = book.read().get(&flow_id).map(|f| f.first_node()).unwrap_or_default();
                // Opens the repository on that flow, as a click on
                // the row would, and starts it there — so the run,
                // and any approval it stops at, is in view. A review
                // flow arrives with the pull request the button named
                // already chosen; the others need none.
                selected_repo.set(Some(path));
                selected_flow.set(flow_id);
                selected_node.set(first);
                selected_pr.set(pr);
                drawer.set(None);
                diff_panel.set(None);
                begin(false);
            },
            on_reprobe: move |path: String| {
                if settling.peek().get(&path) == Some(&Settle::StillWriting) {
                    settling.write().remove(&path);
                }
                forget_finished(&mut states.write(), &running.read(), Some(&path));
                reprobe(path, statuses);
            },
            on_select: move |path: String| {
                let (flow_id, node_id, pr_id) =
                    default_selection(
                        &book.read(),
                        &states.read(),
                        &path,
                        statuses
                            .read()
                            .get(&path)
                            .map(|s| book.read().wants_here(s, &repo_flows.read().hidden_for(&path))),
                        statuses
                            .read()
                            .get(&path)
                            .map(|s| s.prs.clone())
                            .unwrap_or_default()
                            .as_slice(),
                        &repo_flows.read().hidden_for(&path),
                    );
                selected_repo.set(Some(path));
                if !flow_id.is_empty() {
                    selected_flow.set(flow_id);
                }
                selected_node.set(node_id);
                selected_pr.set(pr_id);
                // A panel left open would show a step of the
                // repository you just left.
                drawer.set(None);
                diff_panel.set(None);
            },
            on_change_workspace: move |_| props.on_change_workspace.call(()),
            width: *sidebar_w.read(),
        }
    }
}
