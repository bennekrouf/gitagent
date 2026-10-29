//! How one step of a flow is run, and a driver that runs a whole flow without
//! a window.
//!
//! Two layers. The step layer — when a step needs approval, what it proposes,
//! which steps Settings leaves nothing to run with, how a step's result lands
//! in the run — is shared by every driver: the window's `drive`, and `Run`
//! below. The window keeps its own loop around these, because it waits on
//! buttons, streams logs into signals and races each step against Skip; none
//! of that has a meaning without a window.
//!
//! `Run` is the other layer: a flow driven a call at a time, stopping at each
//! approval and carried on only by `decide`. It is what `gitagent mcp` and
//! `gitagent run` both drive, so the two cannot drift apart on what a step
//! does or what a trusted run lets through.

use super::flow::{self, StepFailure, StepOutcome};
use super::flowdef::FlowBook;
use super::graph::{Graph, NodeKind, NodeSpec, NodeStatus, ProposalItem, RunState, Step};
use super::llm::LlmConfig;
use super::{busy, forge, git, live, probe, release, store, trusted};
use crate::telemetry;

// ── One step ──────────────────────────────────────────────────────────────

/// Whether this step stops for a person before it runs. A gated step with
/// nothing to act on — no notes written, a first commit with no PR to open —
/// runs straight through.
pub fn needs_approval(node: &NodeSpec, state: &RunState) -> bool {
    node.requires_approval && !flow::nothing_to_approve(node, &state.resolved_for(node))
}

/// What a gated step will do, read before the run stops on it.
pub struct Proposal {
    pub proposal: String,
    pub items: Vec<ProposalItem>,
    pub preview_diff: String,
}

impl Proposal {
    /// Read from the state the step itself will see — otherwise the proposal
    /// quotes one node's `commit_subject` and the commit uses another's.
    pub async fn read(node: &NodeSpec, repo: &str, resolved: &RunState) -> Self {
        Proposal {
            proposal: flow::proposal(node, resolved),
            items: flow::proposal_items(node, resolved),
            preview_diff: flow::diff_preview(node, repo, resolved)
                .await
                .unwrap_or_default(),
        }
    }

    /// Puts the proposal on the node and parks the run there.
    pub fn post(self, state: &mut RunState, node: &str) {
        let entry = state.runs.entry(node.into()).or_default();
        entry.proposal = self.proposal;
        entry.items = self.items;
        entry.preview_diff = self.preview_diff;
        state.set_status(node, NodeStatus::AwaitingApproval);
    }
}

/// Settles a step that Settings leaves nothing to run with, and says whether
/// it did. Such a step is bypassed rather than failed: the merge behind it
/// waits on it, and an optional step nobody set up must not stop a merge.
pub fn bypass_unconfigured(state: &mut RunState, node: &NodeSpec, cfg: &LlmConfig) -> bool {
    let bypass = |state: &mut RunState, summary: String, log: String| {
        state.set_status(&node.id, NodeStatus::Bypassed);
        let entry = state.runs.entry(node.id.clone()).or_default();
        entry.summary = summary;
        entry.log = log;
    };
    // A model step on an install with no model never calls one: it writes
    // what can be derived from the diff and is marked skipped.
    if node.kind == NodeKind::Model && !cfg.uses_model() {
        let stand_in = flow::without_model(node, state);
        for (k, v) in stand_in.artifacts {
            state
                .artifacts
                .insert(super::graph::qualified(&node.id, &k), v.clone());
            state.artifacts.insert(k, v);
        }
        bypass(state, stand_in.summary, stand_in.log);
        return true;
    }
    if node.step == Step::Lenses && !cfg.lenses.any() {
        bypass(
            state,
            "skipped \u{2014} no focused review turned on".into(),
            "No focused review is turned on in Settings, so none ran. Turn on \
             alignment, security or architecture under Model provider \u{2192} \
             Focused reviews to have every pull request checked for it."
                .into(),
        );
        return true;
    }
    if node.step == Step::SecondOpinion && !cfg.second_config().uses_model() {
        bypass(
            state,
            "skipped \u{2014} no second reviewer chosen".into(),
            "No second reviewer is chosen in Settings, so nothing gave a second \
             opinion. Pick one under Model provider \u{2192} Second reviewer to have \
             every pull request reviewed twice."
                .into(),
        );
        return true;
    }
    false
}

