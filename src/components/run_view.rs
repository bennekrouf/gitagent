//! A run drawn as a line map, filling in as it goes.
//!
//! The list view says what each step is doing; this says where the run *is*.
//! Every dependency is a track. Once the step at its start is done, the track
//! fills toward the next one — blue while that step is under way, green once
//! it is done too, orange if it was sent back — so a run reads left to right
//! like a train working down the line, and a branch that two steps take at
//! once is two tracks filling side by side.
//!
//! The fill is a CSS animation on `stroke-dashoffset` that plays when the fill
//! is first drawn. Nothing here keeps time: a track appears the render after
//! its step finishes, animates once, and only its colour changes after that.
//!
//! A dot rides the head of each fill. While the step ahead has not finished,
//! the fill stops just short of it and the dot waits there, like a train at a
//! signal; when the step finishes, the fill closes the gap and the dot runs
//! into the station and goes out. The dot is a zero-length dash with a round
//! cap on a copy of the track, so it moves on the same `stroke-dashoffset`
//! timing as the fill and cannot drift away from its head.

use dioxus::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use crate::components::dag_view::layers_of;
use crate::services::graph::{live_subtitle, Graph, NodeKind, NodeStatus, RunState, Step};
use crate::services::llm::Lenses;
use crate::services::probe::{Checks, PrBrief};
use crate::services::review::LENSES;
use crate::services::store;
use crate::services::testprogress::{self, Tier};
mod hub;
use hub::*;
mod foot;
mod head;
mod layout;
mod map_view;
use layout::*;

/// Spacing between columns and between stacked rows. The map stretches
/// between these to fill whatever room the window gives it: below the least,
/// labels collide; past the most, a short flow drifts apart.
const COL_MIN: f64 = 190.0;
const COL_MAX: f64 = 340.0;
const ROW_MIN: f64 = 180.0;
const ROW_MAX: f64 = 320.0;
/// From the start bar to the first station, and the last station to the end
/// bar.
const LEAD: f64 = 110.0;
/// Room left and right of the bars.
const PAD_X: f64 = 40.0;
/// Room above the top station for its status, and below the bottom one for
/// its two-line name and description.
const PAD_TOP: f64 = 70.0;
/// Extra room above when a revise loop has to be drawn over the top row.
const LOOP_ROOM: f64 = 90.0;
/// Tallest a revise loop rises above the higher of its two stations.
const LOOP_MAX: f64 = 80.0;
const LOOP_MIN: f64 = 34.0;
const PAD_BOTTOM: f64 = 136.0;
/// Lines a step's description may take under its name: three, so a real
/// branch name broken across lines still reads whole.
const META_LINES: usize = 3;
/// One line of a test step's progress bars: unit, integration, and so on.
const TIER_ROW: f64 = 32.0;
/// Room under the map for the review circle and its reviewers.
const HUB_ROOM: f64 = 290.0;
/// Rows can sit closer in a flow with a review circle under them, which
/// otherwise needs more height than most windows have.
const ROW_MIN_WITH_HUB: f64 = 150.0;
const HUB_R: f64 = 42.0;
const SPOKE: f64 = 112.0;
/// How far the map zooms, out and in, and by how much a button press moves it.
const ZOOM_MIN: f64 = 0.5;
const ZOOM_MAX: f64 = 2.0;
const ZOOM_STEP: f64 = 0.1;

