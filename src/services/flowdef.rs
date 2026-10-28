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
use super::probe::{FlowKind, Need};
mod book;
mod validate;
pub use validate::*;

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
    /// Which producer each input comes from: input key → `node_id.output_key`.
    ///
    /// Only for the inputs where it matters. Left out — which is every node in
    /// every flow that exists today — an input is satisfied by whichever
    /// ancestor declares it, exactly as before. That default is fine until two
    /// nodes write the same key: `pr_url` comes from both `open_pr` and
    /// `find_pr`, and with nothing to name which, the answer is whichever ran
    /// last. A binding is how you say which you meant, and it is what lets the
    /// same step appear twice in one flow without the second silently
    /// overwriting the first.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bind: BTreeMap<String, String>,
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
            bind: BTreeMap::new(),
        }
    }

    /// The node and output a given input is bound to, if it is bound at all.
    pub fn binding<'a>(&'a self, input: &'a str) -> Option<(&'a str, &'a str)> {
        let source = self.bind.get(input)?;
        // `draft_commit.commit_subject` names an output; a bare `draft_commit`
        // means "the one with this input's own name", which is what you want
        // nine times out of ten and is far less to type.
        Some(match source.split_once('.') {
            Some((node, key)) => (node, key),
            None => (source.as_str(), input),
        })
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
    ///
    /// In dependency order, not file order: a step added in Setup lands at the
    /// end of the file wherever it sits in the graph, and the run view lists
    /// the graph as it comes. Anything in a cycle keeps its file position at
    /// the end — such a flow does not validate, so it is only ever displayed.
    pub fn to_graph(&self) -> Graph {
        let mut order = topological_order(self);
        for node in &self.nodes {
            if !order.contains(&node.id) {
                order.push(node.id.clone());
            }
        }
        let rank = |id: &str| order.iter().position(|o| o == id).unwrap_or(usize::MAX);
        let mut sorted: Vec<&NodeDef> = self.nodes.iter().collect();
        sorted.sort_by_key(|n| rank(&n.id));

        Graph {
            nodes: sorted
                .into_iter()
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
                        // Stored resolved, so the executor never has to know
                        // the shorthand form.
                        bind: catalogue::expand(info.reads, &def.id)
                            .iter()
                            .filter_map(|input| {
                                let (node, key) = def.binding(input)?;
                                Some((input.clone(), crate::services::graph::qualified(node, key)))
                            })
                            .collect(),
                    })
                })
                .collect(),
        }
    }

    /// What this flow does, for the Start button and for what happens once a
    /// run of it ends. Read from what the flow declares and contains, never
    /// its id, so a copy made in Setup counts whatever it is called: a commit
    /// flow answers uncommitted work or commits; a review flow answers an
    /// open pull request, or reads or merges one. Commit is checked first — a
    /// flow that does both starts from a dirty tree.
    pub fn kind(&self) -> FlowKind {
        if self.answers(Need::Uncommitted) || self.nodes.iter().any(|n| n.step == "commit") {
            FlowKind::Commit
        } else if self.answers(Need::OpenPullRequest)
            || self
                .nodes
                .iter()
                .any(|n| n.step == "pr_diff" || n.step == "merge")
        {
            FlowKind::Review
        } else {
            FlowKind::Other
        }
    }

    /// Whether this flow says it answers `need`.
    pub fn answers(&self, need: Need) -> bool {
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

    /// Wires a test step between this flow's scan and its commit, if it has
    /// both and no test step already. Returns whether anything changed.
    ///
    /// It hangs off the scan rather than off the commit's own dependencies, so
    /// the suite runs while the message is still being drafted — the shape the
    /// shipped flow has, arrived at from whatever the flow already looked like.
    pub fn insert_test_step(&mut self) -> bool {
        if self.nodes.iter().any(|n| n.step == "run_tests") {
            return false;
        }
        let Some(scan) = self
            .nodes
            .iter()
            .position(|n| n.step.starts_with("scan_changes"))
        else {
            return false;
        };
        let Some(commit) = self.nodes.iter().position(|n| n.step == "commit") else {
            return false;
        };

        let id = self.free_id("test");
        let after = self.nodes[scan].id.clone();
        self.nodes[commit].deps.push(id.clone());
        self.nodes.insert(
            scan + 1,
            dep(NodeDef::from_catalogue(&id, "run_tests"), &[&after]),
        );
        true
    }

    /// Puts a second opinion beside this flow's analysis, if it has one and no
    /// second opinion already, and makes whatever waits on the analysis wait
    /// on both. Returns whether anything changed.
    ///
    /// Harmless to a flow whose owner never picks a second model: the step is
    /// marked skipped at once and nothing behind it is held up.
    pub fn insert_second_opinion(&mut self) -> bool {
        self.insert_beside_analysis("second_opinion")
    }

    /// The same for the focused reviews, and harmless in the same way to a
    /// flow whose owner never turns a lens on.
    pub fn insert_lenses(&mut self) -> bool {
        self.insert_beside_analysis("lenses")
    }

    /// Adds a `step` node that reads what the analysis reads and is waited on
    /// by whatever waits on the analysis. Not if the flow has no analysis, or
    /// has one of these already.
    fn insert_beside_analysis(&mut self, step: &str) -> bool {
        if self.nodes.iter().any(|n| n.step == step) {
            return false;
        }
        let Some(at) = self.nodes.iter().position(|n| n.step == "analyse") else {
            return false;
        };
        let analyse = self.nodes[at].id.clone();
        let id = self.free_id(step);
        for node in &mut self.nodes {
            if node.deps.contains(&analyse) {
                node.deps.push(id.clone());
            }
        }
        let deps: Vec<&str> = self.nodes[at].deps.iter().map(String::as_str).collect();
        let node = dep(NodeDef::from_catalogue(&id, step), &deps);
        // After any sibling already placed beside the analysis, so the file
        // keeps reading analysis, second opinion, focused reviews.
        let mut after = at;
        while after + 1 < self.nodes.len()
            && matches!(
                self.nodes[after + 1].step.as_str(),
                "second_opinion" | "lenses"
            )
        {
            after += 1;
        }
        self.nodes.insert(after + 1, node);
        true
    }

    /// Puts `draft_notes → write_notes` in front of a step that runs
    /// `release.sh`, so a release with no notes gets a draft to approve
    /// rather than a script refusing to run. Returns whether anything changed.
    pub fn insert_notes_steps(&mut self) -> bool {
        if self.nodes.iter().any(|n| n.step == "draft_notes") {
            return false;
        }
        let Some(release) = self.nodes.iter().position(|n| {
            n.step == "run_script"
                && n.config
                    .get("command")
                    .is_some_and(|c| c.contains("release.sh"))
        }) else {
            return false;
        };

        let draft = self.free_id("draft_notes");
        let write = self.free_id("write_notes");
        let mut draft_node = NodeDef::from_catalogue(&draft, "draft_notes");
        draft_node.deps = std::mem::replace(&mut self.nodes[release].deps, vec![write.clone()]);
        self.nodes.insert(release, draft_node);
        self.nodes.insert(
            release + 1,
            dep(NodeDef::from_catalogue(&write, "write_notes"), &[&draft]),
        );
        true
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

/// True when a dependency may be added without closing a loop.
pub fn can_depend_on(flow: &FlowDef, node: &str, candidate: &str) -> bool {
    node != candidate && !ancestors(flow, candidate).contains(node)
}

/// Every flow the app knows, and the file they live in.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct FlowBook {
    /// One-time adoptions already applied to this book.
    ///
    /// Without it, a step someone deliberately deleted would be helpfully put
    /// back on the next launch, and there would be no way to refuse it.
    /// Declared before `flows` because TOML puts plain values before the
    /// `[[flow]]` tables, not after them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopted: Vec<String>,
    #[serde(default, rename = "flow")]
    pub flows: Vec<FlowDef>,
}

/// Names the one-time adoption that adds the test step to an existing book.
pub const ADOPTED_TEST_STEP: &str = "run_tests";

/// Names the one-time adoption that gives every flow you made an id read
/// from its name.
pub const ADOPTED_IDS_FROM_LABELS: &str = "flow_ids_from_labels";

/// A flow id from its name: "Deploy api0 landing" → `deploy_api0_landing`.
///
/// What a flow is called is what anything reading its id should see. An id
/// left as `review_and_merge_copy_copy_copy` on a deploy flow told a safety
/// check reading a `run_flow` call that it was about to merge without review.
pub fn id_from_label(label: &str) -> String {
    let mut id = String::new();
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.ends_with('_') && !id.is_empty() {
            id.push('_');
        }
    }
    let id = id.trim_end_matches('_').to_string();
    if id.is_empty() {
        "flow".into()
    } else {
        id
    }
}

/// Names the one-time adoption that adds the release-notes steps.
pub const ADOPTED_NOTES_STEPS: &str = "release_notes";

/// Names the one-time adoption that adds the second-opinion step.
pub const ADOPTED_SECOND_OPINION: &str = "second_opinion";

/// Names the one-time adoption that adds the focused-reviews step.
pub const ADOPTED_LENSES: &str = "lenses";

const FLOWS_FILE: &str = "flows.toml";

fn dep(mut node: NodeDef, deps: &[&str]) -> NodeDef {
    node.deps = deps.iter().map(|d| d.to_string()).collect();
    node
}

#[cfg(test)]
mod tests;
