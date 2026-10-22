//! Model settings — pick the model the flow's steps call, and prove it works
//! before a run depends on it.
//!
//! The default model is one click among api0's three regions (Europe, China,
//! the United States) and the local ollama, each region with what a typical
//! step costs in credits. Which model api0 runs in a region is not shown — it
//! is api0's to choose. A provider with a key of your own is one more click.
//!
//! Two tabs, one per flow step that chooses its own model: the default every
//! model step uses, and the second opinion that reviews a pull request again.
//! Settings that belong to the default model — its focused reviews — sit with
//! it rather than in a tab of their own.

use dioxus::prelude::*;

use crate::services::licence;
use crate::services::llm::{self, LlmConfig, ProviderKind, REMOTES};
use crate::services::review::LENSES;
use crate::services::store;
use crate::telemetry;

#[derive(Props, Clone, PartialEq)]
pub struct SettingsPanelProps {
    pub llm_config: Signal<LlmConfig>,
    pub on_close: EventHandler<()>,
}

/// Which step's model the panel is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Default,
    Second,
}

#[component]
pub fn SettingsPanel(props: SettingsPanelProps) -> Element {
    let mut cfg = props.llm_config;
    let mut tab = use_signal(|| Tab::Default);
    let mut probe_result = use_signal(|| Option::<Result<String, String>>::None);
    let mut probing = use_signal(|| false);
    // The models this ollama has, read again only when its address changes —
    // not on every keystroke in the model field.
    let ollama_url = use_memo(move || cfg.read().ollama_url.clone());
    let mut installed = use_signal(|| Option::<Result<Vec<llm::LocalModel>, String>>::None);
    use_effect(move || {
        let url = ollama_url();
        spawn(async move {
            installed.set(Some(llm::local_models(&url).await));
        });
    });
    // What each api0 region costs, asked once when the panel opens.
    let mut offers = use_signal(|| Option::<Result<Vec<llm::RegionOffer>, String>>::None);
    use_effect(move || {
        spawn(async move {
            offers.set(Some(llm::api0_regions().await));
        });
    });
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
    let key_source = llm::key_source(preset);
    let second = current.second.clone();
    let second_cfg = current.second_config();
    let showing = *tab.read();
    let has_licence = llm::key_for(llm::remote("api0")).is_some();
    let region_notes: Vec<(&'static llm::Region, String)> = llm::REGIONS
        .iter()
        .map(|r| {
            let note = match offers.read().as_ref() {
                _ if !has_licence => "needs a licence".to_string(),
                None => "\u{2026}".to_string(),
                Some(Err(_)) => "unavailable".to_string(),
                Some(Ok(list)) => match list.iter().find(|o| o.id == r.id) {
                    Some(o) => {
                        let n = o.credits_per_step();
                        format!("\u{2248} {n} {} a step", if n == 1 { "credit" } else { "credits" })
                    }
                    None => "not offered".to_string(),
                },
            };
            (r, note)
        })
        .collect();
    let region = current.region();
    let own_key = current.kind == ProviderKind::Remote && region.is_none();
    let offers_error = match offers.read().as_ref() {
        Some(Err(e)) if has_licence => Some(e.clone()),
        _ => None,
    };
    // Each tab says which model it is set to, so the one you are not looking
    // at is not a mystery — above all whether the second reviewer is on.
    let notes = [
        (Tab::Default, "Default", current.active_model().to_string()),
        (
            Tab::Second,
            "Second reviewer",
            if second_cfg.uses_model() {
                second_cfg.active_model().to_string()
            } else {
                "off".to_string()
            },
        ),
    ];

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div {
                class: "modal",
                onclick: move |e: Event<MouseData>| e.stop_propagation(),

                div { class: "modal-head",
                    span { "Model provider" }
                    button { class: "modal-close", onclick: close, "×" }
                }

                div { class: "modal-tabs",
                    for (which, label, note) in notes {
                        button {
                            key: "{which:?}",
                            class: if showing == which { "modal-tab modal-tab-on" } else { "modal-tab" },
                            onclick: move |_| tab.set(which),
                            span { class: "modal-tab-label", "{label}" }
                            span { class: "modal-tab-note", "{note}" }
                        }
                    }
                }

                div { class: "modal-body",
                    if showing == Tab::Default {
                        div { class: "region-row",
                            for (r, note) in region_notes {
                                button {
                                    key: "{r.id}",
                                    class: if region == Some(r) { "region region-on" } else { "region" },
                                    title: "api0, in {r.label}",
                                    onclick: move |_| {
                                        cfg.write().use_region(r.id);
                                        probe_result.set(None);
                                    },
                                    span { class: "region-flag", "{r.flag}" }
                                    span { class: "region-label", "{r.label}" }
                                    span { class: "region-note", "{note}" }
                                }
                            }
                            button {
                                class: if current.kind == ProviderKind::Ollama { "region region-on" } else { "region" },
                                title: "A model running on this computer",
                                onclick: move |_| {
                                    cfg.write().kind = ProviderKind::Ollama;
                                    probe_result.set(None);
                                },
                                span { class: "region-flag", "\u{1F4BB}" }
                                span { class: "region-label", "Ollama" }
                                span { class: "region-note", "on this computer, free" }
                            }
                        }
                        div { class: "field-row",
                            button {
                                class: if own_key { "seg seg-on" } else { "seg" },
                                onclick: move |_| {
                                    cfg.write().use_own_key();
                                    probe_result.set(None);
                                },
                                "your own API key"
                            }
                        }

                        if let Some(r) = region {
                            div { class: if has_licence { "key-state key-ok" } else { "key-state key-missing" },
                                if has_licence {
                                    "Uses the licence saved on this computer"
                                } else {
                                    "api0 needs a licence. Get free AI credits by email under Get Pro\u{2026} in \
                                     the top bar, or a plan for more: "
                                    a { href: "{licence::PRICING_URL}", target: "_blank", "see the plans" }
                                    "."
                                }
                            }
                            if let Some(e) = offers_error.clone() {
                                div { class: "probe probe-bad", "{e}" }
                            }
                            p { class: "field-note",
                                "api0 runs the model in {r.label} and takes AI credits from your plan each \
                                 month. Which model runs there is api0's choice. A step is one model call \
                                 \u{2014} a commit message, a pull request description, one review \u{2014} \
                                 of about {llm::TYPICAL_STEP_TOKENS} tokens; a large diff costs more, and \
                                 each focused review below is one more step. Test connection shows the \
                                 credits left."
                            }
                        } else if current.kind == ProviderKind::Ollama {
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
                            // What this ollama actually has, to pick from rather
                            // than type exactly right. Typing still works, for a
                            // model about to be pulled.
                            ModelChoices {
                                installed: installed.read().clone(),
                                selected: current.ollama_model.clone(),
                                on_pick: move |name: String| {
                                    cfg.write().ollama_model = name;
                                    probe_result.set(None);
                                },
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
                                            class: if llm::key_for(entry).is_some() {
                                                "item-note note-new"
                                            } else {
                                                "item-note note-deleted"
                                            },
                                            if llm::key_for(entry).is_some() {
                                                "key set"
                                            } else if llm::is_api0(entry) {
                                                "needs a licence"
                                            } else {
                                                "no key"
                                            }
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
                            div { class: if key_source.is_some() { "key-state key-ok" } else { "key-state key-missing" },
                                match &key_source {
                                    Some(source) => rsx! { "{source}" },
                                    None if llm::is_api0(preset) => rsx! {
                                        "api0 needs a licence. Get free AI credits by email under Get Pro\u{2026} in \
                                         the top bar, or a plan for more: "
                                        a { href: "{licence::PRICING_URL}", target: "_blank", "see the plans" }
                                        ". Or export a licence number as {preset.env} and restart GitAgent."
                                    },
                                    None => rsx! { "{preset.env} is not set — export it and restart GitAgent" },
                                }
                            }
                            p { class: "field-note",
                                "All of these speak the OpenAI wire format, so they share one client. \
                                 Leave URL and model empty to use the provider's defaults, or fill \
                                 them in for a proxy, a self-hosted vLLM, or a provider not listed. \
                                 The key is read from the environment on every call and never written \
                                 to disk."
                            }
                            if llm::is_api0(preset) {
                                p { class: "field-note",
                                    "api0 runs the model for you and takes its AI credits from your \
                                     plan each month. Test connection shows how many are left."
                                }
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

                        p { class: "field-note",
                            "Every model step uses this one — the commit message, the pull request \
                             description, the review, the release notes — except the second opinion, \
                             which has its own tab."
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
                            "Each one reviews every pull request again with the default model, looking \
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
                    } else {
                        // A second model that reviews every pull request again,
                        // for a second opinion. Off unless chosen.
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
                            // Same ollama, so the same list. Left empty, the
                            // second opinion uses the default model — highlighted so.
                            ModelChoices {
                                installed: installed.read().clone(),
                                selected: if second.ollama_model.trim().is_empty() {
                                    current.ollama_model.clone()
                                } else {
                                    second.ollama_model.clone()
                                },
                                on_pick: move |name: String| {
                                    cfg.write().second.ollama_model = name;
                                    second_probe.set(None);
                                },
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
                                            if llm::key_for(entry).is_none() {
                                                if llm::is_api0(entry) { " (needs a licence)" } else { " (no key)" }
                                            }
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
                        if !current.uses_model() {
                            p { class: "field-note",
                                "The default is set to no AI, so no model runs at all and this \
                                 second opinion stays off whatever is chosen here."
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The models ollama has installed, as choices under a model field.
#[component]
fn ModelChoices(
    installed: Option<Result<Vec<llm::LocalModel>, String>>,
    selected: String,
    on_pick: EventHandler<String>,
) -> Element {
    match installed {
        None => rsx! { div { class: "field-note", "Looking for installed models\u{2026}" } },
        Some(Err(e)) => rsx! { div { class: "field-note", "Couldn't list installed models: {e}" } },
        Some(Ok(models)) if models.is_empty() => rsx! {
            div { class: "field-note",
                "No models installed yet. Pull one with "
                code { "ollama pull <name>" }
                ", then reopen Settings."
            }
        },
        Some(Ok(models)) => rsx! {
            div { class: "model-choices",
                for m in models {
                    button {
                        key: "{m.name}",
                        class: if m.name == selected { "model-choice model-choice-on" } else { "model-choice" },
                        title: "Use {m.name}",
                        onclick: {
                            let name = m.name.clone();
                            move |_| on_pick.call(name.clone())
                        },
                        span { class: "model-choice-name", "{m.name}" }
                        span { class: "model-choice-size", "{m.size_label()}" }
                    }
                }
            }
        },
    }
}
