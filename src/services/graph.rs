//! The execution graph — nodes, edges, and the state that survives between them.
//!
//! This module is deliberately free of IO and of any knowledge about git, LLMs,
//! or the commit-and-PR flow in particular. It knows four things:
//!
//!   * a **node** is one bounded unit of work with a declared contract
//!   * an **edge** is a dependency: `deps` names the nodes whose output this
//!     node is allowed to consume
//!   * **state** is the artifact map that crosses those edges
//!   * **scheduling** is deciding which node may run next, and blocking the
//!     ones downstream of a failure
//!
//! Keeping it IO-free is what makes it testable, and what will make it
//! swappable when the flow stops being hardcoded: a generic version replaces
//! `Step` with a registry key and touches nothing else in here.

use std::collections::BTreeMap;

pub type NodeId = String;

/// Which unit of work a node runs. In this first version the set is closed and
/// the flow is hardcoded; the generic version turns this into a lookup key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Preflight,
    ScanChanges,
    DraftCommit,
    Commit,
    DraftPr,
    Push,
    OpenPr,
    // ── Review and merge ──
    FindPr,
    PrStatus,
    PrDiff,
    Analyse,
    Merge,
    Sync,
    // ── Generic ──
    RunScript,
    RunRemote,
}

/// Whether the node spends a model call. Shown in the UI, and the thing to
/// route on once cost matters: deterministic nodes are free, model nodes are
/// not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    Deterministic,
    Model,
}

/// A node's contract: one job, declared inputs, declared outputs, and whether
/// a human has to say yes before it runs.
#[derive(Clone, PartialEq, Debug)]
pub struct NodeSpec {
    pub id: NodeId,
    pub title: String,
    pub subtitle: String,
    pub step: Step,
    pub kind: NodeKind,
    /// Nodes that must be `Done` before this one becomes runnable.
    pub deps: Vec<NodeId>,
    /// Artifact keys this node reads. Declared for display and for the
    /// generic version's validation; the step implementations read them.
    pub reads: Vec<String>,
    /// Artifact keys this node writes on success.
    pub writes: Vec<String>,
    /// Anything that touches git history or the remote waits for approval.
    pub requires_approval: bool,
    /// Per-node settings for steps that take them — the command a script step
    /// runs, and so on. Empty for steps that are fully specified by their code.
    pub config: std::collections::BTreeMap<String, String>,
}

impl NodeSpec {
    pub fn setting(&self, key: &str) -> &str {
        self.config.get(key).map(|s| s.as_str()).unwrap_or("")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NodeStatus {
    /// Waiting on its dependencies, or on the run to start.
    #[default]
    Pending,
    Running,
    /// Work is prepared and described; waiting for the human to approve.
    AwaitingApproval,
    Done,
    /// Ran, found nothing to do, and said so. Not an error — but nothing
    /// downstream can proceed, because the artifacts it would have produced do
    /// not exist.
    Skipped,
    Failed,
    /// The human declined this node.
    Rejected,
    /// A node upstream failed or was rejected, so this can never run.
    Blocked,
}

impl NodeStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeStatus::Done
                | NodeStatus::Skipped
                | NodeStatus::Failed
                | NodeStatus::Rejected
                | NodeStatus::Blocked
        )
    }

    /// CSS class suffix, so the stylesheet owns the colours.
    pub fn css(self) -> &'static str {
        match self {
            NodeStatus::Pending => "pending",
            NodeStatus::Running => "running",
            NodeStatus::AwaitingApproval => "awaiting",
            NodeStatus::Done => "done",
            NodeStatus::Skipped => "skipped",
            NodeStatus::Failed => "failed",
            NodeStatus::Rejected => "rejected",
            NodeStatus::Blocked => "blocked",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NodeStatus::Pending => "pending",
            NodeStatus::Running => "running",
            NodeStatus::AwaitingApproval => "needs approval",
            NodeStatus::Done => "done",
            NodeStatus::Skipped => "skipped",
            NodeStatus::Failed => "failed",
            NodeStatus::Rejected => "rejected",
            NodeStatus::Blocked => "blocked",
        }
    }
}

/// One thing a gated node proposes to act on, which the human may drop before
/// approving. Generic on purpose: today it is the files going into a commit,
/// but "here is the set I am about to act on, uncheck what does not belong" is
/// the shape of most approvals worth having.
#[derive(Clone, PartialEq, Debug)]
pub struct ProposalItem {
    /// Stable identity — the path, for a commit.
    pub key: String,
    pub label: String,
    /// Short qualifier shown beside the label, e.g. "new" or "modified".
    pub note: String,
    pub included: bool,
}

