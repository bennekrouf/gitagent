//! Parts of the detail pane: the step's contract, the approval it waits on, and the fixes
//! offered for a failure.

use super::*;
use crate::services::graph::NodeRun;

/// What the step reads and what it writes.
pub(super) fn contract(spec: &NodeSpec) -> Element {
    rsx! {
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
    }
}

/// What a gated step will do, and the files it offers, each to include or leave out.
pub(super) fn approval(
    run: &NodeRun,
    toggle_id: String,
    chosen: usize,
    read_only: bool,
    is_light: bool,
    on_toggle: EventHandler<(String, String)>,
    mut show_files: Signal<bool>,
) -> Element {
    rsx! {
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
                DiffView { diff: run.preview_diff.clone(), is_light }
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
                                    disabled: read_only,
                                    onchange: {
                                        let node = toggle_id.clone();
                                        let key = item.key.clone();
                                        move |_| on_toggle.call((node.clone(), key.clone()))
                                    },
                                }
                                span { class: "item-note note-{item.note}", "{item.note}" }
                                span { class: "item-label", "{item.label}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The fixes on offer for a failed step, each with its command, button and output.
pub(super) fn remedies(
    run: &NodeRun,
    remedy_id: String,
    on_remedy: EventHandler<(String, usize)>,
    unresolved: Signal<Vec<String>>,
    mut rechecks: Signal<u32>,
) -> Element {
    rsx! {
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
                            let held = remedy.once_resolved && !unresolved.read().is_empty();
                            let btn_class = if remedy.done {
                                "btn"
                            } else if remedy.abandons {
                                "btn btn-danger"
                            } else {
                                "btn btn-primary"
                            };
                            rsx! {
                        button {
                            class: btn_class,
                            disabled: remedy.running || remedy.done || held,
                            title: if held { "Resolve every file in Resolve conflicts above first" } else { "" },
                            onclick: {
                                let node = remedy_id.clone();
                                move |_| on_remedy.call((node.clone(), index))
                            },
                            if remedy.running { "Running…" }
                            else if remedy.done { "Done" }
                            else if remedy.failed { "Run again" }
                            else if remedy.abandons { "Abandon" }
                            else { "Run" }
                        }
                            }
                        }
                    }
                    if remedy.once_resolved && !remedy.done && !unresolved.read().is_empty() {
                        {
                            let left = unresolved.read().clone();
                            rsx! {
                                div { class: "remedy-waiting",
                                    "Git is still waiting on {left.join(\", \")}. Resolve with AI or \
                                     pick a side for each conflict in Resolve conflicts above, and \
                                     save. If you already \
                                     fixed it in your editor, pick it in that list and click Mark \
                                     resolved. This unlocks once nothing is left. "
                                    button {
                                        class: "clash-btn",
                                        onclick: move |_| *rechecks.write() += 1,
                                        "Check again"
                                    }
                                }
                            }
                        }
                    }
                    if !remedy.output.is_empty() {
                        if remedy.failed {
                            div { class: "remedy-stopped", "That stopped before finishing:" }
                        }
                        if remedy.failed && remedy.output.contains("unmerged files") {
                            div { class: "remedy-waiting",
                                "Git refused because a file still has conflicts in it. Resolve it \
                                 in Resolve conflicts above, then run this again."
                            }
                        }
                        pre { class: if remedy.failed { "remedy-out remedy-out-failed" } else { "remedy-out" }, "{remedy.output}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
