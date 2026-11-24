//! The detail pane for the selected step, and what it needs from the workspace.

use super::*;
use crate::services::live;

/// The signals a step's panel acts on. Copy, like the signals themselves, so
/// one value can be handed to every place that shows a step.
#[derive(Clone, Copy)]
pub(super) struct Wiring {
    pub(super) states: Signal<States>,
    pub(super) running: Signal<BTreeSet<Key>>,
    pub(super) selected_node: Signal<String>,
    pub(super) selected_repo: Signal<Option<String>>,
    pub(super) selected_flow: Signal<String>,
    pub(super) selected_pr: Signal<String>,
    pub(super) llm_config: Signal<LlmConfig>,
    pub(super) statuses: Signal<BTreeMap<String, RepoStatus>>,
    pub(super) trusted: Signal<BTreeSet<Key>>,
    /// Starts, on a repository, the flow that answers a need, then the flow
    /// and pull request given: for a fix that is a flow of its own rather
    /// than a command, after which the flow it stopped runs again.
    pub(super) start_flow: Callback<FlowFix>,
}

/// A fix that is a flow: the repository, the need its flow answers, and the
/// flow and pull request to go back to after it.
pub(super) type FlowFix = (String, Need, Option<(String, String)>);

/// One step of one run, with everything you can do to it: approve or reject,
/// pick files, run a fix, retry, skip, cancel. The list view shows it in its
/// right-hand column and the run view in a panel over the map — the same
/// panel, built here once, so the two can never drift apart.
///
/// `panel` is the run view's open panel. Skipping closes it: the run moves on,
/// and so does your attention. Approving keeps it open on the step's log while
/// the step runs, and it closes once the step succeeds — a failure stays in
/// front of you. Reject leaves it open, since that is where you would retry.
/// The list view passes `None`.
/// The run view's panel over the map, as `step_detail` needs it.
#[derive(Clone, Copy)]
pub(super) struct Panel {
    /// The step the panel shows, if it is open.
    pub(super) drawer: Signal<Option<String>>,
    /// A step approved from the panel, by run, whose panel closes when it
    /// succeeds.
    pub(super) close_when_done: Signal<Option<(Key, String)>>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn step_detail(
    wiring: Wiring,
    key: Key,
    graph: Graph,
    state: RunState,
    node_id: String,
    is_light: bool,
    panel: Option<Panel>,
    elsewhere: Option<String>,
    remote: Option<(u32, u64)>,
) -> Element {
    let Wiring {
        mut states,
        mut running,
        selected_node,
        selected_repo,
        selected_flow,
        selected_pr,
        llm_config,
        statuses,
        trusted,
        start_flow,
    } = wiring;
    rsx! {
        DetailPane {
            spec: graph.get(&node_id).cloned(),
            run: state.runs.get(&node_id).cloned().unwrap_or_else(NodeRun::default),
            repo: key.0.clone(),
            diff: graph.get(&node_id).and_then(|spec| {
                spec.writes.iter()
                    .find(|w| w.as_str() == "diff" || w.as_str() == "pr_diff")
                    .and_then(|key| state.artifacts.get(key).cloned())
            }).or_else(|| {
                // `merge` writes no diff of its own, but the one
                // `pr_diff` already fetched earlier in this same
                // run is exactly the code a conflict — or a
                // decision to abandon — is about.
                if node_id == "merge" {
                    state.artifacts.get("pr_diff").cloned()
                } else {
                    None
                }
            }),
            is_light,
            run_started: state.started,
            elsewhere,
            answerable: remote.is_some(),
            on_approve: {
                let key = key.clone();
                move |id: String| {
                    // Claude's run: its own server decides the step.
                    if let Some((pid, run)) = remote {
                        live::answer(live::Answer { pid, run, step: id, approved: true });
                        return;
                    }
                    if let Some(mut panel) = panel {
                        panel.close_when_done.set(Some((key.clone(), id.clone())));
                    }
                    states.write().entry(key.clone()).or_default()
                        .decisions.insert(id, true);
                    approvals().notify_waiters();
                }
            },
            on_reject: {
                let key = key.clone();
                move |id: String| {
                    if let Some((pid, run)) = remote {
                        live::answer(live::Answer { pid, run, step: id, approved: false });
                        return;
                    }
                    states.write().entry(key.clone()).or_default()
                        .decisions.insert(id, false);
                    approvals().notify_waiters();
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

                    // A fix that is a flow: this run has nothing left to do
                    // until that flow has, so it ends here and the flow starts
                    // in its place, then this flow again from the top — on
                    // the pull request it was about, which an empty key
                    // leaves to the checked-out branch.
                    if let Some(need) = remedy.starts {
                        let pr = states.read().get(&key)
                            .map(|s| s.artifact("pr_number").to_string())
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| key.2.clone());
                        states.write().remove(&key);
                        running.write().remove(&key);
                        start_flow.call((key.0.clone(), need, Some((key.1.clone(), pr))));
                        return;
                    }

                    set_remedy(states, &key, &node, index, |r| {
                        r.running = true;
                        r.output.clear();
                    });

                    spawn(async move {
                        // In the repo: `gh pr close 11` from anywhere
                        // else closes #11 of whichever repo that is.
                        let (ok, output) = git::run_streaming(
                            &remedy.program,
                            &remedy.args,
                            Some(&key.0),
                            "",
                            &mut |_| {},
                        )
                        .await;
                        set_remedy(states, &key, &node, index, |r| {
                            r.running = false;
                            r.done = ok;
                            r.failed = !ok;
                            r.output = if output.is_empty() && ok {
                                "done".into()
                            } else {
                                output.clone()
                            };
                        });
                        // Bringing the base in that stopped on conflicts is not a
                        // dead end but the next step: the step says so, and
                        // offers to finish or abandon the merge it started.
                        // Check status offers the same "Bring the base in" as
                        // Merge does, so it must hand over to Finish the same
                        // way: left out, a merge resolved here kept the old
                        // "conflicts with main" under "Nothing is left to
                        // resolve", with no Finish to click.
                        let merging = retry_graph
                            .get(&node)
                            .is_some_and(|n| matches!(n.step, Step::Merge | Step::PrStatus));
                        // Git mid-merge is the truth here, not the output: run
                        // again during the merge, it prints no conflict lines.
                        // Changelogs first: both sides adding notes is the
                        // usual clash, and one GitAgent settles itself.
                        let mut settled = vec![];
                        let stopped = if !ok && merging
                            && git::in_progress(&key.0).await == Some(git::InProgress::Merge)
                        {
                            settled = crate::services::conflicts::changelog::settle_all(&key.0).await;
                            let mut files: Vec<String> = crate::services::conflicts::list(&key.0)
                                .await
                                .into_iter()
                                .map(|c| c.path)
                                .collect();
                            files.extend(crate::services::conflicts::resolved(&key.0).await);
                            files.sort();
                            files.dedup();
                            files
                        } else {
                            vec![]
                        };
                        if !ok && merging {
                            let mut w = states.write();
                            if let Some(entry) = w.get_mut(&key) {
                                let number = entry.artifact("pr_number").to_string();
                                let base = entry.artifact("pr_base").to_string();
                                let files: Vec<&str> = stopped.iter().map(String::as_str).collect();
                                let next = crate::services::review::conflict_followup(&number, &base, &output, &settled)
                                    .or_else(|| crate::services::review::merge_stopped(&number, &base, &files, &settled));
                                if let Some(next) = next {
                                    if let Some(run) = entry.runs.get_mut(&node) {
                                        run.log = next.message;
                                        // Keep what the remedy just printed: a Finish that
                                        // refused (markers left, a change about to be lost)
                                        // otherwise came back as a fresh Run button, with
                                        // its reason gone — a click that seemed to do nothing.
                                        let mut fresh = next.remedies;
                                        if let Some(same) = fresh.iter_mut().find(|r| r.label == remedy.label) {
                                            same.failed = true;
                                            same.output = output.clone();
                                        }
                                        run.remedies = fresh;
                                    }
                                }
                            }
                        }
                        if ok && !remedy.sets.is_empty() {
                            let mut w = states.write();
                            let entry = w.entry(key.clone()).or_default();
                            for (k, v) in &remedy.sets {
                                entry.artifacts.insert(k.clone(), v.clone());
                            }
                        }
                        if ok && remedy.retry_after {
                            // A fix that unblocked this step is only
                            // useful if the run moves on, so re-queue it.
                            retry_node(
                                states, running, selected_node,
                                selected_repo, selected_flow, selected_pr,
                                llm_config, statuses, retry_graph,
                                key, &node, trusted,
                            );
                        } else if ok {
                            // A terminal remedy resolves the failure by
                            // abandoning the step, not by unblocking it —
                            // retrying would just fail again differently.
                            states.write().remove(&key);
                            running.write().remove(&key);
                            reprobe(key.0.clone(), statuses);
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
                        selected_repo, selected_flow, selected_pr,
                        llm_config, statuses, retry_graph.clone(),
                        key.clone(), &node, trusted,
                    );
                }
            },
            on_skip: {
                let key = key.clone();
                let skip_graph = graph.clone();
                move |node: String| {
                    skip_node(
                        states, running, selected_node,
                        selected_repo, selected_flow, selected_pr,
                        llm_config, statuses, skip_graph.clone(),
                        key.clone(), &node, trusted,
                    );
                    if let Some(mut panel) = panel {
                        panel.drawer.set(None);
                    }
                }
            },
            on_cancel: {
                let key = key.clone();
                move |_| {
                    states.write().remove(&key);
                    running.write().remove(&key);
                    // The repository is free the moment the run is
                    // cancelled, not when its task next wakes to notice.
                    if !running.read().iter().any(|k| k.0 == key.0) {
                        crate::services::busy::release_here(&key.0);
                    }
                    // A run parked at an approval
                    // hears about it now, not on its
                    // next timeout.
                    approvals().notify_waiters();
                    reprobe(key.0.clone(), statuses);
                }
            },
        }
    }
}
