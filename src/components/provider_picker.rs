//! Who answers a model step: a model on this computer, or api0 in one of its
//! three regions.
//!
//! Four choices, no more. There is no key to bring and no provider to name:
//! api0 runs the hosted ones on your plan's AI credits, and ollama runs the
//! local one for nothing. The same row is shown wherever that question is
//! asked — both tabs of the model window and the first-run screen — so the
//! answer looks the same everywhere it is given. What the choice then needs
//! (a model name, an address) belongs to the screen around it; this is only
//! the picking.

use dioxus::prelude::*;

use crate::services::licence;
use crate::services::llm::{self, LlmConfig, ProviderKind, Region};

/// One of the four. Which provider, not yet which model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    Ollama,
    Api0(&'static Region),
}

impl Pick {
    fn key(self) -> &'static str {
        match self {
            Pick::Ollama => "ollama",
            Pick::Api0(r) => r.id,
        }
    }
}

/// What `cfg` is set to now, read from the default model or the second one.
///
/// `None` for anything this window no longer offers: a settings file from a
/// version with a key of your own in it, or one with no model at all. Such a
/// choice keeps working — it simply has no button lit, and the screen around
/// this one says so rather than quietly moving it somewhere else.
pub fn current_pick(cfg: &LlmConfig, second: bool) -> Option<Pick> {
    let (kind, region) = if second {
        (cfg.second.kind, cfg.second.region())
    } else {
        (cfg.kind, cfg.region())
    };
    match kind {
        ProviderKind::Ollama => Some(Pick::Ollama),
        ProviderKind::Remote => region.map(Pick::Api0),
        ProviderKind::Off => None,
    }
}

