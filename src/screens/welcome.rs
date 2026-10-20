//! Pick a workspace — one folder holding repositories, `~/code` typically.
//!
//! Individual repositories are not registered anywhere. They are rediscovered
//! every time a folder opens, so cloning something new needs no bookkeeping.

use std::collections::BTreeMap;

use dioxus::prelude::*;

use crate::components::settings_panel::SettingsPanel;
use crate::screens::setup::Setup;
use crate::services::git;
use crate::services::licence;
use crate::services::llm::LlmConfig;
use crate::services::probe::{self, Wants};
use crate::services::store::{self, Registry};

/// A folder that was not opened, and the repositories in it that could not be
/// reached: each by name, with what git said.
type Denied = (String, Vec<(String, String)>);

#[derive(Props, Clone, PartialEq)]
pub struct WelcomeProps {
    pub llm_config: Signal<LlmConfig>,
    pub is_light: Signal<bool>,
    pub theme_overridden: Signal<bool>,
}

#[component]
pub fn Welcome(props: WelcomeProps) -> Element {
    let mut registry = use_signal(store::load_registry);
    let mut settings_open = use_signal(|| false);
    let mut setup_open = use_signal(|| false);
    let mut error = use_signal(String::new);
    // The folder whose repositories are being checked before it opens, and
    // the ones that failed.
    let opening = use_signal(|| Option::<String>::None);
    let mut denied = use_signal(|| Option::<Denied>::None);

    let mut is_light = props.is_light;
    let mut theme_overridden = props.theme_overridden;

    // What each recent folder's repositories want, so the list says which
    // folder has work waiting before you open it. Keyed by repository rather
    // than folder: `~/code` and `~/code/api0` can both be on the list, and the
    // repository between them is checked once.
    let mut folder_repos = use_signal(BTreeMap::<String, Vec<String>>::new);
    let mut repo_wants = use_signal(BTreeMap::<String, Wants>::new);
    // Bumped by Refresh, which is what makes the check below run again.
    let mut rechecks = use_signal(|| 0u32);
    let mut checking = use_signal(|| false);

    use_effect(move || {
        let recent = registry.read().recent.clone();
        // Before any Refresh, a check from the last two minutes will do —
        // reopening the home screen soon after a workspace, say. After one,
        // every repository is asked again: that is what Refresh is for.
        let fresh = *rechecks.read() > 0;
        spawn(async move {
            checking.set(true);
            use futures_util::stream::StreamExt;

            // Read, not filled: the home screen must not hand out a free
            // slot just by looking. A locked repository is not checked here
            // any more than it is in the workspace.
            let licence_now = licence::current();
            let slots = licence::load_slots();
            let mut todo: Vec<String> = vec![];
            for folder in recent {
                if folder_repos.peek().contains_key(&folder) {
                    continue;
                }
                let repos: Vec<String> = store::discover_repos(&folder)
                    .into_iter()
                    .map(|r| r.path)
                    .filter(|p| !licence::is_locked(&licence_now, &slots, p))
                    .collect();
                todo.extend(repos.iter().cloned());
                folder_repos.write().insert(folder, repos);
            }
            todo.sort();
            todo.dedup();
            todo.retain(|p| !repo_wants.peek().contains_key(p));

            futures_util::stream::iter(todo)
                .for_each_concurrent(6, |path| async move {
                    let status = if fresh {
                        probe::probe(&path).await
                    } else {
                        probe::probe_recent(&path).await
                    };
                    let wants = status.wants();
                    repo_wants.write().insert(path, wants);
                })
                .await;
            checking.set(false);
        });
    });

    // Forgets every result, so the folders are listed again — a repository
    // cloned since shows up — and every repository is read afresh.
    let refresh = move |_| {
        if *checking.peek() {
            return;
        }
        folder_repos.write().clear();
        repo_wants.write().clear();
        *rechecks.write() += 1;
    };

    // Opening always lands in a new window and leaves this one on the list —
    // the welcome screen is a launcher, not a workspace you leave.
    let pick = move |_| {
        spawn(async move {
            error.set(String::new());
            let Some(handle) = rfd::AsyncFileDialog::new()
                .set_title("Pick a folder of repositories")
                .pick_folder()
                .await
            else {
                return;
            };
            let path = handle.path().to_string_lossy().to_string();
            open_folder(path, registry, error, opening, denied).await;
        });
    };

    let recent = registry.read().recent.clone();
    let cfg = props.llm_config.read().clone();

    if *setup_open.read() {
        return rsx! { Setup {
            llm_config: props.llm_config,
            on_close: move |_| setup_open.set(false),
        } };
    }

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "GitAgent" }
                p { class: "subtitle", "An agentic graph for commit and deploy" }

                div { class: "welcome-box",
                    div { class: "welcome-pick",
                        div { class: "field-row",
                            button {
                                class: "btn btn-primary",
                                disabled: opening.read().is_some(),
                                onclick: pick,
                                "Open folder…"
                            }
                        }
                        div { class: "welcome-pick-note",
                            "Every git repository directly inside it becomes available."
                        }
                    }

                    if !recent.is_empty() {
                        div { class: "repo-hint repo-hint-row",
                            span { "Recent" }
                            button {
                                class: "btn btn-ghost repo-refresh",
                                disabled: *checking.read(),
                                title: "Check every repository in these folders again, including ones cloned since",
                                onclick: refresh,
                                if *checking.read() { "Checking\u{2026}" } else { "\u{21bb} Refresh" }
                            }
                        }
                        div { class: "repo-list",
                            for path in recent.iter().cloned() {
                                div {
                                    key: "{path}",
                                    class: "repo-row",
                                    onclick: {
                                        let path = path.clone();
                                        move |_| {
                                            if opening.peek().is_some() {
                                                return;
                                            }
                                            let path = path.clone();
                                            spawn(async move {
                                                open_folder(path, registry, error, opening, denied).await;
                                            });
                                        }
                                    },
                                    div { class: "repo-main",
                                        div { class: "repo-label", "{folder_name(&path)}" }
                                        div { class: "repo-path", "{path}" }
                                        {
                                            let tasks = folder_tasks(
                                                folder_repos.read().get(&path),
                                                &repo_wants.read(),
                                            );
                                            rsx! {
                                                div { class: "repo-tasks",
                                                    for (wants , names) in tasks.counts.iter() {
                                                        span {
                                                            key: "{wants.note()}",
                                                            class: "sidebar-note status-{wants.css()}",
                                                            title: "{names.join(\", \")}",
                                                            span { class: "note-icon", "{wants.icon()}" }
                                                            "{names.len()} {wants.note()}"
                                                        }
                                                    }
                                                    if tasks.checking {
                                                        span { class: "repo-tasks-quiet", "checking…" }
                                                    } else if tasks.counts.is_empty() && tasks.total > 0 {
                                                        span { class: "repo-tasks-quiet", "nothing to do" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    span { class: "repo-open", "Open ›" }
                                    button {
                                        class: "repo-remove",
                                        title: "Forget this folder",
                                        onclick: {
                                            let path = path.clone();
                                            move |e: Event<MouseData>| {
                                                e.stop_propagation();
                                                registry.write().forget(&path);
                                                store::save_registry(&registry.read());
                                            }
                                        },
                                        "×"
                                    }
                                }
                            }
                        }
                    }

                    div { class: "welcome-actions",
                        button {
                            class: "btn",
                            onclick: move |_| setup_open.set(true),
                            "Setup"
                        }
                        button {
                            class: "btn",
                            onclick: move |_| settings_open.set(true),
                            "Model: {cfg.active_model()}"
                        }
                        button {
                            class: "btn btn-ghost",
                            onclick: move |_| {
                                theme_overridden.set(true);
                                let now = *is_light.read();
                                is_light.set(!now);
                            },
                            if *props.is_light.read() { "Dark" } else { "Light" }
                        }
                    }
                }

                if let Some(path) = opening.read().clone() {
                    div { class: "welcome-checking",
                        "Checking access to the repositories in {folder_name(&path)}\u{2026}"
                    }
                }
                if let Some((path, failed)) = denied.read().clone() {
                    div { class: "welcome-error welcome-denied",
                        div {
                            "GitAgent cannot reach "
                            if failed.len() == 1 { "this repository" } else { "these repositories" }
                            " in {folder_name(&path)}:"
                        }
                        for (name , why) in failed.iter() {
                            div { key: "{name}", class: "welcome-denied-repo",
                                strong { "{name}" }
                                pre { "{why}" }
                            }
                        }
                        div { class: "welcome-denied-hint",
                            "Check that you are signed in and have been given access, \
                             then open the folder again."
                        }
                        div { class: "field-row",
                            button {
                                class: "btn",
                                onclick: move |_| {
                                    if let Some((path, _)) = denied.take() {
                                        launch(path, registry);
                                    }
                                },
                                "Open anyway"
                            }
                            button {
                                class: "btn btn-ghost",
                                onclick: move |_| denied.set(None),
                                "Dismiss"
                            }
                        }
                    }
                }
                if !error.read().is_empty() {
                    div { class: "welcome-error", "{error}" }
                }
                crate::feedback::FeedbackLine {}
            }
        }

        if *settings_open.read() {
            SettingsPanel {
                llm_config: props.llm_config,
                on_close: move |_| settings_open.set(false),
            }
        }
    }
}

/// Opens a folder in a new window if it holds at least one repository and
/// every one of them can be reached, and remembers it. This window stays on
/// the welcome list. A free function rather than a closure: two different
/// handlers need it.
///
/// The check comes first because a window opened on a repository we have no
/// rights to — an Azure DevOps project you were never added to, a folder owned
/// by another account — could sit blank waiting on it. Here, the ones that
/// fail are listed with git's reason, and opening anyway stays your call.
async fn open_folder(
    path: String,
    registry: Signal<Registry>,
    mut error: Signal<String>,
    mut opening: Signal<Option<String>>,
    mut denied: Signal<Option<Denied>>,
) {
    use futures_util::stream::StreamExt;

    error.set(String::new());
    denied.set(None);
    if let Err(e) = std::fs::read_dir(&path) {
        error.set(format!("GitAgent cannot read {path}: {e}"));
        return;
    }
    let repos = store::discover_repos(&path);
    if repos.is_empty() {
        error.set(format!("No git repositories found in {path}"));
        return;
    }

    opening.set(Some(path.clone()));
    let failed: Vec<(String, String)> = futures_util::stream::iter(repos)
        .map(|repo| async move {
            let result = git::check_access(&repo.path).await;
            (repo.label, result)
        })
        .buffered(6)
        .filter_map(|(label, result)| async move { result.err().map(|why| (label, why)) })
        .collect()
        .await;
    opening.set(None);

    if failed.is_empty() {
        launch(path, registry);
    } else {
        denied.set(Some((path, failed)));
    }
}

/// Remembers the folder and opens it in a new window.
fn launch(path: String, mut registry: Signal<Registry>) {
    registry.write().remember(&path);
    store::save_registry(&registry.read());
    crate::open_in_new_window(path);
}

/// One folder's waiting work, most urgent first, with the repositories behind
/// each count.
#[derive(Debug, PartialEq)]
struct FolderTasks {
    counts: Vec<(Wants, Vec<String>)>,
    total: usize,
    /// Some repositories (or the folder itself) have not been read yet.
    checking: bool,
}

fn folder_tasks(repos: Option<&Vec<String>>, wants: &BTreeMap<String, Wants>) -> FolderTasks {
    let Some(repos) = repos else {
        return FolderTasks {
            counts: vec![],
            total: 0,
            checking: true,
        };
    };
    let mut by_want: BTreeMap<Wants, Vec<String>> = BTreeMap::new();
    let mut checking = false;
    for repo in repos {
        match wants.get(repo) {
            Some(w) if w.needs_a_person() => by_want.entry(*w).or_default().push(folder_name(repo)),
            Some(_) => {}
            None => checking = true,
        }
    }
    FolderTasks {
        counts: by_want.into_iter().collect(),
        total: repos.len(),
        checking,
    }
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_tasks_counts_only_work_most_urgent_first() {
        let repos = vec!["/c/a".to_string(), "/c/b".to_string(), "/c/d".to_string()];
        let wants = BTreeMap::from([
            ("/c/a".to_string(), Wants::Release),
            ("/c/b".to_string(), Wants::Commit),
            ("/c/d".to_string(), Wants::Nothing),
        ]);
        let tasks = folder_tasks(Some(&repos), &wants);
        assert_eq!(
            tasks.counts,
            vec![
                (Wants::Commit, vec!["b".to_string()]),
                (Wants::Release, vec!["a".to_string()]),
            ]
        );
        assert!(!tasks.checking);
    }

    #[test]
    fn folder_tasks_is_checking_until_every_repo_is_read() {
        let repos = vec!["/c/a".to_string(), "/c/b".to_string()];
        let wants = BTreeMap::from([("/c/a".to_string(), Wants::Nothing)]);
        assert!(folder_tasks(Some(&repos), &wants).checking);
        assert!(folder_tasks(None, &wants).checking);
    }
}
