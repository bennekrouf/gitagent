//! Editing the selected flow: its steps, their order and settings.

use super::*;
use crate::services::flowdef::FlowDef;
use crate::services::store::RepoFlows;
use std::collections::BTreeSet;

#[allow(clippy::too_many_arguments)]
pub(super) fn flow_editor(
    mut book: Signal<FlowBook>,
    mut scopes: Signal<RepoFlows>,
    mut everywhere: Signal<BTreeSet<String>>,
    mut flow_id: Signal<String>,
    mut node_id: Signal<String>,
    mut adding: Signal<bool>,
    mut testing: Signal<bool>,
    mut settings_open: Signal<bool>,
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
    mut edit_flow: impl FnMut(&dyn Fn(&mut crate::services::flowdef::FlowDef)) + Copy + 'static,
    props: SetupProps,
) -> Element {
    rsx! {
        match current.clone() {
            None => rsx! {
                div { class: "placeholder",
                    div { class: "placeholder-title", "No flow selected" }
                }
            },
            Some(flow) => {
                let problems = validate(&flow);
                let flow_for_dag = flow.clone();
                rsx! {
                    div { class: "dag-col",
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
                                    // Typing the name empty is a real
                                    // way to end up here — nothing
                                    // stops it mid-edit, unlike the id
                                    // fields, which are auto-derived
                                    // rather than typed. Left blank,
                                    // the sidebar row would carry no
                                    // name at all with no way back
                                    // except retyping, so losing focus
                                    // is where this is caught rather
                                    // than every keystroke.
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

                        {
                            let id = flow.id.clone();
                            let new = is_new(&id);
                            let only = scopes.read().shown_only_on(&id).map(|r| r.to_vec());
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
                                (None, true, Some(_)) if !everywhere.read().contains(&id) => (
                                    format!(
                                        "New: will be shown only on {origin_label} once saved. Other repositories can add it from their flow list."
                                    ),
                                    Some(("Show on every repository", true)),
                                ),
                                (None, _, Some(_)) if shipped.get(&id).is_none() => (
                                    "Shown on every repository.".into(),
                                    Some(("Only on this repository", false)),
                                ),
                                _ => ("Shown on every repository.".into(), None),
                            };
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
                                            onclick: {
                                                let id = id.clone();
                                                let origin = origin.clone();
                                                move |_| {
                                                    if wide {
                                                        everywhere.write().insert(id.clone());
                                                        scopes.write().everywhere(&id);
                                                    } else if let Some(repo) = origin.as_deref() {
                                                        everywhere.write().remove(&id);
                                                        scopes.write().only_on(&id, repo);
                                                    }
                                                }
                                            },
                                            if wide { "{button}" } else { "Only on {origin_label}" }
                                        }
                                    }
                                }
                            }
                        }

                        // What this flow is the answer to. Declared
                        // rather than guessed from its name, so a flow
                        // called anything at all can be the one the app
                        // opens on.
                        div { class: "handles",
                            div { class: "items-head",
                                span { class: "items-head-label", "Open this flow when a repository has" }
                                // A flow declaring nothing still runs
                                // when you start it by hand — it is
                                // just invisible to anything that
                                // picks a flow on your behalf, which
                                // includes a trusted run's next leg.
                                // Duplicating a flow copies its nodes
                                // and not its purpose, so this is the
                                // normal state of a new flow rather
                                // than a rare mistake.
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

                        if !problems.is_empty() {
                            div { class: "problems",
                                div { class: "problems-head",
                                    "{problems.len()} problem(s) — this flow will not be offered to run"
                                }
                                for problem in problems.iter() {
                                    div { key: "{problem:?}", class: "problem", "{problem.message()}" }
                                }
                            }
                        }

                        DagView {
                            flow: flow_for_dag,
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
                        } else if let Some(def) = flow.nodes.iter().find(|n| n.id == selected_node).cloned() {
                            {
                                let info = catalogue::by_key(&def.step);
                                let others: Vec<NodeDef> = flow.nodes.iter()
                                    .filter(|n| n.id != def.id)
                                    .cloned()
                                    .collect();
                                rsx! {
                                    div { class: "editor",
                                        div { class: "editor-head",
                                            span { "{def.id}" }
                                            button {
                                                class: "btn btn-danger",
                                                onclick: {
                                                    let id = def.id.clone();
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
                                                // Adding the step is not enough on its own: with
                                                // nothing chosen in Settings it is skipped on every
                                                // run. Said here, where it was just added, rather
                                                // than discovered on the first pull request.
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

                                        label { class: "field",
                                            span { "Title" }
                                            input {
                                                autocapitalize: "off",
                                                autocomplete: "off",
                                                spellcheck: "false",
                                                "autocorrect": "off",
                                                value: "{def.title}",
                                                placeholder: info.map(|i| i.title).unwrap_or(""),
                                                oninput: {
                                                    let id = def.id.clone();
                                                    move |e: Event<FormData>| {
                                                        let (id, value) = (id.clone(), e.value());
                                                        edit_flow(&move |f| {
                                                            if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                n.title = value.clone();
                                                            }
                                                        });
                                                    }
                                                },
                                            }
                                        }

                                        label { class: "field",
                                            span { "Subtitle" }
                                            input {
                                                autocapitalize: "off",
                                                autocomplete: "off",
                                                spellcheck: "false",
                                                "autocorrect": "off",
                                                value: "{def.subtitle}",
                                                placeholder: info.map(|i| i.subtitle).unwrap_or(""),
                                                oninput: {
                                                    let id = def.id.clone();
                                                    move |e: Event<FormData>| {
                                                        let (id, value) = (id.clone(), e.value());
                                                        edit_flow(&move |f| {
                                                            if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                n.subtitle = value.clone();
                                                            }
                                                        });
                                                    }
                                                },
                                            }
                                        }

                                        for field in info.map(|i| i.config).unwrap_or(&[]) {
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
                                                        oninput: {
                                                            let (id, key) = (def.id.clone(), field.key);
                                                            move |e: Event<FormData>| {
                                                                let (id, value) = (id.clone(), e.value());
                                                                edit_flow(&move |f| {
                                                                    if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                        n.config.insert(key.to_string(), value.clone());
                                                                    }
                                                                });
                                                            }
                                                        },
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
                                                            oninput: {
                                                                let (id, key) = (def.id.clone(), field.key);
                                                                move |e: Event<FormData>| {
                                                                    let (id, value) = (id.clone(), e.value());
                                                                    edit_flow(&move |f| {
                                                                        if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                            n.config.insert(key.to_string(), value.clone());
                                                                        }
                                                                    });
                                                                }
                                                            },
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
                                                                            edit_flow(&move |f| {
                                                                                if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                                    n.config.insert("identity".into(), path.clone());
                                                                                }
                                                                            });
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
                                                                    move |_| {
                                                                        let (id, path) = (id.clone(), path.clone());
                                                                        edit_flow(&move |f| {
                                                                            if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                                n.config.insert("identity".into(), path.clone());
                                                                            }
                                                                        });
                                                                    }
                                                                },
                                                                "{path}"
                                                            }
                                                        }
                                                    }
                                                }
                                                p { class: "field-note", "{field.help}" }
                                            }
                                        }

                                        if info.map(|i| i.testable).unwrap_or(false) {
                                            div { class: "field-row",
                                                button {
                                                    class: "btn",
                                                    disabled: *testing.read(),
                                                    onclick: {
                                                        let node = def.clone();
                                                        move |_| {
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
                                                        }
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

                                        label { class: "item",
                                            input {
                                                r#type: "checkbox",
                                                checked: def.is_gated(),
                                                onchange: {
                                                    let (id, now) = (def.id.clone(), def.is_gated());
                                                    move |_| {
                                                        let id = id.clone();
                                                        edit_flow(&move |f| {
                                                            if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                n.gated = Some(!now);
                                                            }
                                                        });
                                                    }
                                                },
                                            }
                                            span { class: "item-label", "Stop for approval before running" }
                                        }

                                        div { class: "items-head", span { "Runs after" } }
                                        div { class: "items",
                                            if others.is_empty() {
                                                div { class: "contract-none", "Nothing else in this flow yet." }
                                            } else {
                                                for other in others.iter().cloned() {
                                                    {
                                                        let allowed = can_depend_on(&flow, &def.id, &other.id);
                                                        let on = def.deps.contains(&other.id);
                                                        let label = if other.title.is_empty() {
                                                            catalogue::by_key(&other.step)
                                                                .map(|i| i.title.to_string())
                                                                .unwrap_or_else(|| other.id.clone())
                                                        } else {
                                                            other.title.clone()
                                                        };
                                                        rsx! {
                                                            label {
                                                                key: "{other.id}",
                                                                class: if on { "item" } else { "item item-off" },
                                                                title: if allowed { "" } else { "Would create a loop" },
                                                                input {
                                                                    r#type: "checkbox",
                                                                    checked: on,
                                                                    disabled: !allowed && !on,
                                                                    onchange: {
                                                                        let (id, dep) = (def.id.clone(), other.id.clone());
                                                                        move |_| {
                                                                            let (id, dep) = (id.clone(), dep.clone());
                                                                            edit_flow(&move |f| {
                                                                                if let Some(n) = f.nodes.iter_mut().find(|n| n.id == id) {
                                                                                    if n.deps.contains(&dep) {
                                                                                        n.deps.retain(|d| d != &dep);
                                                                                    } else {
                                                                                        n.deps.push(dep.clone());
                                                                                    }
                                                                                }
                                                                            });
                                                                        }
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

                                        div { class: "remedy-actions",
                                            button {
                                                class: "btn btn-primary",
                                                onclick: move |_| adding.set(true),
                                                "Add a step"
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            div { class: "editor",
                                div { class: "detail-empty",
                                    "Pick a step in the diagram to edit it."
                                }
                                div { class: "remedy-actions",
                                    button {
                                        class: "btn btn-primary",
                                        onclick: move |_| adding.set(true),
                                        "Add a step"
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
