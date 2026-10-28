//! Which flow runs where: the selection a repository starts on, what is already
//! running, and what is next.

use super::*;

/// The run already going in `repo`, other than `key` itself. A repository has
/// one working tree, so it gets one run at a time: a commit switching to a new
/// branch while a merge pulls the base underneath it leaves each one acting on
/// a tree the other has just changed.
pub(super) fn other_run_in<'a>(
    running: &'a BTreeSet<Key>,
    repo: &str,
    key: &Key,
) -> Option<&'a Key> {
    running.iter().find(|k| k.0 == repo && *k != key)
}

/// Why a start is refused while `other` runs, naming it so you know which tab
/// to go and finish.
/// Why a start is refused while the Branches panel is working in the same
/// repository.
pub(super) fn branch_busy_note(branch: &str) -> String {
    format!(
        "The Branches panel is still working on {branch} in this repository \u{2014} wait for it to finish."
    )
}

pub(super) fn busy_note(book: &FlowBook, other: &Key) -> String {
    let (_, flow, pr) = other;
    let label = book
        .get(flow)
        .map(|f| f.label.clone())
        .unwrap_or_else(|| flow.clone());
    let what = if pr.is_empty() {
        format!("\u{201c}{label}\u{201d}")
    } else {
        format!("\u{201c}{label}\u{201d} for pull request #{pr}")
    };
    format!("{what} is already running in this repository \u{2014} finish or cancel it first.")
}

/// Where to land when a repository is picked: the first step, in the first
/// runnable flow, that is waiting on the person looking at the screen — or,
/// if nothing is, the first runnable flow's first step.
pub(super) fn default_selection(
    book: &FlowBook,
    states: &States,
    repo: &str,
    wants: Option<Wants>,
    open_prs: &[probe::PrBrief],
    hidden: &[String],
) -> (String, String, String) {
    // Landing on a flow hidden here would open a tab the strip does not show.
    let runnable = book.runnable_for(hidden);

    // A person being waited on outranks everything else, same precedence as
    // the sidebar's dot — check every flow for one before falling back.
    // Same order the sidebar's dot uses, and it must include Failed: the
    // status is now read across every flow, so a failure in one the user is
    // not looking at would otherwise report "failed" with no way to reach it.
    for status in [
        NodeStatus::AwaitingApproval,
        NodeStatus::Failed,
        NodeStatus::Running,
    ] {
        for flow in &runnable {
            // Any PR-scoped run for this repo+flow, not just a no-PR one — a
            // review under a specific PR must not be missed just because it
            // isn't the "current branch" slot.
            let matching_run = states
                .iter()
                .filter(|((r, f, _), _)| r == repo && f == &flow.id)
                .find_map(|((_, _, pr), run)| {
                    flowdef::topological_order(flow)
                        .into_iter()
                        .find(|id| run.status(id) == status)
                        .map(|node_id| (node_id, pr.clone()))
                });
            if let Some((node_id, pr_id)) = matching_run {
                return (flow.id.clone(), node_id, pr_id);
            }
        }
    }
    // Nothing in flight, so open on the flow that handles whatever the
    // repository actually needs — landing on "Commit → PR" for a repository
    // whose only outstanding work is a release is how the first task to do
    // ends up hidden.
    let need = wants.and_then(|w| w.need());
    let hinted = need.and_then(|need| runnable.iter().find(|f| f.answers(need)));

    // For a review, pick the pull request too. "Which one?" is a question the
    // app can already answer, and leaving the slot empty means arriving at a
    // review flow with nothing selected to review.
    let pr_id = match need {
        Some(Need::OpenPullRequest) => open_prs.first().map(|pr| pr.number.clone()),
        _ => None,
    }
    .unwrap_or_default();

    hinted
        .or_else(|| runnable.first())
        .map(|f| (f.id.clone(), f.first_node(), pr_id))
        .unwrap_or_default()
}

