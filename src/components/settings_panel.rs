//! Model settings — pick local ollama or remote DeepSeek, and prove it works
//! before a run depends on it.

use dioxus::prelude::*;

use crate::services::llm::{self, LlmConfig, ProviderKind, REMOTES};
use crate::services::store;

#[derive(Props, Clone, PartialEq)]
pub struct SettingsPanelProps {
    pub llm_config: Signal<LlmConfig>,
    pub on_close: EventHandler<()>,
}

#[component]
pub fn SettingsPanel(props: SettingsPanelProps) -> Element {
    let mut cfg = props.llm_config;
    let mut probe_result = use_signal(|| Option::<Result<String, String>>::None);
    let mut probing = use_signal(|| false);

    let test = move |_| {
        let snapshot = cfg.read().clone();
        probing.set(true);
        probe_result.set(None);
        spawn(async move {
            let result = llm::probe(&snapshot).await;
            probe_result.set(Some(result));
            probing.set(false);
        });
    };

    let close = move |_| {
        store::save_settings(&cfg.read());
        props.on_close.call(());
    };

    let current = cfg.read().clone();
    let preset = current.preset();
    let key_present = current.remote_key().is_some();

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div {
                class: "modal",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),

                div { class: "modal-head",
                    span { "Model provider" }
                    button { class: "modal-close", onclick: close, "×" }
                }

                div { class: "modal-body",
                    div { class: "field-row",
                        for kind in [ProviderKind::Ollama, ProviderKind::Remote] {
                            button {
                                key: "{kind:?}",
                                class: if current.kind == kind { "seg seg-on" } else { "seg" },
                                onclick: move |_| {
                                    cfg.write().kind = kind;
                                    probe_result.set(None);
                                },
                                "{kind.label()}"
                            }
                        }
                    }

                    if current.kind == ProviderKind::Ollama {
                        label { class: "field",
                            span { "Base URL" }
                            input {
                                value: "{current.ollama_url}",
                                oninput: move |e| cfg.write().ollama_url = e.value(),
                            }
                        }
                        label { class: "field",
                            span { "Model" }
                            input {
                                value: "{current.ollama_model}",
                                oninput: move |e| cfg.write().ollama_model = e.value(),
                            }
                        }
                        label { class: "field",
                            span { "Context window" }
                            input {
                                r#type: "number",
                                value: "{current.ollama_num_ctx}",
                                oninput: move |e| {
                                    if let Ok(n) = e.value().parse::<u32>() {
                                        cfg.write().ollama_num_ctx = n;
                                    }
                                },
                            }
                        }
                        p { class: "field-note",
                            "ollama defaults to 4096 tokens whatever the model supports, which \
                             silently truncates a real diff. This value is sent with every call."
                        }
                    } else {
                        div { class: "items",
                            for entry in REMOTES.iter() {
                                label {
                                    key: "{entry.key}",
                                    class: if current.remote == entry.key { "item" } else { "item item-off" },
                                    input {
                                        r#type: "radio",
                                        name: "remote-provider",
                                        checked: current.remote == entry.key,
                                        onchange: move |_| {
                                            let mut w = cfg.write();
                                            w.remote = entry.key.to_string();
                                            // Overrides belonged to the old
                                            // provider; clearing them falls
                                            // back to this one's defaults.
                                            w.remote_url.clear();
                                            w.remote_model.clear();
                                        },
                                    }
                                    span { class: "item-label", "{entry.label}" }
                                    span {
                                        class: if llm::api_key(entry.env).is_some() {
                                            "item-note note-new"
                                        } else {
                                            "item-note note-deleted"
                                        },
                                        if llm::api_key(entry.env).is_some() { "key set" } else { "no key" }
                                    }
                                }
                            }
                        }

                        label { class: "field",
                            span { "Base URL" }
                            input {
                                value: "{current.remote_url}",
                                placeholder: "{preset.base_url}",
                                oninput: move |e| cfg.write().remote_url = e.value(),
                            }
                        }
                        label { class: "field",
                            span { "Model" }
                            input {
                                value: "{current.remote_model}",
                                placeholder: "{preset.model}",
                                oninput: move |e| cfg.write().remote_model = e.value(),
                            }
                        }
                        div { class: if key_present { "key-state key-ok" } else { "key-state key-missing" },
                            if key_present {
                                "{preset.env} found in the environment"
                            } else {
                                "{preset.env} is not set — export it and restart GitAgent"
                            }
                        }
                        p { class: "field-note",
                            "All of these speak the OpenAI wire format, so they share one client. \
                             Leave URL and model empty to use the provider's defaults, or fill \
                             them in for a proxy, a self-hosted vLLM, or a provider not listed. \
                             The key is read from the environment on every call and never written \
                             to disk."
                        }
                    }

                    div { class: "field-row",
                        button {
                            class: "btn",
                            disabled: *probing.read(),
                            onclick: test,
                            if *probing.read() { "Testing…" } else { "Test connection" }
                        }
                    }

                    match probe_result.read().clone() {
                        Some(Ok(msg)) => rsx! { div { class: "probe probe-ok", "{msg}" } },
                        Some(Err(msg)) => rsx! { div { class: "probe probe-bad", "{msg}" } },
                        None => rsx! {},
                    }
                }
            }
        }
    }
}
