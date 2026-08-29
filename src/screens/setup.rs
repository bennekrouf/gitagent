//! Setup — the meta view: the flows themselves, as editable data.
//!
//! Everything here writes `flows.toml` immediately. There is no save button
//! because there is no draft state: what you see is what the run view will use
//! the next time it loads. A flow with problems is kept and shown rather than
//! rejected — you should be able to leave one half-rewired and come back — but
//! it is not offered to run until the problems are gone.

use dioxus::prelude::*;

use crate::components::dag_view::DagView;
use crate::services::catalogue::{self, CATALOGUE};
use crate::services::flowdef::{can_depend_on, default_deps, validate, FlowBook, NodeDef};
use crate::services::graph::NodeKind;
use crate::services::probe::Need;
use crate::services::remote;
use crate::services::store::{self, Layout};

#[derive(Props, Clone, PartialEq)]
pub struct SetupProps {
    pub on_close: EventHandler<()>,
}

#[component]
pub fn Setup(props: SetupProps) -> Element {
    let mut book = use_signal(FlowBook::load);
    let first = book
        .read()
        .flows
        .first()
        .map(|f| f.id.clone())
        .unwrap_or_default();
    let mut flow_id = use_signal(|| first);
    let mut node_id = use_signal(String::new);
    let mut adding = use_signal(|| false);
    let mut confirm_restore = use_signal(|| false);
    let mut testing = use_signal(|| false);
    let mut test_result = use_signal(|| Option::<Result<String, String>>::None);
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
    let current_id = flow_id.read().clone();
    let current = snapshot.get(&current_id).cloned();
    let selected_node = node_id.read().clone();

    // Every mutation goes through here, so nothing can change without landing
    // on disk.
    let mut edit_flow = move |mutate: &dyn Fn(&mut crate::services::flowdef::FlowDef)| {
        let id = flow_id.read().clone();
        {
            let mut w = book.write();
            if let Some(flow) = w.get_mut(&id) {
                mutate(flow);
            }
        }
        book.read().save();
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
                        class: "btn btn-primary",
                        onclick: move |_| props.on_close.call(()),
                        "Done"
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
                        });
                    }
                },
                onmouseleave: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *editor_w.read(),
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
                                w.save();
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
                                        value: "{flow.label}",
                                        oninput: move |e| {
                                            let value = e.value();
                                            edit_flow(&move |f| f.label = value.clone());
                                        },
                                    }
                                    button {
                                        class: "btn",
                                        title: "Copy this flow, steps and all",
                                        onclick: move |_| {
                                            let id = flow_id.read().clone();
                                            let copy = {
                                                let mut w = book.write();
                                                let made = w.duplicate(&id);
                                                w.save();
                                                made
                                            };
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
                                            w.save();
                                            let next = w.flows.first().map(|f| f.id.clone()).unwrap_or_default();
                                            drop(w);
                                            flow_id.set(next);
                                            node_id.set(String::new());
                                        },
                                        "Delete flow"
                                    }
                                }

                                // What this flow is the answer to. Declared
                                // rather than guessed from its name, so a flow
                                // called anything at all can be the one the app
                                // opens on.
                                div { class: "handles",
                                    div { class: "items-head",
                                        span { class: "items-head-label", "Open this flow when a repository has" }
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
                                                            w.save();
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
                                        "{losing.len()} flow(s) will be lost, and there is no undo."
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
                                            restored.save();
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