/// A known fix for a specific failure, runnable from the app.
///
/// Only ever populated for causes with an exact, idempotent remedy — installing
/// a missing CLI extension, pulling a model that is not on disk. Anything
/// interactive (`gh auth login`, `az login`) stays an instruction, because a
/// subprocess with no terminal cannot complete it.
#[derive(Clone, PartialEq, Debug)]
pub struct Remedy {
    pub label: String,
    /// The command as the user would type it — shown before it runs.
    pub display: String,
    pub program: String,
    pub args: Vec<String>,
    pub running: bool,
    pub output: String,
    pub done: bool,
}

impl Remedy {
    pub fn new(label: &str, program: &str, args: &[&str]) -> Self {
        Self {
            label: label.into(),
            display: format!("{program} {}", args.join(" ")),
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            running: false,
            output: String::new(),
            done: false,
        }
    }
}

/// Per-node execution record. `summary` is the one-line result shown on the
/// card; `log` is the full transcript shown in the detail pane.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct NodeRun {
    pub status: NodeStatus,
    pub summary: String,
    pub log: String,
    /// Set while `AwaitingApproval`: exactly what will happen if approved.
    pub proposal: String,
    /// Set while `AwaitingApproval`, for a step whose whole point is a diff
    /// (`scan_changes`) — the actual diff, read live, so the approval shows
    /// what you're agreeing to rather than a paraphrase of it.
    pub preview_diff: String,
    /// Items the approval covers. Empty for nodes that offer no choice.
    pub items: Vec<ProposalItem>,
    /// Fixes offered after a failure. Empty when nothing is known to help.
    pub remedies: Vec<Remedy>,
}

