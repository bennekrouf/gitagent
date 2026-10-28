//! The run drawn as a map: its tracks, its stations and the loops back for
//! another round.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_map(
    _live: bool,
    tiers: Vec<(String, Vec<Tier>)>,
    hub: Option<Hub>,
    z: f64,
    plan: Map,
    slide: f64,
    name_chars: usize,
    meta_chars: usize,
    state: &RunState,
    started: bool,
    run: String,
    status: &dyn Fn(&str) -> NodeStatus,
    _done: usize,
    tracks: Vec<Track>,
    bar_y: f64,
    start_x: f64,
    end_x: f64,
    finished: bool,
    phases: Vec<Option<(Fill, Phase)>>,
    loops: Vec<Revise>,
    graph: Graph,
    gates: Option<Gates>,
    selected: String,
    on_select: EventHandler<String>,
) -> Element {
    rsx! {
        svg {
            class: "run-svg",
            style: "transform: translateX(-{slide}px);",
            width: "{plan.width * z}",
            height: "{plan.height * z}",
            view_box: "0 0 {plan.width} {plan.height}",

            for t in tracks.iter() {
                path { key: "rail-{t.key}", class: "run-rail", d: "{t.d}", fill: "none" }
            }
            for (t, fp) in tracks.iter().zip(phases.iter()) {
                if let Some((f, phase)) = fp {
                    path {
                        key: "fill-{run}-{t.key}",
                        class: "{f.css()} {phase.css()}",
                        d: "{t.d}",
                        fill: "none",
                        "pathLength": "1",
                    }
                }
            }
            for l in loops.iter() {
                {
                    let (px, py) = l.shape.peak;
                    let (ex, ey) = l.shape.end;
                    let w = l.label.chars().count() as f64 * 7.3 + 22.0;
                    let class = if l.live { "run-loop run-loop-live" } else { "run-loop" };
                    rsx! {
                        g { key: "loop-{run}-{l.key}", class: "{class}",
                            path { class: "run-loop-arc", d: "{l.shape.d}", fill: "none", "pathLength": "1" }
                            circle { class: "run-loop-end", cx: "{ex}", cy: "{ey}", r: "4.5" }
                            rect {
                                class: "run-loop-pill",
                                x: "{px - w / 2.0}", y: "{py - 12.0}",
                                width: "{w}", height: "24", rx: "12",
                            }
                            text { class: "run-loop-text", x: "{px}", y: "{py + 4.5}", "{l.label}" }
                        }
                    }
                }
            }
            // The heads, last of the tracks so no fill paints
            // over a dot. Same key whatever the phase: the element
            // stays, and a new phase's animation starts from where
            // the last one left it.
            for (t, fp) in tracks.iter().zip(phases.iter()) {
                if let Some((f, phase)) = fp {
                    if let Some(class) = phase.head(*f) {
                        path {
                            key: "head-{run}-{t.key}",
                            class: "{class} run-dot-{f.css_colour()}",
                            d: "{t.d}",
                            fill: "none",
                            "pathLength": "1",
                        }
                    }
                }
            }

            line {
                class: if started { "run-bar run-bar-on" } else { "run-bar" },
                x1: "{start_x}", y1: "{bar_y - 30.0}",
                x2: "{start_x}", y2: "{bar_y + 30.0}",
            }
            text { class: "run-bar-label", x: "{start_x}", y: "{bar_y - 42.0}", "start" }
            line {
                class: if finished { "run-bar run-bar-on" } else { "run-bar" },
                x1: "{end_x}", y1: "{bar_y - 30.0}",
                x2: "{end_x}", y2: "{bar_y + 30.0}",
            }
            text { class: "run-bar-label", x: "{end_x}", y: "{bar_y - 42.0}", "end" }

            // The merge gates, stacked above the end of the line
            // and right-aligned to it, as the last thing between
            // the run and a person merging.
            if let Some(g) = gates.clone() {
                {
                    let (ci_class, ci_text, ci_on) = match g.checks {
                        Checks::Passing => ("run-gate-row run-gate-ok", "CI green".to_string(), true),
                        Checks::Pending => ("run-gate-row run-gate-wait", "CI running".to_string(), false),
                        Checks::Failing => ("run-gate-row run-gate-bad", "CI failing".to_string(), false),
                        Checks::Unknown if g.pr.is_some() => ("run-gate-row run-gate-none", "no CI checks".to_string(), false),
                        Checks::Unknown => ("run-gate-row", "CI green".to_string(), false),
                    };
                    let pr_text = match &g.pr {
                        Some(n) => format!("PR ready {n}"),
                        None => "PR ready".to_string(),
                    };
                    let rows = [
                        (if g.pr.is_some() { "run-gate-row run-gate-ok" } else { "run-gate-row" }, pr_text, g.pr.is_some(), false),
                        (ci_class, ci_text, ci_on, g.checks == Checks::Failing),
                    ];
                    let box_x = end_x - 160.0;
                    rsx! {
                        g { class: "run-gates",
                            for (i, (class, text, on, bad)) in rows.into_iter().enumerate() {
                                {
                                    let y = bar_y - 108.0 + i as f64 * 26.0;
                                    rsx! {
                                        g { key: "{i}", class: "{class}",
                                            rect { class: "run-gate-box", x: "{box_x}", y: "{y - 8.0}", width: "16", height: "16", rx: "4" }
                                            if on {
                                                path {
                                                    class: "run-gate-tick",
                                                    d: "M {box_x + 3.5} {y} L {box_x + 7.0} {y + 3.5} L {box_x + 12.5} {y - 3.5}",
                                                    fill: "none",
                                                }
                                            }
                                            if bad {
                                                path {
                                                    class: "run-gate-tick",
                                                    d: "M {box_x + 4.5} {y - 3.5} L {box_x + 11.5} {y + 3.5} M {box_x + 11.5} {y - 3.5} L {box_x + 4.5} {y + 3.5}",
                                                    fill: "none",
                                                }
                                            }
                                            text { class: "run-gate-text", x: "{box_x + 24.0}", y: "{y + 4.5}", "{text}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // The review circle, under the map, tied by a dashed
            // line to the step that does the reviewing.
            if let (Some(h), Some(anchor)) = (hub.clone(), hub.as_ref().and_then(|h| {
                h.anchors
                    .iter()
                    .filter_map(|id| plan.find(id))
                    .max_by(|a, b| a.y.total_cmp(&b.y))
            })) {
                {
                    let cx = anchor.x.clamp(start_x + 190.0, (end_x - 190.0).max(start_x + 190.0));
                    let cy = plan.height - HUB_ROOM / 2.0 - 10.0;
                    // Below the step's name and description, not through them.
                    let anchor_y = anchor.y + R + 26.0 + 2.0 * NAME_PX * 1.2 + 2.0 * META_PX * 1.3 + 4.0;
                    let on_hub = h.anchor.clone();
                    rsx! {
                        g { class: "run-hub run-tone-{h.tone.css()}",
                            path {
                                class: "run-hub-link",
                                d: "M {anchor.x} {anchor_y} C {anchor.x} {cy - HUB_R - 60.0}, {cx} {anchor_y + 60.0}, {cx} {cy - HUB_R}",
                                fill: "none",
                            }
                            for (i, sp) in h.spokes.iter().enumerate() {
                                {
                                    let a = spoke_angle(i, h.spokes.len()).to_radians();
                                    let (dx, dy) = (a.cos(), a.sin());
                                    let (x, y) = (cx + dx * SPOKE, cy + dy * SPOKE);
                                    let (lx, side) = if dx < 0.0 { (x - 20.0, "end") } else { (x + 20.0, "start") };
                                    let step = sp.step.clone();
                                    rsx! {
                                        g {
                                            key: "{sp.name}",
                                            class: "run-spoke run-tone-{sp.tone.css()}",
                                            onclick: move |_| {
                                                if let Some(id) = step.clone() {
                                                    on_select.call(id);
                                                }
                                            },
                                            line {
                                                class: "run-spoke-line",
                                                x1: "{cx + dx * HUB_R}", y1: "{cy + dy * HUB_R}",
                                                x2: "{x - dx * 12.0}", y2: "{y - dy * 12.0}",
                                            }
                                            // The way back into review is a track of its
                                            // own: a gap down the middle of the orange
                                            // line leaves two rails.
                                            if sp.tone == Tone::Back {
                                                line {
                                                    class: "run-spoke-gap",
                                                    x1: "{cx + dx * HUB_R}", y1: "{cy + dy * HUB_R}",
                                                    x2: "{x - dx * 12.0}", y2: "{y - dy * 12.0}",
                                                }
                                            }
                                            circle { class: "run-sat", cx: "{x}", cy: "{y}", r: "11" }
                                            text { class: "run-sat-name", x: "{lx}", y: "{y + 1.0}", "text-anchor": "{side}", "{sp.name}" }
                                            text { class: "run-sat-note", x: "{lx}", y: "{y + 18.0}", "text-anchor": "{side}", "{sp.note}" }
                                        }
                                    }
                                }
                            }
                            g {
                                class: "run-hub-core",
                                onclick: move |_| on_select.call(on_hub.clone()),
                                circle { class: "run-hub-ring", cx: "{cx}", cy: "{cy}", r: "{HUB_R}" }
                                text { class: "run-hub-text", x: "{cx}", y: "{cy + 5.5}", "Review" }
                            }
                        }
                    }
                }
            }

            for s in plan.stations.iter() {
                if let Some(spec) = graph.get(&s.id) {
                    {
                        let st = status(&s.id);
                        let id = s.id.clone();
                        let on = s.id == selected;
                        let class = format!(
                            "run-station run-station-{}{}",
                            st.css(),
                            if on { " run-station-on" } else { "" },
                        );
                        let kind = if spec.kind == NodeKind::Model { "model" } else { "code" };
                        let name = wrap(&spec.title, name_chars, 2);
                        let described = live_subtitle(spec.step, &spec.subtitle, state);
                        let meta = wrap(&described, meta_chars, META_LINES);
                        let meta_y = s.y + R + 26.0 + name.len() as f64 * NAME_PX * 1.2 + 2.0;
                        let bars = tiers
                            .iter()
                            .find(|(id, _)| id == &s.id)
                            .map(|(_, t)| t.clone())
                            .unwrap_or_default();
                        let bars_y = meta_y + meta.len() as f64 * META_PX * 1.3 + 12.0;
                        // Name and count on one line, the bar
                        // under them across a little less than the
                        // column, so neighbours keep a gap.
                        let bars_w = (plan.col_w - 28.0).min(250.0);
                        let bars_left = s.x - bars_w / 2.0;
                        rsx! {
                            g {
                                key: "{s.id}",
                                class: "{class}",
                                onclick: move |_| on_select.call(id.clone()),
                                title { "{spec.title} — {described}\nClick to open this step." }
                                if matches!(st, NodeStatus::Running | NodeStatus::AwaitingApproval) {
                                    circle { class: "run-halo", cx: "{s.x}", cy: "{s.y}", r: "{R}" }
                                }
                                circle { class: "run-ring", cx: "{s.x}", cy: "{s.y}", r: "{R}" }
                                circle { class: "run-core", cx: "{s.x}", cy: "{s.y}", r: "{R * 0.45}" }
                                if spec.requires_approval {
                                    circle {
                                        class: "run-gate",
                                        cx: "{s.x + R * 0.85}", cy: "{s.y - R * 0.85}", r: "4.5",
                                    }
                                }
                                text { class: "run-name", x: "{s.x}", y: "{s.y + R + 26.0}",
                                    for (i, l) in name.iter().enumerate() {
                                        tspan { x: "{s.x}", dy: if i == 0 { "0" } else { "1.2em" }, "{l}" }
                                    }
                                }
                                text { class: "run-meta", x: "{s.x}", y: "{meta_y}",
                                    for (i, l) in meta.iter().enumerate() {
                                        tspan { x: "{s.x}", dy: if i == 0 { "0" } else { "1.3em" }, "{l}" }
                                    }
                                }
                                for (i, t) in bars.iter().enumerate() {
                                    {
                                        let y = bars_y + i as f64 * TIER_ROW;
                                        let filled = bars_w * t.done as f64 / t.total as f64;
                                        let class = if t.failed > 0 {
                                            "run-tier-fill run-tier-failed"
                                        } else if t.complete() {
                                            "run-tier-fill run-tier-done"
                                        } else {
                                            "run-tier-fill run-tier-going"
                                        };
                                        rsx! {
                                            g { key: "{t.kind.label()}",
                                                text { class: "run-tier-name", x: "{bars_left}", y: "{y + 4.5}", "{t.kind.label()}" }
                                                rect { class: "run-tier-rail", x: "{bars_left}", y: "{y + 10.0}", width: "{bars_w}", height: "5", rx: "2.5" }
                                                rect { class: "{class}", x: "{bars_left}", y: "{y + 10.0}", width: "{filled}", height: "5", rx: "2.5" }
                                                text { class: "run-tier-count", x: "{bars_left + bars_w}", y: "{y + 4.5}", "{t.done}/{t.total}" }
                                            }
                                        }
                                    }
                                }
                                if started && st != NodeStatus::Pending {
                                    text {
                                        class: "run-state status-{st.css()}",
                                        x: "{s.x}", y: "{s.y - R - 14.0}",
                                        "{st.label()}"
                                    }
                                } else {
                                    text {
                                        class: "run-state run-kind",
                                        x: "{s.x}", y: "{s.y - R - 14.0}",
                                        "{kind}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
