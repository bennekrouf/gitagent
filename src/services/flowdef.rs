//! Flows as data: what is stored on disk, how it becomes a `Graph`, and what
//! makes one invalid.
//!
//! A flow file names steps from the catalogue and wires them together. It does
//! not describe what a step reads or writes — the catalogue owns that, because
//! those are facts about the implementation. What the file owns is the shape:
//! which steps are present, what depends on what, what each is called, and
//! which ones stop for a human.
//!
//! Everything here is pure. `validate` is the reason the editor can let you
//! rewire freely: it says exactly what is wrong rather than letting you save a
//! graph that deadlocks or reads an artifact nobody produces.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::catalogue;
use super::graph::{Graph, NodeSpec};

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct NodeDef {
    pub id: String,
    /// Catalogue key.
    pub step: String,
    /// Empty means "use the catalogue's". Skipped on write, so a flow file
    /// only ever states what it actually overrides.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
    /// `None` means "use the catalogue's default gating".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated: Option<bool>,
    /// Per-node settings, for steps that take them. See `StepInfo::config`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
}

impl NodeDef {
    pub fn from_catalogue(id: &str, key: &str) -> Self {
        Self {
            id: id.to_string(),
            step: key.to_string(),
            title: String::new(),
            subtitle: String::new(),
            deps: vec![],
            gated: None,
            config: BTreeMap::new(),
        }
    }