impl NodeRun {
    pub fn included_keys(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| i.included)
            .map(|i| i.key.clone())
            .collect()
    }

    /// An approval with items but none selected has nothing to do; the UI
    /// refuses to approve it rather than running an empty command.
    pub fn has_nothing_selected(&self) -> bool {
        !self.items.is_empty() && self.items.iter().all(|i| !i.included)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Graph {
    pub nodes: Vec<NodeSpec>,
}

impl Graph {
    pub fn get(&self, id: &str) -> Option<&NodeSpec> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// Everything that survives between nodes: per-node status and the artifact
/// map. One run of one graph.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct RunState {
    pub runs: BTreeMap<NodeId, NodeRun>,
    pub artifacts: BTreeMap<String, String>,
    pub started: bool,
    /// Approval decisions keyed by node: `true` approved, `false` rejected.
    pub decisions: BTreeMap<NodeId, bool>,
}

impl RunState {
    pub fn fresh(graph: &Graph) -> Self {
        let mut runs = BTreeMap::new();
        for node in &graph.nodes {
            runs.insert(node.id.clone(), NodeRun::default());
        }
        Self {
            runs,
            artifacts: BTreeMap::new(),
            started: false,
            decisions: BTreeMap::new(),
        }
    }

    pub fn status(&self, id: &str) -> NodeStatus {
        self.runs.get(id).map(|r| r.status).unwrap_or_default()
    }

    pub fn set_status(&mut self, id: &str, status: NodeStatus) {
        self.runs.entry(id.to_string()).or_default().status = status;
    }

    pub fn artifact(&self, key: &str) -> &str {
        self.artifacts.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    /// The next node whose dependencies are all satisfied. Returns nodes in
    /// declaration order; the graph permits running the whole ready set at
    /// once, but this first version executes one at a time.
    pub fn next_ready(&self, graph: &Graph) -> Option<NodeSpec> {
        graph
            .nodes
            .iter()
            .find(|n| {
                self.status(&n.id) == NodeStatus::Pending
                    && n.deps.iter().all(|d| self.status(d) == NodeStatus::Done)
            })
            .cloned()
    }

    /// Marks everything downstream of a failed or rejected node as `Blocked`,
    /// so a failure stays a local event instead of leaving the run looking
    /// like it is still going.
    pub fn propagate_block(&mut self, graph: &Graph) {
        loop {
            let mut changed = false;
            for node in &graph.nodes {
                if self.status(&node.id) != NodeStatus::Pending {
                    continue;
                }
                let dead = node.deps.iter().any(|d| {
                    matches!(
                        self.status(d),
                        NodeStatus::Skipped
                            | NodeStatus::Failed
                            | NodeStatus::Rejected
                            | NodeStatus::Blocked
                    )
                });
                if dead {
                    self.set_status(&node.id, NodeStatus::Blocked);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Puts a settled node back in the queue, along with everything that was
    /// blocked behind it, so a run can continue after the cause was fixed
    /// rather than starting over.
    pub fn retry_from(&mut self, id: &str, graph: &Graph) {
        self.decisions.remove(id);
        if let Some(run) = self.runs.get_mut(id) {
            run.status = NodeStatus::Pending;
            run.summary.clear();
            run.log.clear();
            run.proposal.clear();
            run.preview_diff.clear();
            run.items.clear();
        }
        for node in &graph.nodes {
            if self.status(&node.id) == NodeStatus::Blocked {
                self.set_status(&node.id, NodeStatus::Pending);
            }
        }
    }

    /// True once no node can make further progress.
    pub fn is_finished(&self, graph: &Graph) -> bool {
        graph.nodes.iter().all(|n| self.status(&n.id).is_terminal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, deps: &[&str]) -> NodeSpec {
        NodeSpec {
            id: id.into(),
            title: id.into(),
            subtitle: String::new(),
            step: Step::ScanChanges,
            kind: NodeKind::Deterministic,
            deps: deps.iter().map(|d| d.to_string()).collect(),
            reads: vec![],
            writes: vec![],
            requires_approval: false,
            config: Default::default(),
        }
    }

    /// a -> b -> d, a -> c -> d: the diamond. `c` must be reachable without
    /// waiting on `b`, which is the whole point of declaring deps instead of
    /// a sequence.
    fn diamond() -> Graph {
        Graph {
            nodes: vec![
                node("a", &[]),
                node("b", &["a"]),
                node("c", &["a"]),
                node("d", &["b", "c"]),
            ],
        }
    }

    fn item(key: &str, included: bool) -> ProposalItem {
        ProposalItem {
            key: key.into(),
            label: key.into(),
            note: String::new(),
            included,
        }
    }

    #[test]
    fn only_the_included_items_are_handed_to_the_step() {
        let run = NodeRun {
            items: vec![item("a", true), item("b", false), item("c", true)],
            ..Default::default()
        };
        assert_eq!(run.included_keys(), vec!["a", "c"]);
    }

    #[test]
    fn unchecking_everything_is_not_approvable() {
        let run = NodeRun {
            items: vec![item("a", false)],
            ..Default::default()
        };
        assert!(run.has_nothing_selected());
    }

    #[test]
    fn a_node_that_offers_no_choice_is_always_approvable() {
        assert!(!NodeRun::default().has_nothing_selected());
    }

    #[test]
    fn only_the_root_is_ready_at_the_start() {
        let g = diamond();
        let s = RunState::fresh(&g);
        assert_eq!(s.next_ready(&g).unwrap().id, "a");
    }

    #[test]
    fn a_join_waits_for_every_branch() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Done);
        s.set_status("b", NodeStatus::Done);
        // c is still pending, so d must not be offered.
        assert_eq!(s.next_ready(&g).unwrap().id, "c");
        s.set_status("c", NodeStatus::Done);
        assert_eq!(s.next_ready(&g).unwrap().id, "d");
    }

    #[test]
    fn a_failure_blocks_only_what_depends_on_it() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Done);
        s.set_status("b", NodeStatus::Failed);
        s.propagate_block(&g);
        assert_eq!(s.status("c"), NodeStatus::Pending, "c does not depend on b");
        assert_eq!(s.status("d"), NodeStatus::Blocked, "d joins on b");
    }

    #[test]
    fn rejection_blocks_downstream_the_same_way_failure_does() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Rejected);
        s.propagate_block(&g);
        assert!(s.is_finished(&g));
    }

    #[test]
    fn a_step_with_nothing_to_do_stops_the_run_without_failing_it() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Skipped);
        s.propagate_block(&g);
        assert!(s.is_finished(&g));
        assert_eq!(s.status("b"), NodeStatus::Blocked);
        assert_ne!(s.status("a"), NodeStatus::Failed, "not an error");
    }

    #[test]
    fn retrying_a_failed_node_frees_everything_it_had_blocked() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Done);
        s.set_status("b", NodeStatus::Failed);
        s.propagate_block(&g);
        assert_eq!(s.status("d"), NodeStatus::Blocked);

        s.retry_from("b", &g);
        assert_eq!(s.status("b"), NodeStatus::Pending);
        assert_eq!(s.status("d"), NodeStatus::Pending);
        assert_eq!(s.status("a"), NodeStatus::Done, "work already done is kept");
        assert_eq!(s.next_ready(&g).unwrap().id, "b");
    }

    #[test]
    fn retrying_clears_a_stale_approval_so_it_is_asked_again() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.decisions.insert("b".into(), false);
        s.set_status("b", NodeStatus::Rejected);
        s.retry_from("b", &g);
        assert!(!s.decisions.contains_key("b"));
    }

    #[test]
    fn a_run_is_finished_only_when_every_node_settled() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        assert!(!s.is_finished(&g));
        for id in ["a", "b", "c", "d"] {
            s.set_status(id, NodeStatus::Done);
        }
        assert!(s.is_finished(&g));
    }
}
