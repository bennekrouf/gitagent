//! Model settings — pick the model the flow's steps call, and prove it works
//! before a run depends on it.
//!
//! One picker, used twice: api0 (with the region it runs in), a local ollama,
//! a provider with a key of your own, or no model at all. The second opinion
//! chooses from exactly the same row, so the two tabs read the same way and
//! the long list of providers — DeepSeek, OpenAI and the rest — stays out of
//! sight until you say you have a key.
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
    /// The folder this window has open. The model is that folder's own
    /// choice, so two windows can run different ones; `None` — the welcome
    /// screen, the first run — sets the default every folder starts from.
    #[props(default)]
    pub folder: Option<String>,
    /// Opens the GitAgent Pro window, where a licence is activated. Without
    /// it — a window that has no such panel — api0 says where to find it
    /// instead of offering a button that would go nowhere.
    #[props(default)]
    pub on_licence: Option<EventHandler<()>>,
    pub on_close: EventHandler<()>,
}

/// Which step's model the panel is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Default,
    Second,
}

/// The four ways a step can be answered. One row of these, the same in both
/// tabs: which provider, not yet which model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Api0,
    Ollama,
    OwnKey,
    Off,
}

fn pick_of(kind: ProviderKind, remote_key: &str) -> Pick {
    match kind {
        ProviderKind::Off => Pick::Off,
        ProviderKind::Ollama => Pick::Ollama,
        ProviderKind::Remote if llm::is_api0(llm::remote(remote_key)) => Pick::Api0,
        ProviderKind::Remote => Pick::OwnKey,
    }
}

