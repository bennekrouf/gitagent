//! One repository's local branches, and what happened to each one's pull
//! request — merged and closed branches pile up because git never deletes
//! one just because its PR is done, so this is where you clean them out.

use dioxus::prelude::*;

use crate::components::detail_pane::CopyButton;
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
    /// Opens Review → Merge on this pull request number.
    pub on_review: EventHandler<String>,
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

/// What a row says about its pull request.
fn state_text(b: &BranchInfo) -> String {
    let state = match &b.pr_number {
        Some(number) => format!("#{number} {}", b.pr_state.label()),
        None => b.pr_state.label().to_string(),
    };
    if b.worth_a_pr() {
        format!("{state} · {} ahead", b.ahead)
    } else {
        state
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "es"
    }
}

fn merged_branches(list: &[BranchInfo]) -> Vec<&BranchInfo> {
    list.iter()
        .filter(|b| !b.is_current && !b.protected && b.pr_state.merged())
        .collect()
}

/// The sentences above the list: leftovers, branches worth a pull request,
/// merged ones. Empty for any that does not apply.
fn notes(list: &[BranchInfo]) -> [String; 3] {
    let leftovers = list.iter().filter(|b| b.leftover()).count();
    let worth_a_pr = list.iter().filter(|b| b.worth_a_pr()).count();
    let merged = merged_branches(list).len();
    [
        (leftovers > 0).then(|| {
            format!(
                "{leftovers} branch{} marked \u{201c}nothing new\u{201d} hold nothing the base \
                 branch lacks — already merged, or commits that would change no file. Clean up \
                 deletes the branch on GitHub and here.",
                plural(leftovers)
            )
        }),
        (worth_a_pr > 0).then(|| {
            format!(
                "{worth_a_pr} branch{} have commits not on the base branch and no open pull \
                 request — decide per branch below: open a PR, or delete.",
                plural(worth_a_pr)
            )
        }),
        (merged > 0).then(|| {
            format!(
                "{merged} branch{} already merged — nothing here is only on these branches.",
                plural(merged)
            )
        }),
    ]
    .map(Option::unwrap_or_default)
}

