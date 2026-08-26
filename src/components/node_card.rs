//! One node in the flow, as a card in the left column.

use dioxus::prelude::*;

use crate::services::graph::{NodeKind, NodeRun, NodeSpec};

#[derive(Props, Clone, PartialEq)]
pub struct NodeCardProps {
    pub spec: NodeSpec,
    pub run: NodeRun,
    pub selected: bool,
    pub on_select: EventHandler<String>,
}

#[component]
pub fn NodeCard(props: NodeCardProps) -> Element {
    let spec = props.spec.clone();
    let status = props.run.status;
    let id = spec.id.clone();

    let awaiting = status == crate::services::graph::NodeStatus::AwaitingApproval;
    let class = match (props.selected, awaiting) {
        (true, true) => "node-card node-selected node-awaiting",
        (true, false) => "node-card node-selected",
        (false, true) => "node-card node-awaiting",
        (false, false) => "node-card",
    };

    rsx! {
        div {
            class,
            onclick: move |_| props.on_select.call(id.clone()),

            div { class: "node-head",
                span { class: "dot dot-{status.css()}" }
                span { class: "node-title", "{spec.title}" }
                span {
                    class: if spec.kind == NodeKind::Model { "tag tag-model" } else { "tag tag-det" },
                    if spec.kind == NodeKind::Model { "model" } else { "code" }
                }
                if awaiting {
                    span { class: "tag tag-gate tag-gate-live", "⏸ needs you" }
                } else if spec.requires_approval {
                    span { class: "tag tag-gate", "gated" }
                }
            }

            div { class: "node-sub", "{spec.subtitle}" }

            if !spec.deps.is_empty() {
                div { class: "node-deps", "after {spec.deps.join(\", \")}" }
            }

            div { class: "node-status status-{status.css()}", "{status.label()}" }

            if !props.run.summary.is_empty() {
                div { class: "node-summary", "{props.run.summary}" }
            }
        }
    }
}
