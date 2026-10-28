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
    /// The same review, by a second model, for a second opinion.
    SecondOpinion,
    /// Focused reviews — alignment, security, architecture — each turned on
    /// separately in Settings.
    Lenses,
    Merge,
    Sync,
    // ── Release ──
    DraftNotes,
    WriteNotes,
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
    ///
    /// The app also bypasses an optional step that Settings leaves nothing to
    /// run with — no model, no second reviewer, no focused review turned on —
    /// so the merge behind it is not held up. The run's summary says which of
    /// these it was; the label does not claim to know.
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
            NodeStatus::Bypassed => "skipped",
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
/// a missing CLI extension, pulling a model that is not on disk, `az login`
/// (which finishes in the browser). Anything that needs a terminal to answer
/// (`gh auth login`, `az devops login`) stays an instruction, because a
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
    /// Whether ending the run gives the step's work up (closing the pull
    /// request) rather than doing it another way (releasing without notes).
    /// Only decides how the button reads.
    pub abandons: bool,
    /// Artifacts this remedy changes when it succeeds, written before the
    /// step is retried: renaming a branch has to rename it for every later
    /// step too, not only in git.
    pub sets: Vec<(String, String)>,
    /// The last run of it did not finish. Its output says why; the button
    /// says so too, rather than going back to looking untouched.
    pub failed: bool,
    /// Only runnable once git is waiting on no file: finishing a merge while
    /// one is still conflicted just fails, with git's words for why.
    pub once_resolved: bool,
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
            abandons: false,
            sets: vec![],
            failed: false,
            once_resolved: false,
        }
    }

    /// Held back while the step's panel still lists conflicted files.
    pub fn once_resolved(mut self) -> Self {
        self.once_resolved = true;
        self
    }

    pub fn setting(mut self, key: &str, value: &str) -> Self {
        self.sets.push((key.into(), value.into()));
        self
    }

    /// A remedy that resolves the failure by abandoning the step rather than
    /// unblocking it — the run ends here on success instead of re-queuing.
    pub fn terminal(label: &str, program: &str, args: &[&str]) -> Self {
        Self {
            retry_after: false,
            abandons: true,
            ..Self::new(label, program, args)
        }
    }

    /// A remedy that does the step's job itself, differently — the run ends
    /// on success because there is nothing left for the step to do.
    pub fn completes(label: &str, program: &str, args: &[&str]) -> Self {
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
    /// Which run this is. Every fresh run gets its own, kept through retries
    /// and skips, so whatever is driving a run can tell that it was cancelled
    /// — or cancelled and started again — and stop rather than carry on.
    pub run: u64,
    /// How many times each step has been declined in this run. Kept through
    /// retries — that is the point of it: a step approved after being sent
    /// back twice was approved in round three, and the run view says so.
    pub rejections: BTreeMap<NodeId, u32>,
    /// Every status change, in order. What the run view replays: the run as
    /// it happened, step by step, without needing to have been watching.
    pub history: Vec<(NodeId, NodeStatus)>,
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
            run: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            rejections: BTreeMap::new(),
            history: vec![],
        }
    }

    /// A person declined this step: it stops, everything behind it is
    /// blocked, and the round is counted.
    pub fn reject(&mut self, id: &str, graph: &Graph) {
        self.set_status(id, NodeStatus::Rejected);
        self.runs.entry(id.to_string()).or_default().summary = "declined — nothing was run".into();
        *self.rejections.entry(id.to_string()).or_default() += 1;
        self.propagate_block(graph);
    }

    pub fn status(&self, id: &str) -> NodeStatus {
        self.runs.get(id).map(|r| r.status).unwrap_or_default()
    }

    /// The one way a status changes, so that every change is in `history`.
    pub fn set_status(&mut self, id: &str, status: NodeStatus) {
        let run = self.runs.entry(id.to_string()).or_default();
        if run.status != status {
            self.history.push((id.to_string(), status));
        }
        run.status = status;
    }

    /// The step that most recently came to need a person and still does —
    /// one waiting for approval, or one that failed — with where in `history`
    /// that happened. That position is what tells one approval or failure
    /// from the next of the same step after a retry.
    pub fn newest_for_a_person(&self) -> Option<(String, usize)> {
        self.history
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (id, status))| {
                matches!(status, NodeStatus::AwaitingApproval | NodeStatus::Failed)
                    && self.status(id) == *status
            })
            .map(|(at, (id, _))| (id.clone(), at))
    }

    /// This run as it stood after the first `upto` changes in its history:
    /// every status rewound and replayed, and the rejection counts with them.
    /// Everything else — logs, artifacts — is left as it is now.
    pub fn replayed(&self, upto: usize) -> RunState {
        let mut then = self.clone();
        for run in then.runs.values_mut() {
            run.status = NodeStatus::Pending;
        }
        then.rejections.clear();
        for (id, status) in self.history.iter().take(upto) {
            then.runs.entry(id.clone()).or_default().status = *status;
            if *status == NodeStatus::Rejected {
                *then.rejections.entry(id.clone()).or_default() += 1;
            }
        }
        then
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
            run.summary.clear();
            run.log.clear();
            run.proposal.clear();
            run.preview_diff.clear();
            run.items.clear();
            run.held.clear();
            self.set_status(id, NodeStatus::Pending);
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
        self.set_status(id, NodeStatus::Bypassed);
        {
            let run = self.runs.entry(id.to_string()).or_default();
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

/// A step's description, on the map and on its card in the list view, with the run's real branches and pull
/// request in it once the run knows them — `git push -u origin feat/x`
/// rather than `git push -u origin <branch>`. The catalogue's own words until
/// then, and for every step these names say nothing about.
pub fn live_subtitle(step: Step, catalogue: &str, state: &RunState) -> String {
    let art = |key: &str| state.artifact(key).trim().to_string();
    let known = |key: &str| !art(key).is_empty();
    // The branch the work goes out on: the one committed to once it is,
    // the one about to be made before that.
    let branch = if known("work_branch") {
        art("work_branch")
    } else {
        art("branch_name")
    };
    match step {
        Step::Commit if !branch.is_empty() => format!("on {branch}"),
        Step::Push if !branch.is_empty() => format!("git push -u origin {branch}"),
        Step::OpenPr if !branch.is_empty() && known("base") => {
            format!("{branch} \u{2192} {}", art("base"))
        }
        Step::FindPr if known("pr_number") && known("pr_head") => {
            format!("#{} \u{00b7} {}", art("pr_number"), art("pr_head"))
        }
        Step::PrDiff if known("pr_base") && known("pr_head") => {
            format!("git diff {}...{}", art("pr_base"), art("pr_head"))
        }
        Step::Merge if known("pr_number") && known("pr_base") => {
            format!("squash #{} into {}", art("pr_number"), art("pr_base"))
        }
        Step::Sync if known("pr_base") => format!("checkout {} and pull", art("pr_base")),
        _ => catalogue.to_string(),
    }
}

#[cfg(test)]
mod tests;
