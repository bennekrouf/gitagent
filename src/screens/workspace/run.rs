//! Running a flow on one repository: driving its steps, answering approvals,
//! and retrying, skipping or re-probing a step.

use super::*;

/// The base branch to use for the Branches panel — the per-repo override if
/// one is set, else whatever auto-detection finds, same fallback preflight
/// itself uses.
pub(super) async fn resolved_base(repo: &str, override_base: Option<String>) -> String {
    probe::base_branch(repo, override_base).await.0
}

/// Raises an OS notification when a run stops for a person and the window is
/// not in front of them.
///
/// The repository is named by its folder rather than its full path — a
/// notification is two short lines, and `/Users/…/code/ais-runner` spends all
/// of them saying nothing.
pub(super) fn announce(status: NodeStatus, repo_path: &str, node_title: &str, detail: &str) {
    if !notify::should_notify(status, notify::window_focused()) {
        return;
    }
    let repo = std::path::Path::new(repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| repo_path.to_string());

    let (summary, body) = notify::message(status, &repo, node_title, detail);
    notify::raise(summary, body);
}

/// The approval a repository's row can offer as a button, if any.
///
/// Any step waiting on a yes — including one a trusted run held back, such as
/// release notes or a script, whose reason goes at the top of the button's
/// tooltip. Only a step with every file unchecked is left out: it has nothing
/// to commit, and keeps the plain "needs you" that sends you to the run.
pub(super) fn approval_for(states: &States, book: &FlowBook, repo: &str) -> Option<Approval> {
    states
        .iter()
        .filter(|((r, _, _), _)| r == repo)
        .find_map(|(key, state)| {
            let def = book.get(&key.1)?;
            let graph = def.to_graph();
            let node = graph.nodes.iter().find(|n| {
                state.status(&n.id) == NodeStatus::AwaitingApproval
                    && !state.decisions.contains_key(&n.id)
            })?;
            let run = state.runs.get(&node.id)?;
            if run.has_nothing_selected() {
                return None;
            }
            Some(Approval {
                run: key.clone(),
                node: node.id.clone(),
                title: node.title.clone(),
                proposal: run.proposal.clone(),
                held: run.held.clone(),
            })
        })
}

pub(super) fn snapshot(states: &Signal<States>, key: &Key) -> RunState {
    states.read().get(key).cloned().unwrap_or_default()
}

/// Woken whenever an approval decision is recorded, or a run's trusted flag
/// changes — the two things a node parked in `AwaitingApproval` is waiting to
/// hear about.
///
/// Before this, the wait was a 120 ms poll: a run left on an approval
/// overnight woke eight times a second, taking a borrow of every run's state
/// each time, and never went idle. One notifier for the whole process rather
/// than one per run, because a wake only costs a re-read of state the loop
/// looks at anyway — routing them precisely would be more bookkeeping than
/// the spurious wakes are worth.
pub(super) fn approvals() -> &'static tokio::sync::Notify {
    static APPROVALS: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    APPROVALS.get_or_init(tokio::sync::Notify::new)
}

