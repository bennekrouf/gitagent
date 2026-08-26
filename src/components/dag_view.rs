//! Draws a flow as the graph it is.
//!
//! The run view lists steps top to bottom, which quietly reads as a sequence.
//! It is not one: in the commit flow, `commit` and `draft_pr` hang off the same
//! parent and neither waits for the other. That is the single most important
//! fact about a flow and a list cannot show it, so Setup draws the real thing.
//!
//! Layout is computed here rather than in a JS library so it can be tested:
//! a node's layer is the longest path from a root, which is what puts every
//! step below everything it depends on.

use dioxus::prelude::*;
use std::collections::HashMap;

use crate::services::catalogue;
use crate::services::flowdef::FlowDef;
use crate::services::graph::NodeKind;

pub const NODE_W: f64 = 168.0;
pub const NODE_H: f64 = 52.0;
const GAP_X: f64 = 26.0;
const GAP_Y: f64 = 46.0;
const PAD: f64 = 20.0;

#[derive(Clone, PartialEq, Debug)]
pub struct Placed {
    pub id: String,
    pub layer: usize,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Layout {
    pub nodes: Vec<Placed>,
    /// `(from, to)` — from the dependency to the step that needs it.
    pub edges: Vec<(String, String)>,
    pub width: f64,
    pub height: f64,
}

impl Layout {
    pub fn find(&self, id: &str) -> Option<&Placed> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// Longest path from a root. Cycle-safe: the pass runs at most once per node,
/// so anything caught in a loop simply stops improving rather than hanging.
fn layers(flow: &FlowDef) -> HashMap<String, usize> {
    let mut layer: HashMap<String, usize> =
        flow.nodes.iter().map(|n| (n.id.clone(), 0usize)).collect();

    for _ in 0..flow.nodes.len() {
        let mut changed = false;
        for node in &flow.nodes {
            let want = node
                .deps
                .iter()
                .filter_map(|d| layer.get(d))
                .map(|l| l + 1)
                .max()
                .unwrap_or(0);
            if let Some(current) = layer.get_mut(&node.id) {
                if want > *current {
                    *current = want;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    layer
}

pub fn layout(flow: &FlowDef) -> Layout {
    if flow.nodes.is_empty() {
        return Layout::default();
    }
    let layer = layers(flow);

    // Declaration order within a layer, so a flow's file order still reads
    // left to right.
    let mut rows: HashMap<usize, Vec<String>> = HashMap::new();
    for node in &flow.nodes {
        rows.entry(*layer.get(&node.id).unwrap_or(&0))
            .or_default()
            .push(node.id.clone());
    }

    let widest = rows.values().map(|r| r.len()).max().unwrap_or(1) as f64;
    let span = widest * NODE_W + (widest - 1.0).max(0.0) * GAP_X;

    let mut placed = vec![];
    for (index, ids) in &rows {
        let count = ids.len() as f64;
        let row_span = count * NODE_W + (count - 1.0).max(0.0) * GAP_X;
        // Centre each row against the widest one, so the shape reads as a graph
        // rather than a ragged left-aligned column.
        let start = PAD + (span - row_span) / 2.0;
        for (i, id) in ids.iter().enumerate() {
            placed.push(Placed {
                id: id.clone(),
                layer: *index,
                x: start + i as f64 * (NODE_W + GAP_X),
                y: PAD + *index as f64 * (NODE_H + GAP_Y),
            });
        }
    }
    placed.sort_by_key(|p| (p.layer, p.x as i64));

    let depth = rows.len() as f64;
    let edges = flow
        .nodes
        .iter()
        .flat_map(|n| n.deps.iter().map(|d| (d.clone(), n.id.clone())))
        .filter(|(from, _)| flow.nodes.iter().any(|n| &n.id == from))
        .collect();

    Layout {
        nodes: placed,
        edges,
        width: span + PAD * 2.0,
        height: depth * NODE_H + (depth - 1.0).max(0.0) * GAP_Y + PAD * 2.0,
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct DagViewProps {
    pub flow: FlowDef,
    pub selected: String,
    pub on_select: EventHandler<String>,
}

#[component]
pub fn DagView(props: DagViewProps) -> Element {
    let flow = props.flow.clone();
    let plan = layout(&flow);

    if plan.nodes.is_empty() {
        return rsx! {
            div { class: "dag-empty", "This flow has no steps yet. Add one from the right." }
        };
    }

    rsx! {
        div { class: "dag-scroll",
            svg {
                class: "dag",
                width: "{plan.width}",
                height: "{plan.height}",
                view_box: "0 0 {plan.width} {plan.height}",

                for (from, to) in plan.edges.iter() {
                    if let (Some(a), Some(b)) = (plan.find(from), plan.find(to)) {
                        {
                            let x1 = a.x + NODE_W / 2.0;
                            let y1 = a.y + NODE_H;
                            let x2 = b.x + NODE_W / 2.0;
                            let y2 = b.y;
                            let mid = (y1 + y2) / 2.0;
                            rsx! {
                                path {
                                    key: "{from}->{to}",
                                    class: "dag-edge",
                                    d: "M {x1} {y1} C {x1} {mid}, {x2} {mid}, {x2} {y2}",
                                    fill: "none",
                                }
                            }
                        }
                    }
                }

                for placed in plan.nodes.iter() {
                    if let Some(def) = flow.nodes.iter().find(|n| n.id == placed.id) {
                        {
                            let info = catalogue::by_key(&def.step);
                            let title = if def.title.is_empty() {
                                info.map(|i| i.title.to_string()).unwrap_or_else(|| def.step.clone())
                            } else {
                                def.title.clone()
                            };
                            let is_model = info.map(|i| i.kind == NodeKind::Model).unwrap_or(false);
                            let unknown = info.is_none();
                            let selected = placed.id == props.selected;
                            let id = placed.id.clone();
                            let class = if unknown {
                                "dag-node dag-node-broken"
                            } else if selected {
                                "dag-node dag-node-on"
                            } else {
                                "dag-node"
                            };
                            rsx! {
                                g {
                                    key: "{placed.id}",
                                    class: "{class}",
                                    onclick: move |_| props.on_select.call(id.clone()),
                                    rect {
                                        x: "{placed.x}", y: "{placed.y}",
                                        width: "{NODE_W}", height: "{NODE_H}",
                                        rx: "8",
                                    }
                                    text {
                                        x: "{placed.x + 12.0}",
                                        y: "{placed.y + 21.0}",
                                        class: "dag-title",
                                        "{title}"
                                    }
                                    text {
                                        x: "{placed.x + 12.0}",
                                        y: "{placed.y + 38.0}",
                                        class: "dag-meta",
                                        if unknown { "unknown step" }
                                        else if is_model { "model" }
                                        else { "code" }
                                    }
                                    if def.is_gated() {
                                        circle {
                                            cx: "{placed.x + NODE_W - 14.0}",
                                            cy: "{placed.y + 14.0}",
                                            r: "4.5",
                                            class: "dag-gate",
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::flowdef::FlowBook;

    fn commit() -> FlowDef {
        FlowBook::defaults().get("commit_and_pr").unwrap().clone()
    }

    #[test]
    fn a_root_sits_on_the_top_layer() {
        let l = layers(&commit());
        assert_eq!(l["preflight"], 0);
    }

    #[test]
    fn a_step_is_always_below_everything_it_depends_on() {
        let flow = commit();
        let l = layers(&flow);
        for node in &flow.nodes {
            for dep in &node.deps {
                assert!(l[&node.id] > l[dep], "{} must sit below {dep}", node.id);
            }
        }
    }

    #[test]
    fn siblings_share_a_layer_and_are_drawn_apart() {
        // commit and draft_pr both hang off draft_commit — the diamond a list
        // view cannot show.
        let flow = commit();
        let plan = layout(&flow);
        let commit_node = plan.find("commit").unwrap();
        let draft_pr = plan.find("draft_pr").unwrap();
        assert_eq!(commit_node.layer, draft_pr.layer);
        assert_ne!(commit_node.x, draft_pr.x);
        assert_eq!(commit_node.y, draft_pr.y);
    }

    #[test]
    fn a_join_lands_below_both_of_its_parents() {
        let plan = layout(&commit());
        let open_pr = plan.find("open_pr").unwrap();
        assert!(open_pr.layer > plan.find("push").unwrap().layer);
        assert!(open_pr.layer > plan.find("draft_pr").unwrap().layer);
    }

    #[test]
    fn every_dependency_becomes_one_edge() {
        let flow = commit();
        let expected: usize = flow.nodes.iter().map(|n| n.deps.len()).sum();
        assert_eq!(layout(&flow).edges.len(), expected);
    }

    #[test]
    fn an_edge_to_a_deleted_step_is_not_drawn() {
        let mut flow = commit();
        flow.nodes.retain(|n| n.id != "push");
        // open_pr still names push; the layout must not try to draw to nothing.
        assert!(!layout(&flow).edges.iter().any(|(from, _)| from == "push"));
    }

    #[test]
    fn a_cycle_lays_out_instead_of_hanging() {
        let mut flow = commit();
        flow.nodes[0].deps = vec!["open_pr".into()];
        let plan = layout(&flow);
        assert_eq!(plan.nodes.len(), flow.nodes.len());
    }

    #[test]
    fn an_empty_flow_has_no_canvas() {
        let flow = FlowDef {
            id: "x".into(),
            label: "x".into(),
            nodes: vec![],
        };
        assert_eq!(layout(&flow), Layout::default());
    }

    #[test]
    fn the_canvas_is_large_enough_to_hold_every_node() {
        let plan = layout(&commit());
        for node in &plan.nodes {
            assert!(node.x + NODE_W <= plan.width);
            assert!(node.y + NODE_H <= plan.height);
        }
    }
}
