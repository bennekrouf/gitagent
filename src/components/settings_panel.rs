//! Model settings — pick the model the flow's steps call, and prove it works
//! before a run depends on it.
//!
//! One picker, used twice: a model on this computer, or api0 in Europe, China
//! or the United States. Four choices and no fifth — no key to bring, no
//! provider to name, and no way to end up with no model at all. The second
//! opinion chooses from exactly the same row, so the two tabs read the same
//! way; whether it reviews at all is a switch of its own above that row.
//!
//! Two tabs, one per flow step that chooses its own model: the default every
//! model step uses, and the second opinion that reviews a pull request again.
//! Settings that belong to the default model — its focused reviews — sit with
//! it rather than in a tab of their own.

use dioxus::prelude::*;

use crate::components::provider_picker::{current_pick, Pick, ProviderPicker};
use crate::services::llm::{self, LlmConfig, ProviderKind};
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
                        // for a second opinion. Off unless switched on: it is
                        // one more model call per review, and the row below
                        // offers no "none" of its own.
                        label { class: "lens-row",
                            input {
                                r#type: "checkbox",
                                checked: second.kind != ProviderKind::Off,
                                onchange: move |e| {
                                    {
                                        let mut w = cfg.write();
                                        if e.checked() {
                                            // Somewhere other than the default,
                                            // since a second pair of eyes that
                                            // is the same model is not one.
                                            let elsewhere = match w.region() {
                                                Some(r) if r.id == llm::REGIONS[0].id => llm::REGIONS[1],
                                                _ => llm::REGIONS[0],
                                            };
                                            w.second.use_region(elsewhere.id);
                                        } else {
                                            w.second.kind = ProviderKind::Off;
                                        }
                                    }
                                    second_probe.set(None);
                                },
                            }
                            div { class: "lens-text",
                                div { class: "lens-name", "Review every pull request a second time" }
                                div { class: "lens-about",
                                    "A second model reads the same diff beside the first review."
                                }
                            }
                        }

                        if second.kind != ProviderKind::Off {
                            ProviderFields {
                                cfg,
                                second: true,
                                installed,
                                offers,
                                on_change: move |_| second_probe.set(None),
                                on_licence,
                            }

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

/// The picker and whatever the chosen provider then needs — for the default
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
    let pick = current_pick(&current, second);

    // The model the second reviewer falls back to when its own field is left
    // empty: whatever the default is set to.
    let ollama_model = if second {
        current.second.ollama_model.clone()
    } else {
        current.ollama_model.clone()
    };

    rsx! {
        ProviderPicker {
            cfg,
            second,
            offers,
            on_change,
            on_licence,
        }

        match pick {
            Some(Pick::Api0(_)) => rsx! {
                p { class: "field-note",
                    "A step is one model call \u{2014} a commit message, a pull request \
                     description, one review \u{2014} of about {llm::TYPICAL_STEP_TOKENS} \
                     tokens; a large diff costs more, and each focused review is one more \
                     step. Test connection shows the credits left."
                }
            },

            Some(Pick::Ollama) => rsx! {
                if !second {
                    label { class: "field",
                        span { "Base URL" }
                        input {
                            autocapitalize: "off",
                            autocomplete: "off",
                            spellcheck: "false",
                            "autocorrect": "off",
                            value: "{current.ollama_url}",
                            oninput: move |e| cfg.write().ollama_url = e.value(),
                        }
                    }
                }
                label { class: "field",
                    span { "Model" }
                    input {
                        autocapitalize: "off",
                        autocomplete: "off",
                        spellcheck: "false",
                        "autocorrect": "off",
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

            // A settings file from a version that let you name a provider of
            // your own, or run with no model at all. It still runs as it is;
            // none of the four above is lit, and one click replaces it.
            None => rsx! {
                div { class: "key-state key-missing",
                    if current.uses_model() {
                        "Set to {current.active_model()}, which this window no longer offers. \
                         Pick one of the four above to move it."
                    } else {
                        "Set to run with no model at all, which this window no longer offers. \
                         Every model step is skipped until you pick one of the four above."
                    }
                }
            },
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
