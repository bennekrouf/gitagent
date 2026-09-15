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
    RunTests,
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
    /// Which producer each input comes from, when it matters. See
    /// `FlowDef::bind` for why, and `RunState::resolved_for` for how.
    pub bind: std::collections::BTreeMap<String, String>,
}

impl NodeSpec {
    pub fn setting(&self, key: &str) -> &str {
        self.config.get(key).map(|s| s.as_str()).unwrap_or("")
    }
}

/// The artifact key a node's output is also filed under, so it can be named
/// specifically rather than only by its bare key.
///
/// Every output is stored twice: bare, which is what every step already reads
/// and what keeps unbound flows behaving exactly as before, and qualified,
/// which is what a binding points at. The cost is a second copy of each
/// artifact; the alternative is that `pr_url` written by two nodes has no way
/// to say which one you meant.
pub fn qualified(node: &str, key: &str) -> String {
    format!("{node}.{key}")
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
    /// Not run, by explicit choice, and the run carries on regardless.
    ///
    /// The difference from `Skipped` is who decided and what happens next: a
    /// step that skipped itself found no work and stops the branch behind it,
    /// whereas a step you bypassed may well have had work to do — you have
    /// said to proceed without it. Whatever it would have written does not
    /// exist, so everything downstream reads those artifacts as empty. That is
    /// the cost, and it is the reason this is a deliberate button and never
    /// something a trusted run does on your behalf.
    Bypassed,
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
                | NodeStatus::Bypassed
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
            NodeStatus::Bypassed => "skipped",
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
            NodeStatus::Bypassed => "skipped by you",
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
    /// Whether the node that failed is worth re-queuing once this remedy
    /// succeeds. True for anything that unblocks the same step (pulling a
    /// missing model, installing a CLI extension) — false for a remedy that
    /// makes the step moot instead (closing the pull request it was trying
    /// to merge), where retrying would just fail again for a new reason.
    pub retry_after: bool,
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
            retry_after: true,
        }
    }

    /// A remedy that resolves the failure by abandoning the step rather than
    /// unblocking it — the run ends here on success instead of re-queuing.
    pub fn terminal(label: &str, program: &str, args: &[&str]) -> Self {
        Self {
            retry_after: false,
            ..Self::new(label, program, args)
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
    /// Why a trusted run declined to click this one through. Empty when no
    /// trusted run was involved, or when it approved.
    pub held: String,
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

    /// This state as `node` sees it: every bound input replaced by the value
    /// its named producer wrote.
    ///
    /// Resolving into a copy, rather than threading a scoped accessor through
    /// every step, is what keeps the 58 `state.artifact("literal")` reads in
    /// the step implementations exactly as they are. A step still asks for
    /// `commit_subject`; which `commit_subject` it gets is the flow's business,
    /// not the step's.
    ///
    /// A binding whose source produced nothing is left alone rather than
    /// blanked — `validate` refuses that flow before it can run, and if one
    /// ever gets here, falling back to the bare key is the behaviour that was
    /// there before bindings existed.
    pub fn resolved_for(&self, node: &NodeSpec) -> Self {
        if node.bind.is_empty() {
            return self.clone();
        }
        let mut resolved = self.clone();
        for (input, source) in &node.bind {
            if let Some(value) = self.artifacts.get(source) {
                resolved.artifacts.insert(input.clone(), value.clone());
            }
        }
        resolved
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
                    && n.deps
                        .iter()
                        .all(|d| matches!(self.status(d), NodeStatus::Done | NodeStatus::Bypassed))
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
            run.held.clear();
        }
        // Free every blocked node, then work out from scratch which of them
        // are still blocked. The blanket un-block on its own is wrong: with
        // two independent failures, retrying one would also free the nodes
        // waiting behind the other, leaving them `Pending` with a `Failed`
        // dependency — never runnable by `next_ready`, never terminal for
        // `is_finished`, and shown on screen as merely waiting their turn.
        for node in &graph.nodes {
            if self.status(&node.id) == NodeStatus::Blocked {
                self.set_status(&node.id, NodeStatus::Pending);
            }
        }
        self.propagate_block(graph);
    }

    /// Marks a node bypassed and frees whatever was blocked behind it.
    ///
    /// The log is kept: when the bypass follows a failure, what it failed with
    /// is the reason there is a bypass at all, and deleting it would leave the
    /// run with no account of why the step did not run.
    pub fn bypass(&mut self, id: &str, graph: &Graph) {
        self.decisions.remove(id);
        {
            let run = self.runs.entry(id.to_string()).or_default();
            run.status = NodeStatus::Bypassed;
            run.summary = "skipped — the run was told to carry on without it".into();
            run.proposal.clear();
            run.preview_diff.clear();
            run.items.clear();
            run.remedies.clear();
            run.held.clear();
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
            bind: Default::default(),
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
    fn retrying_one_failure_leaves_the_other_failures_block_standing() {
        // a -> b -> d and a -> c -> d. Both b and c fail, so d is blocked
        // twice over. Retrying b must not free d: c is still failed, and a
        // `Pending` d would never run and never let the run finish.
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Done);
        s.set_status("b", NodeStatus::Failed);
        s.set_status("c", NodeStatus::Failed);
        s.propagate_block(&g);
        assert_eq!(s.status("d"), NodeStatus::Blocked);

        s.retry_from("b", &g);
        assert_eq!(s.status("b"), NodeStatus::Pending, "the retried node runs");
        assert_eq!(
            s.status("d"),
            NodeStatus::Blocked,
            "d still joins on c, which is still failed"
        );
        assert_eq!(s.next_ready(&g).unwrap().id, "b");
    }

    #[test]
    fn retrying_the_last_failure_frees_the_join_behind_both() {
        let g = diamond();
        let mut s = RunState::fresh(&g);
        s.set_status("a", NodeStatus::Done);
        s.set_status("b", NodeStatus::Done);
        s.set_status("c", NodeStatus::Failed);
        s.propagate_block(&g);

        s.retry_from("c", &g);
        assert_eq!(s.status("c"), NodeStatus::Pending);
        assert_eq!(s.status("d"), NodeStatus::Pending, "nothing blocks it now");
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

    #[test]
    fn a_bypassed_node_lets_the_run_carry_on_past_it() {
        // What Skip is for: `analyse` will not answer, and `merge` does not
        // actually need it to — the person reads the diff themselves.
        let graph = Graph {
            nodes: vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])],
        };
        let mut s = RunState::fresh(&graph);
        s.set_status("a", NodeStatus::Done);
        s.set_status("b", NodeStatus::Failed);
        s.propagate_block(&graph);
        assert_eq!(s.status("c"), NodeStatus::Blocked);

        s.bypass("b", &graph);
        assert_eq!(s.status("b"), NodeStatus::Bypassed);
        assert_eq!(
            s.status("c"),
            NodeStatus::Pending,
            "freed, not left blocked"
        );
        assert_eq!(
            s.next_ready(&graph).map(|n| n.id),
            Some("c".into()),
            "a bypassed dependency counts as satisfied"
        );
    }

    #[test]
    fn a_bypassed_node_is_terminal_but_a_skipped_one_still_blocks() {
        // The two must not be confused: `Skipped` is a step reporting no work,
        // and everything behind it genuinely cannot run.
        assert!(NodeStatus::Bypassed.is_terminal());
        let graph = Graph {
            nodes: vec![node("a", &[]), node("b", &["a"])],
        };
        let mut s = RunState::fresh(&graph);
        s.set_status("a", NodeStatus::Skipped);
        s.propagate_block(&graph);
        assert_eq!(s.status("b"), NodeStatus::Blocked);
    }

    #[test]
    fn bypassing_keeps_the_failure_it_replaced() {
        // The log is the only account of why the step did not run.
        let graph = Graph {
            nodes: vec![node("a", &[])],
        };
        let mut s = RunState::fresh(&graph);
        s.set_status("a", NodeStatus::Failed);
        s.runs.entry("a".into()).or_default().log = "ollama timed out".into();
        s.bypass("a", &graph);
        assert_eq!(s.runs["a"].log, "ollama timed out");
        assert!(s.runs["a"].summary.contains("carry on without it"));
    }

    #[test]
    fn a_bound_input_reads_the_producer_it_names() {
        // `pr_url` written by two nodes: without a binding the map holds
        // whichever ran last, and with one the reader gets the one it asked
        // for — while still asking by its own literal key.
        let mut node = node("merge", &["open_pr", "find_pr"]);
        node.bind
            .insert("pr_url".into(), qualified("open_pr", "pr_url"));

        let mut s = RunState::default();
        s.artifacts
            .insert(qualified("open_pr", "pr_url"), "from-open".into());
        s.artifacts
            .insert(qualified("find_pr", "pr_url"), "from-find".into());
        s.artifacts.insert("pr_url".into(), "from-find".into());

        assert_eq!(s.artifact("pr_url"), "from-find", "bare key is last-write");
        assert_eq!(
            s.resolved_for(&node).artifact("pr_url"),
            "from-open",
            "the binding decides"
        );
    }

    #[test]
    fn a_node_that_binds_nothing_sees_the_state_unchanged() {
        // The compatibility guarantee, at the one place it is enforced.
        let node = node("a", &[]);
        let mut s = RunState::default();
        s.artifacts.insert("diff".into(), "d".into());
        assert_eq!(s.resolved_for(&node), s);
    }

    #[test]
    fn a_binding_whose_source_produced_nothing_falls_back() {
        // `validate` refuses this flow, so it should not arrive — and if it
        // does, the behaviour is the one that existed before bindings did,
        // not a silently blanked input.
        let mut node = node("b", &["a"]);
        node.bind.insert("diff".into(), qualified("a", "diff"));
        let mut s = RunState::default();
        s.artifacts.insert("diff".into(), "bare".into());
        assert_eq!(s.resolved_for(&node).artifact("diff"), "bare");
    }

    #[test]
    fn a_pending_node_can_be_skipped_before_the_run_reaches_it() {
        // The case Skip could not reach: an ungated step that neither stops to
        // ask nor fails, so it has no other control on it.
        let graph = Graph {
            nodes: vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])],
        };
        let mut s = RunState::fresh(&graph);
        s.set_status("a", NodeStatus::Done);
        assert_eq!(s.status("b"), NodeStatus::Pending);

        s.bypass("b", &graph);

        assert_eq!(s.status("b"), NodeStatus::Bypassed);
        assert_eq!(
            s.next_ready(&graph).map(|n| n.id),
            Some("c".into()),
            "the run goes straight to what was behind it"
        );
    }

    #[test]
    fn skipping_a_running_node_leaves_the_log_it_had_written() {
        // The partial output is how you see how far it got before you gave up
        // on it.
        let graph = Graph {
            nodes: vec![node("a", &[])],
        };
        let mut s = RunState::fresh(&graph);
        s.set_status("a", NodeStatus::Running);
        s.runs.entry("a".into()).or_default().log = "$ cargo test\nrunning 40 tests".into();

        s.bypass("a", &graph);

        assert_eq!(s.status("a"), NodeStatus::Bypassed);
        assert!(s.runs["a"].log.contains("running 40 tests"));
    }
}
