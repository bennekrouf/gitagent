//! Setup — the meta view: the flows themselves, as editable data.
//!
//! Edits are a draft. Nothing reaches `flows.toml` until Save, and Cancel
//! throws the draft away — so a flow can be taken apart, looked at, and put
//! back without the half-rewired state ever being what the run view loads.
//! A flow with problems is still kept and shown rather than rejected, and
//! still saveable: you should be able to leave one half-rewired, save, and
//! come back to it. It is simply not offered to run until the problems are
//! gone.

use dioxus::prelude::*;

use crate::components::dag_view::DagView;
use crate::components::settings_panel::SettingsPanel;
use crate::services::catalogue::{self, CATALOGUE};
use crate::services::flowdef::{can_depend_on, default_deps, validate, FlowBook, NodeDef};
use crate::services::graph::{NodeKind, Step};
use crate::services::llm::LlmConfig;
use crate::services::probe::Need;
use crate::services::remote;
use crate::services::store::{self, Layout};
mod editor;

#[derive(Props, Clone, PartialEq)]
pub struct SetupProps {
    /// The repository Setup was opened from. A flow created here is shown on
    /// it alone until another repository adds it from its flow list.
    #[props(default)]
    pub repo: Option<String>,
    /// The folder the window Setup was opened from has open, so Settings
    /// reached from here writes that folder's model and not the default.
    #[props(default)]
    pub folder: Option<String>,
    /// Read to tell whether a step just added has anything to run with, and
    /// handed to Settings when you go and set that up from here.
    pub llm_config: Signal<LlmConfig>,
    pub on_close: EventHandler<()>,
}

