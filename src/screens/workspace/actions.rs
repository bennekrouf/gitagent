//! What the workspace's buttons do: starting a flow, and the actions offered
//! beside a repository.

use super::*;
use crate::services::forge::Forge;
use crate::services::store::{RepoBases, RepoFlows};

#[allow(clippy::too_many_arguments)]
pub(super) fn begin(
    mut statuses: Signal<BTreeMap<String, RepoStatus>>,
    mut states: Signal<States>,
    selected_repo: Signal<Option<String>>,
    book: Signal<FlowBook>,
    mut selected_flow: Signal<String>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    mut running: Signal<BTreeSet<Key>>,
    mut trusted: Signal<BTreeSet<Key>>,
    global_trust: Signal<bool>,
    mut chain_note: Signal<String>,
    repo_flows: Signal<RepoFlows>,
    branches_busy: Signal<Option<(String, String)>>,
    llm_config: Signal<LlmConfig>,
    mut llm_config_mut: Signal<LlmConfig>,
    trust: bool,
) {
    let Some(repo) = selected_repo.read().clone() else {
        return;
    };
    // The buttons are disabled while another run holds this repository,
    // but a click can still land between that run starting and the
    // re-render, so the rule is enforced here too. The disabled button's
    // tooltip already says which run is in the way.
    if running.read().iter().any(|k| k.0 == repo)
        || branches_busy
            .read()
            .as_ref()
            .is_some_and(|(r, _)| r == &repo)
    {
        return;
    }
    // Two questions, kept apart: which flow to start, and whether its
    // approvals answer themselves. Only the Trusted run button chooses the
    // flow for you. "Trust all" answers the second question for every
    // run — a plain Start or Play runs trusted once it is on, since the
    // whole point is not having to remember which button to press — but
    // it does not take the first one away: Play on the Review tab reviews.
    let chosen_for_you = trust;
    let trust = trust || *global_trust.read();
    // Settings live in a per-window signal but one file on disk. Re-reading
    // here is what stops a second window running against a stale provider
    // — for the folder this repository sits in, so a window set to China
    // stays on China however the one next to it was changed.
    llm_config_mut.set(store::load_settings_for(Some(&repo)));
    chain_note.set(String::new());

    // An ordinary Start runs the flow on screen. A trusted run is for the
    // repository, not for one flow, so it starts on whichever flow answers
    // what the repository actually needs — which is how "Trusted run" on
    // the Commit → PR tab does the release a repository is waiting for
    // instead of refusing because there is nothing to commit.
    //
    // Falling back to the flow on screen matters as much as the preference
    // does. `next_flow` answers "what does this repository most need",
    // which is `None` for a repository that needs nothing in particular —
    // and that used to disable the button on every tab at once, including
    // a "Deploy VPS" flow whose whole point is that you run it when you
    // decide to, not when a probe says so.
    let opening = starting_flow(
        chosen_for_you,
        statuses.read().get(&repo).and_then(|status| {
            trusted::next_flow(&book.read(), status, &repo_flows.read().hidden_for(&repo))
        }),
        (selected_flow.read().clone(), selected_pr.read().clone()),
    );
    let Some((mut id, mut pr)) = opening else {
        // Nothing to start at all. Silently doing nothing is what made
        // this button feel broken, so say why.
        if trust {
            if let Some(status) = statuses.read().get(&repo) {
                chain_note.set(
                    trusted::why_stopped(
                        &book.read(),
                        status,
                        &repo_flows.read().hidden_for(&repo),
                    )
                    .unwrap_or_default(),
                );
            }
        }
        return;
    };

    spawn(async move {
        // Whether the run is *currently* trusted, which is not the same as
        // the button that started it. A run started with plain Start and
        // adopted part-way — "stop asking me" at an approval — has to chain
        // like any other trusted run from that point, so this is re-read
        // from the trusted set after every leg rather than captured once.
        let mut trusting = trust;

        // Each turn of this loop is one flow, start to finish. Only a
        // trusted run goes round twice.
        for _ in 0..trusted::MOST_FLOWS {
            let Some(def) = book.read().get(&id).cloned() else {
                // The flow is not in the book: removed or renamed in Setup
                // while its tab was still on screen. Breaking quietly here
                // is a Start that does nothing whatsoever.
                chain_note.set(format!(
                    "The flow \u{201c}{id}\u{201d} is not in Setup any more \u{2014} it was \
                     removed or renamed. Pick another tab, or put it back in Setup."
                ));
                break;
            };
            let key: Key = (repo.clone(), id.clone(), pr.clone());
            // Repository-wide, not just this key: the next leg of a chain
            // must not start while something else holds the working tree.
            if running.read().iter().any(|k| k.0 == repo)
                || branches_busy
                    .read()
                    .as_ref()
                    .is_some_and(|(r, _)| r == &repo)
            {
                break;
            }

            // The window's own runs are guarded above; this is for the ones
            // Claude or a terminal is driving in another process. Held until
            // this leg is over, cancelled or not.
            let _claim = match crate::services::busy::claim(
                &repo,
                &def.label,
                crate::services::busy::By::Window,
            ) {
                Ok(claim) => claim,
                Err(why) => {
                    chain_note.set(why);
                    break;
                }
            };

            let graph = def.to_graph();
            let mut fresh = RunState::fresh(&graph);
            fresh.started = true;
            if !pr.is_empty() {
                // Read by `find_pr`, so this run reviews the PR that was
                // actually picked rather than falling back to "whatever the
                // checked-out branch has open".
                fresh
                    .artifacts
                    .insert("selected_pr_number".into(), pr.clone());
            }
            states.write().insert(key.clone(), fresh);
            running.write().insert(key.clone());
            if trusting {
                trusted.write().insert(key.clone());
            } else {
                trusted.write().remove(&key);
            }

            // Follow the run on screen, so a chain that moves to another
            // flow does not leave you watching the tab it has left.
            if selected_repo.read().as_deref() == Some(repo.as_str()) {
                selected_flow.set(id.clone());
                selected_pr.set(pr.clone());
                selected_node.set(def.first_node());
            }

            if !drive(
                graph.clone(),
                key.clone(),
                llm_config,
                states,
                selected_node,
                selected_repo,
                selected_flow,
                selected_pr,
                trusted,
            )
            .await
            {
                // Cancelled: the run is over, and nothing chains from it.
                return;
            }
            running.write().remove(&key);

            // `drive` takes the key back out when it stopped at something
            // it would not approve, so this is also how a hold ends the
            // chain rather than only the leg it happened on. It is equally
            // how adoption gets picked up: the key is in the set because a
            // person put it there mid-flight.
            trusting = trusted.read().contains(&key);
            trusted.write().remove(&key);

            let status = probe::probe(&repo).await;
            let finished = snapshot(&states, &key);
            statuses.write().insert(repo.clone(), status.clone());

            // A review that has ended — merged, nothing to do, or
            // declined — is not run again: move the screen on to the next
            // open pull request, if there is one and the screen is still
            // on this one. With none left the finished run stays, its
            // button saying so.
            let on_screen = selected_repo.read().as_deref() == Some(repo.as_str())
                && *selected_flow.read() == id
                && *selected_pr.read() == pr;
            if def.kind() == probe::FlowKind::Review
                && on_screen
                && matches!(
                    phase_of(&finished),
                    Phase::Done | Phase::Nothing | Phase::Declined
                )
            {
                let reviewed = match finished.artifact("pr_number") {
                    "" => pr.as_str(),
                    n => n,
                };
                if let Some(next) = status.next_to_review(reviewed) {
                    selected_pr.set(next);
                    selected_node.set(def.first_node());
                }
            }

            if !trusting || !trusted::may_continue(&finished, &graph) {
                break;
            }
            let hidden_here = repo_flows.read().hidden_for(&repo).to_vec();
            let Some(next) = trusted::next_flow(&book.read(), &status, &hidden_here) else {
                // The commonest end of a chain, and until now the most
                // silent: a release is due and the release flow never
                // declared that it handles releases.
                chain_note.set(
                    trusted::why_stopped(&book.read(), &status, &hidden_here).unwrap_or_default(),
                );
                break;
            };
            // The same flow again means the last one did not move the
            // repository on. Whatever is left needs a person, not another
            // identical lap.
            if next == (id.clone(), pr.clone()) {
                break;
            }
            (id, pr) = next;
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn repo_actions(
    running: Signal<BTreeSet<Key>>,
    mut trusted: Signal<BTreeSet<Key>>,
    repo_flows: Signal<RepoFlows>,
    mut branches_open: Signal<Option<String>>,
    mut branches_data: Signal<Option<Result<Vec<crate::services::branches::BranchInfo>, String>>>,
    repo_bases: Signal<RepoBases>,
    mut branches_action_error: Signal<Option<String>>,
    mut base_editor_open: Signal<Option<String>>,
    mut base_editor_value: Signal<String>,
    status_map: BTreeMap<String, RepoStatus>,
    forge_map: BTreeMap<String, Forge>,
    flows: FlowBook,
    mut begin: impl FnMut(bool) + Copy + 'static,
    _start: impl FnMut(Event<MouseData>) + Copy + 'static,
    repo: &str,
    key: &Key,
    can_run: &probe::Affordance,
    other_run: &Option<String>,
) -> Element {
    let repo = repo.to_string();
    let key = key.clone();
    let is_running = running.read().contains(&key);
    // Trust is granted per leg, but the chain moves between flows, so
    // the button looks at the repository rather than this key alone.
    let is_trusted = trusted.read().iter().any(|(r, _, _)| r == &repo);
    // Flows hidden here: a trusted run must not start one the tab
    // strip does not even show.
    let hidden_here = repo_flows.read().hidden_for(&repo).to_vec();
    // What a trusted run would start: the repository's most urgent
    // need by preference — the point of the button — else the flow on
    // screen when it can run. Requiring a need left the button dead on
    // every tab whenever the probe said "nothing in particular", which
    // read as "trusted runs only work on Commit → PR".
    let trusted_start = status_map
        .get(&repo)
        .and_then(|status| trusted::next_flow(&flows, status, &hidden_here))
        .or_else(|| can_run.enabled.then(|| (key.1.clone(), key.2.clone())));
    let other_running = other_run.is_some();
    rsx! {
    button {
        class: "btn btn-ghost",
        title: "Local branches, and whether their pull request landed",
        onclick: {
            let repo = repo.clone();
            let forge = forge_map.get(&repo).cloned().unwrap_or(crate::services::forge::Forge::None);
            move |_| {
                let repo = repo.clone();
                let forge = forge.clone();
                let override_base = repo_bases.read().get(&repo).map(str::to_string);
                branches_open.set(Some(repo.clone()));
                branches_data.set(None);
                branches_action_error.set(None);
                spawn(async move {
                    let base = resolved_base(&repo, override_base).await;
                    let result = crate::services::branches::list(&repo, &forge, &base).await;
                    branches_data.set(Some(result));
                });
            }
        },
        "Branches"
    }
    button {
        class: "btn btn-ghost",
        title: "Which branch this repository's pull requests target",
        onclick: {
            let repo = repo.clone();
            move |_| {
                base_editor_value.set(
                    repo_bases.read().get(&repo).unwrap_or("").to_string()
                );
                base_editor_open.set(Some(repo.clone()));
            }
        },
        match repo_bases.read().get(&repo) {
            Some(base) => format!("Base: {base}"),
            None => "Base: auto".to_string(),
        }
    }
    // Two ways to start the same run. The
    // trusted one answers the approvals for
    // you, in the open, and stops itself at
    // anything `services::trusted` refuses.
    if is_trusted {
        button {
            class: "btn btn-trusted btn-trusted-on",
            title: "Stop clicking through the approvals. The run keeps going and asks you at the next one, and no further flow is started.",
            onclick: {
                let repo = repo.clone();
                move |_| {
                    // Every leg for this repository, not just the
                    // one on screen — the chain may already have
                    // moved on to another flow's key.
                    trusted.write().retain(|(r, _, _)| r != &repo);
                    approvals().notify_waiters();
                }
            },
            span { class: "flow-tab-dot" }
            "Trusting…"
        }
    } else if is_running {
        // A run is already going, and you are
        // most likely reading this because it
        // has stopped to ask you something.
        // Adopting it is a different action
        // from starting one: nothing is reset,
        // the approval in front of you is
        // answered, and every one after it in
        // this repository is too. `drive` is
        // already watching the trusted set on
        // its approval wait, so putting the key
        // in is the whole mechanism.
        button {
            class: "btn btn-trusted",
            title: "Approve this step, and every step after it in this repository, without asking again. Still stops at a merge the analysis or CI is unhappy about.",
            onclick: {
                let key = key.clone();
                let mut trusted = trusted;
                move |_| {
                    trusted.write().insert(key.clone());
                    approvals().notify_waiters();
                }
            },
            "Stop asking me"
        }
    } else {
        button {
            class: "btn btn-trusted",
            disabled: other_running || trusted_start.is_none(),
            title: match (&trusted_start, other_running) {
                (_, true) => other_run.clone().unwrap_or_default(),
                (None, _) => format!(
                    "Nothing here needs a run, and this flow cannot start — {}.",
                    if can_run.reason.is_empty() {
                        status_map
                            .get(&repo)
                            .map(|s| s.wants().note())
                            .unwrap_or("still reading this repository")
                            .to_string()
                    } else {
                        can_run.reason.clone()
                    }
                ),
                (Some((id, _)), _) => format!(
                    "Work through what this repository needs, starting with \u{201c}{}\u{201d}, \
                     approving each step for you as you watch. Moves on to the next flow \
                     when one finishes, and stops for you at a merge the analysis or CI \
                     is unhappy about.",
                    flows.get(id).map(|f| f.label.clone()).unwrap_or_else(|| id.clone()),
                ),
            },
            onclick: move |_| begin(true),
            "Trusted run"
        }
    }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn open_diff(
    mut drawer: Signal<Option<String>>,
    mut diff_panel: Signal<Option<String>>,
    mut pr_diffs: Signal<PrDiffs>,
    repo: String,
    pr: probe::PrBrief,
) {
    drawer.set(None);
    diff_panel.set(Some(pr.number.clone()));
    let key = (repo.clone(), pr.number.clone());
    if pr.base.is_empty() || pr.head.is_empty() {
        pr_diffs.write().insert(
            key,
            Some(Err(
                "Which branches this pull request compares is not known yet. \
                     Refresh the repository and try again."
                    .into(),
            )),
        );
        return;
    }
    if !pr_diffs.peek().contains_key(&key) {
        pr_diffs.write().insert(key.clone(), None);
    }
    spawn(async move {
        let got = review::pull_request_diff(&repo, &pr.base, &pr.head).await;
        pr_diffs.write().insert(key, Some(got));
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn hide_flow(
    selected_repo: Signal<Option<String>>,
    book: Signal<FlowBook>,
    mut selected_flow: Signal<String>,
    mut selected_node: Signal<String>,
    mut repo_flows: Signal<RepoFlows>,
    repo: &str,
    id: &str,
) {
    repo_flows.write().hide(repo, id);
    store::save_repo_flows(&repo_flows.read());
    if selected_repo.read().as_deref() == Some(repo) && *selected_flow.read() == id {
        let next = {
            let still_hidden = repo_flows.read();
            book.read()
                .runnable()
                .iter()
                .find(|f| !still_hidden.is_hidden(repo, &f.id))
                .map(|f| f.id.clone())
                .unwrap_or_default()
        };
        let first_node = book
            .read()
            .get(&next)
            .map(|f| f.first_node())
            .unwrap_or_default();
        selected_flow.set(next);
        selected_node.set(first_node);
    }
}