    pub fn setting(&self, key: &str) -> &str {
        self.config.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn is_gated(&self) -> bool {
        self.gated.unwrap_or_else(|| {
            catalogue::by_key(&self.step)
                .map(|i| i.gate_by_default)
                .unwrap_or(false)
        })
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct FlowDef {
    pub id: String,
    pub label: String,
    /// Which repository states this flow is the answer to, by `Need::key`.
    ///
    /// Declared rather than inferred from the id, so a flow called "Deploy
    /// VPS" can answer the release need without having to be named `release`.
    /// Empty means the flow is never auto-selected — it still runs when picked.
    #[serde(default)]
    pub handles: Vec<String>,
    #[serde(default, rename = "node")]
    pub nodes: Vec<NodeDef>,
}

impl FlowDef {
    /// Materialises the runnable graph. Nodes naming an unknown step are
    /// dropped — `validate` reports them, and a broken entry must not take the
    /// rest of the flow down with it.
    pub fn to_graph(&self) -> Graph {
        Graph {
            nodes: self
                .nodes
                .iter()
                .filter_map(|def| {
                    let info = catalogue::by_key(&def.step)?;
                    Some(NodeSpec {
                        id: def.id.clone(),
                        title: if def.title.is_empty() {
                            info.title.to_string()
                        } else {
                            def.title.clone()
                        },
                        subtitle: if def.subtitle.is_empty() {
                            info.subtitle.to_string()
                        } else {
                            def.subtitle.clone()
                        },
                        step: info.step,
                        kind: info.kind,
                        deps: def.deps.clone(),
                        reads: catalogue::expand(info.reads, &def.id),
                        writes: catalogue::expand(info.writes, &def.id),
                        requires_approval: def.is_gated(),
                        config: def.config.clone(),
                    })
                })
                .collect(),
        }
    }

    /// Whether this flow says it answers `need`.
    pub fn answers(&self, need: crate::services::probe::Need) -> bool {
        self.handles.iter().any(|h| h == need.key())
    }

    pub fn first_node(&self) -> String {
        self.nodes
            .iter()
            .find(|n| n.deps.is_empty())
            .map(|n| n.id.clone())
            .unwrap_or_default()
    }

    /// An id no existing node uses, derived from the step key.
    pub fn free_id(&self, key: &str) -> String {
        if !self.nodes.iter().any(|n| n.id == key) {
            return key.to_string();
        }
        (2..)
            .map(|i| format!("{key}_{i}"))
            .find(|candidate| !self.nodes.iter().any(|n| &n.id == candidate))
            .unwrap_or_else(|| key.to_string())
    }

    /// Removes a node and every reference to it, so deleting can never leave a
    /// dangling dependency behind.
    pub fn remove_node(&mut self, id: &str) {
        self.nodes.retain(|n| n.id != id);
        for node in &mut self.nodes {
            node.deps.retain(|d| d != id);
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Problem {
    Empty,
    NoRoot,
    DuplicateId(String),
    UnknownStep { node: String, key: String },
    UnknownDep { node: String, dep: String },
    SelfDep(String),
    Cycle(Vec<String>),
    MissingInput { node: String, key: String },
    MissingSetting { node: String, label: String },
}

impl Problem {
    pub fn message(&self) -> String {
        match self {
            Problem::Empty => "This flow has no steps.".into(),
            Problem::NoRoot => "Every step depends on another, so nothing can start. At least one \
                 step needs no dependencies."
                .into(),
            Problem::DuplicateId(id) => format!("Two steps are both called `{id}`."),
            Problem::UnknownStep { node, key } => {
                format!("`{node}` uses the step `{key}`, which does not exist.")
            }
            Problem::UnknownDep { node, dep } => {
                format!("`{node}` depends on `{dep}`, which is not in this flow.")
            }
            Problem::SelfDep(id) => format!("`{id}` depends on itself."),
            Problem::Cycle(ids) => {
                format!(
                    "These steps depend on each other in a loop: {}.",
                    ids.join(" → ")
                )
            }
            Problem::MissingInput { node, key } => {
                format!("`{node}` reads `{key}`, but nothing it depends on produces it.")
            }
            Problem::MissingSetting { node, label } => {
                format!("`{node}` needs its {label} filled in before it can run.")
            }
        }
    }
}

/// Everything wrong with a flow, in the order worth fixing it.
pub fn validate(flow: &FlowDef) -> Vec<Problem> {
    let mut problems = vec![];

    if flow.nodes.is_empty() {
        return vec![Problem::Empty];
    }

    let mut seen = HashSet::new();
    for node in &flow.nodes {
        if !seen.insert(node.id.clone()) {
            problems.push(Problem::DuplicateId(node.id.clone()));
        }
        if catalogue::by_key(&node.step).is_none() {
            problems.push(Problem::UnknownStep {
                node: node.id.clone(),
                key: node.step.clone(),
            });
        }
    }

    let ids: HashSet<&str> = flow.nodes.iter().map(|n| n.id.as_str()).collect();
    for node in &flow.nodes {
        for dep in &node.deps {
            if dep == &node.id {
                problems.push(Problem::SelfDep(node.id.clone()));
            } else if !ids.contains(dep.as_str()) {
                problems.push(Problem::UnknownDep {
                    node: node.id.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    if !flow.nodes.iter().any(|n| n.deps.is_empty()) {
        problems.push(Problem::NoRoot);
    }

    // Kahn's algorithm: whatever cannot be ordered is in, or behind, a cycle.
    let order = topological_order(flow);
    if order.len() != flow.nodes.len() {
        let stuck: Vec<String> = flow
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .filter(|id| !order.contains(id))
            .collect();
        if !stuck.is_empty() {
            problems.push(Problem::Cycle(stuck));
        }
    }

    // An input is satisfied only by a *transitive* dependency. A node that
    // happens to run earlier is not enough — the graph gives no such promise.
    let by_id: HashMap<&str, &NodeDef> = flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for node in &flow.nodes {
        let Some(info) = catalogue::by_key(&node.step) else {
            continue;
        };
        for field in info.config {
            if field.required && node.setting(field.key).trim().is_empty() {
                problems.push(Problem::MissingSetting {
                    node: node.id.clone(),
                    label: field.label.to_lowercase(),
                });
            }
        }

        let upstream = ancestors(flow, &node.id);
        for key in catalogue::expand(info.reads, &node.id) {
            let produced = upstream.iter().any(|id| {
                by_id
                    .get(id.as_str())
                    .and_then(|d| catalogue::by_key(&d.step))
                    .map(|i| catalogue::expand(i.writes, id).contains(&key))
                    .unwrap_or(false)
            });
            if !produced {
                problems.push(Problem::MissingInput {
                    node: node.id.clone(),
                    key,
                });
            }
        }
    }

    problems
}

/// Every node reachable by following dependencies upward. Cycle-safe.
pub fn ancestors(flow: &FlowDef, id: &str) -> BTreeSet<String> {
    let by_id: HashMap<&str, &NodeDef> = flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut found = BTreeSet::new();
    let mut queue: Vec<String> = by_id.get(id).map(|n| n.deps.clone()).unwrap_or_default();

    while let Some(next) = queue.pop() {
        if !found.insert(next.clone()) {
            continue;
        }
        if let Some(node) = by_id.get(next.as_str()) {
            queue.extend(node.deps.clone());
        }
    }
    found
}

pub fn topological_order(flow: &FlowDef) -> Vec<String> {
    let mut remaining: Vec<&NodeDef> = flow.nodes.iter().collect();
    let mut done: Vec<String> = vec![];

    loop {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|n| n.deps.iter().all(|d| done.contains(d)))
            .map(|n| n.id.clone())
            .collect();
        if ready.is_empty() {
            break;
        }
        remaining.retain(|n| !ready.contains(&n.id));
        done.extend(ready);
    }
    done
}

/// True when a dependency may be added without closing a loop.
pub fn can_depend_on(flow: &FlowDef, node: &str, candidate: &str) -> bool {
    node != candidate && !ancestors(flow, candidate).contains(node)
}

/// Where a newly added node should attach. With a node selected, it branches
/// off that one; with nothing selected, it attaches after every step nothing
/// else depends on, so the flow keeps growing at its end.
pub fn default_deps(flow: &FlowDef, selected: &str) -> Vec<String> {
    if !selected.is_empty() && flow.nodes.iter().any(|n| n.id == selected) {
        return vec![selected.to_string()];
    }
    let depended_on: HashSet<&str> = flow
        .nodes
        .iter()
        .flat_map(|n| n.deps.iter().map(|d| d.as_str()))
        .collect();
    flow.nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| !depended_on.contains(id))
        .map(|s| s.to_string())
        .collect()
}

/// Every flow the app knows, and the file they live in.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct FlowBook {
    #[serde(default, rename = "flow")]
    pub flows: Vec<FlowDef>,
}

const FLOWS_FILE: &str = "flows.toml";

impl FlowBook {
    /// The two flows the app ships with. Also what "Restore defaults" restores,
    /// so this stays the definition of standard rather than a one-time seed.
    pub fn defaults() -> Self {
        Self {
            flows: vec![
                FlowDef {
                    id: "commit_and_pr".into(),
                    label: "Commit → PR".into(),
                    handles: vec!["uncommitted".into(), "unpushed_branch".into()],
                    nodes: vec![
                        NodeDef::from_catalogue("preflight", "preflight"),
                        dep(
                            NodeDef::from_catalogue("scan", "scan_changes"),
                            &["preflight"],
                        ),
                        dep(
                            NodeDef::from_catalogue("draft_commit", "draft_commit"),
                            &["scan"],
                        ),
                        // Off `scan` rather than off `draft_commit`, so the
                        // suite runs while the model is still writing the
                        // message instead of after it — and so a red test
                        // stops the run before any history is touched.
                        dep(NodeDef::from_catalogue("test", "run_tests"), &["scan"]),
                        dep(
                            NodeDef::from_catalogue("commit", "commit"),
                            &["draft_commit", "test"],
                        ),
                        dep(
                            NodeDef::from_catalogue("draft_pr", "draft_pr"),
                            &["draft_commit"],
                        ),
                        dep(NodeDef::from_catalogue("push", "push"), &["commit"]),
                        dep(
                            NodeDef::from_catalogue("open_pr", "open_pr"),
                            &["push", "draft_pr"],
                        ),
                    ],
                },
                FlowDef {
                    id: "review_and_merge".into(),
                    label: "Review → Merge".into(),
                    handles: vec!["open_pull_request".into()],
                    nodes: vec![
                        // Reviewing needs the same credential check committing
                        // does: without it, `gh pr view` fails with a raw CLI
                        // error instead of a preflight with a Fix button.
                        NodeDef::from_catalogue("preflight", "preflight"),
                        dep(
                            NodeDef::from_catalogue("find_pr", "find_pr"),
                            &["preflight"],
                        ),
                        dep(
                            NodeDef::from_catalogue("pr_status", "pr_status"),
                            &["find_pr"],
                        ),
                        dep(NodeDef::from_catalogue("pr_diff", "pr_diff"), &["find_pr"]),
                        dep(NodeDef::from_catalogue("analyse", "analyse"), &["pr_diff"]),
                        dep(
                            NodeDef::from_catalogue("merge", "merge"),
                            &["pr_status", "analyse"],
                        ),
                        dep(NodeDef::from_catalogue("sync", "sync"), &["merge"]),
                    ],
                },
            ],
        }
    }

    /// Reads `flows.toml`, falling back to the defaults when it is absent or
    /// unreadable. A corrupt file must not leave the app with no flows at all.
    pub fn load() -> Self {
        let path = super::store::data_dir().join(FLOWS_FILE);
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::defaults();
        };
        match toml::from_str::<FlowBook>(&text) {
            Ok(mut book) if !book.flows.is_empty() => {
                book.adopt_missing_declarations();
                book.adopt_missing_test_step();
                book
            }
            _ => Self::defaults(),
        }
    }

    pub fn save(&self) {
        let dir = super::store::data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(text) = toml::to_string_pretty(self) {
            let _ = std::fs::write(dir.join(FLOWS_FILE), text);
        }
    }

    /// Fills in `handles` for shipped flows saved before the field existed.
    ///
    /// Without this, an existing `flows.toml` loads with every flow declaring
    /// nothing, so no flow answers any need and selection silently falls back
    /// to whichever flow happens to be first — landing on "Commit → PR" for a
    /// repository whose only outstanding work is a release.
    ///
    /// Only ever fills an *empty* list, so a deliberate choice to clear the
    /// declarations is left alone.
    pub fn adopt_missing_declarations(&mut self) {
        let shipped = Self::defaults();
        for flow in &mut self.flows {
            if flow.handles.is_empty() {
                if let Some(original) = shipped.get(&flow.id) {
                    flow.handles = original.handles.clone();
                }
            }
        }
    }

    /// Adds the test step to a commit flow saved before that step existed.
    ///
    /// Only to one still shaped exactly as shipped. The check is the shipped
    /// flow with the test node taken back out — `remove_node` strips the edge
    /// into `commit` too, so what it compares against is precisely the graph
    /// this used to be. Once you have rewired the flow it is yours, and
    /// silently inserting a node into someone's graph is a worse surprise than
    /// a step they have to add themselves.
    pub fn adopt_missing_test_step(&mut self) {
        let Some(shipped) = Self::defaults().get("commit_and_pr").cloned() else {
            return;
        };
        let mut before = shipped.clone();
        before.remove_node("test");
        if before.nodes == shipped.nodes {
            return; // The step is gone from the defaults; nothing to adopt.
        }

        for flow in &mut self.flows {
            if flow.id == shipped.id && flow.nodes == before.nodes {
                flow.nodes = shipped.nodes.clone();
            }
        }
    }

    pub fn get(&self, id: &str) -> Option<&FlowDef> {
        self.flows.iter().find(|f| f.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut FlowDef> {
        self.flows.iter_mut().find(|f| f.id == id)
    }

    pub fn free_flow_id(&self, base: &str) -> String {
        if !self.flows.iter().any(|f| f.id == base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base}_{i}"))
            .find(|c| !self.flows.iter().any(|f| &f.id == c))
            .unwrap_or_else(|| base.to_string())
    }

    /// Flows that are safe to run — anything invalid is offered in Setup for
    /// repair, never handed to the executor.
    pub fn runnable(&self) -> Vec<&FlowDef> {
        self.flows
            .iter()
            .filter(|f| validate(f).is_empty())
            .collect()
    }

    /// Every flow, each with whatever is wrong with it.
    ///
    /// What the tab strip lists, as opposed to `runnable`, which is what may
    /// actually be started. A flow that stopped validating used to vanish from
    /// the strip altogether, which reads exactly like having deleted it — the
    /// flow is still in `flows.toml` and one edit from working, so it is shown
    /// and marked instead.
    pub fn listed(&self) -> Vec<(&FlowDef, Vec<Problem>)> {
        self.flows.iter().map(|f| (f, validate(f))).collect()
    }

    /// Copies a flow's nodes into a new flow, `_copy`/`_copy_2`/... suffixed.
    /// Returns the new id, or `None` if `id` does not name an existing flow.
    pub fn duplicate(&mut self, id: &str) -> Option<String> {
        let mut copy = self.get(id)?.clone();
        let new_id = self.free_flow_id(&format!("{id}_copy"));
        copy.id = new_id.clone();
        self.flows.push(copy);
        Some(new_id)
    }
}

fn dep(mut node: NodeDef, deps: &[&str]) -> NodeDef {
    node.deps = deps.iter().map(|d| d.to_string()).collect();
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, step: &str, deps: &[&str]) -> NodeDef {
        NodeDef {
            id: id.into(),
            step: step.into(),
            title: String::new(),
            subtitle: String::new(),
            deps: deps.iter().map(|d| d.to_string()).collect(),
            gated: None,
            config: BTreeMap::new(),
        }
    }

    fn commit_flow() -> FlowDef {
        FlowDef {
            id: "commit_and_pr".into(),
            label: "Commit → PR".into(),
            handles: vec![],
            nodes: vec![
                node("preflight", "preflight", &[]),
                node("scan", "scan_changes", &["preflight"]),
                node("draft_commit", "draft_commit", &["scan"]),
                node("commit", "commit", &["draft_commit"]),
                node("draft_pr", "draft_pr", &["draft_commit"]),
                node("push", "push", &["commit"]),
                node("open_pr", "open_pr", &["push", "draft_pr"]),
            ],
        }
    }

    #[test]
    fn a_well_formed_flow_has_no_problems() {
        assert_eq!(validate(&commit_flow()), vec![]);
    }

    #[test]
    fn a_book_saved_before_declarations_existed_gets_them_back() {
        // The real flows.toml on disk predates `handles`, so both shipped
        // flows loaded declaring nothing and selection always fell back.
        let mut book = FlowBook::defaults();
        for flow in &mut book.flows {
            flow.handles.clear();
        }

        book.adopt_missing_declarations();
        assert_eq!(
            book.get("commit_and_pr").unwrap().handles,
            vec!["uncommitted".to_string(), "unpushed_branch".to_string()]
        );
        assert_eq!(
            book.get("review_and_merge").unwrap().handles,
            vec!["open_pull_request".to_string()]
        );
    }

    #[test]
    fn a_deliberate_choice_to_declare_nothing_survives() {
        // Only an *unmodified* shipped flow is backfilled; a flow of your own
        // that answers nothing keeps answering nothing.
        let mut book = FlowBook::defaults();
        book.flows.push(FlowDef {
            id: "deploy_vps".into(),
            label: "Deploy VPS".into(),
            handles: vec![],
            nodes: vec![],
        });
        book.adopt_missing_declarations();
        assert!(book.get("deploy_vps").unwrap().handles.is_empty());
    }

    #[test]
    fn backfilling_never_overwrites_what_is_already_declared() {
        let mut book = FlowBook::defaults();
        book.flows[0].handles = vec!["release".into()];
        book.adopt_missing_declarations();
        assert_eq!(book.get("commit_and_pr").unwrap().handles, vec!["release"]);
    }

    #[test]
    fn a_flow_answers_a_need_by_declaring_it_not_by_its_name() {
        use crate::services::probe::Need;
        let book = FlowBook::defaults();

        let commit = book.get("commit_and_pr").unwrap();
        assert!(commit.answers(Need::Uncommitted));
        assert!(commit.answers(Need::UnpushedBranch));
        assert!(!commit.answers(Need::OpenPullRequest));

        let review = book.get("review_and_merge").unwrap();
        assert!(review.answers(Need::OpenPullRequest));
        assert!(!review.answers(Need::Release));
    }

    #[test]
    fn a_flow_called_anything_can_answer_the_release_need() {
        use crate::services::probe::Need;
        // mayorana deploys to a VPS rather than tagging a release.
        let flow = FlowDef {
            id: "deploy_vps".into(),
            label: "Deploy VPS".into(),
            handles: vec!["release".into()],
            nodes: vec![],
        };
        assert!(flow.answers(Need::Release));
        assert!(!flow.answers(Need::Uncommitted));
    }

    #[test]
    fn a_flow_declaring_nothing_is_never_auto_selected() {
        use crate::services::probe::Need;
        let flow = FlowDef {
            id: "x".into(),
            label: "x".into(),
            handles: vec![],
            nodes: vec![],
        };
        assert!(Need::ALL.into_iter().all(|need| !flow.answers(need)));
    }

    #[test]
    fn both_shipped_flows_are_valid() {
        for flow in &FlowBook::defaults().flows {
            assert_eq!(validate(flow), vec![], "{} is broken", flow.id);
        }
    }

    #[test]
    fn the_shipped_flows_survive_a_round_trip_through_toml() {
        let book = FlowBook::defaults();
        let text = toml::to_string_pretty(&book).unwrap();
        let back: FlowBook = toml::from_str(&text).unwrap();
        assert_eq!(book, back);
    }

    #[test]
    fn a_corrupt_flow_file_falls_back_to_the_defaults() {
        let book: FlowBook = toml::from_str("this is not toml").unwrap_or_default();
        assert!(book.flows.is_empty());
        // load() turns that into the defaults rather than leaving the app empty.
        assert_eq!(FlowBook::defaults().flows.len(), 2);
    }

    #[test]
    fn the_shipped_flows_keep_the_diamond() {
        let book = FlowBook::defaults();
        let commit = book.get("commit_and_pr").unwrap();
        let draft_pr = commit.nodes.iter().find(|n| n.id == "draft_pr").unwrap();
        assert_eq!(draft_pr.deps, vec!["draft_commit".to_string()]);

        let review = book.get("review_and_merge").unwrap();
        let status = review.nodes.iter().find(|n| n.id == "pr_status").unwrap();
        assert_eq!(status.deps, vec!["find_pr".to_string()]);
    }

    #[test]
    fn an_invalid_flow_is_kept_for_repair_but_not_offered_to_run() {
        let mut book = FlowBook::defaults();
        book.flows[0].nodes[1].deps = vec!["ghost".into()];
        assert_eq!(book.flows.len(), 2);
        assert_eq!(book.runnable().len(), 1);
    }

    #[test]
    fn an_invalid_flow_is_still_listed_so_it_can_be_seen_and_marked() {
        // `runnable` dropping it is what the executor needs; the tab strip
        // needs the opposite, or a flow one edit from working looks deleted.
        let mut book = FlowBook::defaults();
        book.flows[0].nodes[1].deps = vec!["ghost".into()];

        let listed = book.listed();
        assert_eq!(listed.len(), 2, "every flow is listed, broken or not");

        let (broken, problems) = &listed[0];
        assert_eq!(broken.id, "commit_and_pr");
        assert!(!problems.is_empty(), "and carries what is wrong with it");
        assert!(problems[0].message().contains("ghost"));

        assert!(listed[1].1.is_empty(), "the healthy one has nothing to say");
    }

    #[test]
    fn a_commit_flow_saved_before_the_test_step_picks_it_up() {
        // Everyone with an existing flows.toml would otherwise never see the
        // step, because defaults only apply when the file is absent.
        let mut book = FlowBook::defaults();
        book.get_mut("commit_and_pr").unwrap().remove_node("test");
        assert!(!book
            .get("commit_and_pr")
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.id == "test"));

        book.adopt_missing_test_step();

        let commit_flow = book.get("commit_and_pr").unwrap();
        let test = commit_flow.nodes.iter().find(|n| n.id == "test").unwrap();
        assert_eq!(test.step, "run_tests");
        assert_eq!(test.deps, vec!["scan".to_string()]);
        let commit = commit_flow.nodes.iter().find(|n| n.id == "commit").unwrap();
        assert!(commit.deps.contains(&"test".to_string()));
    }

    #[test]
    fn a_commit_flow_you_have_rewired_is_left_alone() {
        let mut book = FlowBook::defaults();
        {
            let flow = book.get_mut("commit_and_pr").unwrap();
            flow.remove_node("test");
            flow.remove_node("draft_pr");
        }
        book.adopt_missing_test_step();
        assert!(!book
            .get("commit_and_pr")
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.id == "test"));
    }

    #[test]
    fn adopting_twice_changes_nothing() {
        let mut book = FlowBook::defaults();
        let before = book.clone();
        book.adopt_missing_test_step();
        assert_eq!(book, before);
    }

    #[test]
    fn a_new_flow_id_never_collides() {
        let book = FlowBook::defaults();
        assert_eq!(book.free_flow_id("commit_and_pr"), "commit_and_pr_2");
        assert_eq!(book.free_flow_id("release"), "release");
    }

    #[test]
    fn duplicating_a_flow_copies_its_nodes_under_a_free_id() {
        let mut book = FlowBook::defaults();
        let new_id = book.duplicate("commit_and_pr").unwrap();
        assert_eq!(new_id, "commit_and_pr_copy");
        let copy = book.get(&new_id).unwrap();
        assert_eq!(copy.nodes, book.get("commit_and_pr").unwrap().nodes);

        assert_eq!(
            book.duplicate("commit_and_pr").unwrap(),
            "commit_and_pr_copy_2"
        );
    }

    #[test]
    fn default_deps_attaches_to_the_selected_node_or_else_every_leaf() {
        let f = commit_flow();
        // open_pr is the only leaf.
        assert_eq!(default_deps(&f, ""), vec!["open_pr".to_string()]);
        assert_eq!(default_deps(&f, "scan"), vec!["scan".to_string()]);
    }

    #[test]
    fn a_cycle_is_reported_rather_than_hanging_the_executor() {
        let mut f = commit_flow();
        f.nodes[0].deps = vec!["open_pr".into()];
        let problems = validate(&f);
        assert!(problems.iter().any(|p| matches!(p, Problem::Cycle(_))));
        assert!(problems.iter().any(|p| matches!(p, Problem::NoRoot)));
    }

    #[test]
    fn a_dependency_on_a_step_that_is_not_there_is_caught() {
        let mut f = commit_flow();
        f.nodes[1].deps = vec!["ghost".into()];
        assert!(validate(&f)
            .iter()
            .any(|p| matches!(p, Problem::UnknownDep { dep, .. } if dep == "ghost")));
    }

    #[test]
    fn a_step_that_depends_on_itself_is_caught() {
        let mut f = commit_flow();
        f.nodes[1].deps = vec!["scan".into()];
        assert!(validate(&f)
            .iter()
            .any(|p| matches!(p, Problem::SelfDep(_))));
    }

    #[test]
    fn reordering_so_an_input_is_no_longer_upstream_is_caught() {
        // draft_commit reads `diff`, which only scan_changes writes.
        let mut f = commit_flow();
        f.nodes[2].deps = vec!["preflight".into()];
        assert!(validate(&f).iter().any(
            |p| matches!(p, Problem::MissingInput { node, key } if node == "draft_commit" && key == "diff")
        ));
    }

    #[test]
    fn a_sibling_writing_the_key_does_not_satisfy_it() {
        // Two roots: the second reads what the first writes, but does not
        // depend on it, so ordering is not guaranteed.
        let f = FlowDef {
            id: "x".into(),
            label: "x".into(),
            handles: vec![],
            nodes: vec![
                node("preflight", "preflight", &[]),
                node("scan", "scan_changes", &[]),
            ],
        };
        assert!(validate(&f).iter().any(
            |p| matches!(p, Problem::MissingInput { node, key } if node == "scan" && key == "base")
        ));
    }

    #[test]
    fn duplicate_ids_are_caught() {
        let mut f = commit_flow();
        f.nodes[1].id = "preflight".into();
        assert!(validate(&f)
            .iter()
            .any(|p| matches!(p, Problem::DuplicateId(id) if id == "preflight")));
    }

    #[test]
    fn an_unknown_step_key_is_reported_and_dropped_from_the_graph() {
        let mut f = commit_flow();
        f.nodes[3].step = "teleport".into();
        assert!(validate(&f)
            .iter()
            .any(|p| matches!(p, Problem::UnknownStep { key, .. } if key == "teleport")));
        assert_eq!(f.to_graph().nodes.len(), 6, "the rest of the flow survives");
    }

    #[test]
    fn deleting_a_step_takes_its_dependencies_with_it() {
        let mut f = commit_flow();
        f.remove_node("push");
        assert!(!f.nodes.iter().any(|n| n.id == "push"));
        assert!(!f.nodes.iter().any(|n| n.deps.contains(&"push".to_string())));
        assert!(!validate(&f)
            .iter()
            .any(|p| matches!(p, Problem::UnknownDep { .. })));
    }

    #[test]
    fn a_script_step_with_no_command_is_reported_rather_than_run() {
        let mut f = commit_flow();
        f.nodes.push(node("release", "run_script", &["open_pr"]));
        assert!(validate(&f)
            .iter()
            .any(|p| matches!(p, Problem::MissingSetting { node, .. } if node == "release")));

        f.nodes
            .last_mut()
            .unwrap()
            .config
            .insert("command".into(), "./scripts/release.sh --patch".into());
        assert_eq!(validate(&f), vec![]);
    }

    #[test]
    fn two_script_steps_in_one_flow_do_not_collide() {
        let mut f = commit_flow();
        for id in ["release", "smoke"] {
            let mut n = node(id, "run_script", &["open_pr"]);
            n.config.insert("command".into(), "true".into());
            f.nodes.push(n);
        }
        assert_eq!(validate(&f), vec![]);
        let g = f.to_graph();
        assert_eq!(
            g.get("release").unwrap().writes,
            vec!["release_output", "release_exit"]
        );
        assert_eq!(
            g.get("smoke").unwrap().writes,
            vec!["smoke_output", "smoke_exit"]
        );
    }

    #[test]
    fn a_new_id_never_collides() {
        let f = commit_flow();
        assert_eq!(f.free_id("commit"), "commit_2");
        assert_eq!(f.free_id("merge"), "merge");
    }

    #[test]
    fn the_editor_refuses_a_dependency_that_would_close_a_loop() {
        let f = commit_flow();
        assert!(
            !can_depend_on(&f, "preflight", "open_pr"),
            "would be a cycle"
        );
        assert!(!can_depend_on(&f, "scan", "scan"), "self");
        assert!(
            can_depend_on(&f, "open_pr", "scan"),
            "already upstream, harmless"
        );
        assert!(can_depend_on(&f, "draft_pr", "commit"));
    }

    #[test]
    fn the_graph_takes_titles_from_the_catalogue_unless_overridden() {
        let mut f = commit_flow();
        f.nodes[0].title = "Check everything".into();
        let g = f.to_graph();
        assert_eq!(g.get("preflight").unwrap().title, "Check everything");
        assert_eq!(g.get("scan").unwrap().title, "Scan changes");
    }

    #[test]
    fn gating_follows_the_catalogue_until_it_is_overridden() {
        let mut f = commit_flow();
        assert!(f.to_graph().get("push").unwrap().requires_approval);
        f.nodes[5].gated = Some(false);
        assert!(!f.to_graph().get("push").unwrap().requires_approval);
    }

    #[test]
    fn the_contract_always_comes_from_the_catalogue() {
        // A flow file cannot claim a step reads or writes something it does not.
        let f = commit_flow();
        let g = f.to_graph();
        assert_eq!(
            g.get("commit").unwrap().writes,
            vec!["work_branch".to_string(), "commit_sha".to_string()]
        );
    }
}
