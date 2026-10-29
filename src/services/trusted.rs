//! Whether a trusted run may approve a step on its own.
//!
//! A trusted run clicks through the approvals for you. That is only tolerable
//! if there is a written-down list of the things it will not click through, so
//! the list lives here as one pure function rather than as conditions scattered
//! through the driver.
//!
//! The bar is deliberately specific: a trusted run approves work, but it never
//! merges into a branch other people pull from while anything says the change
//! might be wrong. Everything it refuses, it refuses with a sentence you can
//! read at the node it stopped on.

use super::flowdef::FlowBook;
use super::graph::{Graph, NodeSpec, NodeStatus, RunState, Step};
use super::probe::{Need, RepoStatus, Wants};

#[derive(Clone, PartialEq, Debug)]
pub enum Verdict {
    /// Click it, the same way a person would.
    Approve,
    /// Hand it back to the person, saying why.
    Hold(String),
}

impl Verdict {
    pub fn reason(&self) -> Option<&str> {
        match self {
            Verdict::Approve => None,
            Verdict::Hold(why) => Some(why),
        }
    }
}

/// The next flow a trusted run should take on for a repository in this state,
/// and the pull request to scope it to (empty for flows that are not
/// PR-scoped).
///
/// This is what makes a trusted run a run *for a repository* rather than for
/// one flow. Committing produces a pull request, reviewing it produces merged
/// work, merged work produces a release — each answer is a different flow, and
/// stopping after the first one leaves the obvious next thing undone.
///
/// `None` means there is nothing more to do, or nothing a flow can do: an
/// unfinished rebase needs a person and no flow claims it.
///
/// Deliberately keyed on `need()` rather than `needs_a_person()`. A pull
/// request whose checks are still running is `Wants::Wait`, which no person is
/// expected to act on — and gating on that is what stopped every trusted run
/// dead after the commit flow, because a pull request opened seconds ago
/// *always* has pending checks. The review flow is still the right answer
/// there: it reads the diff, which is worth doing while CI runs, and the merge
/// rules in this same module already refuse to merge on pending checks. So the
/// chain moves on and stops at the merge, where a person can see why.
/// `hidden` is the flow ids hidden on this repository. A trusted run must not
/// pick one: hiding a flow here is the person saying it does not apply to this
/// repository, and a chain that starts it anyway overrules them — which is
/// exactly what happened to a repository whose release flow had been hidden
/// and replaced.
/// How many flows one trusted run may chain through — in the window and from
/// the MCP server alike. Commit, review, release is three; the cap is there so
/// two flows handing work to each other cannot spin forever unnoticed.
pub const MOST_FLOWS: usize = 6;

pub fn next_flow(
    book: &FlowBook,
    status: &RepoStatus,
    hidden: &[String],
) -> Option<(String, String)> {
    let need = status.wants().need()?;
    let flow = book
        .runnable_for(hidden)
        .into_iter()
        .find(|f| f.answers(need))?;

    // A review has to know which pull request it is about; leaving the slot
    // empty falls back to "whatever the checked-out branch has open", which is
    // not the same question. The checked-out branch's own pull request comes
    // first: it is the one the commit flow just opened, and `prs` is every open
    // pull request on the repository in whatever order the forge listed them.
    let pr = match need {
        Need::OpenPullRequest => status.default_pr(),
        _ => String::new(),
    };
    Some((flow.id.clone(), pr))
}

