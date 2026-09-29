//! GitAgent Pro — the licence on this computer, and which repositories the
//! free version runs flows in.

use dioxus::prelude::*;

use crate::services::licence::{self, Slots, Status, BUY_URL, FREE_REPOS};
use crate::services::store::Repo;

#[derive(Props, Clone, PartialEq)]
pub struct LicencePanelProps {
    pub status: Signal<Status>,
    pub slots: Signal<Slots>,
    /// For showing a slot's repository by its name rather than its path.
    pub repos: Vec<Repo>,
    /// Set when the panel opened because a run was refused: the repository
    /// that asked for a sixth slot.
    pub wanted: Option<String>,
    pub on_close: EventHandler<()>,
}

#[component]
pub fn LicencePanel(props: LicencePanelProps) -> Element {
    let mut status = props.status;
    let mut slots = props.slots;
    let mut key = use_signal(String::new);
    let mut problem = use_signal(|| Option::<String>::None);
    let close = move |_| props.on_close.call(());

    let activate = move |_| {
        let pasted = key.peek().clone();
        match licence::activate(&pasted) {
            Ok(s) => {
                status.set(s);
                key.set(String::new());
                problem.set(None);
            }
            Err(e) => problem.set(Some(e)),
        }
    };
    let remove = move |_| {
        licence::deactivate();
        status.set(Status::Free);
    };

    let label_of = |path: &str| {
        props
            .repos
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.label.clone())
            .unwrap_or_else(|| {
                std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string())
            })
    };
    let current = status.read().clone();
    let used: Vec<(String, String)> = slots
        .read()
        .repos
        .iter()
        .map(|p| (p.clone(), label_of(p)))
        .collect();
    let wanted = props.wanted.as_deref().map(label_of);

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div {
                class: "modal",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),

                div { class: "modal-head",
                    span { "GitAgent Pro" }
                    button { class: "modal-close", onclick: close, "×" }
                }

                div { class: "modal-body",
                    match &current {
                        Status::Pro(l) => rsx! {
                            p { "Licensed to " strong { "{l.email}" } ". Flows run in every repository." }
                            p { class: "field-note", "Includes every update released until {l.updates_until}." }
                        },
                        Status::Renew(l) => rsx! {
                            p { "Licensed to " strong { "{l.email}" } "." }
                            p { class: "field-note",
                                "This version was released on {licence::release_date()}, after your updates ended on \
                                 {l.updates_until}. Renew to use it with every repository, or keep using a version \
                                 released before that day."
                            }
                        },
                        Status::Unavailable => rsx! {
                            p { class: "field-note",
                                "This build of GitAgent can't check licences, and runs flows in every repository. \
                                 Download GitAgent from mayorana.ch to use a licence."
                            }
                        },
                        Status::Free => rsx! {
                            if let Some(name) = &wanted {
                                p { class: "probe probe-bad",
                                    "The free version runs flows in {FREE_REPOS} repositories, and they are all taken. \
                                     Give one back below to use {name}, or get Pro."
                                }
                            }
                            p {
                                "The free version runs flows in {FREE_REPOS} repositories of your choice: \
                                 the first run in a repository takes one. GitAgent Pro runs them in all of them."
                            }
                        },
                    }

                    if !current.unlimited() {
                        div { class: "field-note", "Free repositories: {used.len()} of {FREE_REPOS} used" }
                        div { class: "items",
                            for (path, name) in used {
                                div { key: "{path}", class: "item",
                                    span { class: "item-label", title: "{path}", "{name}" }
                                    button {
                                        class: "btn btn-ghost",
                                        title: "Free this slot for another repository",
                                        onclick: {
                                            // The row still shows `path`; the handler gets its own copy.
                                            let path = path.clone();
                                            move |_| {
                                            let mut s = slots.write();
                                            s.release(&path);
                                            licence::save_slots(&s);
                                            }
                                        },
                                        "Give back"
                                    }
                                }
                            }
                        }
                    }

                    if matches!(current, Status::Free | Status::Renew(_)) {
                        label { class: "field",
                            span { "Licence key" }
                            textarea {
                                rows: "4",
                                spellcheck: "false",
                                placeholder: "Paste the key from your purchase email",
                                value: "{key}",
                                oninput: move |e| {
                                    problem.set(None);
                                    key.set(e.value());
                                },
                            }
                        }
                        if let Some(p) = problem.read().clone() {
                            div { class: "probe probe-bad", "{p}" }
                        }
                    }

                    div { class: "field-row",
                        match &current {
                            Status::Pro(_) | Status::Renew(_) => rsx! {
                                button {
                                    class: "btn btn-ghost",
                                    title: "Remove the licence from this computer, e.g. to use it on another one",
                                    onclick: remove,
                                    "Remove from this computer"
                                }
                            },
                            _ => rsx! {},
                        }
                        if !matches!(current, Status::Pro(_)) {
                            a { class: "btn", href: "{BUY_URL}", target: "_blank",
                                if matches!(current, Status::Renew(_)) { "Renew…" } else { "Buy GitAgent Pro…" }
                            }
                        }
                        if matches!(current, Status::Free | Status::Renew(_)) {
                            button {
                                class: "btn btn-primary",
                                disabled: key.read().trim().is_empty(),
                                onclick: activate,
                                "Activate"
                            }
                        }
                    }
                }
            }
        }
    }
}
