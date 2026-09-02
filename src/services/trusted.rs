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
use super::probe::{Need, RepoStatus};

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
pub fn next_flow(book: &FlowBook, status: &RepoStatus) -> Option<(String, String)> {
    let wants = status.wants();
    if !wants.needs_a_person() {
        return None;
    }
    let need = wants.need()?;
    let flow = book.runnable().into_iter().find(|f| f.answers(need))?;

    // A review has to know which pull request it is about; leaving the slot
    // empty falls back to "whatever the checked-out branch has open", which is
    // not the same question.
    let pr = match need {
        Need::OpenPullRequest => status
            .prs
            .first()
            .map(|pr| pr.number.clone())
            .unwrap_or_default(),
        _ => String::new(),
    };
    Some((flow.id.clone(), pr))
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
        // Committing, pushing, opening a pull request, running a script or a
        // remote command: all reversible, or reviewable afterwards by the
        // person who asked for the run. The merge is the one that is neither.
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
mod tests {
    use super::*;
    use crate::services::graph::{Graph, NodeKind};

    fn node(step: Step) -> NodeSpec {
        NodeSpec {
            id: "n".into(),
            title: "n".into(),
            subtitle: String::new(),
            step,
            kind: NodeKind::Deterministic,
            deps: vec![],
            reads: vec![],
            writes: vec![],
            requires_approval: true,
            config: Default::default(),
        }
    }

    /// A run state carrying just the artifacts the merge rules read.
    fn state(verdict: &str, findings: &str, checks: &str) -> RunState {
        let mut s = RunState::fresh(&Graph { nodes: vec![] });
        s.artifacts.insert("verdict".into(), verdict.into());
        s.artifacts.insert("finding_count".into(), findings.into());
        s.artifacts.insert("checks_state".into(), checks.into());
        s
    }

    use crate::services::flowdef::FlowBook;
    use crate::services::forge::Forge;
    use crate::services::probe::{Checks, PrBrief};
    use crate::services::release::ReleaseState;

    /// A minimal flow declaring it answers the release need.
    fn release_flow() -> crate::services::flowdef::FlowDef {
        crate::services::flowdef::FlowDef {
            id: "release".into(),
            label: "Release".into(),
            handles: vec![Need::Release.key().to_string()],
            nodes: vec![crate::services::flowdef::NodeDef::from_catalogue(
                "preflight",
                "preflight",
            )],
        }
    }

    /// A repository in whatever state the arguments describe.
    fn repo(changes: usize, unmerged: usize, pr: Option<Checks>, release: bool) -> RepoStatus {
        let brief = |checks| PrBrief {
            number: "7".into(),
            title: "t".into(),
            url: "u".into(),
            checks,
            files: 1,
            additions: 1,
            deletions: 0,
            commits: 1,
        };
        RepoStatus {
            branch: "feat/x".into(),
            changes,
            forge: Forge::GitHub,
            pr: pr.map(brief),
            prs: pr.map(|c| vec![brief(c)]).unwrap_or_default(),
            prs_error: None,
            ahead: 0,
            behind: 0,
            unmerged,
            release: if release {
                ReleaseState {
                    last_tag: Some("v1".into()),
                    commits: 2,
                    prs: vec![],
                    releases: true,
                }
            } else {
                ReleaseState::default()
            },
            in_progress: None,
        }
    }

    #[test]
    fn the_chain_walks_commit_then_review_then_release() {
        // The whole point of chaining: each flow's output is the next one's
        // input, and stopping after the first leaves the obvious work undone.
        let book = FlowBook::defaults();

        let dirty = repo(3, 0, None, false);
        assert_eq!(
            next_flow(&book, &dirty),
            Some(("commit_and_pr".into(), String::new()))
        );

        let has_pr = repo(0, 0, Some(Checks::Passing), false);
        assert_eq!(
            next_flow(&book, &has_pr),
            Some(("review_and_merge".into(), "7".into())),
            "and it names the pull request to review"
        );
    }

    #[test]
    fn a_clean_repository_with_a_release_due_still_has_a_next_flow() {
        // The reported case: nothing to commit, so the Commit → PR tab refuses
        // an ordinary Start — but the repository plainly wants a release, and
        // a trusted run is for the repository.
        let mut book = FlowBook::defaults();
        book.flows.push(release_flow());
        let waiting = repo(0, 0, None, true);
        assert_eq!(waiting.wants(), crate::services::probe::Wants::Release);
        assert_eq!(
            next_flow(&book, &waiting),
            Some(("release".into(), String::new()))
        );
    }

    #[test]
    fn a_repository_with_nothing_to_do_ends_the_chain() {
        let idle = repo(0, 0, None, false);
        assert_eq!(next_flow(&FlowBook::defaults(), &idle), None);
    }

    #[test]
    fn a_need_no_flow_answers_ends_the_chain() {
        // A release is due but no flow says it handles one: there is nothing
        // for a trusted run to start, and inventing one would be worse.
        let waiting = repo(0, 0, None, true);
        assert_eq!(next_flow(&FlowBook::defaults(), &waiting), None);
    }

    #[test]
    fn an_unfinished_rebase_ends_the_chain_even_though_it_needs_a_person() {
        // `Resolve` is deliberately answered by no flow — it is settled in
        // preflight — so a trusted run must stop rather than pick something.
        let mut stuck = repo(3, 0, None, false);
        stuck.in_progress = Some(crate::services::git::InProgress::Rebase);
        assert!(stuck.wants().needs_a_person());
        assert_eq!(next_flow(&FlowBook::defaults(), &stuck), None);
    }

    #[test]
    fn a_failed_or_rejected_run_stops_the_chain() {
        let graph = FlowBook::defaults()
            .get("commit_and_pr")
            .unwrap()
            .to_graph();

        let mut done = RunState::fresh(&graph);
        for node in &graph.nodes {
            done.set_status(&node.id, NodeStatus::Done);
        }
        assert!(may_continue(&done, &graph));

        let mut failed = done.clone();
        failed.set_status("push", NodeStatus::Failed);
        assert!(!may_continue(&failed, &graph));

        let mut rejected = done.clone();
        rejected.set_status("commit", NodeStatus::Rejected);
        assert!(!may_continue(&rejected, &graph));
    }

    #[test]
    fn a_step_with_nothing_to_do_is_not_trouble() {
        // `scan` finding an empty tree skips, blocking everything behind it.
        // That is a flow reporting no work, not a flow going wrong.
        let graph = FlowBook::defaults()
            .get("commit_and_pr")
            .unwrap()
            .to_graph();
        let mut state = RunState::fresh(&graph);
        state.set_status("preflight", NodeStatus::Done);
        state.set_status("scan", NodeStatus::Skipped);
        state.propagate_block(&graph);
        assert!(may_continue(&state, &graph));
    }

    #[test]
    fn ordinary_work_is_clicked_through() {
        for step in [Step::Commit, Step::Push, Step::OpenPr, Step::ScanChanges] {
            let clean = state("looks_safe", "0", "passing");
            assert_eq!(decide(&node(step), &clean), Verdict::Approve);
        }
    }

    #[test]
    fn a_clean_merge_is_clicked_through() {
        let clean = state("looks_safe", "0", "passing");
        assert_eq!(decide(&node(Step::Merge), &clean), Verdict::Approve);
    }

    #[test]
    fn a_risky_verdict_stops_the_merge() {
        let risky = state("risky", "0", "passing");
        let held = decide(&node(Step::Merge), &risky);
        assert!(held.reason().unwrap().contains("risky"));
    }

    #[test]
    fn a_single_finding_stops_the_merge_even_when_ci_is_green() {
        // The whole point: green CI is not the same as nobody having spotted
        // anything, and the analysis is the half that reads the diff.
        let found = state("worth_a_look", "1", "passing");
        let held = decide(&node(Step::Merge), &found);
        let reason = held.reason().unwrap();
        assert!(reason.contains("1 possible regression"), "{reason}");
        assert!(!reason.contains("regressions"), "singular: {reason}");
    }

    #[test]
    fn several_findings_are_counted_in_the_plural() {
        let found = state("worth_a_look", "3", "passing");
        let reason = decide(&node(Step::Merge), &found)
            .reason()
            .unwrap()
            .to_string();
        assert!(reason.contains("3 possible regressions"), "{reason}");
        assert!(reason.contains("read them"), "{reason}");
    }

    #[test]
    fn red_or_still_running_ci_stops_the_merge() {
        for checks in ["failing", "pending"] {
            let s = state("looks_safe", "0", checks);
            assert!(
                decide(&node(Step::Merge), &s).reason().is_some(),
                "{checks} must not merge on its own"
            );
        }
    }

    #[test]
    fn a_merge_with_nothing_vouching_for_it_stops() {
        // Azure reports no checks, and a flow can simply not have a CI step.
        // Neither is evidence of safety.
        let unknown = state("looks_safe", "0", "unknown");
        assert!(decide(&node(Step::Merge), &unknown)
            .reason()
            .unwrap()
            .contains("checked CI"));

        let absent = RunState::fresh(&Graph { nodes: vec![] });
        assert!(decide(&node(Step::Merge), &absent).reason().is_some());
    }

    #[test]
    fn the_shipped_review_flow_produces_everything_the_merge_rules_read() {
        // The rules are only as good as the artifacts reaching them: a review
        // flow that stopped writing `finding_count` would silently downgrade
        // "no regressions found" to "nobody looked", and this says so here
        // rather than at a merge.
        let review = crate::services::flowdef::FlowBook::defaults()
            .get("review_and_merge")
            .expect("the shipped book always contains review_and_merge")
            .to_graph();

        let written: Vec<&str> = review
            .nodes
            .iter()
            .flat_map(|n| n.writes.iter().map(|w| w.as_str()))
            .collect();
        for key in ["verdict", "finding_count", "checks_state"] {
            assert!(written.contains(&key), "the flow must produce `{key}`");
        }
    }

    #[test]
    fn an_unparsable_finding_count_is_not_read_as_zero_findings() {
        // Defaulting a broken count to 0 would be the one direction that
        // silently merges; the CI rule behind it still has to agree.
        let s = state("looks_safe", "not a number", "failing");
        assert!(decide(&node(Step::Merge), &s).reason().is_some());
    }
}
