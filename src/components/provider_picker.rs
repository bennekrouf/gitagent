//! Who answers a model step: api0, a local ollama, a provider you have a key
//! for, or nobody.
//!
//! One row of choices, shown wherever that question is asked — both tabs of
//! the model window and the first-run screen — so the answer looks the same
//! everywhere it is given. What each choice then needs (a model name, an
//! address, a key) belongs to the screen around it; this is only the picking.

use dioxus::prelude::*;

use crate::services::licence;
use crate::services::llm::{self, LlmConfig, ProviderKind, REMOTES};

/// The four ways a step can be answered. Which provider, not yet which model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    Api0,
    Ollama,
    OwnKey,
    Off,
}

pub fn pick_of(kind: ProviderKind, remote_key: &str) -> Pick {
    match kind {
        ProviderKind::Off => Pick::Off,
        ProviderKind::Ollama => Pick::Ollama,
        ProviderKind::Remote if llm::is_api0(llm::remote(remote_key)) => Pick::Api0,
        ProviderKind::Remote => Pick::OwnKey,
    }
}

/// What `cfg` is set to now, read from the default model or the second one.
pub fn current_pick(cfg: &LlmConfig, second: bool) -> Pick {
    if second {
        pick_of(cfg.second.kind, &cfg.second.remote)
    } else {
        pick_of(cfg.kind, &cfg.remote)
    }
}

#[component]
pub fn ProviderPicker(
    cfg: Signal<LlmConfig>,
    /// Writes to the second reviewer's choice rather than the default's.
    second: bool,
    offers: Signal<Option<Result<Vec<llm::RegionOffer>, String>>>,
    /// Whether "no model at all" is one of the choices. The first run does
    /// not offer it: an install that starts with no model has answered the
    /// one question it was asked with "neither".
    allow_off: bool,
    /// A choice changed, so a test result shown beside it is stale.
    on_change: EventHandler<()>,
    /// Opens the GitAgent Pro window, where a licence is activated. Without
    /// it — a screen that has no such window — api0 shows where a licence
    /// comes from rather than a button that would go nowhere.
    on_licence: Option<EventHandler<()>>,
) -> Element {
    let mut cfg = cfg;
    let current = cfg.read().clone();
    let (region, remote_key) = if second {
        (current.second.region(), current.second.remote.clone())
    } else {
        (current.region(), current.remote.clone())
    };
    let pick = current_pick(&current, second);
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

    let mut choices = vec![
        (Pick::Api0, "api0", "AI credits, no key"),
        (Pick::Ollama, "Ollama", "on this computer, free"),
        (Pick::OwnKey, "Your own key", "any OpenAI API"),
    ];
    if allow_off {
        choices.push(if second {
            (Pick::Off, "None", "no second review")
        } else {
            (Pick::Off, "No AI", "model steps skipped")
        });
    }
    let row_class = if choices.len() == 3 {
        "region-row region-row-3"
    } else {
        "region-row"
    };

    rsx! {
        div { class: "{row_class}",
            for (p, label, note) in choices {
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

        // Which region api0 runs it in. The flag is what every other button
        // in GitAgent then shows for this model.
        if pick == Pick::Api0 {
            div { class: "region-row region-row-3",
                for (r, note) in region_notes {
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
                if let Some(open) = on_licence {
                    div { class: "field-row",
                        button {
                            class: "btn btn-pro",
                            onclick: move |_| open.call(()),
                            "Sign in to api0\u{2026}"
                        }
                    }
                }
            }
            if let Some(e) = offers_error {
                div { class: "probe probe-bad", "{e}" }
            }
        }

        // The providers you can bring a key for. api0 is not among them: it
        // has its own choice above, and a licence rather than a key.
        if pick == Pick::OwnKey {
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
        }
    }
}

/// The mayorana mark, which is api0's too: an arch over a pillar.
#[component]
pub fn Api0Mark() -> Element {
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