#[component]
pub fn ProviderPicker(
    cfg: Signal<LlmConfig>,
    /// Writes to the second reviewer's choice rather than the default's.
    second: bool,
    offers: Signal<Option<Result<Vec<llm::RegionOffer>, String>>>,
    /// A choice changed, so a test result shown beside it is stale.
    on_change: EventHandler<()>,
    /// Opens the GitAgent Pro window, where a licence is activated. Without
    /// it — a screen that has no such window — api0 shows where a licence
    /// comes from rather than a button that would go nowhere.
    on_licence: Option<EventHandler<()>>,
) -> Element {
    let mut cfg = cfg;
    let current = cfg.read().clone();
    let pick = current_pick(&current, second);
    let has_licence = llm::key_for(llm::remote("api0")).is_some();

    let mut set_pick = move |p: Pick| {
        {
            let mut w = cfg.write();
            match (p, second) {
                (Pick::Ollama, true) => w.second.kind = ProviderKind::Ollama,
                (Pick::Ollama, false) => w.kind = ProviderKind::Ollama,
                (Pick::Api0(r), true) => w.second.use_region(r.id),
                (Pick::Api0(r), false) => w.use_region(r.id),
            }
        }
        on_change.call(());
    };

    // What a step costs in each region, once api0 has said. Without a licence
    // there is nothing to ask with, so the row says that instead of a price.
    let cost = |r: &'static Region| match offers.read().as_ref() {
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
    let offers_error = match offers.read().as_ref() {
        Some(Err(e)) if has_licence && matches!(pick, Some(Pick::Api0(_))) => Some(e.clone()),
        _ => None,
    };

    let mut choices = vec![(Pick::Ollama, "Ollama", "on this computer, free".to_string())];
    choices.extend(
        llm::REGIONS
            .iter()
            .map(|r| (Pick::Api0(r), r.label, cost(r))),
    );

    rsx! {
        div { class: "region-row",
            for (p, label, note) in choices {
                button {
                    key: "{p.key()}",
                    class: if pick == Some(p) { "region region-on" } else { "region" },
                    title: match p {
                        Pick::Ollama => "A model running on this computer".to_string(),
                        Pick::Api0(r) => format!("api0, in {}", r.label),
                    },
                    onclick: move |_| set_pick(p),
                    span { class: "region-flag",
                        match p {
                            Pick::Ollama => rsx! { "\u{1F4BB}" },
                            Pick::Api0(r) => rsx! { RegionFlag { id: r.id } },
                        }
                    }
                    span { class: "region-label", "{label}" }
                    span { class: "region-note", "{note}" }
                }
            }
        }

        // Whose service the three flags are, said once and in its own hand.
        if let Some(Pick::Api0(r)) = pick {
            div { class: "api0-line",
                Api0Mark {}
                span {
                    "api0 runs the model in {r.label} on your plan\u{2019}s AI credits. \
                     Which model runs there is api0\u{2019}s choice."
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
    }
}

/// A region's flag, drawn rather than typed: Windows has no glyphs for flag
/// emoji and shows the two letters of the country code instead.
#[component]
pub fn RegionFlag(id: &'static str) -> Element {
    let shapes = match id {
        "europe" => {
            // Twelve stars in a ring on blue.
            let stars = (0..12).map(|i| {
                let a = (i as f64) * std::f64::consts::TAU / 12.0;
                star(15.0 + 6.0 * a.sin(), 10.0 - 6.0 * a.cos(), 1.1)
            });
            rsx! {
                rect { width: "30", height: "20", fill: "#003399" }
                for (i, points) in stars.enumerate() {
                    polygon { key: "{i}", points: "{points}", fill: "#FFCC00" }
                }
            }
        }
        "china" => {
            // The flag's own 30 × 20 grid: one large star and four small.
            let small = [(10.0, 2.0), (12.0, 4.0), (12.0, 7.0), (10.0, 9.0)];
            rsx! {
                rect { width: "30", height: "20", fill: "#EE1C25" }
                polygon { points: "{star(5.0, 5.0, 3.0)}", fill: "#FFFF00" }
                for (i, (x, y)) in small.into_iter().enumerate() {
                    polygon { key: "{i}", points: "{star(x, y, 1.0)}", fill: "#FFFF00" }
                }
            }
        }
        "usa" => {
            // Thirteen stripes and a canton; at this size the fifty stars
            // read as a few dots.
            let h = 20.0 / 13.0;
            let dots = (0..4).flat_map(|row| (0..5).map(move |col| (row, col)));
            rsx! {
                rect { width: "30", height: "20", fill: "#FFFFFF" }
                for i in (0..13).step_by(2) {
                    rect { key: "{i}", y: "{i as f64 * h}", width: "30", height: "{h}", fill: "#B22234" }
                }
                rect { width: "12", height: "{7.0 * h}", fill: "#3C3B6E" }
                for (row, col) in dots {
                    circle {
                        key: "{row}-{col}",
                        cx: "{1.4 + col as f64 * 2.3}",
                        cy: "{1.5 + row as f64 * 2.6}",
                        r: "0.55",
                        fill: "#FFFFFF",
                    }
                }
            }
        }
        _ => rsx! {},
    };
    rsx! {
        svg {
            class: "region-flag-svg",
            width: "30",
            height: "20",
            view_box: "0 0 30 20",
            role: "img",
            {shapes}
        }
    }
}

/// A five-pointed star, point up, as polygon points.
fn star(cx: f64, cy: f64, r: f64) -> String {
    (0..10)
        .map(|i| {
            let a = (i as f64) * std::f64::consts::PI / 5.0;
            let len = if i % 2 == 0 { r } else { r * 0.382 };
            format!("{:.2},{:.2}", cx + len * a.sin(), cy - len * a.cos())
        })
        .collect::<Vec<_>>()
        .join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_of_the_four_choices_reads_back_as_itself() {
        let mut cfg = LlmConfig::default();
        assert_eq!(current_pick(&cfg, false), Some(Pick::Ollama));
        for r in llm::REGIONS {
            cfg.use_region(r.id);
            assert_eq!(current_pick(&cfg, false), Some(Pick::Api0(r)));
        }
    }

    #[test]
    fn a_choice_this_window_no_longer_offers_lights_nothing() {
        // A settings file from a version with keys of your own in it, and one
        // from an install that ran without a model at all. Both still run;
        // neither is one of the four.
        let mut cfg = LlmConfig {
            kind: ProviderKind::Remote,
            remote: "openai".into(),
            ..LlmConfig::default()
        };
        assert_eq!(current_pick(&cfg, false), None);
        cfg.kind = ProviderKind::Off;
        assert_eq!(current_pick(&cfg, false), None);
    }

    #[test]
    fn the_second_reviewer_is_read_from_its_own_choice() {
        let mut cfg = LlmConfig::default();
        // Off until one is chosen, and the default's model is not it.
        assert_eq!(current_pick(&cfg, true), None);
        cfg.second.use_region("china");
        assert_eq!(current_pick(&cfg, true), Some(Pick::Api0(&llm::REGIONS[1])));
        assert_eq!(current_pick(&cfg, false), Some(Pick::Ollama));
    }
}