/// Writes a finished step's result into the run.
pub fn settle(
    state: &mut RunState,
    graph: &Graph,
    node: &str,
    result: Result<StepOutcome, StepFailure>,
) {
    match result {
        Ok(outcome) => {
            let nothing = outcome.nothing_to_do;
            for (k, v) in outcome.artifacts {
                state.artifacts.insert(k, v);
            }
            state.set_status(
                node,
                if nothing {
                    NodeStatus::Skipped
                } else {
                    NodeStatus::Done
                },
            );
            let entry = state.runs.entry(node.into()).or_default();
            entry.summary = outcome.summary;
            entry.log = outcome.log;
            // A step may offer files for deselection (scan does), and a
            // re-run must not silently restore ones already dropped.
            if !outcome.items.is_empty() {
                entry.items = outcome.items;
            }
            // Nothing to do is still nothing downstream can build on.
            if nothing {
                state.propagate_block(graph);
            }
        }
        Err(failure) => {
            state.set_status(node, NodeStatus::Failed);
            let entry = state.runs.entry(node.into()).or_default();
            entry.summary = "failed".into();
            entry.log = failure.message;
            entry.remedies = failure.remedies;
            // A failure stays local: only what depends on this node is blocked.
            state.propagate_block(graph);
        }
    }
}

/// Tells the usage statistics how a run ended, if it ended — a run that
/// stopped with work still pending has not produced an outcome yet.
///
/// Gated on `telemetry::active()` before doing anything, including the git
/// call that names the forge: with statistics off this costs nothing.
pub async fn report_finished(repo: &str, flow: &str, graph: &Graph, state: &RunState) {
    if !telemetry::active() || !state.is_finished(graph) {
        return;
    }
    let statuses: Vec<NodeStatus> = graph.nodes.iter().map(|n| state.status(&n.id)).collect();
    let forge = match git::remote_url(repo).await {
        Some(url) => forge::detect(&url),
        None => forge::Forge::None,
    };
    telemetry::record(telemetry::Event::flow_finished(
        flow,
        telemetry::outcome_of(&statuses),
        &forge,
        store::load_settings_for(Some(repo)).kind,
    ));
    // A finished run is the moment worth getting to the server promptly;
    // detached so the run never waits on the network.
    tokio::spawn(telemetry::flush());
}

// ── A flow, without a window ──────────────────────────────────────────────

