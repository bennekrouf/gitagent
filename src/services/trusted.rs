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

use super::graph::{NodeSpec, RunState, Step};

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
