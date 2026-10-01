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
    /// The branch a delete or create-PR is currently running against, if
    /// any — that row's buttons disable and show it's working instead of
    /// looking like the click did nothing.
    #[props(default)]
    pub busy: Option<String>,
    /// Set while a flow is running in this repository, saying which. Every
    /// action here waits for it: deleting or pushing a branch underneath a
    /// run is the same race as two runs in one working tree.
    #[props(default)]
    pub held_by: Option<String>,
    pub on_close: EventHandler<()>,
    pub on_refresh: EventHandler<()>,
    /// `(branch, force)` — force is set for a merged branch, whose commit is
    /// squashed into the base and so never looks locally merged.
    pub on_delete: EventHandler<(String, bool)>,
    /// Deletes every merged branch named, one after another.
    pub on_delete_merged: EventHandler<Vec<String>>,
    /// Pushes the branch and opens a pull request for it — the alternative
    /// to deleting, offered wherever a branch has real work and no live PR.
    pub on_create_pr: EventHandler<String>,
    /// Closes the branch's open pull request, deletes it on origin and here —
    /// offered only for a branch that changes nothing.
    pub on_clean_up: EventHandler<String>,
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
    // One action at a time per repository: while a flow runs here, or while
    // one branch is being worked on, every other row waits too.
    let held = props.held_by.is_some() || props.busy.is_some();
    let held_title = props.held_by.clone().unwrap_or_default();

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
                    if let Some(note) = props.held_by.clone() {
                        div { class: "branches-cleanup branches-worth-pr", "{note}" }
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
                            let leftovers = list.iter().filter(|b| b.leftover()).count();
                            rsx! {
                                if leftovers > 0 {
                                    div { class: "branches-cleanup branches-worth-pr",
                                        span {
                                            "{leftovers} branch" if leftovers != 1 { "es" }
                                            " marked \u{201c}nothing new\u{201d} would change no file if merged \
                                             — their commits are old merges, or work the base branch \
                                             already has. Clean up closes any open pull request and \
                                             deletes the branch on GitHub and here."
                                        }
                                    }
                                }
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
                                            disabled: held,
                                            title: "{held_title}",
                                            onclick: {
                                                let names: Vec<String> = cleanup.iter().map(|b| b.name.clone()).collect();
                                                move |_| props.on_delete_merged.call(names.clone())
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
                                            if b.leftover() {
                                                span {
                                                    class: "branch-pr branch-pr-none",
                                                    title: "{b.ahead} commit(s) ahead, but merging them would change no file.",
                                                    "nothing new"
                                                }
                                            }
                                            if b.leftover() && !b.is_current {
                                                {
                                                    let is_busy = props.busy.as_deref() == Some(b.name.as_str());
                                                    let asking = confirming.read().as_deref() == Some(b.name.as_str());
                                                    let what = match (&b.pr_number, b.pr_state) {
                                                        (Some(n), PrState::Open) => format!("Closes #{n}, then deletes {} on GitHub and here.", b.name),
                                                        _ => format!("Deletes {} on GitHub and here.", b.name),
                                                    };
                                                    let what = if b.protected {
                                                        format!("{what} It is named like a main branch, but it is not this repository's base, and it changes nothing.")
                                                    } else {
                                                        what
                                                    };
                                                    rsx! {
                                                        button {
                                                            class: if asking { "btn btn-danger branch-delete" } else { "btn branch-delete" },
                                                            disabled: held,
                                                            title: "{what}",
                                                            onclick: {
                                                                let name = b.name.clone();
                                                                move |_| {
                                                                    if asking {
                                                                        confirming.set(None);
                                                                        props.on_clean_up.call(name.clone());
                                                                    } else {
                                                                        confirming.set(Some(name.clone()));
                                                                    }
                                                                }
                                                            },
                                                            if is_busy {
                                                                span { class: "btn-spinner" }
                                                                "Cleaning up…"
                                                            } else if asking {
                                                                "Really clean up?"
                                                            } else {
                                                                "Clean up"
                                                            }
                                                        }
                                                    }
                                                }
                                            } else if !b.is_current && !b.protected {
                                                {
                                                    let is_busy = props.busy.as_deref() == Some(b.name.as_str());
                                                    rsx! {
                                                        if b.worth_a_pr() {
                                                            button {
                                                                class: "btn btn-primary branch-create-pr",
                                                                disabled: held,
                                                                title: if is_busy {
                                                                    "Pushing and opening the pull request…".to_string()
                                                                } else {
                                                                    format!("Pushes this branch and opens a pull request from its {} commit(s).", b.ahead)
                                                                },
                                                                onclick: {
                                                                    let name = b.name.clone();
                                                                    move |_| props.on_create_pr.call(name.clone())
                                                                },
                                                                if is_busy {
                                                                    span { class: "btn-spinner" }
                                                                    "Creating…"
                                                                } else {
                                                                    "Create PR"
                                                                }
                                                            }
                                                        }
                                                        if b.pr_state.merged() {
                                                            button {
                                                                class: "btn btn-danger branch-delete",
                                                                disabled: held,
                                                                title: "Merged — safe to delete, its commit already lives on the base branch.",
                                                                onclick: {
                                                                    let name = b.name.clone();
                                                                    move |_| props.on_delete.call((name.clone(), true))
                                                                },
                                                                if is_busy {
                                                                    span { class: "btn-spinner" }
                                                                    "Deleting…"
                                                                } else {
                                                                    "Delete"
                                                                }
                                                            }
                                                        } else if confirming.read().as_deref() == Some(b.name.as_str()) {
                                                            button {
                                                                class: "btn btn-danger branch-delete",
                                                                disabled: held,
                                                                title: "This discards any commits that exist only on this branch.",
                                                                onclick: {
                                                                    let name = b.name.clone();
                                                                    move |_| {
                                                                        confirming.set(None);
                                                                        props.on_delete.call((name.clone(), false));
                                                                    }
                                                                },
                                                                if is_busy {
                                                                    span { class: "btn-spinner" }
                                                                    "Deleting…"
                                                                } else {
                                                                    "Really delete?"
                                                                }
                                                            }
                                                        } else {
                                                            button {
                                                                class: "btn branch-delete",
                                                                disabled: held,
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
    }
}
