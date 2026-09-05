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
    /// Whether this node's run has actually begun. A node in a flow nobody has
    /// started is `Pending` too, and offering to skip it there would write to
    /// state that `Start` throws away.
    pub run_started: bool,
    pub on_approve: EventHandler<String>,
    pub on_reject: EventHandler<String>,
    /// `(node id, item key)` — flips one item in and out of the approval.
    pub on_toggle: EventHandler<(String, String)>,
    /// `(node id, remedy index)` — runs one offered fix.
    pub on_remedy: EventHandler<(String, usize)>,
    /// Re-queues a settled node and everything blocked behind it.
    pub on_retry: EventHandler<String>,
    /// Marks a node bypassed and lets the run carry on without it. Offered at
    /// an approval and after a failure, because both are places a step can
    /// stand between you and work that does not actually depend on it.
    pub on_skip: EventHandler<String>,
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
    let skip_id = spec.id.clone();
    let skip_failed_id = spec.id.clone();
    let early_skip_id = spec.id.clone();
    let diff_id = spec.id.clone();
    // What this node would have written, named at the button, because those
    // are exactly the artifacts every later step will read as empty.
    let skip_cost = if spec.writes.is_empty() {
        "Nothing downstream reads anything from this step.".to_string()
    } else {
        format!("Later steps will read {} as empty.", spec.writes.join(", "))
    };
    let failed = matches!(run.status, NodeStatus::Failed | NodeStatus::Rejected);
    // Skip is not only for a step that has stopped to ask or has fallen over.
    // An ungated step that simply takes too long — a model call against a diff
    // it cannot chew through — never reaches either of those states, so
    // without this the one step most worth skipping is the one you cannot.
    let skippable_early =
        props.run_started && matches!(run.status, NodeStatus::Pending | NodeStatus::Running);
    let running_now = run.status == NodeStatus::Running;
    let nothing_selected = run.has_nothing_selected();
    let chosen = run.items.iter().filter(|i| i.included).count();
    // Collapsed by default — the count already says "all N selected" at a
    // glance, and most approvals want everything committed. The list is one
    // click away for the times a subset needs picking.
    // Open. The file list only exists on a node that has stopped to ask
    // whether to commit these files, so folding away the one thing the
    // question is about — behind an unlabelled "…" — meant the answer to "how
    // do I choose the files" was a control nobody could find.
    let mut show_files = use_signal(|| true);

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

            if skippable_early {
                div { class: "step-actions",
                    button {
                        class: "btn",
                        title: if running_now {
                            "Stop waiting for this step and carry on without it. Whatever it \
                             has already started is not undone. {skip_cost}"
                        } else {
                            "Don't run this step when the run reaches it. {skip_cost}"
                        },
                        onclick: move |_| props.on_skip.call(early_skip_id.clone()),
                        if running_now { "Skip this step" } else { "Skip when reached" }
                    }
                }
            }

            if awaiting {
                div { class: "approval",
                    // A trusted run that stopped here says so at the node it
                    // stopped on, rather than leaving an approval that looks
                    // no different from any other while the run sits still.
                    if !run.held.is_empty() {
                        div { class: "approval-held", "{run.held}" }
                    }
                    if run.preview_diff.is_empty() {
                        div { class: "approval-head", "This will run:" }
                        pre { class: "approval-body", "{run.proposal}" }
                    } else {
                        div { class: "approval-head", "Diff to review:" }
                        DiffView { diff: run.preview_diff.clone(), is_light: props.is_light }
                    }

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
                                    if *show_files.read() { "Hide" } else { "Choose files" }
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
                        button {
                            class: "btn btn-ghost",
                            title: "Don't run this step, and carry on anyway. Unlike Reject, \
                                    which stops everything behind it. {skip_cost}",
                            onclick: move |_| props.on_skip.call(skip_id.clone()),
                            "Skip"
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
                                {
                                    let btn_class = if remedy.done {
                                        "btn"
                                    } else if remedy.retry_after {
                                        "btn btn-primary"
                                    } else {
                                        "btn btn-danger"
                                    };
                                    rsx! {
                                button {
                                    class: btn_class,
                                    disabled: remedy.running || remedy.done,
                                    onclick: {
                                        let node = remedy_id.clone();
                                        move |_| props.on_remedy.call((node.clone(), index))
                                    },
                                    if remedy.running { "Running…" }
                                    else if remedy.done { "Done" }
                                    else if remedy.retry_after { "Run" }
                                    else { "Abandon" }
                                }
                                    }
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
                            class: "btn",
                            title: "Carry on without this step — for one that is failing on \
                                    something the rest of the run does not need. {skip_cost}",
                            onclick: move |_| props.on_skip.call(skip_failed_id.clone()),
                            "Skip this step"
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
                {
                    // A step that offered files (scan does) lets them be
                    // dropped here, against the diff — which is the only place
                    // the contents are visible to judge by.
                    let selectable = !run.items.is_empty();
                    let excluded: Vec<String> = run
                        .items
                        .iter()
                        .filter(|i| !i.included)
                        .map(|i| i.key.clone())
                        .collect();
                    let node = diff_id.clone();
                    rsx! {
                        div { class: "log-head",
                            span { "Diff" }
                            if selectable {
                                span { class: "items-count",
                                    "{chosen} of {run.items.len()} files selected"
                                }
                            }
                        }
                        DiffView {
                            diff,
                            is_light: props.is_light,
                            excluded,
                            on_toggle: selectable.then(|| {
                                let on_toggle = props.on_toggle;
                                EventHandler::new(move |path: String| {
                                    on_toggle.call((node.clone(), path))
                                })
                            }),
                        }
                    }
                }
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
