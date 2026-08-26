//! The right-hand pane: one node's contract, its output, and — when the node
//! is gated — exactly what it will do and the two buttons that decide.

use dioxus::prelude::*;

use crate::components::diff_view::DiffView;
use crate::services::graph::{NodeRun, NodeSpec, NodeStatus};

#[derive(Props, Clone, PartialEq)]
pub struct DetailPaneProps {
    pub spec: Option<NodeSpec>,
    pub run: NodeRun,
    /// The node's own diff artifact (`diff` or `pr_diff`), when it wrote
    /// one — shown as a highlighted diff instead of the plain output log.
    #[props(default)]
    pub diff: Option<String>,
    pub is_light: bool,
    pub on_approve: EventHandler<String>,
    pub on_reject: EventHandler<String>,
    /// `(node id, item key)` — flips one item in and out of the approval.
    pub on_toggle: EventHandler<(String, String)>,
    /// `(node id, remedy index)` — runs one offered fix.
    pub on_remedy: EventHandler<(String, usize)>,
    /// Re-queues a settled node and everything blocked behind it.
    pub on_retry: EventHandler<String>,
    /// Abandons the whole run for this repo+flow — for a failure retrying
    /// can never fix (the branch this was about already merged, the PR
    /// already exists under a different run). Puts the flow back to never
    /// having started, the same state a fresh repository would show.
    pub on_cancel: EventHandler<()>,
}

#[component]
pub fn DetailPane(props: DetailPaneProps) -> Element {
    let Some(spec) = props.spec.clone() else {
        return rsx! {
            div { class: "detail detail-empty", "Select a node to see its contract and output." }
        };
    };

    let run = props.run.clone();
    let awaiting = run.status == NodeStatus::AwaitingApproval;
    let approve_id = spec.id.clone();
    let reject_id = spec.id.clone();
    let toggle_id = spec.id.clone();
    let remedy_id = spec.id.clone();
    let retry_id = spec.id.clone();
    let failed = matches!(run.status, NodeStatus::Failed | NodeStatus::Rejected);
    let nothing_selected = run.has_nothing_selected();
    let chosen = run.items.iter().filter(|i| i.included).count();
    // Collapsed by default — the count already says "all N selected" at a
    // glance, and most approvals want everything committed. The list is one
    // click away for the times a subset needs picking.
    let mut show_files = use_signal(|| false);

    rsx! {
        div { class: "detail",
            div { class: "detail-head",
                span { class: "dot dot-{run.status.css()}" }
                span { class: "detail-title", "{spec.title}" }
                span { class: "detail-status status-{run.status.css()}", "{run.status.label()}" }
            }

            div { class: "contract",
                div { class: "contract-col",
                    div { class: "contract-label", "reads" }
                    if spec.reads.is_empty() {
                        div { class: "contract-none", "—" }
                    } else {
                        for key in spec.reads.iter() {
                            div { key: "{key}", class: "chip", "{key}" }
                        }
                    }
                }
                div { class: "contract-col",
                    div { class: "contract-label", "writes" }
                    if spec.writes.is_empty() {
                        div { class: "contract-none", "—" }
                    } else {
                        for key in spec.writes.iter() {
                            div { key: "{key}", class: "chip chip-out", "{key}" }
                        }
                    }
                }
            }

            if awaiting {
                div { class: "approval",
                    div { class: "approval-head", "This will run:" }
                    pre { class: "approval-body", "{run.proposal}" }

                    if !run.items.is_empty() {
                        div { class: "items-head",
                            span { class: "items-head-label", "Files" }
                            div { class: "items-head-right",
                                span { class: "items-count", "{chosen} of {run.items.len()} selected" }
                                button {
                                    class: "items-toggle",
                                    title: if *show_files.read() { "Hide the file list" } else { "Choose which files to include" },
                                    onclick: {
                                        let now = *show_files.read();
                                        move |_| show_files.set(!now)
                                    },
                                    "…"
                                }
                            }
                        }
                        if *show_files.read() {
                            div { class: "items",
                                for item in run.items.iter().cloned() {
                                    label {
                                        key: "{item.key}",
                                        class: if item.included { "item" } else { "item item-off" },
                                        input {
                                            r#type: "checkbox",
                                            checked: item.included,
                                            onchange: {
                                                let node = toggle_id.clone();
                                                let key = item.key.clone();
                                                move |_| props.on_toggle.call((node.clone(), key.clone()))
                                            },
                                        }
                                        span { class: "item-note note-{item.note}", "{item.note}" }
                                        span { class: "item-label", "{item.label}" }
                                    }
                                }
                            }
                        }
                    }

                    div { class: "approval-actions",
                        button {
                            class: "btn btn-primary",
                            disabled: nothing_selected,
                            onclick: move |_| props.on_approve.call(approve_id.clone()),
                            if nothing_selected { "Nothing selected" } else { "Approve" }
                        }
                        button {
                            class: "btn btn-danger",
                            onclick: move |_| props.on_reject.call(reject_id.clone()),
                            "Reject"
                        }
                    }
                }
            }

            if failed {
                div { class: "remedies",
                    if run.remedies.is_empty() {
                        div { class: "remedies-none",
                            "No automatic fix for this. Resolve it, then retry."
                        }
                    } else {
                        div { class: "remedies-head", "Fix it from here" }
                        for (index, remedy) in run.remedies.iter().enumerate() {
                            div { key: "{remedy.display}", class: "remedy",
                                div { class: "remedy-main",
                                    div { class: "remedy-label", "{remedy.label}" }
                                    code { class: "remedy-cmd", "{remedy.display}" }
                                }
                                button {
                                    class: if remedy.done { "btn" } else { "btn btn-primary" },
                                    disabled: remedy.running || remedy.done,
                                    onclick: {
                                        let node = remedy_id.clone();
                                        move |_| props.on_remedy.call((node.clone(), index))
                                    },
                                    if remedy.running { "Running…" }
                                    else if remedy.done { "Done" }
                                    else { "Run" }
                                }
                            }
                            if !remedy.output.is_empty() {
                                pre { class: "remedy-out", "{remedy.output}" }
                            }
                        }
                    }
                    div { class: "remedy-actions",
                        button {
                            class: "btn",
                            onclick: move |_| props.on_retry.call(retry_id.clone()),
                            "Retry this step"
                        }
                        button {
                            class: "btn btn-ghost",
                            title: "Give up on this run — for a failure retrying can't fix \
                                     (already merged, already open elsewhere). Puts this repo's \
                                     flow back to not-started.",
                            onclick: move |_| props.on_cancel.call(()),
                            "Cancel run"
                        }
                    }
                }
            }

            if let Some(diff) = props.diff.clone().filter(|d| !d.trim().is_empty()) {
                div { class: "log-head", "Diff" }
                DiffView { diff, is_light: props.is_light }
            } else if !run.log.is_empty() {
                div { class: "log-head", if run.status == NodeStatus::Failed { "Error" } else { "Output" } }
                pre {
                    class: if run.status == NodeStatus::Failed { "log log-error" } else { "log" },
                    "{run.log}"
                }
            }
        }
    }
}