/// Everything the panel shows, as text to paste elsewhere.
fn page_text(
    repo_label: &str,
    error: Option<&str>,
    held_by: Option<&str>,
    list: &[BranchInfo],
) -> String {
    let mut lines = vec![format!("Branches — {repo_label}")];
    lines.extend(error.into_iter().chain(held_by).map(str::to_string));
    lines.extend(notes(list).into_iter().filter(|note| !note.is_empty()));
    lines.push(String::new());
    lines.extend(list.iter().map(|b| {
        let mut row = vec![
            format!("{}{}", if b.is_current { "▸ " } else { "" }, b.name),
            state_text(b),
        ];
        if b.protected {
            row.push("protected".to_string());
        }
        if b.leftover() {
            row.push("nothing new".to_string());
        }
        row.join("\t")
    }));
    lines.join("\n")
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
                    div { class: "branches-tools",
                        button {
                            class: "btn",
                            disabled: props.branches.is_none(),
                            title: "Asks the forge what became of each branch's pull request.",
                            onclick: move |_| props.on_refresh.call(()),
                            if props.branches.is_none() {
                                span { class: "btn-spinner" }
                                "Checking…"
                            } else {
                                "Run check"
                            }
                        }
                        if let Some(Ok(list)) = props.branches.as_ref() {
                            CopyButton {
                                class: "btn",
                                label: "Copy page",
                                text: page_text(
                                    &props.repo_label,
                                    props.action_error.as_deref(),
                                    props.held_by.as_deref(),
                                    list,
                                ),
                            }
                        }
                    }
                    match props.branches.clone() {
                        None => rsx! { div { class: "branches-loading", "Checking branches…" } },
                        Some(Err(e)) => rsx! {
                            div { class: "pr-list-error", "Couldn't read branches: {e}" }
                        },
                        Some(Ok(list)) => {
                            let cleanup: Vec<BranchInfo> = merged_branches(&list).into_iter().cloned().collect();
                            let [leftover_note, worth_note, merged_note] = notes(&list);
                            rsx! {
                                if !leftover_note.is_empty() {
                                    div { class: "branches-cleanup branches-worth-pr",
                                        span { "{leftover_note}" }
                                    }
                                }
                                if !worth_note.is_empty() {
                                    div { class: "branches-cleanup branches-worth-pr",
                                        span { "{worth_note}" }
                                    }
                                }
                                if !cleanup.is_empty() {
                                    div { class: "branches-cleanup",
                                        span { "{merged_note}" }
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
                                                "{state_text(&b)}"
                                            }
                                            if b.leftover() {
                                                span {
                                                    class: "branch-pr branch-pr-none",
                                                    title: "{b.leftover_reason()}",
                                                    "nothing new"
                                                }
                                            }
                                            if let (PrState::Open, Some(number)) = (b.pr_state, b.pr_number.clone()) {
                                                button {
                                                    class: "btn btn-primary branch-create-pr",
                                                    title: "Open Review \u{2192} Merge on #{number}: CI and the model's read side by side, then merge.",
                                                    onclick: move |_| props.on_review.call(number.clone()),
                                                    "Review & merge"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, pr_state: PrState, pr_number: Option<&str>, ahead: usize) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            is_current: false,
            protected: false,
            pr_number: pr_number.map(str::to_string),
            pr_title: String::new(),
            pr_state,
            ahead,
            changes_nothing: false,
            merged_as_is: false,
            is_base: false,
        }
    }

    #[test]
    fn the_copied_page_has_the_title_the_notes_and_every_row() {
        let mut main = branch("main", PrState::Unchecked, None, 0);
        main.is_current = true;
        main.protected = true;
        main.is_base = true;
        let list = [
            main,
            branch("feature/a", PrState::None, None, 3),
            branch("feature/b", PrState::Open, Some("12"), 1),
        ];
        let text = page_text("client-repo", Some("it failed"), None, &list);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Branches — client-repo");
        assert_eq!(lines[1], "it failed");
        assert!(
            lines[2].starts_with("1 branch have commits"),
            "{}",
            lines[2]
        );
        assert!(lines.contains(&"▸ main\tnot checked\tprotected"));
        assert!(lines.contains(&"feature/a\tno PR · 3 ahead"));
        assert!(lines.contains(&"feature/b\t#12 open"));
    }

    #[component]
    fn Panel(branches: Option<Result<Vec<BranchInfo>, String>>) -> Element {
        rsx! {
            BranchesPanel {
                repo_label: "repo",
                branches,
                on_close: |_| {},
                on_refresh: |_| {},
                on_delete: |_| {},
                on_delete_merged: |_| {},
                on_create_pr: |_| {},
                on_clean_up: |_| {},
                on_review: |_| {},
            }
        }
    }

    fn render(branches: Option<Result<Vec<BranchInfo>, String>>) -> String {
        let mut dom = VirtualDom::new_with_props(Panel, PanelProps { branches });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    #[test]
    fn both_buttons_show_once_the_list_is_there() {
        let html = render(Some(Ok(vec![branch("x", PrState::Unchecked, None, 1)])));
        assert!(html.contains("Run check"));
        assert!(html.contains("Copy page"));
    }

    #[test]
    fn a_failed_read_can_be_checked_again_but_leaves_nothing_to_copy() {
        let html = render(Some(Err("nope".into())));
        assert!(html.contains("Run check"));
        assert!(!html.contains("Copy page"));
    }

    #[test]
    fn the_check_button_says_it_is_working() {
        let html = render(None);
        assert!(html.contains("Checking…"));
        assert!(!html.contains("Run check"));
    }
}
