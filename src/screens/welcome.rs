//! Pick a workspace — one folder holding repositories, `~/code` typically.
//!
//! Individual repositories are not registered anywhere. They are rediscovered
//! every time a folder opens, so cloning something new needs no bookkeeping.

use dioxus::prelude::*;

use crate::components::settings_panel::SettingsPanel;
use crate::screens::setup::Setup;
use crate::services::llm::LlmConfig;
use crate::services::store::{self, Registry};

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

    let mut is_light = props.is_light;
    let mut theme_overridden = props.theme_overridden;

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
            open_folder(handle.path().to_string_lossy().to_string(), registry, error);
        });
    };

    let recent = registry.read().recent.clone();
    let cfg = props.llm_config.read().clone();

    if *setup_open.read() {
        return rsx! { Setup { on_close: move |_| setup_open.set(false) } };
    }

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "GitAgent" }
                p { class: "subtitle", "An agentic graph for commit and deploy" }

                div { class: "welcome-box",
                    div { class: "welcome-pick",
                        div { class: "field-row",
                            button { class: "btn btn-primary", onclick: pick, "Open folder…" }
                        }
                        div { class: "welcome-pick-note",
                            "Every git repository directly inside it becomes available."
                        }
                    }

                    if !recent.is_empty() {
                        div { class: "repo-hint", "Recent" }
                        div { class: "repo-list",
                            for path in recent.iter().cloned() {
                                div {
                                    key: "{path}",
                                    class: "repo-row",
                                    onclick: {
                                        let path = path.clone();
                                        move |_| open_folder(path.clone(), registry, error)
                                    },
                                    div { class: "repo-main",
                                        div { class: "repo-label", "{folder_name(&path)}" }
                                        div { class: "repo-path", "{path}" }
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

                if !error.read().is_empty() {
                    div { class: "welcome-error", "{error}" }
                }
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

/// Opens a folder in a new window if it holds at least one repository, and
/// remembers it. This window stays on the welcome list. A free function
/// rather than a closure: two different handlers need it.
fn open_folder(path: String, mut registry: Signal<Registry>, mut error: Signal<String>) {
    if store::discover_repos(&path).is_empty() {
        error.set(format!("No git repositories found in {path}"));
        return;
    }
    registry.write().remember(&path);
    store::save_registry(&registry.read());
    crate::open_in_new_window(path);
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}