/// Why the chain has no next flow to take on, in a sentence for the person who
/// pressed the button. `None` when there is genuinely nothing left to do.
///
/// This exists because "no next flow" had exactly one presentation — the run
/// quietly ending — and the two reasons behind it are nothing alike. A
/// repository that is finished and a repository whose release flow never
/// declared that it handles releases looked identical: press Trusted run,
/// watch one flow, and get no explanation of why it stopped there. The second
/// is a one-tick fix in Setup, and it is unfindable without being told.
pub fn why_stopped(book: &FlowBook, status: &RepoStatus, hidden: &[String]) -> Option<String> {
    let wants = status.wants();
    let Some(need) = wants.need() else {
        return match wants {
            // Answered by no flow on purpose: preflight settles it, where the
            // continue and abort buttons already are.
            Wants::Resolve => Some(
                "This repository has a git operation that stopped part-way. Finish or abandon \
                 it — Preflight offers both — before anything else can run."
                    .into(),
            ),
            _ => None,
        };
    };

    if book.runnable_for(hidden).iter().any(|f| f.answers(need)) {
        return None;
    }

    // A flow that answers this but is hidden *here* is a different situation
    // from none existing at all, and the fix is somewhere else entirely — the
    // repository's own flow list, not Setup. Saying "no flow handles that"
    // about a flow sitting right there, hidden, is how someone concludes the
    // chain is ignoring them.
    let hidden_answer = book
        .runnable()
        .into_iter()
        .find(|f| f.answers(need) && hidden.iter().any(|id| id == &f.id));
    if let Some(flow) = hidden_answer {
        return Some(format!(
            "This repository wants {}, and \u{201c}{}\u{201d} handles that — but it is hidden \
             on this repository, so a trusted run will not start it. Show it again from the \
             flow tabs, or tick \u{201c}{}\u{201d} on whichever flow replaced it.",
            wants.note(),
            flow.label,
            need.label(),
        ));
    }

    // The flow may well exist and simply not say so. Naming the tick box is
    // the whole point of the message.
    Some(format!(
        "This repository wants {}, but no flow says it handles that. Open Setup and tick \
         \u{201c}{}\u{201d} on the flow that should.",
        wants.note(),
        need.label(),
    ))
}

/// Whether a finished run is one a trusted run may move on from.
///
/// A failure or a rejection is where the person has to come back in, so the
/// chain stops there rather than starting the next flow on top of a run that
/// did not do what it said. `Skipped` and the `Blocked` nodes behind it are
/// not trouble — that is a step reporting there was nothing to do.
pub fn may_continue(state: &RunState, graph: &Graph) -> bool {
    !graph.nodes.iter().any(|n| {
        matches!(
            state.status(&n.id),
            NodeStatus::Failed | NodeStatus::Rejected
        )
    })
}

/// What a trusted run should do with `node`, given everything the run has
/// produced so far.
pub fn decide(node: &NodeSpec, state: &RunState) -> Verdict {
    match node.step {
        Step::Merge => merge(state),
        // A script or a remote command is whatever the person typed into
        // Setup, and this module cannot reason about it at all: `rm -rf` and
        // `cargo build` are the same shape here. The old rule waved both
        // through as "reversible, or reviewable afterwards", which is not
        // true of an arbitrary command and not something any list in this
        // file could make true.
        //
        // The gate is the person's own statement about the step. `drive`
        // never asks about an ungated node, so a step that reaches this
        // function is one they said should stop for a human — and a trusted
        // run overriding exactly that is the thing to refuse. Wanting it
        // automatic is one tick in Setup, and the message says so.
        Step::RunScript | Step::RunRemote => Verdict::Hold(format!(
            "\u{201c}{}\u{201d} runs a command this app cannot vet, and it is marked as \
             needing approval. A trusted run will not click that through — approve it \
             here, or untick \u{201c}Needs approval\u{201d} on the step in Setup if it \
             should always run unattended.",
            node.title,
        )),
        // Published word for word on the releases page. Everything else a
        // trusted run approves can be reviewed afterwards; public text nobody
        // read before it shipped cannot be unread.
        Step::WriteNotes => Verdict::Hold(
            "These release notes are published as written, and nobody has read them yet. \
             A trusted run will not approve them — read them here and approve, or reject \
             and write them yourself."
                .into(),
        ),
        // Committing, pushing and opening a pull request are this app's own
        // work, in shapes it controls: all reversible, or reviewable
        // afterwards by the person who asked for the run.
        _ => Verdict::Approve,
    }
}

