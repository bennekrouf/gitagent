//! The workspace's dialogs: hiding a flow, the base branch, the branches panel.

use super::*;
use crate::services::forge::Forge;
use crate::services::store::{Repo, RepoBases, RepoFlows};

#[allow(clippy::too_many_arguments)]
pub(super) fn confirm_hide_dialog(
    repos: Signal<Vec<Repo>>,
    book: Signal<FlowBook>,
    mut confirm_hide: Signal<Option<(String, String)>>,
    mut hide_flow: impl FnMut(&str, &str) + Copy + 'static,
) -> Element {
    rsx! {
        if let Some((repo, id)) = confirm_hide.read().clone() {
            {
                let label = book.read().get(&id).map(|f| f.label.clone()).unwrap_or(id.clone());
                let repo_label = repos.read().iter()
                    .find(|r| r.path == repo)
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| repo.clone());
                rsx! {
                    div { class: "modal-backdrop", onclick: move |_| confirm_hide.set(None),
                        div {
                            class: "modal",
                            onclick: move |e: Event<MouseData>| e.stop_propagation(),
                            div { class: "modal-head",
                                span { "Hide this flow?" }
                                button {
                                    class: "modal-close",
                                    onclick: move |_| confirm_hide.set(None),
                                    "×"
                                }
                            }
                            div { class: "modal-body",
                                p { class: "field-note",
                                    "\"{label}\" will no longer show as a tab for {repo_label}. \
                                     It still exists — every other repository keeps seeing it, \
                                     and you can bring it back from the picker at the end of \
                                     the tab strip."
                                }
                                div { class: "approval-actions",
                                    button {
                                        class: "btn btn-danger",
                                        onclick: {
                                            let repo = repo.clone();
                                            let id = id.clone();
                                            move |_| {
                                                hide_flow(&repo, &id);
                                                confirm_hide.set(None);
                                            }
                                        },
                                        "Hide"
                                    }
                                    button {
                                        class: "btn",
                                        onclick: move |_| confirm_hide.set(None),
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

#[allow(clippy::too_many_arguments)]
pub(super) fn base_editor_dialog(
    repos: Signal<Vec<Repo>>,
    mut repo_bases: Signal<RepoBases>,
    mut base_editor_open: Signal<Option<String>>,
    mut base_editor_value: Signal<String>,
) -> Element {
    rsx! {
        if let Some(repo) = base_editor_open.read().clone() {
            {
                let repo_label = repos.read().iter()
                    .find(|r| r.path == repo)
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| repo.clone());
                let close = move |_| base_editor_open.set(None);
                rsx! {
                    div { class: "modal-backdrop", onclick: close,
                        div {
                            class: "modal",
                            onclick: move |e: Event<MouseData>| e.stop_propagation(),
                            div { class: "modal-head",
                                span { "Base branch — {repo_label}" }
                                button { class: "modal-close", onclick: close, "×" }
                            }
                            div { class: "modal-body",
                                label { class: "field",
                                    span { "Target branch" }
                                    input {
                                        value: "{base_editor_value.read()}",
                                        placeholder: "auto-detected (origin/HEAD, then main, then master)",
                                        // A branch name is not prose: git takes
                                        // it exactly as typed, so nothing here
                                        // may capitalise, correct or complete
                                        // it. `autocorrect` is WebKit's own
                                        // rather than one dioxus knows, hence
                                        // the quoted name.
                                        autocapitalize: "off",
                                        autocomplete: "off",
                                        spellcheck: "false",
                                        "autocorrect": "off",
                                        oninput: move |e| base_editor_value.set(e.value()),
                                    }
                                }
                                p { class: "field-note",
                                    "Where this repository's pull requests go. Leave empty to let \
                                     GitAgent detect it — set this only when a repository's pull \
                                     requests target something other than its default branch, e.g. \
                                     \"develop\"."
                                }
                                div { class: "field-row",
                                    button {
                                        class: "btn",
                                        onclick: {
                                            let repo = repo.clone();
                                            move |_| {
                                                base_editor_value.set(String::new());
                                                repo_bases.write().set(&repo, "");
                                                store::save_repo_bases(&repo_bases.read());
                                                base_editor_open.set(None);
                                            }
                                        },
                                        "Clear (use auto-detection)"
                                    }
                                    button {
                                        class: "btn btn-primary",
                                        onclick: {
                                            let repo = repo.clone();
                                            move |_| {
                                                repo_bases.write().set(&repo, &base_editor_value.read());
                                                store::save_repo_bases(&repo_bases.read());
                                                base_editor_open.set(None);
                                            }
                                        },
                                        "Save"
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

#[allow(clippy::too_many_arguments)]
pub(super) fn branches_dialog(
    repos: Signal<Vec<Repo>>,
    mut selected_repo: Signal<Option<String>>,
    book: Signal<FlowBook>,
    mut selected_flow: Signal<String>,
    mut selected_pr: Signal<String>,
    mut selected_node: Signal<String>,
    running: Signal<BTreeSet<Key>>,
    repo_flows: Signal<RepoFlows>,
    mut branches_open: Signal<Option<String>>,
    mut branches_data: Signal<Option<Result<Vec<crate::services::branches::BranchInfo>, String>>>,
    repo_bases: Signal<RepoBases>,
    mut branches_action_error: Signal<Option<String>>,
    mut branches_busy: Signal<Option<(String, String)>>,
    forge_map: BTreeMap<String, Forge>,
    _flows: FlowBook,
) -> Element {
    rsx! {
        if let Some(repo) = branches_open.read().clone() {
            {
                let repo_label = repos.read().iter()
                    .find(|r| r.path == repo)
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| repo.clone());
                let forge = forge_map.get(&repo).cloned().unwrap_or(crate::services::forge::Forge::None);
                let override_base = repo_bases.read().get(&repo).map(str::to_string);
                let reload = {
                    let repo = repo.clone();
                    let forge = forge.clone();
                    let override_base = override_base.clone();
                    move || {
                        let repo = repo.clone();
                        let forge = forge.clone();
                        let override_base = override_base.clone();
                        branches_data.set(None);
                        spawn(async move {
                            let base = resolved_base(&repo, override_base).await;
                            let result = crate::services::branches::list(&repo, &forge, &base).await;
                            branches_data.set(Some(result));
                        });
                    }
                };
                // A flow running here holds the working tree; deleting or
                // pushing a branch underneath it is the same race as two flows.
                let held_by = running
                    .read()
                    .iter()
                    .find(|k| k.0 == repo)
                    .map(|k| busy_note(&book.read(), k));
                // Checked again when a click lands, not just when the panel
                // last rendered: the run may have started in between.
                let holds = {
                    let repo = repo.clone();
                    move || {
                        running.read().iter().any(|k| k.0 == repo)
                            || branches_busy.read().as_ref().is_some_and(|(r, _)| r == &repo)
                    }
                };
                rsx! {
                    BranchesPanel {
                        repo_label,
                        branches: branches_data.read().clone(),
                        action_error: branches_action_error.read().clone(),
                        busy: branches_busy
                            .read()
                            .as_ref()
                            .filter(|(r, _)| r == &repo)
                            .map(|(_, branch)| branch.clone()),
                        held_by: held_by.clone(),
                        on_close: move |_| branches_open.set(None),
                        on_refresh: {
                            let mut reload = reload.clone();
                            move |_| reload()
                        },
                        on_delete: {
                            let repo = repo.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |(branch, force): (String, bool)| {
                                if holds() {
                                    return;
                                }
                                let repo = repo.clone();
                                let mut reload = reload.clone();
                                branches_busy.set(Some((repo.clone(), branch.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    if let Err(e) = crate::services::branches::delete(&repo, &branch, force).await {
                                        branches_action_error.set(Some(format!("Couldn't delete {branch}: {e}")));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                        on_delete_merged: {
                            let repo = repo.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |names: Vec<String>| {
                                if holds() {
                                    return;
                                }
                                let repo = repo.clone();
                                let mut reload = reload.clone();
                                // One after another, as a single action: the
                                // repository stays held from the first delete
                                // to the last, and git never has two of them
                                // writing its refs at once.
                                branches_busy.set(names.first().map(|b| (repo.clone(), b.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    let mut failed = vec![];
                                    for branch in names {
                                        branches_busy.set(Some((repo.clone(), branch.clone())));
                                        if let Err(e) = crate::services::branches::delete(&repo, &branch, true).await {
                                            failed.push(format!("{branch}: {e}"));
                                        }
                                    }
                                    if !failed.is_empty() {
                                        branches_action_error.set(Some(format!("Couldn't delete {}", failed.join("; "))));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                        on_review: {
                            let repo = repo.clone();
                            move |number: String| {
                                // Whichever flow says it handles an open pull
                                // request here, whatever it is called.
                                let hidden = repo_flows.read().hidden_for(&repo);
                                let flow = book
                                    .read()
                                    .runnable_for(&hidden)
                                    .iter()
                                    .find(|f| f.answers(Need::OpenPullRequest))
                                    .map(|f| f.id.clone());
                                let Some(flow) = flow else {
                                    branches_action_error.set(Some(
                                        "No flow shown on this repository handles an open pull request. \
                                         In Setup, tick \u{201c}an open pull request\u{201d} on the review flow."
                                            .into(),
                                    ));
                                    return;
                                };
                                branches_open.set(None);
                                selected_repo.set(Some(repo.clone()));
                                selected_flow.set(flow);
                                selected_pr.set(number);
                                selected_node.set(String::new());
                            }
                        },
                        on_clean_up: {
                            let repo = repo.clone();
                            let forge = forge.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |branch: String| {
                                let repo = repo.clone();
                                let forge = forge.clone();
                                let mut reload = reload.clone();
                                let found = branches_data
                                    .read()
                                    .as_ref()
                                    .and_then(|r| r.as_ref().ok())
                                    .and_then(|list| list.iter().find(|b| b.name == branch).cloned());
                                let Some(info) = found else { return };
                                if holds() {
                                    return;
                                }
                                branches_busy.set(Some((repo.clone(), branch.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    if let Err(e) = crate::services::branches::clean_up(&repo, &forge, &info).await {
                                        branches_action_error.set(Some(format!("Couldn't clean up {branch}: {e}")));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                        on_create_pr: {
                            let repo = repo.clone();
                            let forge = forge.clone();
                            let override_base = override_base.clone();
                            let reload = reload.clone();
                            let holds = holds.clone();
                            move |branch: String| {
                                if holds() {
                                    return;
                                }
                                let repo = repo.clone();
                                let forge = forge.clone();
                                let override_base = override_base.clone();
                                let mut reload = reload.clone();
                                branches_busy.set(Some((repo.clone(), branch.clone())));
                                spawn(async move {
                                    branches_action_error.set(None);
                                    let base = resolved_base(&repo, override_base).await;
                                    if let Err(e) = crate::services::branches::create_pr(&repo, &branch, &base, &forge).await {
                                        branches_action_error.set(Some(format!("Couldn't open a pull request for {branch}: {e}")));
                                    }
                                    branches_busy.set(None);
                                    reload();
                                });
                            }
                        },
                    }
                }
            }
        }
    }
}