/// One press of − or +: to the next tenth, so a level reached by scrolling
/// lands back on round numbers.
fn zoom_step(zoom: f64, up: bool) -> f64 {
    let tenths = (zoom / ZOOM_STEP).round();
    let next = if up { tenths + 1.0 } else { tenths - 1.0 };
    (next * ZOOM_STEP).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// ⌘ + scroll, or a pinch, which reaches the page as a wheel with Ctrl held:
/// smooth rather than in steps, scrolling up zooming in.
fn zoom_wheel(zoom: f64, delta_y: f64) -> f64 {
    (zoom * (-delta_y * 0.0015).exp()).clamp(ZOOM_MIN, ZOOM_MAX)
}

/// The zoom a keyboard shortcut asks for: `in` for ⌘+, `out` for ⌘−, `reset`
/// for ⌘0 — the same keys a browser zooms with.
fn zoom_key(zoom: f64, key: &str) -> f64 {
    match key {
        "in" => zoom_step(zoom, true),
        "out" => zoom_step(zoom, false),
        "reset" => 1.0,
        _ => zoom,
    }
}

/// How long a replay spends on each change in the run's history, at 1×.
const REPLAY_STEP_MS: f64 = 700.0;
const SPEEDS: [f64; 3] = [0.5, 1.0, 2.0];
const R: f64 = 16.0;
/// Font sizes, in pixels — the map is drawn at its real size, never scaled,
/// so these are what you read.
const NAME_PX: f64 = 16.0;
const META_PX: f64 = 13.0;

#[derive(Clone, PartialEq, Debug)]
struct Station {
    id: String,
    x: f64,
    y: f64,
}

impl Map {
    fn find(&self, id: &str) -> Option<&Station> {
        self.stations.iter().find(|s| s.id == id)
    }
}

/// A step lets the run through once it is done, or once you said to go on
/// without it.
fn departed(status: NodeStatus) -> bool {
    matches!(status, NodeStatus::Done | NodeStatus::Bypassed)
}

fn curve(x1: f64, y1: f64, x2: f64, y2: f64) -> String {
    let mid = (x1 + x2) / 2.0;
    format!("M {x1} {y1} C {mid} {y1}, {mid} {y2}, {x2} {y2}")
}

/// How many characters of a monospace font fit across a label.
fn chars_across(col_w: f64, px: f64) -> usize {
    ((col_w - 24.0) / (px * 0.62)) as usize
}

/// A revise loop: an arc from a step that was sent back, over the track it
/// arrived on, to where that track starts — the way the run went back round.
#[derive(Clone, PartialEq, Debug)]
struct Loop {
    d: String,
    /// Where the label sits: the top of the arc.
    peak: (f64, f64),
    /// Where the arc lands, marked with a dot so its direction reads.
    end: (f64, f64),
}

/// What the loop's label says. `rounds` is how many times the step has been
/// sent back so far.
fn loop_label(status: NodeStatus, rounds: u32) -> String {
    let next = rounds + 1;
    match status {
        NodeStatus::Rejected if rounds <= 1 => "sent back".into(),
        NodeStatus::Rejected => format!("sent back \u{00b7} round {rounds}"),
        NodeStatus::Done => format!("approved in round {next}"),
        _ => format!("round {next}"),
    }
}

/// How wide a step's panel is folded: a strip with its name and state.
/// Must agree with `.run-drawer-folded`'s width in the stylesheet.
const FOLDED_PANEL: f64 = 46.0;

/// How far to slide the map left so a station at `x`, with labels `col_w`
/// wide, stays clear of an open panel. Only as far as that takes, and not at
/// all when it is already clear, so as much of the map stays in view as can.
fn slide_for(x: f64, col_w: f64, area_w: f64, folded: bool, chosen: Option<f64>) -> f64 {
    let visible = area_w - panel_width(area_w, folded, chosen);
    (x + col_w / 2.0 + 16.0 - visible).max(0.0)
}

/// One tick of a replay: show one more change, or go back to following the
/// run live once the replay has caught up with it.
fn advance(cursor: Option<(u64, usize)>, len: usize) -> Option<(u64, usize)> {
    let (run, at) = cursor?;
    (at + 1 < len).then_some((run, at + 1))
}

/// One reviewer around the review circle.
#[derive(Clone, PartialEq, Debug)]
struct Spoke {
    name: String,
    note: String,
    tone: Tone,
    /// The step to open when it is clicked.
    step: Option<String>,
}

fn plural(n: &str, one: &str) -> String {
    let n = if n.is_empty() { "0" } else { n };
    format!("{n} {one}{}", if n == "1" { "" } else { "s" })
}

/// The two things that have to be true before a person merges: there is a
/// pull request, and its checks pass. Shown by the end of the line, ticked or
/// not, for a flow that has a pull request in it at all.
#[derive(Clone, PartialEq, Debug)]
pub struct Gates {
    /// How to name the pull request — `#42` — once there is one.
    pub pr: Option<String>,
    pub checks: Checks,
}

impl Gates {
    /// The gates for this run, or `None` for a flow with no pull request in
    /// it — a deploy, a release — where they would be two boxes that can
    /// never be ticked.
    ///
    /// Which pull request is the run's: the one a step in it found or named,
    /// else the one picked to review, else the one open for the checked-out
    /// branch. `prs` is every open pull request, `branch_pr` the branch's.
    pub fn for_run(
        graph: &Graph,
        state: &RunState,
        picked: &str,
        prs: &[PrBrief],
        branch_pr: Option<&PrBrief>,
    ) -> Option<Gates> {
        let has_pr = graph.nodes.iter().any(|n| {
            n.writes.iter().any(|w| w == "pr_url") || n.reads.iter().any(|r| r == "pr_number")
        });
        if !has_pr {
            return None;
        }
        let number = [
            state.artifact("pr_number"),
            picked,
            state.artifact("selected_pr_number"),
        ]
        .into_iter()
        .map(str::trim)
        .find(|n| !n.is_empty())
        .unwrap_or("");
        let url = state.artifact("pr_url").trim();
        let known = || prs.iter().chain(branch_pr);
        let brief = if !number.is_empty() {
            known().find(|p| p.number == number)
        } else if !url.is_empty() {
            known().find(|p| p.url == url)
        } else {
            branch_pr
        };
        let pr = match (brief, number, url) {
            (Some(b), _, _) => Some(format!("#{}", b.number)),
            (None, n, _) if !n.is_empty() => Some(format!("#{n}")),
            // Just opened, and the repository not read again since: the URL
            // is the proof, and usually ends in the number.
            (None, _, u) if !u.is_empty() => Some(match u.rsplit('/').next() {
                Some(tail) if tail.chars().all(|c| c.is_ascii_digit()) && !tail.is_empty() => {
                    format!("#{tail}")
                }
                _ => "opened".to_string(),
            }),
            _ => None,
        };
        Some(Gates {
            checks: if pr.is_some() {
                brief.map(|b| b.checks).unwrap_or(Checks::Unknown)
            } else {
                Checks::Unknown
            },
            pr,
        })
    }
}

#[derive(Props, Clone, PartialEq)]
pub struct RunViewProps {
    pub graph: Graph,
    pub state: RunState,
    pub repo_label: String,
    pub flow_label: String,
    pub selected: String,
    pub gates: Option<Gates>,
    /// Which focused reviews are on, for the review circle's spokes.
    pub lenses: Lenses,
    /// Whether the flow can be started from here, and why not when it
    /// cannot — the list view's Start button, asked the same questions.
    pub can_start: bool,
    pub start_note: String,
    /// What starting it would do, as the list view's Start button says it —
    /// "Commit 5 files again" — shown on Play when Play would start a run.
    #[props(default)]
    pub start_label: String,
    /// Before anything has run there is nothing to replay, so Play starts
    /// the flow instead, exactly as Start does in the list view.
    pub on_start: EventHandler<()>,
    /// Whether a step's panel is open over the map. The map slides left so
    /// the selected step is not left underneath it.
    #[props(default)]
    pub panel_open: bool,
    /// Whether that panel is folded to a strip, giving the map its room back
    /// while the step stays one click away.
    #[props(default)]
    pub panel_folded: bool,
    /// The width the panel was dragged to, or `None` for its own size.
    #[props(default)]
    pub panel_width: Option<f64>,
    /// A drag of the panel's edge has ended at this width — `None` when a
    /// double-click put it back to its own size.
    #[props(default)]
    pub on_panel_width: EventHandler<Option<f64>>,
    /// The step's panel itself, drawn over the map below the header — so the
    /// play controls stay in reach while it is open.
    #[props(default)]
    pub children: Element,
    /// Why a start was refused, or where a chain stopped — the same note the
    /// list view shows, under the header. Without it a refused Play is a
    /// button that visibly does nothing.
    pub note: Element,
    /// The flow tabs, the same strip the list view shows, so the flow is
    /// chosen here too. Drawn under the header.
    pub flows: Element,
    /// What the flow is about, also the list view's: why it cannot run if it
    /// cannot, and the pull request it acts on — every open one to pick from
    /// for a review — so the one being validated is in sight here too.
    pub context: Element,
    /// The repository's own controls — Branches, Base, Trusted run — the
    /// same buttons the list view's header has.
    pub actions: Element,
    pub on_select: EventHandler<String>,
}

// A track is drawn twice: the pale rail, always, and the fill over it.
#[derive(Clone)]
struct Track {
    key: String,
    d: String,
    fill: Option<Fill>,
}

// One loop per step that has been sent back, over the track it came in
// on. With several, the one from the nearest height: that is the track
// that runs alongside, so the loop never has to cut across the map.
#[derive(Clone)]
struct Revise {
    key: String,
    shape: Loop,
    label: String,
    live: bool,
}

#[component]
pub fn RunView(props: RunViewProps) -> Element {
    // The room the canvas has, measured, so the map can spread to fill it
    // instead of scaling — scaling a wide flow down is what made the text
    // small and left the height empty.
    let mut area = use_signal(|| Option::<(f64, f64)>::None);
    // A drag of the panel's edge: where the pointer and the panel started,
    // and the width it has now.
    let mut resizing = use_signal(|| Option::<(f64, f64, f64)>::None);
    // The map's zoom, remembered between runs and restarts. The map is laid
    // out for the room it would have at this zoom, then drawn scaled to it —
    // so a big flow fits zoomed out, and zoomed in everything grows, scrolls,
    // and stays sharp, being drawn rather than magnified.
    let mut zoom = use_signal(|| store::run_zoom().clamp(ZOOM_MIN, ZOOM_MAX));
    // ⌘+ / ⌘− / ⌘0, around the middle of the view as the buttons do.
    use_future(move || async move {
        let mut keys = document::eval(ZOOM_KEYS_JS);
        while let Ok(key) = keys.recv::<String>().await {
            let now = *zoom.peek();
            let next = zoom_key(now, &key);
            if next != now {
                document::eval(&zoom_anchor_js(None));
                zoom.set(next);
            }
        }
    });
    // Each change of zoom flashes its percentage over the map for a moment:
    // the count re-keys the label, which restarts its fade. Not on opening —
    // only when the zoom actually moves.
    let mut zoom_flash = use_signal(|| 0u32);
    let last_zoom = use_hook(|| Rc::new(Cell::new(f64::NAN)));
    // Saved once it settles, not on every tick of a scroll or pinch.
    use_effect(move || {
        let level = *zoom.read();
        let before = last_zoom.replace(level);
        if !before.is_nan() && before != level {
            *zoom_flash.write() += 1;
        }
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if *zoom.peek() == level {
                store::save_run_zoom(level);
            }
        });
    });

    // The player. The run itself is never paused — it is real work — but the
    // view of it can be: frozen where it is, or wound back and replayed from
    // the run's history at a chosen pace. `cursor` is how many changes of
    // which run are shown; `None` is live, following the run as it goes.
    let mut cursor = use_signal(|| Option::<(u64, usize)>::None);
    let playing = use_signal(|| true);
    let speed = use_signal(|| 1.0f64);
    // Bumped by Restart so every track and dot is drawn afresh, not left
    // where the last pass put it.
    let epoch = use_signal(|| 0u32);
    let len = props.state.history.len();
    let this_run = props.state.run;
    let at = match *cursor.read() {
        Some((r, i)) if r == this_run && i < len => Some(i),
        _ => None,
    };
    let live = at.is_none();
    // Nothing has happened in this run yet: no history to play back.
    let fresh = len == 0 && !props.state.started;
    // Or it has all happened: every step settled, nothing left running. Then
    // too Play starts the flow — again, as the list view's Start does —
    // rather than standing pressed over a run that has nothing left to play.
    // Restart still replays the one that finished.
    let over = props.state.started
        && props
            .graph
            .nodes
            .iter()
            .all(|n| props.state.status(&n.id).is_terminal());
    let startable = fresh || (over && at.is_none());
    // Read by the timer below, which outlives any one render's props.
    let history_len = use_hook(|| Rc::new(Cell::new(0usize)));
    history_len.set(len);
    use_future({
        let history_len = history_len.clone();
        move || {
            let history_len = history_len.clone();
            async move {
                loop {
                    let wait = REPLAY_STEP_MS / *speed.peek();
                    tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
                    if !*playing.peek() {
                        continue;
                    }
                    let now = *cursor.peek();
                    if now.is_some() {
                        cursor.set(advance(now, history_len.get()));
                    }
                }
            }
        }
    });
    let shown: RunState = match at {
        Some(i) => props.state.replayed(i),
        None => props.state.clone(),
    };

    let looped = shown.started && shown.rejections.values().any(|n| *n > 0);
    // Test steps' progress, by tier, from what they have printed so far. A
    // replay has only the finished output, so it shows the bars once the
    // replay reaches the end of the step rather than all at once.
    let tiers: Vec<(String, Vec<Tier>)> = if shown.started {
        props
            .graph
            .nodes
            .iter()
            .filter(|n| n.step == Step::RunTests)
            .filter(|n| live || shown.status(&n.id).is_terminal())
            .filter_map(|n| {
                let log = &shown.runs.get(&n.id)?.log;
                let found = testprogress::parse(log);
                (!found.is_empty()).then(|| (n.id.clone(), found))
            })
            .collect()
    } else {
        vec![]
    };
    let below = tiers
        .iter()
        .map(|(_, t)| t.len() as f64 * TIER_ROW + 14.0)
        .fold(0.0, f64::max);
    let hub = review_hub(&props.graph, &shown, &props.lenses);
    let z = *zoom.read();
    let plan = map(
        &props.graph,
        (*area.read()).map(|(w, h)| (w / z, h / z)),
        looped,
        props.gates.is_some(),
        below,
        hub.is_some(),
    );
    // With a panel open, the map moves left until the step it is about is
    // clear of it, and back when it closes.
    // Worked out on screen, where the panel is: the station and its labels at
    // their zoomed size, against the panel's own width.
    let area_w = (*area.read()).map(|(w, _)| w).unwrap_or(plan.width * z);
    // A map wider than its window scrolls, and scrolling does the moving:
    // sliding it as well would add the two together. One that fits slides.
    let overflows = plan.width * z > area_w + 0.5;
    // The panel's width: as it is being dragged, else as it was left.
    let chosen = (*resizing.read()).map(|(_, _, w)| w).or(props.panel_width);
    let open_w = panel_width(area_w, false, chosen);
    let slide = if props.panel_open && !overflows {
        plan.find(&props.selected)
            .map(|s| slide_for(s.x * z, plan.col_w * z, area_w, props.panel_folded, chosen))
            .unwrap_or(0.0)
    } else {
        0.0
    };

    // On a map that does not fit across, keep the run in sight: scroll to
    // the step it is at whenever that changes — or, with a step's panel open,
    // to that step, left of the panel. A third of the way in, so the steps
    // still to come show to its right. Only when the step changes, never on a
    // zoom or a scroll of your own, so it does not fight you.
    let focus = if props.panel_open {
        Some(props.selected.clone()).filter(|id| plan.find(id).is_some())
    } else {
        progress_focus(&shown, at.unwrap_or(len))
    };
    let follow_left = focus.as_ref().and_then(|id| plan.find(id)).map(|s| {
        let visible = if props.panel_open {
            area_w - panel_width(area_w, props.panel_folded, chosen)
        } else {
            area_w
        };
        (s.x * z - visible * 0.35).max(0.0)
    });
    let follow_key = focus.map(|id| (id, props.panel_open, props.panel_folded));
    let measured = area.read().is_some();
    let followed = use_hook(|| Rc::new(RefCell::new(None::<(String, bool, bool)>)));
    use_effect(use_reactive!(|(
        follow_key,
        follow_left,
        overflows,
        measured,
    )| {
        // Not before the window has been measured: the first guess at its
        // width would scroll to the wrong place and count as followed.
        if !measured || !overflows {
            return;
        }
        if *followed.borrow() == follow_key {
            return;
        }
        *followed.borrow_mut() = follow_key.clone();
        if let Some(left) = follow_left {
            document::eval(&follow_js(left));
        }
    }));
    let name_chars = chars_across(plan.col_w, NAME_PX);
    let meta_chars = chars_across(plan.col_w, META_PX);
    let state = &shown;
    let started = state.started;
    // Part of every fill's key, so a fresh run — or a restarted replay —
    // draws its tracks from empty again instead of inheriting the last pass.
    let run = format!("{}-{}", state.run, epoch);

    let status = |id: &str| {
        if started {
            state.status(id)
        } else {
            NodeStatus::Pending
        }
    };
    let total = props.graph.nodes.len();
    let done = props
        .graph
        .nodes
        .iter()
        .filter(|n| status(&n.id) == NodeStatus::Done)
        .count();
    let waiting = props
        .graph
        .nodes
        .iter()
        .any(|n| status(&n.id) == NodeStatus::AwaitingApproval);

    let mut tracks: Vec<Track> = vec![];
    let (start_x, end_x) = (plan.start_x, plan.end_x);
    let mid_y = plan
        .stations
        .iter()
        .map(|s| s.y)
        .fold((f64::MAX, f64::MIN), |(lo, hi), y| (lo.min(y), hi.max(y)));
    let bar_y = (mid_y.0 + mid_y.1) / 2.0;

    for root in &plan.roots {
        if let Some(s) = plan.find(root) {
            let to = status(root);
            tracks.push(Track {
                key: format!("start->{root}"),
                d: curve(start_x, bar_y, s.x - R, s.y),
                fill: fill(
                    started,
                    Some(to),
                    beyond_skipped(root, &plan.edges, &status),
                ),
            });
        }
    }
    for (from, to) in &plan.edges {
        if let (Some(a), Some(b)) = (plan.find(from), plan.find(to)) {
            let target = status(to);
            tracks.push(Track {
                key: format!("{from}->{to}"),
                d: curve(a.x + R, a.y, b.x - R, b.y),
                fill: fill(
                    departed(status(from)),
                    Some(target),
                    beyond_skipped(to, &plan.edges, &status),
                ),
            });
        }
    }
    for leaf in &plan.leaves {
        if let Some(s) = plan.find(leaf) {
            tracks.push(Track {
                key: format!("{leaf}->end"),
                d: curve(s.x + R, s.y, end_x, bar_y),
                fill: fill(departed(status(leaf)), None, None),
            });
        }
    }
    let finished = started && plan.leaves.iter().all(|l| departed(status(l)));

    // Which tracks this view has drawn stopping short, by run. Only so a track
    // that was waiting can finish from where it stopped: remembered across
    // renders but not a signal, since nothing needs to re-render when it
    // changes.
    let approached = use_hook(|| Rc::new(RefCell::new(HashSet::<String>::new())));
    let phases: Vec<Option<(Fill, Phase)>> = tracks
        .iter()
        .map(|t| {
            let f = t.fill?;
            let id = format!("{run}:{}", t.key);
            let phase = Phase::of(f, approached.borrow().contains(&id));
            if phase == Phase::Approach {
                approached.borrow_mut().insert(id);
            }
            Some((f, phase))
        })
        .collect();

    let mut loops: Vec<Revise> = vec![];
    if started {
        for (id, rounds) in state.rejections.iter().filter(|(_, n)| **n > 0) {
            let (Some(spec), Some(me)) = (props.graph.get(id), plan.find(id)) else {
                continue;
            };
            let back_to = spec
                .deps
                .iter()
                .filter_map(|d| plan.find(d))
                .min_by(|a, b| (a.y - me.y).abs().total_cmp(&(b.y - me.y).abs()))
                .map(|s| (s.x, s.y))
                .unwrap_or((start_x + R, bar_y));
            let low = back_to.1.min(me.y);
            let above = plan
                .stations
                .iter()
                .filter(|s| s.x > back_to.0 + 1.0 && s.x < me.x + R && s.y < low - 1.0)
                .map(|s| s.y)
                .max_by(f64::total_cmp);
            let st = status(id);
            loops.push(Revise {
                key: format!("{id}-{rounds}"),
                shape: revise_loop((me.x, me.y), back_to, above),
                label: loop_label(st, *rounds),
                live: st == NodeStatus::Rejected,
            });
        }
    }

    rsx! {
        div {
            class: "run-view",
            // Every animation's duration is divided by this, so 2× is twice
            // as quick on screen as well as through the history.
            style: "--run-speed: {speed};",
            {head::run_head(cursor, playing, epoch, len, this_run, at, live, fresh, startable, started, total, done, waiting, finished, props.clone())}

            // Under the player, where the button that was just pressed is.
            {props.note}

            // Above the body, so a step's panel never covers the flow tabs or
            // the pull request being validated.
            {props.flows}
            div { class: "run-context", {props.context} }

            div {
                class: if resizing.read().is_some() { "run-body run-body-resizing" } else { "run-body" },
                // The panel's width, for `.run-drawer` to take.
                style: "--panel-w: {open_w}px;",
                // Listened for on the whole body, not the edge: the pointer
                // outruns a 6px grip as soon as it moves.
                onmousemove: move |e| {
                    let started = *resizing.peek();
                    if let Some((from_x, from_w, _)) = started {
                        // Leftwards widens: the panel is pinned to the right.
                        let w = store::Layout::clamp_panel(from_w + from_x - e.client_coordinates().x, area_w);
                        resizing.set(Some((from_x, from_w, w)));
                    }
                },
                onmouseup: move |_| {
                    if let Some((_, _, w)) = resizing.take() {
                        props.on_panel_width.call(Some(w));
                    }
                },
                // A pointer that leaves mid-drag would otherwise leave the
                // edge stuck to it.
                onmouseleave: move |_| {
                    if let Some((_, _, w)) = resizing.take() {
                        props.on_panel_width.call(Some(w));
                    }
                },
            div {
                class: if *playing.read() { "run-canvas" } else { "run-canvas run-paused" },
                // ⌘ + scroll, or a pinch, zooms the map; plain scrolling
                // scrolls it as before.
                onwheel: move |e: Event<WheelData>| {
                    let held = e.modifiers();
                    if held.meta() || held.ctrl() {
                        e.prevent_default();
                        let dy = e.delta().strip_units().y;
                        let now = *zoom.peek();
                        let next = zoom_wheel(now, dy);
                        if next != now {
                            // Around the cursor, not the corner: what you are
                            // pointing at stays under the pointer.
                            let at = e.client_coordinates();
                            document::eval(&zoom_anchor_js(Some((at.x, at.y))));
                            zoom.set(next);
                        }
                    }
                },
                onresize: move |e| {
                    if let Ok(size) = e.get_content_box_size() {
                        let next = Some((size.width, size.height));
                        if *area.peek() != next {
                            area.set(next);
                        }
                    }
                },
                if plan.stations.is_empty() {
                    div { class: "dag-empty", "This flow has no steps." }
                } else {
                    {map_view::run_map(live, tiers.clone(), hub.clone(), z, plan.clone(), slide, name_chars, meta_chars, state, started, run.clone(), &status, done, tracks.clone(), bar_y, start_x, end_x, finished, phases.clone(), loops.clone(), props.graph.clone(), props.gates.clone(), props.selected.clone(), props.on_select)}
                }
            }

            // Every step as a numbered pill, in the order the map reads, so
            // where the run is shows at a glance even when the map scrolls.
            {foot::run_foot(zoom, speed, z, plan.clone(), started, &status, props.graph.clone(), props.selected.clone(), props.on_select)}
            if *zoom_flash.read() > 0 {
                div {
                    key: "zoom-flash-{zoom_flash}",
                    class: "run-zoom-flash",
                    "{(z * 100.0).round()}%"
                }
            }
            // The panel's left edge, dragged to widen it as far as the whole
            // view — for reading a diff — and back down to `PANEL_MIN`.
            if props.panel_open && !props.panel_folded {
                div {
                    class: "run-drawer-grip",
                    style: "right: {open_w}px;",
                    title: "Drag to resize, double-click for the usual width",
                    onmousedown: move |e| {
                        e.prevent_default();
                        resizing.set(Some((e.client_coordinates().x, open_w, open_w)));
                    },
                    ondoubleclick: move |_| {
                        resizing.set(None);
                        props.on_panel_width.call(None);
                    },
                }
            }
            {props.children}
            }
        }
    }
}

#[cfg(test)]
mod tests;
