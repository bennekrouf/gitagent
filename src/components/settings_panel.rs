//! Model settings — pick local ollama or remote DeepSeek, and prove it works
//! before a run depends on it.

use dioxus::prelude::*;

use crate::services::llm::{self, LlmConfig, ProviderKind, REMOTES};
use crate::services::review::LENSES;
use crate::services::store;
use crate::telemetry;

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
    let mut second_probe = use_signal(|| Option::<Result<String, String>>::None);
    let mut second_probing = use_signal(|| false);
    // Read once when the panel opens; the buttons below write straight through
    // to disk, so this only has to follow what was just clicked.
    let mut sharing = use_signal(telemetry::shared);

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

    let test_second = move |_| {
        let snapshot = cfg.read().second_config();
        second_probing.set(true);
        second_probe.set(None);
        spawn(async move {
            let result = llm::probe(&snapshot).await;
            second_probe.set(Some(result));
            second_probing.set(false);
        });
    };

    let close = move |_| {
        store::save_settings(&cfg.read());
        props.on_close.call(());
    };

    let current = cfg.read().clone();
    let preset = current.preset();
    let key_present = current.remote_key().is_some();
    let second = current.second.clone();
    let second_cfg = current.second_config();

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
                             silently truncates a real diff. This value is sent with every call, \
                             and a diff is cut to fit it — about {current.input_budget()} characters \
                             now. Lower it for faster answers on large changes, raise it to review \
                             more of them."
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

                    // A second model that reviews every pull request again,
                    // for a second opinion. Off unless chosen.
                    div { class: "field-row field-head", span { "Second reviewer" } }
                    div { class: "field-row",
                        for kind in [ProviderKind::Off, ProviderKind::Ollama, ProviderKind::Remote] {
                            button {
                                key: "second-{kind:?}",
                                class: if second.kind == kind { "seg seg-on" } else { "seg" },
                                onclick: move |_| {
                                    cfg.write().second.kind = kind;
                                    second_probe.set(None);
                                },
                                if kind == ProviderKind::Off { "none" } else { "{kind.label()}" }
                            }
                        }
                    }
                    if second.kind == ProviderKind::Ollama {
                        label { class: "field",
                            span { "Model" }
                            input {
                                value: "{second.ollama_model}",
                                placeholder: "{current.ollama_model}",
                                oninput: move |e| cfg.write().second.ollama_model = e.value(),
                            }
                        }
                    } else if second.kind == ProviderKind::Remote {
                        label { class: "field",
                            span { "Provider" }
                            select {
                                value: "{second.remote}",
                                onchange: move |e| {
                                    let mut w = cfg.write();
                                    w.second.remote = e.value();
                                    w.second.remote_model.clear();
                                    second_probe.set(None);
                                },
                                for entry in REMOTES.iter() {
                                    option {
                                        key: "{entry.key}",
                                        value: "{entry.key}",
                                        selected: second.remote == entry.key,
                                        "{entry.label}"
                                        if llm::api_key(entry.env).is_none() { " (no key)" }
                                    }
                                }
                            }
                        }
                        label { class: "field",
                            span { "Model" }
                            input {
                                value: "{second.remote_model}",
                                placeholder: "{second_cfg.preset().model}",
                                oninput: move |e| cfg.write().second.remote_model = e.value(),
                            }
                        }
                    }
                    if second.kind != ProviderKind::Off {
                        div { class: "field-row",
                            button {
                                class: "btn",
                                disabled: *second_probing.read(),
                                onclick: test_second,
                                if *second_probing.read() { "Testing…" } else { "Test second reviewer" }
                            }
                        }
                        match second_probe.read().clone() {
                            Some(Ok(msg)) => rsx! { div { class: "probe probe-ok", "{msg}" } },
                            Some(Err(msg)) => rsx! { div { class: "probe probe-bad", "{msg}" } },
                            None => rsx! {},
                        }
                    }
                    p { class: "field-note",
                        "Reviews every pull request a second time, with a different model, \
                         beside the first review. A second pair of eyes catches what one model \
                         misses, at the cost of one more model call per review. Its findings \
                         show at the merge, and stop a trusted run like the first review's do."
                    }

                    // Focused reviews, one switch each: every one is another
                    // model call per review, so each is chosen on its own.
                    div { class: "field-row field-head", span { "Focused reviews" } }
                    div { class: "lens-list",
                        for lens in LENSES {
                            label { key: "{lens.key()}", class: "lens-row",
                                input {
                                    r#type: "checkbox",
                                    checked: current.lenses.is_on(lens.key()),
                                    onchange: move |e| cfg.write().lenses.set(lens.key(), e.checked()),
                                }
                                div { class: "lens-text",
                                    div { class: "lens-name", "{lens.label()}" }
                                    div { class: "lens-about", "{lens.about()}" }
                                }
                            }
                        }
                    }
                    p { class: "field-note",
                        "Each one reviews every pull request again with the main model, looking \
                         for one thing only. One more model call per review for each, run one \
                         after another, so on a local model they add up. Their findings show at \
                         the merge, and stop a trusted run like the first review's do."
                    }

                    // Only in a build that can send anything: a switch that
                    // controls nothing would be a false promise either way.
                    if telemetry::available() {
                        div { class: "field-row",
                            span { "Usage statistics" }
                        }
                        div { class: "field-row",
                            for (on, label) in [(true, "Share"), (false, "Off")] {
                                button {
                                    key: "{label}",
                                    class: if *sharing.read() == on { "seg seg-on" } else { "seg" },
                                    onclick: move |_| {
                                        telemetry::set_consent(on);
                                        sharing.set(on);
                                    },
                                    "{label}"
                                }
                            }
                        }
                        p { class: "field-note",
                            "Anonymous: which steps ran, whether they succeeded, your operating \
                             system and GitAgent version, and GitHub or Azure DevOps. Never \
                             repository names, paths, code, commit messages, branch names or \
                             errors. It is identified only by a random number kept on this \
                             computer, not by you. Turning it off deletes anything not yet sent. \
                             Setting DISABLE_UPDATE_CHECK, DO_NOT_TRACK or GITAGENT_NO_TELEMETRY \
                             turns it off regardless."
                        }
                    }
                }
            }
        }
    }
}