/// Walks one flow, for one repository, to completion.
///
/// Returns `false` when the run was cancelled under it — or cancelled and
/// started afresh — so the caller knows the run, and the `running` entry,
/// belong to someone else now.
///
/// One node at a time, in dependency order. The graph already permits running
/// the whole ready set together — `next_ready` returns the first of a set, not
/// the next link in a chain — so making this concurrent is a change to this
/// function alone. Across repositories it already is concurrent: each call gets
/// its own task and writes only its own key.
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive(
    graph: Graph,
    key: Key,
    cfg: Signal<LlmConfig>,
    mut states: Signal<States>,
    mut selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    mut trusted: Signal<BTreeSet<Key>>,
) -> bool {
    let repo = key.0.clone();
    // Cancel run drops the run's state; a new Start puts a fresh one in its
    // place. Either way the run this call was driving is gone, and carrying
    // on would run its steps — model calls included — for nobody, or worse,
    // into the new run.
    let run = snapshot(&states, &key).run;
    let gone = {
        let key = key.clone();
        move || states.read().get(&key).map(|s| s.run) != Some(run)
    };

    loop {
        if gone() {
            return false;
        }
        let state = snapshot(&states, &key);
        let Some(node) = state.next_ready(&graph) else {
            break;
        };

        // Only steer the selection when this run is the one on screen;
        // otherwise a background run would yank the view around.
        let viewing = selected_repo.read().as_deref() == Some(repo.as_str())
            && *selected_flow.read() == key.1
            && *selected_pr.read() == key.2;
        if viewing {
            selected_node.set(node.id.clone());
        }

        if runner::needs_approval(&node, &state) {
            let state = state.resolved_for(&node);
            let proposal = runner::Proposal::read(&node, &repo, &state).await;
            proposal.post(states.write().entry(key.clone()).or_default(), &node.id);

            // The run has stopped and cannot continue without a person. If they
            // are not looking at the window, say so.
            announce(NodeStatus::AwaitingApproval, &key.0, &node.title, "");

            // A trusted run answers the approval by writing the same decision
            // the button writes, after a beat long enough to read the proposal
            // it is agreeing to. Going through `decisions` rather than short-
            // circuiting the wait is deliberate: there is exactly one path an
            // approval can take, so what you watch happen on screen is what
            // actually happened.
            let mut auto: Option<trusted::Verdict> = None;
            let approved = loop {
                // Registered before the state is read, not after. `enable`
                // puts this future in the waiter list now rather than on
                // first poll, so a decision written in the window between
                // the read below and the park at the bottom still wakes it
                // — the lost-wakeup race every condition-variable wait has.
                let waiter = approvals().notified();
                tokio::pin!(waiter);
                waiter.as_mut().enable();

                // `None` breaks out as "bypassed": somebody pressed Skip
                // while this sat here, so there is no decision coming and
                // nothing to run. Reading the status rather than inventing a
                // third decision value is what makes Skip behave identically
                // whether the run is still waiting here or has already
                // finished and failed.
                let (bypassed, decision) = {
                    let snapshot = states.read();
                    let entry = snapshot.get(&key);
                    (
                        entry.map(|s| s.status(&node.id)) == Some(NodeStatus::Bypassed),
                        entry.and_then(|s| s.decisions.get(&node.id).copied()),
                    )
                };
                if bypassed {
                    break None;
                }
                if gone() {
                    return false;
                }
                if let Some(decision) = decision {
                    break Some(decision);
                }

                if auto.is_none() && trusted.read().contains(&key) {
                    let verdict = trusted::decide(&node, &state);
                    if let Some(why) = verdict.reason() {
                        // It stopped, so it is no longer trusting: the button
                        // goes back to offering it, and the rest of the run is
                        // the person's again. Resuming on their approval and
                        // carrying on clicking would be the one behaviour that
                        // makes "it stopped for you" untrue.
                        states
                            .write()
                            .entry(key.clone())
                            .or_default()
                            .runs
                            .entry(node.id.clone())
                            .or_default()
                            .held = why.to_string();
                        trusted.write().remove(&key);
                        // No second notification: becoming `AwaitingApproval`
                        // already sent one, and the reason is on the node.
                    }
                    auto = Some(verdict);
                }

                if auto == Some(trusted::Verdict::Approve) {
                    // Long enough to see the node light up and read what it is
                    // about to do, short enough that a whole flow still feels
                    // like one action.
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    // Still trusted, and still nobody else's decision: a person
                    // who hit Stop during that pause gets their run back.
                    if !trusted.read().contains(&key) {
                        auto = None;
                        continue;
                    }
                    let mut w = states.write();
                    let entry = w.entry(key.clone()).or_default();
                    entry.decisions.entry(node.id.clone()).or_insert(true);
                    continue;
                }

                // Nothing left but to wait for a person. The timeout is a
                // safety net rather than the mechanism: if a wake is ever
                // missed the run resumes late instead of never.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(30), waiter).await;
            };

            let Some(approved) = approved else {
                // `bypass` already set the status and freed whatever was
                // blocked behind it, so there is nothing to do here but move
                // on to the next ready node.
                continue;
            };
            telemetry::record(telemetry::Event::ApprovalDecided {
                approved,
                trusted: auto == Some(trusted::Verdict::Approve),
            });
            if !approved {
                states
                    .write()
                    .entry(key.clone())
                    .or_default()
                    .reject(&node.id, &graph);
                continue;
            }
        }

        states
            .write()
            .entry(key.clone())
            .or_default()
            .set_status(&node.id, NodeStatus::Running);

        // No model, no focused review, no second reviewer: settled here,
        // once, rather than in each step that would have used one.
        if runner::bypass_unconfigured(
            states.write().entry(key.clone()).or_default(),
            &node,
            &cfg.read(),
        ) {
            continue;
        }

        // As this node sees it: bound inputs already resolved to the
        // producer each one names, so the step reads its own literal keys.
        let state = snapshot(&states, &key).resolved_for(&node);
        let cfg_snapshot = cfg.read().clone();

        // Fills in the node's log as its command's output arrives, rather
        // than only once the whole thing finishes — the point of streaming
        // at all. `result`'s own `outcome.log`/`failure.message` still wins
        // once the step settles, so formatting (a placeholder for empty
        // output, the failing command echoed back) stays exactly as before.
        // The live log is a preview, and only a preview: whatever the step
        // finally returns replaces `run.log` wholesale below, on both the
        // success and the failure path. That is what makes coalescing the
        // writes and capping the growth here free at the end and worth a lot
        // in the middle — one signal write per line meant one full Dioxus
        // render per line, each cloning the entire run map and re-running
        // syntect over the diff. `cargo test` on a real project emits
        // thousands of lines, so the cost was quadratic in output length.
        const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(100);
        /// Enough to watch a command work. The whole output still arrives
        /// when the step settles.
        const LIVE_LOG_CAP: usize = 200_000;

        let mut pending = String::new();
        // Starts "due", so a step's first line shows at once. A model step
        // says what it is waiting on and then nothing for a while; held back
        // until a second line, that one line is the one you never saw.
        let mut last_flush = std::time::Instant::now()
            .checked_sub(FLUSH_EVERY)
            .unwrap_or_else(std::time::Instant::now);
        let mut push_line = {
            let mut states = states;
            let key = key.clone();
            let node_id = node.id.clone();
            move |line: &str| {
                if !pending.is_empty() {
                    pending.push('\n');
                }
                pending.push_str(line);
                if last_flush.elapsed() < FLUSH_EVERY {
                    return;
                }
                last_flush = std::time::Instant::now();

                let mut w = states.write();
                let entry = w.entry(key.clone()).or_default();
                let run = entry.runs.entry(node_id.clone()).or_default();
                if !run.log.is_empty() {
                    run.log.push('\n');
                }
                run.log.push_str(&pending);
                pending.clear();

                // Keep the tail: the end of a log is the part worth watching,
                // and an unbounded one is a step away from a looping script
                // eating the heap.
                if run.log.len() > LIVE_LOG_CAP {
                    let mut cut = run.log.len() - LIVE_LOG_CAP;
                    while cut < run.log.len() && !run.log.is_char_boundary(cut) {
                        cut += 1;
                    }
                    run.log.drain(..cut);
                }
            }
        };
        // Racing the step against a skip, rather than only awaiting it, is what
        // makes Skip work on a step that is *already running* — which is the
        // case that matters, because a model call that will take fifteen
        // minutes is exactly the one you want to abandon. Dropping the future
        // cancels it: an in-flight request is dropped, and `git` children are
        // spawned `kill_on_drop`.
        let result = {
            let step = flow::execute(&node, &repo, &cfg_snapshot, &state, &mut push_line);
            tokio::pin!(step);
            loop {
                tokio::select! {
                    // Biased so a step that has finished is never discarded in
                    // favour of a skip that arrived in the same breath.
                    biased;
                    settled = &mut step => break Some(settled),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                        let bypassed = states
                            .read()
                            .get(&key)
                            .map(|s| s.status(&node.id))
                            == Some(NodeStatus::Bypassed);
                        if bypassed || gone() {
                            break None;
                        }
                    }
                }
            }
        };
        if gone() {
            return false;
        }
        let Some(result) = result else {
            // Skipped mid-flight. `bypass` has already set the status and freed
            // whatever was blocked behind it, and the log it wrote so far is
            // kept as the account of how far it got.
            continue;
        };

        if let Err(failure) = &result {
            announce(NodeStatus::Failed, &key.0, &node.title, &failure.message);
        }
        runner::settle(
            states.write().entry(key.clone()).or_default(),
            &graph,
            &node.id,
            result,
        );
    }

    let finished = snapshot(&states, &key);
    release::record_shipped(&key.0, &key.1, &graph, &finished).await;
    runner::report_finished(&key.0, &key.1, &graph, &finished).await;
    true
}