/// The flow a repository list row offers to start, its button's words, and
/// the pull request that flow would act on — empty when it needs none.
///
/// A commit for uncommitted files, a release that is due, a merge for a pull
/// request whose checks are green. `None` for anything else, or when no flow
/// shown in this repository answers it.
pub(super) fn row_start(
    book: &FlowBook,
    status: &RepoStatus,
    hidden: &[String],
) -> Option<(String, String, String)> {
    let wants = status.wants();
    let need = wants.need()?;
    let flow = book
        .runnable_for(hidden)
        .into_iter()
        .find(|f| f.answers(need))?;
    let mut pr = String::new();
    let label = match wants {
        Wants::Commit if status.changes == 1 => "Commit 1 file".to_string(),
        Wants::Commit => format!("Commit {} files", status.changes),
        // In place of "needs a PR": the commits are made, so what is left is
        // the push and the pull request. Short, since the row is narrow.
        Wants::OpenPr => "Push & open PR".to_string(),
        // The flow's own name rather than a generic "Run release": several
        // flows can answer a release — "Release", "Deploy VPS", "Deploy
        // API0" — and a button that does not say which one it starts is
        // worse than no button.
        Wants::Release => flow.label.clone(),
        // The decision this row is waiting on, named: four rows reading
        // "ready to merge" are four rows you have to open to tell apart.
        // The number is the one the state was read from, which is not
        // always the checked-out branch's.
        Wants::Merge => {
            let decided = status.pr_to_act_on()?;
            pr = decided.number.clone();
            format!("Merge #{}", decided.number)
        }
        _ => return None,
    };
    Some((flow.id.clone(), label, pr))
}

/// Where a flow's run in one repository stands, for its tab and for the line
/// that points to whatever needs you.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct FlowRun {
    pub(super) phase: Phase,
    /// "running · 3/8", "needs you · Merge", "failed · Open pull request".
    pub(super) note: String,
    /// Which pull request's run it is, empty for a flow not about one.
    pub(super) pr: String,
    /// The step that needs you, for a run that waits or failed.
    pub(super) step: String,
}

/// The most urgent of a flow's runs in `repo` — one per pull request it was
/// run for — or `None` when it has not run there. `graph` is the flow's, for
/// step names and the count of steps.
pub(super) fn flow_run(states: &States, repo: &str, flow: &str, graph: &Graph) -> Option<FlowRun> {
    let (key, state) = states
        .iter()
        .filter(|((r, f, _), s)| r == repo && f == flow && s.started)
        .min_by_key(|(_, s)| phase_of(s).priority())?;
    let phase = phase_of(state);
    let step = match phase {
        Phase::NeedsApproval | Phase::Failed => state
            .newest_for_a_person()
            .map(|(id, _)| id)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let title = graph
        .get(&step)
        .map(|spec| spec.title.clone())
        .unwrap_or_else(|| step.clone());
    let settled = graph
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                state.status(&n.id),
                NodeStatus::Done | NodeStatus::Skipped | NodeStatus::Bypassed
            )
        })
        .count();
    let note = match phase {
        Phase::NeedsApproval => format!("needs you \u{00b7} {title}"),
        Phase::Failed => format!("failed \u{00b7} {title}"),
        Phase::Running => format!("running \u{00b7} {settled}/{}", graph.nodes.len()),
        other => other.note().to_string(),
    };
    Some(FlowRun {
        phase,
        note,
        pr: key.2.clone(),
        step,
    })
}

/// What a Refresh clears besides re-reading the repositories: runs that have
/// ended with nothing left to act on — done, nothing to do, or declined — in
/// `repo`, or in every repository when it is `None`. A finished run left on
/// screen reads as the repository's present; after a refresh the map should
/// show where things stand now, ready to start. Runs still going, waiting
/// for you, or failed with a retry or a fix on offer are kept.
pub(super) fn forget_finished(states: &mut States, running: &BTreeSet<Key>, repo: Option<&str>) {
    states.retain(|key, state| {
        let here = repo.is_none_or(|r| key.0 == r);
        let ended = matches!(
            phase_of(state),
            Phase::Done | Phase::Nothing | Phase::Declined
        );
        !(here && ended && !running.contains(key))
    });
}

/// Which flow a run starts on, as `(flow, pull request)`. The one on screen —
/// what Start and Play mean — unless the Trusted run button asked for the
/// flow to be chosen for it, when it is whatever the repository most `needs`,
/// falling back to the one on screen when it needs nothing in particular.
pub(super) fn starting_flow(
    chosen_for_you: bool,
    needs: Option<(String, String)>,
    on_screen: (String, String),
) -> Option<(String, String)> {
    if chosen_for_you {
        needs.or(Some(on_screen))
    } else {
        Some(on_screen)
    }
}

/// The repository to open on: the first in the list, top to bottom as the
/// sidebar shows it, that has something left to do. Not the most urgent one
/// further down — the list is the order you read in, and opening halfway down
/// it reads as a jump.
pub(super) fn first_with_work(
    repos: impl IntoIterator<Item = (String, probe::Wants)>,
) -> Option<(String, probe::Wants)> {
    repos.into_iter().find(|(_, wants)| wants.needs_a_person())
}
