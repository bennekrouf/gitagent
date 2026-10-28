//! The bar under a run: the zoom and the playback speed.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_foot(
    mut zoom: Signal<f64>,
    mut speed: Signal<f64>,
    z: f64,
    plan: Map,
    started: bool,
    status: &dyn Fn(&str) -> NodeStatus,
    graph: Graph,
    selected: String,
    on_select: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "run-foot",
            div { class: "run-stages",
                for (n, s) in plan.stations.iter().enumerate() {
                    if let Some(spec) = graph.get(&s.id) {
                        {
                            let st = status(&s.id);
                            let id = s.id.clone();
                            let class = format!(
                                "run-stage run-stage-{}{}",
                                st.css(),
                                if s.id == selected { " run-stage-on" } else { "" },
                            );
                            rsx! {
                                button {
                                    key: "{s.id}",
                                    class: "{class}",
                                    title: "{spec.title}: {st.label()}. Click to open this step.",
                                    onclick: move |_| on_select.call(id.clone()),
                                    span { class: "run-stage-num", "{n + 1}" }
                                    span { class: "run-stage-name", "{spec.title}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "run-foot-zoom",
                span { class: "run-speeds-label", "Zoom" }
                div { class: "run-speeds",
                    button {
                        class: "run-speed",
                        title: "Zoom out (\u{2318}\u{2212}, \u{2318} + scroll down, or pinch)",
                        disabled: z <= ZOOM_MIN + 1e-9,
                        onclick: move |_| {
                            let now = *zoom.peek();
                            document::eval(&zoom_anchor_js(None));
                            zoom.set(zoom_step(now, false));
                        },
                        "\u{2212}"
                    }
                    button {
                        class: "run-speed run-zoom-level",
                        title: "Back to 100% (\u{2318}0)",
                        onclick: move |_| {
                            document::eval(&zoom_anchor_js(None));
                            zoom.set(1.0);
                        },
                        "{(z * 100.0).round()}%"
                    }
                    button {
                        class: "run-speed",
                        title: "Zoom in (\u{2318}+, \u{2318} + scroll up, or pinch)",
                        disabled: z >= ZOOM_MAX - 1e-9,
                        onclick: move |_| {
                            let now = *zoom.peek();
                            document::eval(&zoom_anchor_js(None));
                            zoom.set(zoom_step(now, true));
                        },
                        "+"
                    }
                }
            }
            div { class: "run-foot-speed",
                // Named, since three numbers alone read as anything: how
                // fast a replay steps through the run, and how fast the
                // map animates. Never how fast the run itself goes. Down
                // here rather than in the header, which they crowded: they
                // only matter while a replay is playing.
                span { class: "run-speeds-label", "Replay speed" }
                div { class: "run-speeds",
                    for v in SPEEDS {
                        button {
                            key: "{v}",
                            class: if *speed.read() == v { "run-speed run-speed-on" } else { "run-speed" },
                            title: "Replay and animate at {v}\u{d7}",
                            onclick: move |_| speed.set(v),
                            "{v}\u{d7}"
                        }
                    }
                }
            }
            span { class: "run-foot-hint",
                if started {
                    "Click a step to see it and act on it"
                } else {
                    "Press Play to start this flow and watch it fill in"
                }
            }
        }
    }
}
