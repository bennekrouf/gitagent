//! One repository's local branches, and what happened to each one's pull
//! request — merged and closed branches pile up because git never deletes
//! one just because its PR is done, so this is where you clean them out.

use dioxus::prelude::*;

use crate::services::branches::{BranchInfo, PrState};

#[derive(Props, Clone, PartialEq)]
pub struct BranchesPanelProps {
    pub repo_label: String,
    /// `None` while still loading.
    pub branches: Option<Result<Vec<BranchInfo>, String>>,
    /// Set when the last push/create-PR attempt failed — shown once, above
    /// the list, rather than silently leaving the branch exactly as it was.
    #[props(default)]
    pub action_error: Option<String>,
    pub on_close: EventHandler<()>,
    pub on_refresh: EventHandler<()>,
    /// `(branch, force)` — force is set for a merged branch, whose commit is
    /// squashed into the base and so never looks locally merged.
    pub on_delete: EventHandler<(String, bool)>,
    /// Pushes the branch and opens a pull request for it — the alternative
    /// to deleting, offered wherever a branch has real work and no live PR.
    pub on_create_pr: EventHandler<String>,
}

fn state_class(state: PrState) -> &'static str {
    match state {
        PrState::Open => "branch-pr branch-pr-open",
        PrState::Merged => "branch-pr branch-pr-merged",
        PrState::Closed => "branch-pr branch-pr-closed",
        PrState::None => "branch-pr branch-pr-none",
        PrState::Unchecked => "branch-pr branch-pr-unchecked",
    }
}

#[component]
pub fn BranchesPanel(props: BranchesPanelProps) -> Element {
    let close = move |_| props.on_close.call(());
    // A merged branch's commit already lives on the base branch, so deleting
    // it loses nothing — one click is enough. Anything else (closed without
    // merging, still open, no PR, unchecked) can lose real commits, so it
    // needs a second click naming what it's about to do.
    let mut confirming = use_signal(|| Option::<String>::None);

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div {
                class: "modal modal-wide",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),

                div { class: "modal-head",
                    span { "Branches — {props.repo_label}" }
                    button {
                        class: "modal-close",
                        onclick: close,
                        "×"
                    }
                }

                div { class: "modal-body",
                    if let Some(err) = props.action_error.clone() {
                        div { class: "pr-list-error", "{err}" }
                    }
                    match props.branches.clone() {
                        None => rsx! { div { class: "branches-loading", "Checking branches…" } },
                        Some(Err(e)) => rsx! {
                            div { class: "pr-list-error", "Couldn't read branches: {e}" }
                        },
                        Some(Ok(list)) => {
                            let cleanup: Vec<BranchInfo> = list.iter()
                                .filter(|b| !b.is_current && !b.protected && b.pr_state.merged())
                                .cloned()
                                .collect();
                            let worth_a_pr: Vec<BranchInfo> = list.iter()
                                .filter(|b| b.worth_a_pr())
                                .cloned()
                                .collect();
                            rsx! {
                                if !worth_a_pr.is_empty() {
                                    div { class: "branches-cleanup branches-worth-pr",
                                        span {
                                            "{worth_a_pr.len()} branch" if worth_a_pr.len() != 1 { "es" }
                                            " have commits not on the base branch and no open pull \
                                             request — decide per branch below: open a PR, or delete."
                                        }
                                    }
                                }
                                if !cleanup.is_empty() {
                                    div { class: "branches-cleanup",
                                        span {
                                            "{cleanup.len()} branch" if cleanup.len() != 1 { "es" }
                                            " already merged — nothing here is only on these branches."
                                        }
                                        button {
                                            class: "btn btn-danger",
                                            onclick: {
                                                let names: Vec<String> = cleanup.iter().map(|b| b.name.clone()).collect();
                                                move |_| {
                                                    for name in names.iter() {
                                                        props.on_delete.call((name.clone(), true));
                                                    }
                                                }
                                            },
                                            "Delete all merged"
                                        }
                                    }
                                }
                                div { class: "branches-list",
                                    for b in list.iter().cloned() {
                                        div {
                                            key: "{b.name}",
                                            class: "branch-row",
                                            span {
                                                class: if b.is_current { "branch-name branch-name-current" } else { "branch-name" },
                                                if b.is_current { "▸ " }
                                                "{b.name}"
                                            }
                                            if b.protected {
                                                span { class: "branch-protected", "protected" }
                                            }
                                            span {
                                                class: state_class(b.pr_state),
                                                title: "{b.pr_title}",
                                                if let Some(number) = &b.pr_number {
                                                    "#{number} {b.pr_state.label()}"
                                                } else {
                                                    "{b.pr_state.label()}"
                                                }
                                                if b.worth_a_pr() {
                                                    " · {b.ahead} ahead"
                                                }
                                            }
                                            if !b.is_current && !b.protected {
                                                if b.worth_a_pr() {
                                                    button {
                                                        class: "btn btn-primary branch-create-pr",
                                                        title: "Pushes this branch and opens a pull request from its {b.ahead} commit(s).",
                                                        onclick: {
                                                            let name = b.name.clone();
                                                            move |_| props.on_create_pr.call(name.clone())
                                                        },
                                                        "Create PR"
                                                    }
                                                }
                                                if b.pr_state.merged() {
                                                    button {
                                                        class: "btn btn-danger branch-delete",
                                                        title: "Merged — safe to delete, its commit already lives on the base branch.",
                                                        onclick: {
                                                            let name = b.name.clone();
                                                            move |_| props.on_delete.call((name.clone(), true))
                                                        },
                                                        "Delete"
                                                    }
                                                } else if confirming.read().as_deref() == Some(b.name.as_str()) {
                                                    button {
                                                        class: "btn btn-danger branch-delete",
                                                        title: "This discards any commits that exist only on this branch.",
                                                        onclick: {
                                                            let name = b.name.clone();
                                                            move |_| {
                                                                confirming.set(None);
                                                                props.on_delete.call((name.clone(), false));
                                                            }
                                                        },
                                                        "Really delete?"
                                                    }
                                                } else {
                                                    button {
                                                        class: "btn branch-delete",
                                                        title: "Not confirmed merged — deleting removes any commits that exist only here.",
                                                        onclick: {
                                                            let name = b.name.clone();
                                                            move |_| confirming.set(Some(name.clone()))
                                                        },
                                                        "Delete"
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
            }
        }
    }
}