pub(super) fn set_remedy(
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
pub(super) fn retry_node(
    mut states: Signal<States>,
    mut running: Signal<BTreeSet<Key>>,
    selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    cfg: Signal<LlmConfig>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    graph: Graph,
    key: Key,
    node: &str,
    trusted: Signal<BTreeSet<Key>>,
) {
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
        if !drive(
            graph,
            key.clone(),
            cfg,
            states,
            selected_node,
            selected_repo,
            selected_flow,
            selected_pr,
            trusted,
        )
        .await
        {
            return;
        }
        running.write().remove(&key);
        reprobe(key.0.clone(), statuses);
    });
}

/// Marks a node bypassed and lets the run carry on without it.
///
/// Shares its shape with `retry_node` because it answers the same shape of
/// question — a settled node and what to do about it — and differs in one
/// place: the node ends `Bypassed` rather than back in the queue.
///
/// Both entry points land here. A node still sitting at its approval has a
/// driver waiting on it, which sees the new status and moves on, so the
/// `running` guard below is what stops a second one being started. A node that
/// failed has no driver left, so this starts one.
#[allow(clippy::too_many_arguments)]
pub(super) fn skip_node(
    mut states: Signal<States>,
    mut running: Signal<BTreeSet<Key>>,
    selected_node: Signal<String>,
    selected_repo: Signal<Option<String>>,
    selected_flow: Signal<String>,
    selected_pr: Signal<String>,
    cfg: Signal<LlmConfig>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
    graph: Graph,
    key: Key,
    node: &str,
    trusted: Signal<BTreeSet<Key>>,
) {
    states
        .write()
        .entry(key.clone())
        .or_default()
        .bypass(node, &graph);
    // Wake the waiting driver the same way an approval does, so a skip takes
    // effect immediately rather than on the next timeout tick.
    approvals().notify_waiters();

    if running.read().contains(&key) {
        return;
    }
    running.write().insert(key.clone());
    spawn(async move {
        if !drive(
            graph,
            key.clone(),
            cfg,
            states,
            selected_node,
            selected_repo,
            selected_flow,
            selected_pr,
            trusted,
        )
        .await
        {
            return;
        }
        running.write().remove(&key);
        reprobe(key.0.clone(), statuses);
    });
}