/// The merge is the only irreversible step in the shipped flows, so it is the
/// only one with a list.
///
/// Each rule answers a different question, and any one of them is enough to
/// stop: does the model think the change is dangerous, did it find anything
/// concrete, and does CI agree. They are checked in that order because that is
/// the order of how much a person would want to hear about them.
fn merge(state: &RunState) -> Verdict {
    let verdict = state.artifact("verdict");
    let findings: usize = state.artifact("finding_count").parse().unwrap_or(0);
    let checks = state.artifact("checks_state");

    // Skipping the analysis is a legitimate thing to do by hand — the button
    // exists — but it must not quietly buy a trusted merge. An absent verdict
    // is the same evidence as an absent CI result: none.
    if verdict.is_empty() {
        return Verdict::Hold(
            "Nothing analysed this change — the analysis step did not run, or was skipped. \
             A trusted run will not merge on no evidence."
                .into(),
        );
    }
    if verdict == "risky" {
        return Verdict::Hold(
            "The analysis called this change risky. A trusted run will not merge that on \
             its own — read the findings and merge it yourself if you disagree."
                .into(),
        );
    }
    if findings > 0 {
        return Verdict::Hold(format!(
            "The analysis found {findings} possible regression{}. A trusted run stops here \
             so you can read {} before this is merged.",
            if findings == 1 { "" } else { "s" },
            if findings == 1 { "it" } else { "them" },
        ));
    }
    // A focused review that ran and found something stops it just the same.
    for lens in crate::services::review::LENSES {
        let verdict = state.artifact(&format!("{}_verdict", lens.key()));
        let found: usize = state
            .artifact(&format!("{}_finding_count", lens.key()))
            .parse()
            .unwrap_or(0);
        if verdict == "risky" || found > 0 {
            return Verdict::Hold(format!(
                "The {} review {}. A trusted run stops here so you can read it before \
                 this is merged.",
                lens.label().to_lowercase(),
                if verdict == "risky" {
                    "called this change risky".to_string()
                } else {
                    format!("found {found} problem{}", if found == 1 { "" } else { "s" })
                },
            ));
        }
    }
    // A second opinion is optional — none chosen is no evidence either way —
    // but when one was given it counts as much as the first.
    let second = state.artifact("second_verdict");
    let second_findings: usize = state.artifact("second_finding_count").parse().unwrap_or(0);
    if second == "risky" || second_findings > 0 {
        return Verdict::Hold(format!(
            "The second opinion ({}) {}. A trusted run stops here so you can read it before \
             this is merged.",
            state.artifact("second_model"),
            if second == "risky" {
                "called this change risky".to_string()
            } else {
                format!(
                    "found {second_findings} possible regression{}",
                    if second_findings == 1 { "" } else { "s" }
                )
            },
        ));
    }

    match checks {
        "passing" => Verdict::Approve,
        "failing" => Verdict::Hold(
            "CI is failing on this pull request, so a trusted run will not merge it.".into(),
        ),
        "pending" => Verdict::Hold(
            "CI is still running, so nothing yet says this is safe to merge. Approve it \
             yourself once the checks are in."
                .into(),
        ),
        "none" => Verdict::Hold(
            "No CI checks ran on this pull request — 0 of 0 is not a pass. A trusted run \
             has nothing but the model's reading to go on, so it will not merge this on \
             its own. Merge it yourself if that is enough here."
                .into(),
        ),
        // Either the flow never looked (no CI step wired in) or the forge does
        // not report checks — Azure, today. Both mean the same thing here:
        // nothing has vouched for this change, so a person has to.
        _ => Verdict::Hold(
            "Nothing in this run checked CI, so a trusted run has no evidence this is safe \
             to merge."
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests;
