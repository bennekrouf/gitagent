//! Parts of Setup: saving the draft, the list of flows, and the two questions asked before a
//! draft is thrown away.

use super::*;

/// Writes the draft: the flows to flows.toml and where each is shown. A flow made here is
/// shown only on `origin`, the repository Setup was opened from, unless the person asked
/// for it everywhere.
pub(super) fn save_draft(
    origin: Option<&str>,
    book: Signal<FlowBook>,
    scopes: Signal<store::RepoFlows>,
    on_disk: Signal<FlowBook>,
    everywhere: Signal<std::collections::BTreeSet<String>>,
    on_close: EventHandler<()>,
) {
    let mut book_now = book.read().clone();
    let mut scoped = scopes.read().clone();
    if let Some(repo) = origin {
        let shipped = FlowBook::defaults();
        for flow in &book_now.flows {
            let new = on_disk.read().get(&flow.id).is_none() && shipped.get(&flow.id).is_none();
            if new && !everywhere.read().contains(&flow.id) {
                scoped.only_on(&flow.id, repo);
            }
        }
    }
    // Scoped under the id it was made with, then
    // renamed with everything else that names it.
    for (old, new) in book_now.ids_from_labels() {
        scoped.rename_flow(&old, &new);
    }
    book_now.save();
    store::save_repo_flows(&scoped);
    on_close.call(());
}

/// The flows, each with a dot saying whether it has problems, and New flow.
pub(super) fn flow_list(
    snapshot: &FlowBook,
    current_id: &str,
    sidebar_w: Signal<f64>,
    mut book: Signal<FlowBook>,
    mut flow_id: Signal<String>,
    mut node_id: Signal<String>,
) -> Element {
    rsx! {
        div { class: "sidebar", style: "width: {sidebar_w}px;",
            div { class: "sidebar-head",
                div { class: "sidebar-title", "Flows" }
            }
            div { class: "sidebar-list",
                for flow in snapshot.flows.iter().cloned() {
                    {
                        let problems = validate(&flow).len();
                        let id = flow.id.clone();
                        rsx! {
                            div {
                                key: "{flow.id}",
                                class: if flow.id == current_id { "sidebar-row sidebar-row-on" } else { "sidebar-row" },
                                onclick: move |_| {
                                    flow_id.set(id.clone());
                                    node_id.set(String::new());
                                },
                                span { class: if problems == 0 { "dot dot-done" } else { "dot dot-failed" } }
                                span { class: "sidebar-label", "{flow.label}" }
                                if problems > 0 {
                                    span { class: "sidebar-note status-failed", "{problems}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "sidebar-foot",
                button {
                    class: "btn",
                    onclick: move |_| {
                        let mut w = book.write();
                        let id = w.free_flow_id("new_flow");
                        w.flows.push(crate::services::flowdef::FlowDef {
                            id: id.clone(),
                            label: "New flow".into(),
                            handles: vec![],
                            nodes: vec![],
                        });
                        drop(w);
                        flow_id.set(id);
                        node_id.set(String::new());
                    },
                    "New flow"
                }
            }
        }
    }
}

/// Asked before Cancel throws an edited draft away.
pub(super) fn discard_dialog(
    mut confirm_discard: Signal<bool>,
    on_close: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "modal-backdrop", onclick: move |_| confirm_discard.set(false),
            div {
                class: "modal",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                div { class: "modal-head",
                    span { "Discard these changes?" }
                    button {
                        class: "modal-close",
                        onclick: move |_| confirm_discard.set(false),
                        "×"
                    }
                }
                div { class: "modal-body",
                    div { class: "probe probe-bad",
                        "Your edits have not been written to flows.toml. Closing now "
                        "leaves the flows exactly as they were when you opened Setup."
                    }
                    div { class: "approval-actions",
                        button {
                            class: "btn btn-danger",
                            onclick: move |_| {
                                confirm_discard.set(false);
                                on_close.call(());
                            },
                            "Discard changes"
                        }
                        button {
                            class: "btn",
                            onclick: move |_| confirm_discard.set(false),
                            "Keep editing"
                        }
                    }
                }
            }
        }
    }
}

/// Asked before Restore defaults replaces the flows, naming the ones that would be lost.
pub(super) fn restore_dialog(
    snapshot: &FlowBook,
    mut confirm_restore: Signal<bool>,
    mut book: Signal<FlowBook>,
    mut flow_id: Signal<String>,
    mut node_id: Signal<String>,
) -> Element {
    let shipped = FlowBook::defaults();
    let losing: Vec<String> = snapshot
        .flows
        .iter()
        .filter(|f| {
            shipped
                .get(&f.id)
                .map(|original| original != *f)
                .unwrap_or(true)
        })
        .map(|f| f.label.clone())
        .collect();

    rsx! {
        div { class: "modal-backdrop", onclick: move |_| confirm_restore.set(false),
            div {
                class: "modal",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                div { class: "modal-head",
                    span { "Restore the shipped flows?" }
                    button {
                        class: "modal-close",
                        onclick: move |_| confirm_restore.set(false),
                        "×"
                    }
                }
                div { class: "modal-body",
                    if losing.is_empty() {
                        p { class: "field-note",
                            "Nothing has been changed — restoring will leave the flows \
                             exactly as they are."
                        }
                    } else {
                        div { class: "probe probe-bad",
                            "This replaces every flow with the two shipped ones. "
                            "{losing.len()} flow(s) would be lost. Nothing is written "
                            "until you press Save, so Cancel still puts them back."
                        }
                        div { class: "items",
                            for label in losing.iter() {
                                div { key: "{label}", class: "item",
                                    span { class: "item-note note-deleted", "lost" }
                                    span { class: "item-label", "{label}" }
                                }
                            }
                        }
                    }
                    div { class: "approval-actions",
                        button {
                            class: "btn btn-danger",
                            onclick: move |_| {
                                let restored = FlowBook::defaults();
                                flow_id.set(restored.flows[0].id.clone());
                                node_id.set(String::new());
                                book.set(restored);
                                confirm_restore.set(false);
                            },
                            "Restore defaults"
                        }
                        button {
                            class: "btn",
                            onclick: move |_| confirm_restore.set(false),
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}
