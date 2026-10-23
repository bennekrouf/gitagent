//! The one question a new install sometimes has to ask: what writes the prose.
//!
//! Only sometimes. The question is worth asking when there is a model on this
//! machine already, because then it is a one-click screen and the alternative
//! — paying for calls that could have run locally for nothing — is a real
//! loss. Without ollama there is nothing to choose between, so the screen is
//! skipped outright: a new install settles on api0 and comes straight up. An
//! intermediate step whose answer the app can work out for itself is just a
//! door to open.

use dioxus::prelude::*;

use crate::components::provider_picker::{current_pick, Pick, ProviderPicker};
use crate::services::llm::{self, LlmConfig};
use crate::services::store;

#[derive(Props, Clone, PartialEq)]
pub struct FirstRunProps {
    pub llm_config: Signal<LlmConfig>,
    /// Called once the choice is saved; the app carries on to the welcome
    /// screen. Also called without asking anything when there is no local
    /// model to ask about.
    pub on_done: EventHandler<()>,
}

/// What the ollama probe found.
#[derive(Clone, PartialEq, Debug)]
enum Detected {
    Looking,
    /// ollama answered and has at least one model to offer.
    Ollama(Vec<llm::LocalModel>),
    /// Nothing local worth a question. The screen never appears.
    Absent,
}

/// The ollama model to run: the configured one when it is actually pulled,
/// otherwise whatever this machine does have.
///
/// Without this, a machine running ollama with some other model would be
/// offered a one-click "ready" that then failed on the first call.
fn pick_model(configured: &str, installed: &[llm::LocalModel]) -> Option<String> {
    if installed.iter().any(|m| m.name == configured) {
        return Some(configured.to_string());
    }
    installed.first().map(|m| m.name.clone())
}

#[component]
pub fn FirstRun(props: FirstRunProps) -> Element {
    let mut cfg = props.llm_config;
    let on_done = props.on_done;
    let mut detected = use_signal(|| Detected::Looking);
    let mut key_input = use_signal(String::new);
    // What each api0 region costs, for the picker. Asked once, and only ever
    // answered for a licence that is already on this computer.
    let mut offers = use_signal(|| Option::<Result<Vec<llm::RegionOffer>, String>>::None);
    use_effect(move || {
        spawn(async move {
            offers.set(Some(llm::api0_regions().await));
        });
    });

    // `local_models` rather than `probe`, deliberately: probe fails when
    // ollama is answering but the configured model is not pulled, and that
    // machine has ollama — it is exactly the one this screen is for.
    use_future(move || async move {
        let base = LlmConfig::default();
        match llm::local_models(&base.ollama_url).await {
            Ok(models) if !models.is_empty() => {
                if let Some(name) = pick_model(&base.ollama_model, &models) {
                    cfg.write().ollama_model = name;
                }
                detected.set(Detected::Ollama(models));
            }
            // Nothing local, or ollama with nothing pulled: either way there
            // is no local model to offer, so there is no question. Settle on
            // api0 — it runs on the licence, with no key to paste — and let
            // the welcome screen come up. Settings can change it any time.
            _ => {
                let mut next = cfg.read().clone();
                next.use_region(llm::REGIONS[0].id);
                store::save_settings(&next);
                cfg.set(next);
                detected.set(Detected::Absent);
                on_done.call(());
            }
        }
    });

    // Nothing is drawn until ollama is known to be there. While the probe is
    // out, and when it came back empty, the branch above has already chosen —
    // so render nothing rather than flash a card nobody has to answer.
    let installed = match &*detected.read() {
        Detected::Ollama(models) => models.clone(),
        _ => return rsx! {},
    };

    let current = cfg.read().clone();
    let pick = current_pick(&current, false);
    let preset = current.preset();
    let using = current.ollama_model.clone();
    let typed_key = !key_input.read().trim().is_empty();
    let can_continue = match pick {
        Pick::Ollama => true,
        // api0 can run on the licence already saved here; any other provider
        // needs the key that is being typed, or one in the environment.
        Pick::Api0 | Pick::OwnKey => typed_key || llm::key_for(preset).is_some(),
        // Not offered: see the module docs.
        Pick::Off => false,
    };

    let save = move |_| {
        // Held for this session only. The key never reaches settings.json: a
        // secret written to a config file outlives every reason it was put
        // there, and the env var is the one place the rest of the app already
        // looks.
        let typed = key_input.read().trim().to_string();
        if !typed.is_empty() {
            std::env::set_var(cfg.read().preset().env, typed);
        }
        store::save_settings(&cfg.read());
        on_done.call(());
    };

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "GitAgent" }
                p { class: "subtitle",
                    "One question before you start: what writes the commit messages?"
                }

                div { class: "first-run",
                    // The same row as the model window, so the choice made
                    // here is recognisably the one Settings offers later.
                    ProviderPicker {
                        cfg,
                        second: false,
                        offers,
                        allow_off: false,
                        on_change: move |_| key_input.set(String::new()),
                        on_licence: None,
                    }

                    match pick {
                        Pick::Ollama => rsx! {
                            div { class: "first-run-detail",
                                "ollama is answering at {current.ollama_url} \u{2014} {using} is ready."
                                if installed.len() > 1 {
                                    " It has {installed.len()} models; Settings picks another."
                                }
                            }
                        },
                        // api0 takes a licence number here as well as a key,
                        // for someone who has one but has not pasted it yet.
                        Pick::Api0 | Pick::OwnKey => rsx! {
                            div { class: "first-run-detail",
                                input {
                                    class: "input",
                                    r#type: "password",
                                    placeholder: "{key_placeholder(preset)}",
                                    value: "{key_input.read()}",
                                    oninput: move |e| key_input.set(e.value()),
                                }
                                div { class: "choice-note",
                                    "Kept for this session only and never written to disk. To keep "
                                    "it between launches, export "
                                    code { "{preset.env}" }
                                    " in your shell."
                                }
                            }
                        },
                        Pick::Off => rsx! {},
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

/// What the key field asks for: api0 needs nothing more when a licence is
/// already saved on this computer.
fn key_placeholder(remote: &llm::Remote) -> String {
    match llm::key_source(remote) {
        Some(source) if llm::is_api0(remote) => format!("{source} — no key needed"),
        _ => remote.env.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> llm::LocalModel {
        llm::LocalModel {
            name: name.into(),
            size: 1,
        }
    }

    #[test]
    fn the_configured_model_is_used_when_this_machine_has_it() {
        let installed = [model("llama3.2:latest"), model("qwen2.5-coder:14b")];
        assert_eq!(
            pick_model("qwen2.5-coder:14b", &installed).as_deref(),
            Some("qwen2.5-coder:14b")
        );
    }

    #[test]
    fn a_machine_with_another_model_is_offered_that_one() {
        // Otherwise this screen says "ready" and the first call fails.
        let installed = [model("llama3.2:latest")];
        assert_eq!(
            pick_model("qwen2.5-coder:14b", &installed).as_deref(),
            Some("llama3.2:latest")
        );
    }

    #[test]
    fn ollama_with_nothing_pulled_offers_nothing() {
        assert_eq!(pick_model("qwen2.5-coder:14b", &[]), None);
    }
}