#[component]
pub fn SettingsPanel(props: SettingsPanelProps) -> Element {
    let mut cfg = props.llm_config;
    // In a signal so the close handler below stays `Copy`. The panel is a
    // modal: the folder cannot change under it while it is open.
    let folder = use_signal(|| props.folder.clone());
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
        store::save_settings_for(folder.read().as_deref(), &cfg.read());
        props.on_close.call(());
    };

    // Named, because the model chosen here belongs to this folder alone: the
    // window beside it is not about to change under you.
    let scope_path = props.folder.clone().unwrap_or_default();
    let scope_name = props.folder.as_deref().map(|f| {
        std::path::Path::new(f)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| f.to_string())
    });

    let current = cfg.read().clone();
    let second = current.second.clone();
    let second_cfg = current.second_config();
    let showing = *tab.read();
    let on_licence = props.on_licence;
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
                        match scope_name.clone() {
                            Some(name) => rsx! {
                                p { class: "field-note", title: "{scope_path}",
                                    "The model is this folder\u{2019}s own \u{2014} {name}. A window with "
                                    "a different folder open keeps the model it was set to, so one can "
                                    "run in China while another runs in the United States."
                                }
                            },
                            None => rsx! {
                                p { class: "field-note",
                                    "No folder is open, so this is the model a folder starts on until "
                                    "one is chosen for it."
                                }
                            },
                        }

                        ProviderFields {
                            cfg,
                            second: false,
                            installed,
                            offers,
                            on_change: move |_| probe_result.set(None),
                            on_licence,
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
                        ProviderFields {
                            cfg,
                            second: true,
                            installed,
                            offers,
                            on_change: move |_| second_probe.set(None),
                            on_licence,
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

/// The provider row and whatever the chosen one needs — for the default
/// model, or for the second opinion. One component for both, so the two tabs
/// cannot drift apart.
#[component]
fn ProviderFields(
    cfg: Signal<LlmConfig>,
    /// Writes to the second reviewer's choice rather than the default's.
    second: bool,
    installed: Signal<Option<Result<Vec<llm::LocalModel>, String>>>,
    offers: Signal<Option<Result<Vec<llm::RegionOffer>, String>>>,
    /// A choice changed, so a test result shown above it is stale.
    on_change: EventHandler<()>,
    on_licence: Option<EventHandler<()>>,
) -> Element {
    let mut cfg = cfg;
    let current = cfg.read().clone();
    let (kind, remote_key, region) = if second {
        (
            current.second.kind,
            current.second.remote.clone(),
            current.second.region(),
        )
    } else {
        (current.kind, current.remote.clone(), current.region())
    };
    let pick = pick_of(kind, &remote_key);
    let preset = llm::remote(&remote_key);
    let key_source = llm::key_source(preset);
    let has_licence = llm::key_for(llm::remote("api0")).is_some();

    let mut set_pick = move |p: Pick| {
        {
            let mut w = cfg.write();
            match p {
                Pick::Api0 => {
                    let id = if second {
                        w.second.region().map_or(llm::REGIONS[0].id, |r| r.id)
                    } else {
                        w.region().map_or(llm::REGIONS[0].id, |r| r.id)
                    };
                    if second {
                        w.second.use_region(id);
                    } else {
                        w.use_region(id);
                    }
                }
                Pick::Ollama if second => w.second.kind = ProviderKind::Ollama,
                Pick::Ollama => w.kind = ProviderKind::Ollama,
                Pick::OwnKey if second => w.second.use_own_key(),
                Pick::OwnKey => w.use_own_key(),
                Pick::Off if second => w.second.kind = ProviderKind::Off,
                Pick::Off => w.kind = ProviderKind::Off,
            }
        }
        on_change.call(());
    };

    let mut set_region = move |id: String| {
        {
            let mut w = cfg.write();
            if second {
                w.second.use_region(&id);
            } else {
                w.use_region(&id);
            }
        }
        on_change.call(());
    };

    let mut set_provider = move |key: String| {
        {
            let mut w = cfg.write();
            if second {
                w.second.remote = key;
                w.second.remote_model.clear();
            } else {
                w.remote = key;
                // Overrides belonged to the old provider; clearing them falls
                // back to this one's defaults.
                w.remote_url.clear();
                w.remote_model.clear();
            }
        }
        on_change.call(());
    };

    // What a step costs in each region, once api0 has said. Without a licence
    // there is nothing to ask with, so the row says that instead of a price.
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
                        format!(
                            "\u{2248} {n} {} a step",
                            if n == 1 { "credit" } else { "credits" }
                        )
                    }
                    None => "not offered".to_string(),
                },
            };
            (r, note)
        })
        .collect();
    let offers_error = match offers.read().as_ref() {
        Some(Err(e)) if has_licence && pick == Pick::Api0 => Some(e.clone()),
        _ => None,
    };

    // The model the second reviewer falls back to when its own field is left
    // empty: whatever the default is set to.
    let ollama_model = if second {
        current.second.ollama_model.clone()
    } else {
        current.ollama_model.clone()
    };
    let remote_model = if second {
        current.second.remote_model.clone()
    } else {
        current.remote_model.clone()
    };

    rsx! {
        div { class: "region-row",
            for (p, label, note) in [
                (Pick::Api0, "api0", "AI credits, no key"),
                (Pick::Ollama, "Ollama", "on this computer, free"),
                (Pick::OwnKey, "Your own key", "any OpenAI API"),
                (Pick::Off, if second { "None" } else { "No AI" }, if second { "no second review" } else { "model steps skipped" }),
            ] {
                button {
                    key: "{p:?}",
                    class: if pick == p { "region region-on" } else { "region" },
                    title: match p {
                        Pick::Api0 => "api0 runs the model and bills your plan's AI credits",
                        Pick::Ollama => "A model running on this computer",
                        Pick::OwnKey => "A provider you have an API key for",
                        Pick::Off => "No model call at all",
                    },
                    onclick: move |_| set_pick(p),
                    span { class: "region-flag",
                        match p {
                            Pick::Api0 => rsx! { Api0Mark {} },
                            Pick::Ollama => rsx! { "\u{1F4BB}" },
                            Pick::OwnKey => rsx! { "\u{1F511}" },
                            Pick::Off => rsx! { "\u{2300}" },
                        }
                    }
                    span { class: "region-label", "{label}" }
                    span { class: "region-note", "{note}" }
                }
            }
        }

        match pick {
            // Which region api0 runs it in. The flag is what every other
            // button in GitAgent then shows for this model.
            Pick::Api0 => rsx! {
                div { class: "region-row region-row-3",
                    for (r, note) in region_notes.clone() {
                        button {
                            key: "{r.id}",
                            class: if region == Some(r) { "region region-on" } else { "region" },
                            title: "api0, in {r.label}",
                            onclick: move |_| set_region(r.id.to_string()),
                            span { class: "region-flag", "{r.flag}" }
                            span { class: "region-label", "{r.label}" }
                            span { class: "region-note", "{note}" }
                        }
                    }
                }
                div { class: if has_licence { "key-state key-ok" } else { "key-state key-missing" },
                    if has_licence {
                        "Uses the licence saved on this computer"
                    } else {
                        "api0 needs a licence: free AI credits by email, or a plan for more \u{2014} "
                        a { href: "{licence::PRICING_URL}", target: "_blank", "see the plans" }
                        "."
                    }
                }
                if !has_licence {
                    div { class: "field-row",
                        match on_licence {
                            Some(open) => rsx! {
                                button {
                                    class: "btn btn-pro",
                                    onclick: move |_| open.call(()),
                                    "Sign in to api0\u{2026}"
                                }
                            },
                            None => rsx! {
                                p { class: "field-note",
                                    "Open a folder, then Get Pro\u{2026} in the top bar to get credits \
                                     or paste a licence number."
                                }
                            },
                        }
                    }
                }
                if let Some(e) = offers_error.clone() {
                    div { class: "probe probe-bad", "{e}" }
                }
                if let Some(r) = region {
                    p { class: "field-note",
                        "api0 runs the model in {r.label} and takes AI credits from your plan each \
                         month. Which model runs there is api0's choice. A step is one model call \
                         \u{2014} a commit message, a pull request description, one review \u{2014} \
                         of about {llm::TYPICAL_STEP_TOKENS} tokens; a large diff costs more. \
                         Test connection shows the credits left."
                    }
                }
            },

            Pick::Ollama => rsx! {
                if !second {
                    label { class: "field",
                        span { "Base URL" }
                        input {
                            value: "{current.ollama_url}",
                            oninput: move |e| cfg.write().ollama_url = e.value(),
                        }
                    }
                }
                label { class: "field",
                    span { "Model" }
                    input {
                        value: "{ollama_model}",
                        placeholder: if second { "{current.ollama_model}" } else { "" },
                        oninput: move |e| {
                            let v = e.value();
                            let mut w = cfg.write();
                            if second { w.second.ollama_model = v } else { w.ollama_model = v }
                        },
                    }
                }
                // What this ollama actually has, to pick from rather than
                // type exactly right. Typing still works, for a model about
                // to be pulled. Left empty, the second opinion runs the
                // default's model — shown here as the one in use.
                ModelChoices {
                    installed: installed.read().clone(),
                    selected: if second && ollama_model.trim().is_empty() {
                        current.ollama_model.clone()
                    } else {
                        ollama_model.clone()
                    },
                    on_pick: move |name: String| {
                        {
                            let mut w = cfg.write();
                            if second { w.second.ollama_model = name } else { w.ollama_model = name }
                        }
                        on_change.call(());
                    },
                }
                if second {
                    p { class: "field-note",
                        "The same ollama as the default model \u{2014} its address and context \
                         window are shared. Leave the model empty to review twice with the \
                         default's model."
                    }
                } else {
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
                }
            },

            // The providers you can bring a key for. api0 is not among them:
            // it has its own choice above, and a licence rather than a key.
            Pick::OwnKey => rsx! {
                div { class: "items",
                    for entry in REMOTES.iter().filter(|e| !llm::is_api0(e)) {
                        label {
                            key: "{entry.key}",
                            class: if remote_key == entry.key { "item" } else { "item item-off" },
                            input {
                                r#type: "radio",
                                name: if second { "second-provider" } else { "remote-provider" },
                                checked: remote_key == entry.key,
                                onchange: move |_| set_provider(entry.key.to_string()),
                            }
                            span { class: "item-label", "{entry.label}" }
                            span {
                                class: if llm::key_for(entry).is_some() {
                                    "item-note note-new"
                                } else {
                                    "item-note note-deleted"
                                },
                                if llm::key_for(entry).is_some() { "key set" } else { "no key" }
                            }
                        }
                    }
                }
                if !second {
                    label { class: "field",
                        span { "Base URL" }
                        input {
                            value: "{current.remote_url}",
                            placeholder: "{preset.base_url}",
                            oninput: move |e| cfg.write().remote_url = e.value(),
                        }
                    }
                }
                label { class: "field",
                    span { "Model" }
                    input {
                        value: "{remote_model}",
                        placeholder: "{preset.model}",
                        oninput: move |e| {
                            let v = e.value();
                            let mut w = cfg.write();
                            if second { w.second.remote_model = v } else { w.remote_model = v }
                        },
                    }
                }
                div { class: if key_source.is_some() { "key-state key-ok" } else { "key-state key-missing" },
                    match &key_source {
                        Some(source) => rsx! { "{source}" },
                        None => rsx! { "{preset.env} is not set — export it and restart GitAgent" },
                    }
                }
                p { class: "field-note",
                    "All of these speak the OpenAI wire format, so they share one client. Leave \
                     the model empty to use the provider's default. The key is read from the \
                     environment on every call and never written to disk."
                }
                if second {
                    p { class: "field-note",
                        "A proxy or self-hosted endpoint is set once, on the default model's tab."
                    }
                }
            },

            Pick::Off => rsx! {
                p { class: "field-note",
                    if second {
                        "One review per pull request. Nothing else changes."
                    } else {
                        "No model is called at all: every model step is skipped, and the flows \
                         still run with the graph, the approvals and the git handling."
                    }
                }
            },
        }
    }
}

/// The mayorana mark, which is api0's too: an arch over a pillar.
#[component]
fn Api0Mark() -> Element {
    rsx! {
        svg {
            class: "api0-mark",
            width: "22",
            height: "22",
            view_box: "0 0 24 24",
            role: "img",
            path {
                d: "M5.9 21.4 V 12.2 a 6.1 6.1 0 0 1 12.2 0 V 21.4",
                fill: "none",
                stroke: "#FF6B00",
                stroke_width: "3.6",
            }
            rect {
                x: "10.4",
                y: "12.6",
                width: "3.2",
                height: "8.8",
                rx: "0.4",
                fill: "#FF6B00",
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
