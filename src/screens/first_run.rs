//! The one question a new install has to ask: what, if anything, writes the
//! prose.
//!
//! Asked once, on first launch, because the honest answer is not obvious from
//! anything the app can see, and getting it wrong is expensive in both
//! directions — a person with no model waiting on calls that will never
//! succeed, or a person with ollama already running being told to paste an API
//! key. So: detect what is there, preselect it, and let the third option be as
//! ordinary as the other two.

use dioxus::prelude::*;

use crate::services::llm::{self, LlmConfig, ProviderKind, REMOTES};
use crate::services::store;

#[derive(Props, Clone, PartialEq)]
pub struct FirstRunProps {
    pub llm_config: Signal<LlmConfig>,
    /// Called once the choice is saved; the app carries on to the welcome
    /// screen.
    pub on_done: EventHandler<()>,
}

/// What the ollama probe found, if it has finished.
#[derive(Clone, PartialEq, Debug)]
enum Detected {
    Looking,
    Found(String),
    Missing,
}

#[component]
pub fn FirstRun(props: FirstRunProps) -> Element {
    let mut cfg = props.llm_config;
    let mut detected = use_signal(|| Detected::Looking);
    let mut choice = use_signal(|| ProviderKind::Off);
    let mut remote = use_signal(|| REMOTES[0].key.to_string());
    let mut key_input = use_signal(String::new);
    let mut touched = use_signal(|| false);

    // Ollama first, because if it is already running this is a one-click
    // screen. The probe is the same one the settings panel uses, so "detected"
    // here means the same thing as "reachable" there.
    use_future(move || async move {
        let probing = LlmConfig::default();
        match llm::probe(&probing).await {
            Ok(models) => {
                detected.set(Detected::Found(models));
                // Only preselect while nobody has touched the radios — a probe
                // that lands late must never move a choice already made.
                if !*touched.read() {
                    choice.set(ProviderKind::Ollama);
                }
            }
            Err(_) => detected.set(Detected::Missing),
        }
    });

    let picked = *choice.read();
    let ollama_ready = matches!(*detected.read(), Detected::Found(_));
    let can_continue = match picked {
        ProviderKind::Ollama => ollama_ready,
        ProviderKind::Remote => !key_input.read().trim().is_empty(),
        ProviderKind::Off => true,
    };

    let mut pick = move |kind: ProviderKind| {
        touched.set(true);
        choice.set(kind);
    };

    let save = move |_| {
        let mut next = cfg.read().clone();
        next.kind = picked;
        if picked == ProviderKind::Remote {
            next.remote = remote.read().clone();
            // Held for this session only. The key never reaches settings.json:
            // a secret written to a config file outlives every reason it was
            // put there, and the env var is the one place the rest of the app
            // already looks.
            let preset = llm::remote(&remote.read());
            std::env::set_var(preset.env, key_input.read().trim());
        }
        store::save_settings(&next);
        cfg.set(next);
        props.on_done.call(());
    };

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "GitAgent" }
                p { class: "subtitle",
                    "One question before you start: what writes the commit messages?"
                }

                div { class: "first-run",
                    // ── Ollama ────────────────────────────────────────────
                    div {
                        class: if picked == ProviderKind::Ollama { "choice choice-on" } else { "choice" },
                        onclick: move |_| pick(ProviderKind::Ollama),
                        div { class: "choice-head",
                            span { class: if picked == ProviderKind::Ollama { "radio radio-on" } else { "radio" } }
                            span { class: "choice-title", "Run a model on this machine" }
                            match &*detected.read() {
                                Detected::Looking => rsx! { span { class: "choice-tag", "looking…" } },
                                Detected::Found(_) => rsx! { span { class: "choice-tag choice-tag-ok", "found" } },
                                Detected::Missing => rsx! { span { class: "choice-tag choice-tag-off", "not running" } },
                            }
                        }
                        div { class: "choice-body",
                            match &*detected.read() {
                                Detected::Found(models) => rsx! {
                                    "ollama is answering at localhost:11434 — {models}"
                                },
                                Detected::Looking => rsx! { "Checking localhost:11434…" },
                                Detected::Missing => rsx! {
                                    "Nothing is answering at localhost:11434. Install ollama and \
                                     pull a model, then reopen this — or pick one of the others."
                                },
                            }
                        }
                    }

                    // ── Remote ────────────────────────────────────────────
                    div {
                        class: if picked == ProviderKind::Remote { "choice choice-on" } else { "choice" },
                        onclick: move |_| pick(ProviderKind::Remote),
                        div { class: "choice-head",
                            span { class: if picked == ProviderKind::Remote { "radio radio-on" } else { "radio" } }
                            span { class: "choice-title", "Use a hosted API" }
                        }
                        div { class: "choice-body",
                            "Your key is kept for this session only and never written to disk."
                            if picked == ProviderKind::Remote {
                                div { class: "choice-fields",
                                    select {
                                        class: "input",
                                        value: "{remote.read()}",
                                        onchange: move |e| remote.set(e.value()),
                                        for r in REMOTES.iter() {
                                            option { key: "{r.key}", value: "{r.key}", "{r.label}" }
                                        }
                                    }
                                    input {
                                        class: "input",
                                        r#type: "password",
                                        placeholder: "{llm::remote(&remote.read()).env}",
                                        value: "{key_input.read()}",
                                        oninput: move |e| key_input.set(e.value()),
                                    }
                                    div { class: "choice-note",
                                        "To keep it between launches, export "
                                        code { "{llm::remote(&remote.read()).env}" }
                                        " in your shell."
                                    }
                                }
                            }
                        }
                    }

                    // ── No AI ─────────────────────────────────────────────
                    div {
                        class: if picked == ProviderKind::Off { "choice choice-on" } else { "choice" },
                        onclick: move |_| pick(ProviderKind::Off),
                        div { class: "choice-head",
                            span { class: if picked == ProviderKind::Off { "radio radio-on" } else { "radio" } }
                            span { class: "choice-title", "No AI" }
                        }
                        div { class: "choice-body",
                            "Every step that would call a model is skipped. The flows still run: \
                             commit messages and pull request titles are written from the diff, \
                             and you see them at the approval before anything happens."
                        }
                    }
                }

                div { class: "first-run-actions",
                    button {
                        class: "btn btn-primary",
                        disabled: !can_continue,
                        onclick: save,
                        "Continue"
                    }
                    span { class: "first-run-note", "You can change this any time in Settings." }
                }
            }
        }
    }
}
