//! Editing the selected flow: its steps, their order and settings.
//!
//! `flow_editor` lays out the two columns; each section is its own function
//! below: the flow's name and buttons, where it is shown, what it answers,
//! then the step catalogue or the selected step's settings.

use super::*;
use crate::services::catalogue::ConfigField;
use crate::services::flowdef::FlowDef;
use crate::services::store::RepoFlows;
use std::collections::BTreeSet;

/// Changes the flow being edited: `edit_flow` as Setup hands it down.
trait EditFlow: FnMut(&dyn Fn(&mut FlowDef)) + Copy + 'static {}
impl<T: FnMut(&dyn Fn(&mut FlowDef)) + Copy + 'static> EditFlow for T {}

/// Changes the step `id` of the flow being edited, if it is still there.
fn edit_node(mut edit_flow: impl EditFlow, id: String, change: impl Fn(&mut NodeDef) + 'static) {
    edit_flow(&move |f| {
        if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
            change(n);
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn flow_editor(
    book: Signal<FlowBook>,
    scopes: Signal<RepoFlows>,
    everywhere: Signal<BTreeSet<String>>,
    flow_id: Signal<String>,
    mut node_id: Signal<String>,
    mut adding: Signal<bool>,
    testing: Signal<bool>,
    settings_open: Signal<bool>,
    mut test_result: Signal<Option<Result<String, String>>>,
    identities: Signal<Vec<String>>,
    _saved: Signal<Layout>,
    editor_w: Signal<f64>,
    mut dragging: Signal<u8>,
    mut drag_from: Signal<(f64, f64)>,
    shipped: FlowBook,
    is_new: impl Fn(&str) -> bool,
    origin: Option<String>,
    origin_label: String,
    current: Option<FlowDef>,
    selected_node: String,
    edit_flow: impl FnMut(&dyn Fn(&mut FlowDef)) + Copy + 'static,
    props: SetupProps,
) -> Element {
    let Some(flow) = current else {
        return rsx! {
            div { class: "placeholder",
                div { class: "placeholder-title", "No flow selected" }
            }
        };
    };
    let problems = validate(&flow);
    let new = is_new(&flow.id);
    let selected = flow.nodes.iter().find(|n| n.id == selected_node).cloned();
    rsx! {
        div { class: "dag-col",
            {flow_head(book, flow_id, node_id, edit_flow, &flow)}
            {flow_scope(scopes, everywhere, &shipped, new, origin, origin_label, &flow.id)}
            {flow_handles(edit_flow, &flow)}

            if !problems.is_empty() {
                div { class: "problems",
                    div { class: "problems-head",
                        "{problems.len()} problem(s) — this flow will not be offered to run"
                    }
                    for problem in problems.iter() {
                        div { key: "{problem:?}", class: "problem", "{problem.message()}" }
                    }
                    if flow.repaired().is_some() {
                        button {
                            class: "btn",
                            title: "Puts back the steps this flow needs",
                            onclick: {
                                let mut edit_flow = edit_flow;
                                move |_| {
                                    edit_flow(&|f| {
                                        if let Some(fixed) = f.repaired() {
                                            *f = fixed;
                                        }
                                    })
                                }
                            },
                            "Restore missing step"
                        }
                    }
                }
            }

            DagView {
                flow: flow.clone(),
                selected: selected_node.clone(),
                on_select: move |id: String| {
                    node_id.set(id);
                    adding.set(false);
                    test_result.set(None);
                },
            }
        }

        div {
            class: "divider",
            onmousedown: move |e| {
                drag_from.set((e.client_coordinates().x, *editor_w.read()));
                dragging.set(2);
            },
        }

        // ── Step editor / catalogue ──────────────────
        div { class: "editor-col", style: "width: {editor_w}px;",
            if *adding.read() {
                {step_catalogue(book, flow_id, node_id, adding)}
            } else if let Some(def) = selected {
                {step_editor(node_id, adding, testing, settings_open, test_result, identities, edit_flow, &flow, def, &props)}
            } else {
                div { class: "editor",
                    div { class: "detail-empty",
                        "Pick a step in the diagram to edit it."
                    }
                    {add_step_button(adding)}
                }
            }
        }
    }
}

/// The flow's name, and duplicating or deleting it.
fn flow_head(
    mut book: Signal<FlowBook>,
    mut flow_id: Signal<String>,
    mut node_id: Signal<String>,
    mut edit_flow: impl EditFlow,
    flow: &FlowDef,
) -> Element {
    rsx! {
        div { class: "col-head",
            input {
                class: "flow-name",
                autocapitalize: "off",
                autocomplete: "off",
                spellcheck: "false",
                "autocorrect": "off",
                value: "{flow.label}",
                oninput: move |e| {
                    let value = e.value();
                    edit_flow(&move |f| f.label = value.clone());
                },
                onblur: move |_| {
                    // Typing the name empty is a real way to end up here —
                    // nothing stops it mid-edit, unlike the id fields, which
                    // are auto-derived rather than typed. Left blank, the
                    // sidebar row would carry no name at all with no way back
                    // except retyping, so losing focus is where this is caught
                    // rather than every keystroke.
                    let empty = book
                        .read()
                        .get(&flow_id.read())
                        .is_some_and(|f| f.label.trim().is_empty());
                    if empty {
                        edit_flow(&|f| f.label = "Untitled flow".into());
                    }
                },
            }
            button {
                class: "btn",
                title: "Copy this flow, steps and all",
                onclick: move |_| {
                    let id = flow_id.read().clone();
                    let copy = book.write().duplicate(&id);
                    if let Some(new_id) = copy {
                        flow_id.set(new_id);
                        node_id.set(String::new());
                    }
                },
                "Duplicate"
            }
            button {
                class: "btn btn-danger",
                onclick: move |_| {
                    let id = flow_id.read().clone();
                    let mut w = book.write();
                    w.flows.retain(|f| f.id != id);
                    let next = w.flows.first().map(|f| f.id.clone()).unwrap_or_default();
                    drop(w);
                    flow_id.set(next);
                    node_id.set(String::new());
                },
                "Delete flow"
            }
        }
    }
}

/// Which repositories show the flow, and the button that widens or narrows
/// that. `new`: the flow has not been saved yet.
fn flow_scope(
    mut scopes: Signal<RepoFlows>,
    mut everywhere: Signal<BTreeSet<String>>,
    shipped: &FlowBook,
    new: bool,
    origin: Option<String>,
    origin_label: String,
    id: &str,
) -> Element {
    let only = scopes.read().shown_only_on(id).map(|r| r.to_vec());
    let label_of = |p: &String| p.rsplit('/').next().unwrap_or(p).to_string();
    let (text, action): (String, Option<(&str, bool)>) = match (&only, new, &origin) {
        (Some(repos), _, _) if repos.is_empty() => (
            "Shown on no repository. Add it from a repository's flow list.".into(),
            Some(("Show on every repository", true)),
        ),
        (Some(repos), _, _) => (
            format!(
                "Shown only on {}. Other repositories can add it from their flow list.",
                repos.iter().map(label_of).collect::<Vec<_>>().join(", ")
            ),
            Some(("Show on every repository", true)),
        ),
        (None, true, Some(_)) if !everywhere.read().contains(id) => (
            format!(
                "New: will be shown only on {origin_label} once saved. Other repositories can add it from their flow list."
            ),
            Some(("Show on every repository", true)),
        ),
        (None, _, Some(_)) if shipped.get(id).is_none() => (
            "Shown on every repository.".into(),
            Some(("Only on this repository", false)),
        ),
        _ => ("Shown on every repository.".into(), None),
    };
    let id = id.to_string();
    rsx! {
        div { class: "flow-scope",
            span { class: "flow-scope-text", "{text}" }
            if let Some((button, wide)) = action {
                button {
                    class: "btn btn-ghost",
                    title: if wide {
                        "Every repository shows this flow; any one of them can still hide it from its flow list."
                    } else {
                        "Only the repository Setup was opened from shows it; others can add it from their flow list."
                    },
                    onclick: move |_| {
                        if wide {
                            everywhere.write().insert(id.clone());
                            scopes.write().everywhere(&id);
                        } else if let Some(repo) = origin.as_deref() {
                            everywhere.write().remove(&id);
                            scopes.write().only_on(&id, repo);
                        }
                    },
                    if wide { "{button}" } else { "Only on {origin_label}" }
                }
            }
        }
    }
}

/// What this flow is the answer to. Declared rather than guessed from its
/// name, so a flow called anything at all can be the one the app opens on.
fn flow_handles(mut edit_flow: impl EditFlow, flow: &FlowDef) -> Element {
    rsx! {
        div { class: "handles",
            div { class: "items-head",
                span { class: "items-head-label", "Open this flow when a repository has" }
                // A flow declaring nothing still runs when you start it by
                // hand — it is just invisible to anything that picks a flow on
                // your behalf, which includes a trusted run's next leg.
                // Duplicating a flow copies its nodes and not its purpose, so
                // this is the normal state of a new flow rather than a rare
                // mistake.
                if flow.handles.is_empty() {
                    span {
                        class: "handles-none",
                        title: "Nothing opens this flow automatically, and a trusted run will never continue into it.",
                        "answers nothing yet"
                    }
                }
            }
            div { class: "items",
                for need in Need::ALL {
                    label {
                        key: "{need.key()}",
                        class: if flow.answers(need) { "item" } else { "item item-off" },
                        input {
                            r#type: "checkbox",
                            checked: flow.answers(need),
                            onchange: move |_| {
                                edit_flow(&move |f| {
                                    let key = need.key().to_string();
                                    if f.handles.contains(&key) {
                                        f.handles.retain(|h| h != &key);
                                    } else {
                                        f.handles.push(key.clone());
                                    }
                                });
                            },
                        }
                        span { class: "item-label", "{need.label()}" }
                    }
                }
            }
        }
    }
}

/// Every step this build knows, to add one after the selected step.
fn step_catalogue(
    mut book: Signal<FlowBook>,
    flow_id: Signal<String>,
    mut node_id: Signal<String>,
    mut adding: Signal<bool>,
) -> Element {
    rsx! {
        div { class: "editor",
            div { class: "editor-head",
                span { "Add a step" }
                button {
                    class: "modal-close",
                    onclick: move |_| adding.set(false),
                    "×"
                }
            }
            p { class: "field-note",
                "A step is a function in the binary; a flow decides which \
                 ones run and in what order. New steps arrive with the app, \
                 not from here."
            }
            div { class: "catalogue",
                for entry in CATALOGUE.iter() {
                    div {
                        key: "{entry.key}",
                        class: "cat-item",
                        onclick: move |_| {
                            let key = entry.key;
                            let selected = node_id.read().clone();
                            let mut new_id = String::new();
                            {
                                let mut w = book.write();
                                let id = flow_id.read().clone();
                                if let Some(f) = w.get_mut(&id) {
                                    new_id = f.free_id(key);
                                    let mut node = NodeDef::from_catalogue(&new_id, key);
                                    node.deps = default_deps(f, &selected);
                                    f.nodes.push(node);
                                }
                            }
                            node_id.set(new_id);
                            adding.set(false);
                        },
                        div { class: "cat-head",
                            span { class: "cat-title", "{entry.title}" }
                            span {
                                class: if entry.kind == NodeKind::Model { "tag tag-model" } else { "tag tag-det" },
                                if entry.kind == NodeKind::Model { "model" } else { "code" }
                            }
                        }
                        div { class: "cat-about", "{entry.about}" }
                    }
                }
            }
        }
    }
}

fn add_step_button(mut adding: Signal<bool>) -> Element {
    rsx! {
        div { class: "remedy-actions",
            button {
                class: "btn btn-primary",
                onclick: move |_| adding.set(true),
                "Add a step"
            }
        }
    }
}

/// The selected step: what it does, its titles and settings, whether it
/// stops for approval, and what it runs after.
#[allow(clippy::too_many_arguments)]
fn step_editor(
    mut node_id: Signal<String>,
    adding: Signal<bool>,
    testing: Signal<bool>,
    mut settings_open: Signal<bool>,
    test_result: Signal<Option<Result<String, String>>>,
    identities: Signal<Vec<String>>,
    edit_flow: impl EditFlow,
    flow: &FlowDef,
    def: NodeDef,
    props: &SetupProps,
) -> Element {
    let info = catalogue::by_key(&def.step);
    rsx! {
        div { class: "editor",
            div { class: "editor-head",
                span { "{def.id}" }
                button {
                    class: "btn btn-danger",
                    onclick: {
                        let id = def.id.clone();
                        let mut edit_flow = edit_flow;
                        move |_| {
                            let target = id.clone();
                            edit_flow(&move |f| f.remove_node(&target));
                            node_id.set(String::new());
                        }
                    },
                    "Remove"
                }
            }

            match info {
                Some(i) => rsx! {
                    p { class: "field-note", "{i.about}" }
                    // Adding the step is not enough on its own: with nothing
                    // chosen in Settings it is skipped on every run. Said here,
                    // where it was just added, rather than discovered on the
                    // first pull request.
                    if let Some((why, action)) = unset_in_settings(i.step, &props.llm_config.read()) {
                        div { class: "probe probe-warn",
                            "{why}"
                            div { class: "probe-actions",
                                button {
                                    class: "btn",
                                    onclick: move |_| settings_open.set(true),
                                    "{action}"
                                }
                            }
                        }
                    }
                    div { class: "contract",
                        div { class: "contract-col",
                            div { class: "contract-label", "reads" }
                            if i.reads.is_empty() {
                                div { class: "contract-none", "—" }
                            } else {
                                for key in i.reads.iter() {
                                    div { key: "{key}", class: "chip", "{key}" }
                                }
                            }
                        }
                        div { class: "contract-col",
                            div { class: "contract-label", "writes" }
                            for key in i.writes.iter() {
                                div { key: "{key}", class: "chip chip-out", "{key}" }
                            }
                        }
                    }
                },
                None => rsx! {
                    div { class: "probe probe-bad",
                        "`{def.step}` is not a step this build knows."
                    }
                },
            }

            {text_field(edit_flow, "Title", &def.id, &def.title, info.map(|i| i.title).unwrap_or(""), |n, v| n.title = v)}
            {text_field(edit_flow, "Subtitle", &def.id, &def.subtitle, info.map(|i| i.subtitle).unwrap_or(""), |n, v| n.subtitle = v)}

            for field in info.map(|i| i.config).unwrap_or(&[]) {
                {config_field(identities, edit_flow, &def, field)}
            }

            if info.map(|i| i.testable).unwrap_or(false) {
                {connection_test(testing, test_result, &def)}
            }

            label { class: "item",
                input {
                    r#type: "checkbox",
                    checked: def.is_gated(),
                    onchange: {
                        let (id, now) = (def.id.clone(), def.is_gated());
                        move |_| edit_node(edit_flow, id.clone(), move |n| n.gated = Some(!now))
                    },
                }
                span { class: "item-label", "Stop for approval before running" }
            }

            {runs_after(edit_flow, flow, &def)}
            {add_step_button(adding)}
        }
    }
}

/// A one-line text setting of the step `id` that every step has, written
/// into it by `set`.
fn text_field(
    edit_flow: impl EditFlow,
    label: &str,
    id: &str,
    value: &str,
    placeholder: &str,
    set: fn(&mut NodeDef, String),
) -> Element {
    let id = id.to_string();
    rsx! {
        label { class: "field",
            span { "{label}" }
            input {
                autocapitalize: "off",
                autocomplete: "off",
                spellcheck: "false",
                "autocorrect": "off",
                value: "{value}",
                placeholder: "{placeholder}",
                oninput: move |e: Event<FormData>| {
                    let value = e.value();
                    edit_node(edit_flow, id.clone(), move |n| set(n, value.clone()));
                },
            }
        }
    }
}

/// One of the step's own settings, as its catalogue entry describes it. An
/// `identity` (an SSH key) also gets a file picker and the keys found in
/// `~/.ssh`.
fn config_field(
    identities: Signal<Vec<String>>,
    edit_flow: impl EditFlow,
    def: &NodeDef,
    field: &'static ConfigField,
) -> Element {
    let key = field.key;
    let set = |id: String| {
        move |e: Event<FormData>| {
            let value = e.value();
            edit_node(edit_flow, id.clone(), move |n| {
                n.config.insert(key.to_string(), value.clone());
            });
        }
    };
    let use_identity = move |id: String, path: String| {
        edit_node(edit_flow, id, move |n| {
            n.config.insert("identity".into(), path.clone());
        });
    };
    rsx! {
        label { key: "{field.key}", class: "field",
            span {
                "{field.label}"
                if field.required { " *" }
            }
            if field.multiline {
                textarea {
                    rows: "2",
                    autocapitalize: "off",
                    autocomplete: "off",
                    spellcheck: "false",
                    "autocorrect": "off",
                    value: "{def.setting(field.key)}",
                    placeholder: "{field.placeholder}",
                    oninput: set(def.id.clone()),
                }
            } else {
                div { class: "field-row",
                    input {
                        class: "field-grow",
                        autocapitalize: "off",
                        autocomplete: "off",
                        spellcheck: "false",
                        "autocorrect": "off",
                        value: "{def.setting(field.key)}",
                        placeholder: "{field.placeholder}",
                        oninput: set(def.id.clone()),
                    }
                    if field.key == "identity" {
                        button {
                            class: "btn",
                            title: "Pick a private key file — starts in ~/.ssh",
                            onclick: {
                                let id = def.id.clone();
                                move |_| {
                                    let id = id.clone();
                                    spawn(async move {
                                        let start = dirs::home_dir()
                                            .map(|h| h.join(".ssh"))
                                            .unwrap_or_default();
                                        let Some(handle) = rfd::AsyncFileDialog::new()
                                            .set_title("Pick a private key")
                                            .set_directory(&start)
                                            .pick_file()
                                            .await
                                        else {
                                            return;
                                        };
                                        // The public half is never the right pick — steer
                                        // towards its private sibling instead of silently
                                        // accepting a key that can never authenticate.
                                        let mut path = handle.path().to_string_lossy().to_string();
                                        if let Some(stripped) = path.strip_suffix(".pub") {
                                            path = stripped.to_string();
                                        }
                                        use_identity(id, path);
                                    });
                                }
                            },
                            "Browse…"
                        }
                    }
                }
            }
            if field.key == "identity" && !identities.read().is_empty() {
                div { class: "identity-picks",
                    for path in identities.read().iter().cloned() {
                        button {
                            key: "{path}",
                            class: "identity-pick",
                            r#type: "button",
                            title: "Use {path}",
                            onclick: {
                                let (id, path) = (def.id.clone(), path.clone());
                                move |_| use_identity(id.clone(), path.clone())
                            },
                            "{path}"
                        }
                    }
                }
            }
            p { class: "field-note", "{field.help}" }
        }
    }
}

/// Trying a remote step's host, port and key before a run depends on them.
fn connection_test(
    mut testing: Signal<bool>,
    mut test_result: Signal<Option<Result<String, String>>>,
    def: &NodeDef,
) -> Element {
    let node = def.clone();
    rsx! {
        div { class: "field-row",
            button {
                class: "btn",
                disabled: *testing.read(),
                onclick: move |_| {
                    let node = node.clone();
                    testing.set(true);
                    test_result.set(None);
                    spawn(async move {
                        let outcome = crate::services::remote::test(
                            node.setting("host"),
                            node.setting("port"),
                            node.setting("identity"),
                        )
                        .await;
                        test_result.set(Some(outcome));
                        testing.set(false);
                    });
                },
                if *testing.read() { "Testing…" } else { "Test connection" }
            }
        }
        match test_result.read().clone() {
            Some(Ok(msg)) => rsx! { div { class: "probe probe-ok", "{msg}" } },
            Some(Err(msg)) => rsx! { div { class: "probe probe-bad", "{msg}" } },
            None => rsx! {},
        }
    }
}

/// The other steps this one waits for. One that would make a loop can't be
/// ticked.
fn runs_after(edit_flow: impl EditFlow, flow: &FlowDef, def: &NodeDef) -> Element {
    let others: Vec<NodeDef> = flow
        .nodes
        .iter()
        .filter(|n| n.id != def.id)
        .cloned()
        .collect();
    rsx! {
        div { class: "items-head", span { "Runs after" } }
        div { class: "items",
            if others.is_empty() {
                div { class: "contract-none", "Nothing else in this flow yet." }
            } else {
                for other in others {
                    {
                        let allowed = can_depend_on(flow, &def.id, &other.id);
                        let on = def.deps.contains(&other.id);
                        let label = if other.title.is_empty() {
                            catalogue::by_key(&other.step)
                                .map(|i| i.title.to_string())
                                .unwrap_or_else(|| other.id.clone())
                        } else {
                            other.title.clone()
                        };
                        let (id, dep) = (def.id.clone(), other.id.clone());
                        rsx! {
                            label {
                                key: "{other.id}",
                                class: if on { "item" } else { "item item-off" },
                                title: if allowed { "" } else { "Would create a loop" },
                                input {
                                    r#type: "checkbox",
                                    checked: on,
                                    disabled: !allowed && !on,
                                    onchange: move |_| {
                                        let dep = dep.clone();
                                        edit_node(edit_flow, id.clone(), move |n| {
                                            if n.deps.contains(&dep) {
                                                n.deps.retain(|d| d != &dep);
                                            } else {
                                                n.deps.push(dep.clone());
                                            }
                                        });
                                    },
                                }
                                span { class: "item-label", "{label}" }
                                if !allowed && !on {
                                    span { class: "item-note note-deleted", "loop" }
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
mod tests;