/// What a driver without a window hears while a run moves, for a terminal to
/// show it as it happens — and for `gitagent mcp` to pass on as progress
/// notifications, when the client asked for them with its call.
pub enum Progress<'a> {
    /// A step has started.
    Step(&'a NodeSpec),
    /// A line of a running step's output.
    Line(&'a str),
    /// A step has settled, one way or another.
    Settled(&'a NodeSpec, &'a RunState),
    /// A trusted run approved this step itself.
    Trusted(&'a NodeSpec),
}

/// One flow being run for one repository.
pub struct Run {
    pub repo: String,
    pub flow: String,
    pub graph: Graph,
    pub state: RunState,
    /// Started as a trusted run: approvals it would let through are answered
    /// here, by the same rules as the window's. Cleared at the first hold,
    /// as there — from then on the run is the person's again.
    pub trusted: bool,
    /// Steps approved on the strength of trust, not a person — said in the
    /// report, so the driver can tell its user what went through.
    pub approved_by_trust: Vec<String>,
    /// Why a trusted run stopped where it did.
    pub held: Option<String>,
    /// Driven from a terminal: an open window says to answer it there.
    pub terminal: bool,
    pub progress: fn(Progress<'_>),
    /// This repository, held for as long as the run is going — see
    /// `services::busy`. Let go the moment the run ends.
    pub claim: Option<busy::Claim>,
}

/// Where a trusted run goes after a flow it finished.
pub enum Next {
    /// On into this flow, scoped to this pull request (empty for none).
    Flow(String, String),
    /// Nowhere, and why when there is something to say.
    Stop(Option<String>),
}

/// The path as the app keys repositories, so a run here is the same
/// repository the window shows.
pub fn repo_path(raw: &str) -> Result<String, String> {
    let path = dunce::canonicalize(raw).map_err(|e| format!("{raw}: {} ({e})", io_reason(&e)))?;
    if !path.join(".git").exists() {
        return Err(format!("{} is not a git repository", path.display()));
    }
    Ok(path.to_string_lossy().into_owned())
}

/// Why a path could not be opened, in English. The OS text that follows it
/// in brackets comes in the system's display language — French on a French
/// Windows — and the model reading the reply should not have to translate.
fn io_reason(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => "path not found",
        PermissionDenied => "permission denied",
        NotADirectory => "not a folder",
        InvalidFilename => "not a valid path",
        _ => "could not open",
    }
}

impl Run {
    pub fn new(repo: String, flow: String, graph: Graph, state: RunState, trusted: bool) -> Self {
        Run {
            repo,
            flow,
            graph,
            state,
            trusted,
            approved_by_trust: vec![],
            held: None,
            terminal: false,
            progress: |_| {},
            claim: None,
        }
    }

    /// The flow `flow_id`, set up for `repo` (as `repo_path` gives it) and
    /// not yet started, with the repository claimed for it — refused while
    /// another run, in any GitAgent process, has it.
    pub fn prepare(
        repo: &str,
        flow_id: &str,
        pr: &str,
        trusted: bool,
        by: busy::By,
    ) -> Result<Self, String> {
        let book = FlowBook::load();
        let hidden = store::load_repo_flows().hidden_for(repo);
        let def = book
            .runnable_for(&hidden)
            .into_iter()
            .find(|f| f.id == flow_id)
            .cloned()
            .ok_or(format!("no flow {flow_id} for this repository"))?;
        let graph = def.to_graph();
        let mut state = RunState::fresh(&graph);
        state.started = true;
        if !pr.is_empty() {
            state
                .artifacts
                .insert("selected_pr_number".into(), pr.into());
        }
        let claim = busy::claim(repo, &def.label, by)?;
        let mut run = Run::new(repo.into(), def.id, graph, state, trusted);
        run.claim = Some(claim);
        Ok(run)
    }

    /// Answered by trust: asked for when the run started, or "Trust all"
    /// turned on in the window, re-read at every step so turning it off there
    /// takes effect here too.
    pub fn trusting(&self) -> bool {
        self.held.is_none() && (self.trusted || store::trust_all())
    }

    /// The step stopped at an approval, if one is.
    pub fn waiting(&self) -> Option<NodeSpec> {
        self.graph
            .nodes
            .iter()
            .find(|n| self.state.status(&n.id) == NodeStatus::AwaitingApproval)
            .cloned()
    }

    pub fn is_finished(&self) -> bool {
        self.state.is_finished(&self.graph)
    }

    /// Where a trusted run that just finished this flow goes next.
    pub async fn next(&self) -> Next {
        if !self.trusting()
            || !self.is_finished()
            || !trusted::may_continue(&self.state, &self.graph)
        {
            return Next::Stop(None);
        }
        let status = probe::probe(&self.repo).await;
        let hidden = store::load_repo_flows().hidden_for(&self.repo);
        let book = FlowBook::load();
        match trusted::next_flow(&book, &status, &hidden) {
            // The same flow on the same pull request again means this one did
            // not move the repository on; what is left needs a person.
            Some((flow, pr))
                if flow == self.flow && pr == self.state.artifact("selected_pr_number") =>
            {
                Next::Stop(None)
            }
            Some((flow, pr)) => Next::Flow(flow, pr),
            None => Next::Stop(trusted::why_stopped(&book, &status, &hidden)),
        }
    }

    /// The person's answer to the step the run is waiting on, then on to the
    /// next stop. `exclude` leaves offered files out of what is approved.
    pub async fn decide(&mut self, approved: bool, exclude: &[String]) -> Result<(), String> {
        let node = self
            .waiting()
            .ok_or("this run is not waiting on an approval")?;
        // An exclusion that matches nothing on offer is refused rather than
        // dropped: the user unchecked that file, and approving anyway would
        // commit it. Scan is where this bites — its files are only offered at
        // the commit step, once scan has found them.
        let offered: Vec<String> = self
            .state
            .runs
            .get(&node.id)
            .map(|r| r.items.iter().map(|i| i.key.clone()).collect())
            .unwrap_or_default();
        let unknown: Vec<&String> = exclude.iter().filter(|p| !offered.contains(p)).collect();
        if approved && !unknown.is_empty() {
            return Err(if offered.is_empty() {
                format!(
                    "{} offers no files to leave out; nothing was approved. Approve without \
                     `exclude`, and leave files out at the commit step, which lists them.",
                    node.title
                )
            } else {
                format!(
                    "not offered here: {}; nothing was approved. The files are: {}",
                    unknown
                        .iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    offered.join(", ")
                )
            });
        }
        telemetry::record(telemetry::Event::ApprovalDecided {
            approved,
            trusted: false,
        });
        if !approved {
            self.state.reject(&node.id, &self.graph);
            self.advance().await;
            return Ok(());
        }

        // Unchecking a file here is unchecking it in the window: the commit
        // step stages what is still included on its own node.
        if let Some(entry) = self.state.runs.get_mut(&node.id) {
            for item in entry.items.iter_mut() {
                if exclude.iter().any(|path| path == &item.key) {
                    item.included = false;
                }
            }
            if entry.has_nothing_selected() {
                return Err(
                    "every file is excluded, so there is nothing to commit; reject instead".into(),
                );
            }
        }
        self.state.decisions.insert(node.id.clone(), true);
        self.run_node(&node).await;
        self.advance().await;
        Ok(())
    }

    /// Tells an open window where this run stands — see `services::live`.
    fn publish(&self) {
        let label = FlowBook::load()
            .get(&self.flow)
            .map(|f| f.label.clone())
            .unwrap_or_else(|| self.flow.clone());
        let mut live = live::snapshot(
            self.state.run,
            &self.repo,
            &self.flow,
            &label,
            &self.graph,
            &self.state,
            self.trusting(),
        );
        live.terminal = self.terminal;
        live::publish(live);
    }

    /// Runs ready steps until the run ends or one stops for approval.
    pub async fn advance(&mut self) {
        loop {
            if self.waiting().is_some() {
                return;
            }
            let Some(node) = self.state.next_ready(&self.graph) else {
                break;
            };
            if needs_approval(&node, &self.state) {
                let resolved = self.state.resolved_for(&node);
                Proposal::read(&node, &self.repo, &resolved)
                    .await
                    .post(&mut self.state, &node.id);
                // Trusted: the window's rules decide, not this driver's leniency.
                if self.trusting() {
                    match trusted::decide(&node, &resolved) {
                        trusted::Verdict::Approve => {
                            telemetry::record(telemetry::Event::ApprovalDecided {
                                approved: true,
                                trusted: true,
                            });
                            self.state.decisions.insert(node.id.clone(), true);
                            self.approved_by_trust.push(node.title.clone());
                            (self.progress)(Progress::Trusted(&node));
                            self.publish();
                            self.run_node(&node).await;
                            continue;
                        }
                        trusted::Verdict::Hold(why) => {
                            if let Some(entry) = self.state.runs.get_mut(&node.id) {
                                entry.held = why.clone();
                            }
                            self.held = Some(why);
                        }
                    }
                }
                self.publish();
                return;
            }
            self.run_node(&node).await;
        }
        self.claim = None;
        self.publish();
        release::record_shipped(&self.repo, &self.flow, &self.graph, &self.state).await;
        report_finished(&self.repo, &self.flow, &self.graph, &self.state).await;
    }

    /// One step, start to settled.
    async fn run_node(&mut self, node: &NodeSpec) {
        // Read each time: Settings may change in the window while a run is
        // held open, and the next step should use what is set now. Scoped to
        // the repository, so a run here uses the model chosen for the folder
        // it sits in rather than a different window's.
        let cfg: LlmConfig = store::load_settings_for(Some(&self.repo));
        self.state.set_status(&node.id, NodeStatus::Running);
        (self.progress)(Progress::Step(node));
        self.publish();

        if !bypass_unconfigured(&mut self.state, node, &cfg) {
            let resolved = self.state.resolved_for(node);
            let progress = self.progress;
            let result = flow::execute(node, &self.repo, &cfg, &resolved, &mut |line| {
                progress(Progress::Line(line))
            })
            .await;
            settle(&mut self.state, &self.graph, &node.id, result);
        }
        (self.progress)(Progress::Settled(node, &self.state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_repo_is_named_in_english() {
        let err = repo_path("/gitagent/no/such/repo").unwrap_err();
        assert!(
            err.starts_with("/gitagent/no/such/repo: path not found ("),
            "got {err}"
        );
    }
}