/// Re-reads one repository. Called after a run settles, so the Start button
/// reflects what the run just did — committing empties the tree, opening a pull
/// request gives the review flow something to work on.
pub(super) fn reprobe(path: String, mut statuses: Signal<BTreeMap<String, RepoStatus>>) {
    spawn(async move {
        let status = probe::probe(&path).await;
        statuses.write().insert(path, status);
    });
}

/// Re-reads the workspace folder, re-probes every repository in it
/// concurrently, and opens on whichever one wants a person if nothing has been
/// chosen yet.
#[allow(clippy::too_many_arguments)]
pub(super) fn refresh_all(
    workspace: &str,
    mut repos: Signal<Vec<store::Repo>>,
    mut statuses: Signal<BTreeMap<String, RepoStatus>>,
    mut probing: Signal<usize>,
    mut picked: Signal<bool>,
    mut selected_repo: Signal<Option<String>>,
    mut selected_flow: Signal<String>,
    book: Signal<FlowBook>,
    // False when opening the folder: a check from the last two minutes — the
    // home screen's, made a moment ago — is shown rather than asked for
    // again. True for Refresh, which is asking for news.
    fresh: bool,
) {
    if *probing.read() > 0 {
        return;
    }
    // The folder, not the list read when the workspace opened: a repository
    // cloned since then belongs in it, and one deleted since does not.
    let list = store::discover_repos(workspace);
    if list != *repos.read() {
        let kept: std::collections::HashSet<&str> = list.iter().map(|r| r.path.as_str()).collect();
        statuses
            .write()
            .retain(|path, _| kept.contains(path.as_str()));
        let gone = selected_repo
            .read()
            .as_deref()
            .is_some_and(|path| !kept.contains(path));
        if gone {
            selected_repo.set(None);
        }
        repos.set(list.clone());
    }

    if list.is_empty() {
        return;
    }
    probing.set(list.len());

    // One task for the whole sweep rather than one per repository, with the
    // concurrency bounded.
    //
    // Two things were wrong with a task each. `probe` runs several
    // subprocesses, two of them network calls to the forge, so a workspace of
    // forty repositories opened forty concurrent `gh` calls — enough to trip
    // GitHub's secondary rate limiter — alongside a few hundred process
    // spawns at once. And the "are we done" counter was decremented inside
    // each task, so a single task that never got there left `probing` above
    // zero and wedged every later refresh for the life of the window. There
    // is one completion point now, and it does not depend on arithmetic.
    const AT_ONCE: usize = 6;

    spawn(async move {
        use futures_util::stream::StreamExt;

        // Each result lands as it arrives, so the sidebar fills in rather
        // than appearing all at once at the end.
        futures_util::stream::iter(list.clone())
            .for_each_concurrent(AT_ONCE, |repo| {
                let mut statuses = statuses;
                async move {
                    let status = if fresh {
                        probe::probe(&repo.path).await
                    } else {
                        probe::probe_recent(&repo.path).await
                    };
                    statuses.write().insert(repo.path.clone(), status);
                }
            })
            .await;
        probing.set(0);

        if *picked.read() {
            return;
        }
        picked.set(true);
        let map = statuses.read().clone();
        let best = first_with_work(
            list.iter()
                .filter_map(|r| map.get(&r.path).map(|s| (r.path.clone(), s.wants()))),
        );

        if let Some((path, wants)) = best {
            // Open on whichever flow says it answers this, whatever its name.
            let hidden_here = store::load_repo_flows().hidden_for(&path).to_vec();
            let answering = wants.need().and_then(|need| {
                book.read()
                    .runnable_for(&hidden_here)
                    .iter()
                    .find(|f| f.answers(need))
                    .map(|f| f.id.clone())
            });
            if let Some(id) = answering {
                selected_flow.set(id);
            }
            selected_repo.set(Some(path));
        }
    });
}

/// Whether `step`, waiting for approval, is about to be answered without a
/// person: its run is still trusted and has not held it back. A held step is
/// taken out of the trusted run and given a reason, and is a person's to
/// answer — whether or not Trust all is on. A failed step never answers
/// itself: a trusted run stops at a failure like any other.
pub(super) fn answers_itself(state: &RunState, step: &str, trusted: bool) -> bool {
    trusted
        && state.status(step) == NodeStatus::AwaitingApproval
        && state.runs.get(step).is_none_or(|run| run.held.is_empty())
}

/// Whether a step is one only a person can move on: waiting for approval, or
/// failed. The step panel opens on these by itself.
pub(super) fn needs_a_person(status: NodeStatus) -> bool {
    matches!(status, NodeStatus::AwaitingApproval | NodeStatus::Failed)
}