#[component]
pub fn Setup(props: SetupProps) -> Element {
    let mut book = use_signal(FlowBook::load);
    // What is on disk, as of when Setup opened. Kept so Cancel knows whether
    // there is anything to throw away, and so Save can tell an untouched
    // visit from an edited one.
    let on_disk = use_signal(FlowBook::load);
    // Which repositories each flow is shown on: part of the same draft, so
    // Cancel throws a change here away with everything else.
    let scopes = use_signal(store::load_repo_flows);
    let scopes_on_disk = use_signal(store::load_repo_flows);
    // New flows the person asked to show everywhere rather than only on the
    // repository Setup was opened from.
    let everywhere = use_signal(std::collections::BTreeSet::<String>::new);
    let first = book
        .read()
        .flows
        .first()
        .map(|f| f.id.clone())
        .unwrap_or_default();
    let mut flow_id = use_signal(|| first);
    let mut node_id = use_signal(String::new);
    let adding = use_signal(|| false);
    let mut confirm_restore = use_signal(|| false);
    let mut confirm_discard = use_signal(|| false);
    let testing = use_signal(|| false);
    let mut settings_open = use_signal(|| false);
    let test_result = use_signal(|| Option::<Result<String, String>>::None);
    // Private keys found in ~/.ssh, offered as one-click picks for the
    // "Identity file" field on a run_remote step. Read once: a key added
    // mid-session shows up next time Setup opens, which is a fine trade for
    // not re-scanning the filesystem on every render.
    let identities = use_signal(remote::discover_identities);

    // Same dividers as the workspace, sharing the same saved widths.
    let saved = use_signal(store::load_layout);
    let mut sidebar_w = use_signal(|| saved.read().sidebar);
    let mut editor_w = use_signal(|| saved.read().middle);
    let mut dragging = use_signal(|| 0u8);
    let mut drag_from = use_signal(|| (0.0f64, 0.0f64));

    let snapshot = book.read().clone();
    // Whether the draft has moved away from what is on disk. Drives the Save
    // button's state and whether Cancel has to ask before throwing anything
    // away.
    let dirty = snapshot != *on_disk.read() || *scopes.read() != *scopes_on_disk.read();
    let shipped = FlowBook::defaults();
    let is_new = |id: &str| on_disk.read().get(id).is_none() && shipped.get(id).is_none();
    let origin = props.repo.clone();
    let origin_label = origin
        .as_deref()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .unwrap_or_default();
    let current_id = flow_id.read().clone();
    let current = snapshot.get(&current_id).cloned();
    let selected_node = node_id.read().clone();

    // Every mutation goes through here. It changes the draft only — Save is
    // the one place in this screen that writes to disk.
    let edit_flow = move |mutate: &dyn Fn(&mut crate::services::flowdef::FlowDef)| {
        let id = flow_id.read().clone();
        let mut w = book.write();
        if let Some(flow) = w.get_mut(&id) {
            mutate(flow);
        }
    };

    rsx! {
        div { class: "screen",
            div { class: "topbar",
                span { class: "topbar-brand", "Setup" }
                div { class: "topbar-title",
                    span { class: "topbar-path", "Flows are shared by every repository and every window" }
                }
                div { class: "topbar-right",
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| confirm_restore.set(true),
                        "Restore defaults"
                    }
                    button {
                        class: "btn",
                        onclick: move |_| {
                            // Nothing has been written, so an untouched visit
                            // just closes. Only a real draft is worth a prompt.
                            if dirty {
                                confirm_discard.set(true);
                            } else {
                                props.on_close.call(());
                            }
                        },
                        "Cancel"
                    }
                    button {
                        class: "btn btn-primary",
                        disabled: !dirty,
                        title: if dirty {
                            "Write these flows to flows.toml"
                        } else {
                            "No changes to save"
                        },
                        onclick: {
                            let origin = props.repo.clone();
                            move |_| {
                                let mut book_now = book.read().clone();
                                let mut scoped = scopes.read().clone();
                                if let Some(repo) = origin.as_deref() {
                                    let shipped = FlowBook::defaults();
                                    for flow in &book_now.flows {
                                        let new = on_disk.read().get(&flow.id).is_none()
                                            && shipped.get(&flow.id).is_none();
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
                                props.on_close.call(());
                            }
                        },
                        "Save"
                    }
                }
            }

            div {
                class: if *dragging.read() > 0 { "body body-dragging" } else { "body" },
                onmousemove: move |e| {
                    let which = *dragging.read();
                    if which == 0 {
                        return;
                    }
                    let (start_x, start_w) = *drag_from.read();
                    let delta = e.client_coordinates().x - start_x;
                    if which == 1 {
                        sidebar_w.set(Layout::clamp_sidebar(start_w + delta));
                    } else {
                        // The editor is on the right, so dragging right shrinks it.
                        editor_w.set(Layout::clamp_middle(start_w - delta));
                    }
                },
                onmouseup: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *editor_w.read(),
                            ..store::load_layout()
                        });
                    }
                },
                onmouseleave: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *editor_w.read(),
                            ..store::load_layout()
                        });
                    }
                },

                // ── Flows ────────────────────────────────────────────────
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

                div {
                    class: "divider",
                    onmousedown: move |e| {
                        drag_from.set((e.client_coordinates().x, *sidebar_w.read()));
                        dragging.set(1);
                    },
                }

                {editor::flow_editor(book, scopes, everywhere, flow_id, node_id, adding, testing, settings_open, test_result, identities, saved, editor_w, dragging, drag_from, shipped.clone(), is_new, origin.clone(), origin_label.clone(), current.clone(), selected_node.clone(), edit_flow, props.clone())}
            }
        }

        if *settings_open.read() {
            SettingsPanel {
                llm_config: props.llm_config,
                folder: props.folder.clone(),
                on_close: move |_| settings_open.set(false),
            }
        }

        if *confirm_discard.read() {
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
                                    props.on_close.call(());
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

        if *confirm_restore.read() {
            {
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
        }
    }
}

/// Why a step will be skipped on every run because Settings gives it nothing
/// to run with, and the button that goes and fixes that. `None` for a step
/// that needs nothing from Settings, or already has it.
///
/// Mirrors the checks the run makes before such a step, so the notice here
/// and the skip there cannot disagree.
fn unset_in_settings(step: Step, cfg: &LlmConfig) -> Option<(&'static str, &'static str)> {
    match step {
        Step::SecondOpinion | Step::Lenses if !cfg.uses_model() => Some((
            "No model is set up in Settings, so this step will be skipped on every \
             run. Choose a model first.",
            "Choose a model",
        )),
        Step::SecondOpinion if !cfg.second_config().uses_model() => Some((
            "No second reviewer is chosen in Settings, so this step will be skipped \
             on every run.",
            "Choose a second reviewer",
        )),
        Step::Lenses if !cfg.lenses.any() => Some((
            "No focused review is turned on in Settings, so this step will be skipped \
             on every run.",
            "Turn on a focused review",
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
