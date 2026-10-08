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
use crate::services::graph::{Graph, NodeKind, NodeStatus, RunState, Step};
use crate::services::llm::Lenses;
use crate::services::probe::{Checks, PrBrief};
use crate::services::review::LENSES;
use crate::services::testprogress::{self, Tier};

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
const PAD_BOTTOM: f64 = 120.0;
/// One line of a test step's progress bars: unit, integration, and so on.
const TIER_ROW: f64 = 32.0;
/// Room under the map for the review circle and its reviewers.
const HUB_ROOM: f64 = 290.0;
/// Rows can sit closer in a flow with a review circle under them, which
/// otherwise needs more height than most windows have.
const ROW_MIN_WITH_HUB: f64 = 150.0;
const HUB_R: f64 = 42.0;
const SPOKE: f64 = 112.0;
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

#[derive(Clone, PartialEq, Debug, Default)]
struct Map {
    stations: Vec<Station>,
    edges: Vec<(String, String)>,
    roots: Vec<String>,
    leaves: Vec<String>,
    /// Distance between columns, which is also how wide a label may be.
    col_w: f64,
    start_x: f64,
    end_x: f64,
    width: f64,
    height: f64,
}

impl Map {
    fn find(&self, id: &str) -> Option<&Station> {
        self.stations.iter().find(|s| s.id == id)
    }
}

/// Layers run left to right; within a layer, stations stack around the
/// middle so a fork opens symmetrically, the way the line splits in a map.
///
/// `area` is the room on screen, once it has been measured. The spacing grows
/// to fill it in both directions, and the map is centred in what is left.
/// `below` is extra room some station needs under its labels — a test step
/// showing a progress bar per tier. Every row gets it, so the bars never run
/// into the station underneath.
/// `gates` is whether the merge checkboxes sit above the end of the line;
/// they need room above only when the line is a single row, since with more
/// rows the space above the end is already there.
fn map(
    graph: &Graph,
    area: Option<(f64, f64)>,
    loops: bool,
    gates: bool,
    below: f64,
    hub: bool,
) -> Map {
    if graph.nodes.is_empty() {
        return Map::default();
    }
    let layer = layers_of(
        graph
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.deps.as_slice())),
    );
    let depth = layer.values().max().copied().unwrap_or(0) + 1;

    let mut columns: Vec<Vec<&str>> = vec![vec![]; depth];
    for node in &graph.nodes {
        columns[layer[&node.id]].push(node.id.as_str());
    }
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(1);

    let (area_w, area_h) = area.unwrap_or((0.0, 0.0));
    let room_above = loops || (gates && tallest == 1);
    let pad_top = PAD_TOP + if room_above { LOOP_ROOM } else { 0.0 };
    let pad_bottom = PAD_BOTTOM + below + if hub { HUB_ROOM } else { 0.0 };
    let row_min = if hub { ROW_MIN_WITH_HUB } else { ROW_MIN } + below;
    let gaps_x = (depth - 1) as f64;
    let gaps_y = (tallest - 1) as f64;
    let col_w = if gaps_x == 0.0 {
        COL_MAX
    } else {
        ((area_w - 2.0 * (PAD_X + LEAD)) / gaps_x).clamp(COL_MIN, COL_MAX)
    };
    let row_h = if gaps_y == 0.0 {
        row_min
    } else {
        ((area_h - pad_top - pad_bottom) / gaps_y).clamp(row_min, ROW_MAX.max(row_min))
    };
    let need_w = 2.0 * (PAD_X + LEAD) + gaps_x * col_w;
    let need_h = pad_top + pad_bottom + gaps_y * row_h;
    let width = need_w.max(area_w);
    let height = need_h.max(area_h);
    let left = PAD_X + LEAD + (width - need_w) / 2.0;
    let middle = pad_top + (height - pad_top - pad_bottom) / 2.0;

    let mut stations: Vec<Station> = vec![];
    for (col, ids) in columns.iter_mut().enumerate() {
        // Each step sits level with the average of the steps it follows, in
        // so far as its column allows. Declaration order alone crosses tracks
        // that need not cross: a step that only follows the upper of two
        // parents belongs above one that follows both.
        let height = |id: &str| -> f64 {
            let deps = &graph.get(id).map(|n| n.deps.clone()).unwrap_or_default();
            let ys: Vec<f64> = deps
                .iter()
                .filter_map(|d| stations.iter().find(|s| &s.id == d))
                .map(|s| s.y)
                .collect();
            if ys.is_empty() {
                middle
            } else {
                ys.iter().sum::<f64>() / ys.len() as f64
            }
        };
        ids.sort_by(|a, b| height(a).total_cmp(&height(b)));
        let count = ids.len() as f64;
        for (i, id) in ids.iter().enumerate() {
            stations.push(Station {
                id: id.to_string(),
                x: left + col as f64 * col_w,
                y: middle + (i as f64 - (count - 1.0) / 2.0) * row_h,
            });
        }
    }

    let known = |id: &str| graph.nodes.iter().any(|n| n.id == id);
    let edges: Vec<(String, String)> = graph
        .nodes
        .iter()
        .flat_map(|n| n.deps.iter().map(|d| (d.clone(), n.id.clone())))
        .filter(|(from, _)| known(from))
        .collect();
    let roots = graph
        .nodes
        .iter()
        .filter(|n| !n.deps.iter().any(|d| known(d)))
        .map(|n| n.id.clone())
        .collect();
    let leaves = graph
        .nodes
        .iter()
        .filter(|n| !edges.iter().any(|(from, _)| from == &n.id))
        .map(|n| n.id.clone())
        .collect();

    Map {
        stations,
        edges,
        roots,
        leaves,
        col_w,
        start_x: left - LEAD,
        end_x: left + gaps_x * col_w + LEAD,
        width,
        height,
    }
}

/// What the filled part of one track looks like, or `None` while the train
/// has not left the station at its start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fill {
    /// The step it leads to is waiting or under way.
    Moving,
    /// Both ends done.
    Arrived,
    /// The step it leads to was rejected.
    SentBack,
    Failed,
    /// The step it leads to will not run: skipped, bypassed or blocked.
    Idle,
}

impl Fill {
    /// Just the colour, for the dot riding this fill.
    fn css_colour(self) -> &'static str {
        match self {
            Fill::Moving => "moving",
            Fill::Arrived => "arrived",
            Fill::SentBack => "back",
            Fill::Failed => "failed",
            Fill::Idle => "idle",
        }
    }

    fn css(self) -> &'static str {
        match self {
            Fill::Moving => "run-fill run-fill-moving",
            Fill::Arrived => "run-fill run-fill-arrived",
            Fill::SentBack => "run-fill run-fill-back",
            Fill::Failed => "run-fill run-fill-failed",
            Fill::Idle => "run-fill run-fill-idle",
        }
    }
}

/// How a fill reaches where it is going, which decides its animation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Heading for a step that has not finished: stop short of it and wait.
    Approach,
    /// Was waiting short of its step, and the step has now settled: close the
    /// gap from where it stopped, rather than drawing again from the start.
    Finish,
    /// Seen for the first time already settled — on opening the view partway
    /// through a run, or on the track into the end bar: draw it whole.
    Draw,
}

impl Phase {
    fn of(fill: Fill, was_approaching: bool) -> Self {
        match fill {
            Fill::Moving => Phase::Approach,
            _ if was_approaching => Phase::Finish,
            _ => Phase::Draw,
        }
    }

    fn css(self) -> &'static str {
        match self {
            Phase::Approach => "run-phase-approach",
            Phase::Finish => "run-phase-finish",
            Phase::Draw => "run-phase-draw",
        }
    }

    /// The dot at the head of the fill, if there is one. It runs on into the
    /// station only when the step there is done; a step that failed or was
    /// sent back stops it where it stood.
    fn head(self, fill: Fill) -> Option<&'static str> {
        match (self, fill) {
            (Phase::Approach, _) => Some("run-dot run-dot-approach"),
            (Phase::Finish, Fill::Arrived) => Some("run-dot run-dot-finish"),
            (Phase::Draw, Fill::Arrived) => Some("run-dot run-dot-draw"),
            (Phase::Finish, _) => Some("run-dot run-dot-stop"),
            (Phase::Draw, _) => None,
        }
    }
}

/// `to` is `None` for the track into the end bar, which is reached as soon as
/// the step before it is done.
fn fill(departed: bool, to: Option<NodeStatus>) -> Option<Fill> {
    if !departed {
        return None;
    }
    Some(match to {
        None | Some(NodeStatus::Done) => Fill::Arrived,
        Some(NodeStatus::Pending | NodeStatus::Running | NodeStatus::AwaitingApproval) => {
            Fill::Moving
        }
        Some(NodeStatus::Rejected) => Fill::SentBack,
        Some(NodeStatus::Failed) => Fill::Failed,
        Some(NodeStatus::Skipped | NodeStatus::Bypassed | NodeStatus::Blocked) => Fill::Idle,
    })
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

/// Up to `lines` lines of at most `width` characters, broken between words,
/// with an ellipsis if there was more. A monospace font is what makes a
/// character count a width.
fn wrap(text: &str, width: usize, lines: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out: Vec<String> = vec![];
    let mut line = String::new();
    for word in text.split_whitespace() {
        let fits = line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width;
        if !fits {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    let more = out.len() > lines;
    out.truncate(lines);
    for (i, l) in out.iter_mut().enumerate() {
        let last = i + 1 == lines;
        if l.chars().count() > width || (last && more) {
            let cut: String = l.chars().take(width - 1).collect();
            *l = format!("{}\u{2026}", cut.trim_end());
        }
    }
    out
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

/// `from` is the step sent back, `to` where its track starts. The arc rises
/// as high as the gap allows: it must not reach the labels of a station above
/// that sits between the two ends.
fn revise_loop(from: (f64, f64), to: (f64, f64), above: Option<f64>) -> Loop {
    let (x2, y2) = from;
    let (x1, y1) = to;
    let dx = (x2 - x1).abs().max(1.0);
    let base = y1.min(y2) - R;
    let mut rise = (dx * 0.3).clamp(LOOP_MIN, LOOP_MAX);
    if let Some(y) = above {
        // Leave room for that station's name and description underneath it.
        rise = rise.min(base - (y + PAD_BOTTOM - 10.0)).max(LOOP_MIN);
    }
    // Both control points at `top`: a cubic like that peaks three quarters of
    // the way there, so aim past the height wanted.
    let top = base - rise / 0.75;
    let (sx, sy) = (x2 - R * 0.7, y2 - R * 0.7);
    let (ex, ey) = (x1 + R * 0.7, y1 - R * 0.7);
    let pull = dx * 0.3;
    Loop {
        d: format!(
            "M {sx} {sy} C {} {top}, {} {top}, {ex} {ey}",
            sx - pull,
            ex + pull
        ),
        peak: ((sx + ex) / 2.0, (sy + ey) / 8.0 + 0.75 * top),
        end: (ex, ey),
    }
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

/// How wide a step's panel is over a map `area_w` wide, open or folded. Must
/// agree with `.run-drawer`'s width in the stylesheet: this is what the map
/// slides out from under.
fn panel_width(area_w: f64, folded: bool) -> f64 {
    if folded {
        FOLDED_PANEL
    } else {
        (area_w * 0.58).min(620.0)
    }
}

/// How far to slide the map left so a station at `x`, with labels `col_w`
/// wide, stays clear of an open panel. Only as far as that takes, and not at
/// all when it is already clear, so as much of the map stays in view as can.
fn slide_for(x: f64, col_w: f64, area_w: f64, folded: bool) -> f64 {
    let visible = area_w - panel_width(area_w, folded);
    (x + col_w / 2.0 + 16.0 - visible).max(0.0)
}

/// One tick of a replay: show one more change, or go back to following the
/// run live once the replay has caught up with it.
fn advance(cursor: Option<(u64, usize)>, len: usize) -> Option<(u64, usize)> {
    let (run, at) = cursor?;
    (at + 1 < len).then_some((run, at + 1))
}

/// How a reviewer, or the review as a whole, stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Tone {
    // Ordered least to most urgent: the circle takes the most urgent of its
    // reviewers, and reads clean only when every one of them is.
    Idle,
    Good,
    Working,
    Warn,
    Waiting,
    Bad,
    Back,
}

impl Tone {
    fn css(self) -> &'static str {
        match self {
            Tone::Idle => "idle",
            Tone::Good => "good",
            Tone::Working => "working",
            Tone::Warn => "warn",
            Tone::Waiting => "waiting",
            Tone::Bad => "bad",
            Tone::Back => "back",
        }
    }
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

/// The review circle: the reviews a pull request gets in this flow, around
/// the steps that do the reviewing. Only what the flow really does — you
/// reading the diff, the model's analysis, a second model's opinion, the CI
/// checks — plus a Fix spoke once anything in the review has been sent back.
#[derive(Clone, PartialEq, Debug)]
struct Hub {
    /// The steps that do the reviewing — the analysis, and the second opinion
    /// when there is one. The circle hangs from whichever sits lowest on the
    /// map, so its line never runs down through the other.
    anchors: Vec<String>,
    /// Which step the circle itself opens.
    anchor: String,
    spokes: Vec<Spoke>,
    tone: Tone,
}

fn plural(n: &str, one: &str) -> String {
    let n = if n.is_empty() { "0" } else { n };
    format!("{n} {one}{}", if n == "1" { "" } else { "s" })
}

/// Where spoke `i` of `n` points, in degrees, clockwise from the right.
///
/// Only out to the sides: the first half down the left, the rest down the
/// right, each between 75° above and below the horizontal. Labels then always
/// read outward, one above another, rather than two near the top or bottom
/// colliding — and the top stays clear for the line down from the map.
fn spoke_angle(i: usize, n: usize) -> f64 {
    let left = n.div_ceil(2);
    let (j, count, top, step) = if i < left {
        (i, left, -105.0, -1.0)
    } else {
        (i - left, n - left, -75.0, 1.0)
    };
    if count <= 1 {
        return if i < left { 180.0 } else { 0.0 };
    }
    top + step * j as f64 * 150.0 / (count - 1) as f64
}

/// `lenses` is which focused reviews are turned on now, for the ones that
/// have not run yet; one that has run is read from what it wrote.
fn review_hub(graph: &Graph, state: &RunState, lenses: &Lenses) -> Option<Hub> {
    let analyse = graph.nodes.iter().find(|n| n.step == Step::Analyse)?;
    let status = |id: &str| {
        if state.started {
            state.status(id)
        } else {
            NodeStatus::Pending
        }
    };
    let mut spokes = vec![];

    if let Some(n) = graph.nodes.iter().find(|n| n.step == Step::PrDiff) {
        let (tone, note) = match status(&n.id) {
            NodeStatus::Running => (Tone::Working, "fetching the diff"),
            NodeStatus::AwaitingApproval => (Tone::Waiting, "your turn: read the diff"),
            NodeStatus::Done => (Tone::Good, "diff read"),
            NodeStatus::Rejected => (Tone::Back, "declined"),
            NodeStatus::Failed => (Tone::Bad, "could not fetch"),
            NodeStatus::Skipped | NodeStatus::Bypassed => (Tone::Idle, "skipped"),
            NodeStatus::Pending | NodeStatus::Blocked => (Tone::Idle, "not yet"),
        };
        spokes.push(Spoke {
            name: "You".into(),
            note: note.into(),
            tone,
            step: Some(n.id.clone()),
        });
    }

    let findings = state.artifact("finding_count");
    let (tone, note) = match status(&analyse.id) {
        NodeStatus::Running => (Tone::Working, "reviewing".to_string()),
        NodeStatus::Done => match state.artifact("verdict") {
            "looks_safe" => (Tone::Good, "looks safe".to_string()),
            "risky" => (
                Tone::Bad,
                format!("risky \u{00b7} {}", plural(findings, "finding")),
            ),
            _ => (
                Tone::Warn,
                format!("worth a look \u{00b7} {}", plural(findings, "finding")),
            ),
        },
        NodeStatus::Bypassed => (Tone::Idle, "skipped \u{2014} no model".to_string()),
        NodeStatus::Failed => (Tone::Bad, "failed".to_string()),
        _ => (Tone::Idle, "after the diff".to_string()),
    };
    spokes.push(Spoke {
        name: "Model".into(),
        note,
        tone,
        step: Some(analyse.id.clone()),
    });

    // The second opinion, named after the model that gave it once one has:
    // "deepseek-chat" says more than "second model" does.
    if let Some(n) = graph.nodes.iter().find(|n| n.step == Step::SecondOpinion) {
        let model = state.artifact("second_model");
        let findings = state.artifact("second_finding_count");
        let (tone, note) = match status(&n.id) {
            NodeStatus::Running => (Tone::Working, "second opinion, reviewing".to_string()),
            NodeStatus::Done => match state.artifact("second_verdict") {
                "looks_safe" => (Tone::Good, "second opinion: looks safe".to_string()),
                "risky" => (
                    Tone::Bad,
                    format!(
                        "second opinion: risky \u{00b7} {}",
                        plural(findings, "finding")
                    ),
                ),
                _ => (
                    Tone::Warn,
                    format!(
                        "second opinion: worth a look \u{00b7} {}",
                        plural(findings, "finding")
                    ),
                ),
            },
            NodeStatus::Bypassed => (Tone::Idle, "none chosen in Settings".to_string()),
            NodeStatus::Failed => (Tone::Bad, "second opinion failed".to_string()),
            _ => (Tone::Idle, "second AI model".to_string()),
        };
        spokes.push(Spoke {
            name: if model.is_empty() {
                "Second model".into()
            } else {
                model.to_string()
            },
            note,
            tone,
            step: Some(n.id.clone()),
        });
    }

    // One spoke per focused review, whether it is on or not: an off one says
    // where to turn it on, rather than the circle quietly having fewer.
    if let Some(n) = graph.nodes.iter().find(|n| n.step == Step::Lenses) {
        let ran: Vec<&str> = state.artifact("lenses_run").split(',').collect();
        for lens in LENSES {
            let key = lens.key();
            let did_run = ran.contains(&key);
            let findings = state.artifact(&format!("{key}_finding_count"));
            let (tone, note) = match status(&n.id) {
                NodeStatus::Done if did_run => match state.artifact(&format!("{key}_verdict")) {
                    "looks_safe" => (Tone::Good, "looks safe".to_string()),
                    "risky" => (
                        Tone::Bad,
                        format!("risky \u{00b7} {}", plural(findings, "finding")),
                    ),
                    _ => (
                        Tone::Warn,
                        format!("worth a look \u{00b7} {}", plural(findings, "finding")),
                    ),
                },
                NodeStatus::Done | NodeStatus::Bypassed => {
                    (Tone::Idle, "off \u{2014} turn on in Settings".to_string())
                }
                _ if !lenses.is_on(key) => {
                    (Tone::Idle, "off \u{2014} turn on in Settings".to_string())
                }
                NodeStatus::Running => (Tone::Working, "reviewing".to_string()),
                NodeStatus::Failed => (Tone::Bad, "failed".to_string()),
                _ => (Tone::Idle, "after the diff".to_string()),
            };
            spokes.push(Spoke {
                name: lens.label().into(),
                note,
                tone,
                step: Some(n.id.clone()),
            });
        }
    }

    if let Some(n) = graph.nodes.iter().find(|n| n.step == Step::PrStatus) {
        let (tone, note) = match status(&n.id) {
            NodeStatus::Running => (Tone::Working, "reading checks"),
            NodeStatus::Done => match state.artifact("checks_state") {
                "passing" => (Tone::Good, "checks pass"),
                "failing" => (Tone::Bad, "checks failing"),
                "pending" => (Tone::Waiting, "checks running"),
                _ => (Tone::Idle, "no checks"),
            },
            NodeStatus::Failed => (Tone::Bad, "could not read"),
            _ => (Tone::Idle, "not read yet"),
        };
        spokes.push(Spoke {
            name: "CI".into(),
            note: note.into(),
            tone,
            step: Some(n.id.clone()),
        });
    }

    // Sent back from anywhere in the review — the diff, the analysis, or the
    // merge it all leads to.
    let reviewing = |s: Step| matches!(s, Step::PrDiff | Step::Analyse | Step::Merge);
    let sent_back = graph
        .nodes
        .iter()
        .filter(|n| reviewing(n.step))
        .find_map(|n| {
            let times = state.rejections.get(&n.id).copied().unwrap_or(0);
            (times > 0 || status(&n.id) == NodeStatus::Rejected).then(|| (n, times.max(1)))
        });
    if let Some((n, times)) = sent_back {
        let still = status(&n.id) == NodeStatus::Rejected;
        spokes.push(Spoke {
            name: "Fix".into(),
            note: if still {
                "then review again".into()
            } else if times == 1 {
                "fixed, reviewed again".into()
            } else {
                format!("fixed {times}\u{d7}, reviewed again")
            },
            tone: if still { Tone::Back } else { Tone::Good },
            step: Some(n.id.clone()),
        });
    }

    let worst = spokes.iter().map(|s| s.tone).max().unwrap_or(Tone::Idle);
    let tone = if spokes.iter().all(|s| s.tone == Tone::Good) {
        Tone::Good
    } else if worst == Tone::Good {
        Tone::Idle
    } else {
        worst
    };
    Some(Hub {
        anchors: graph
            .nodes
            .iter()
            .filter(|n| matches!(n.step, Step::Analyse | Step::SecondOpinion | Step::Lenses))
            .map(|n| n.id.clone())
            .collect(),
        anchor: analyse.id.clone(),
        spokes,
        tone,
    })
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
    /// The step's panel itself, drawn over the map below the header — so the
    /// play controls stay in reach while it is open.
    #[props(default)]
    pub children: Element,
    /// The flow tabs, the same strip the list view shows, so the flow is
    /// chosen here too. Drawn under the header.
    pub flows: Element,
    pub on_select: EventHandler<String>,
}

#[component]
pub fn RunView(props: RunViewProps) -> Element {
    // The room the canvas has, measured, so the map can spread to fill it
    // instead of scaling — scaling a wide flow down is what made the text
    // small and left the height empty.
    let mut area = use_signal(|| Option::<(f64, f64)>::None);

    // The player. The run itself is never paused — it is real work — but the
    // view of it can be: frozen where it is, or wound back and replayed from
    // the run's history at a chosen pace. `cursor` is how many changes of
    // which run are shown; `None` is live, following the run as it goes.
    let mut cursor = use_signal(|| Option::<(u64, usize)>::None);
    let mut playing = use_signal(|| true);
    let mut speed = use_signal(|| 1.0f64);
    // Bumped by Restart so every track and dot is drawn afresh, not left
    // where the last pass put it.
    let mut epoch = use_signal(|| 0u32);
    let len = props.state.history.len();
    let this_run = props.state.run;
    let at = match *cursor.read() {
        Some((r, i)) if r == this_run && i < len => Some(i),
        _ => None,
    };
    let live = at.is_none();
    // Nothing has happened in this run yet: no history to play back.
    let fresh = len == 0 && !props.state.started;
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
    let plan = map(
        &props.graph,
        *area.read(),
        looped,
        props.gates.is_some(),
        below,
        hub.is_some(),
    );
    // With a panel open, the map moves left until the step it is about is
    // clear of it, and back when it closes.
    let slide = if props.panel_open {
        let area_w = (*area.read()).map(|(w, _)| w).unwrap_or(plan.width);
        plan.find(&props.selected)
            .map(|s| slide_for(s.x, plan.col_w, area_w, props.panel_folded))
            .unwrap_or(0.0)
    } else {
        0.0
    };
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

    // A track is drawn twice: the pale rail, always, and the fill over it.
    struct Track {
        key: String,
        d: String,
        fill: Option<Fill>,
    }
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
                fill: fill(started, Some(to)),
            });
        }
    }
    for (from, to) in &plan.edges {
        if let (Some(a), Some(b)) = (plan.find(from), plan.find(to)) {
            let target = status(to);
            tracks.push(Track {
                key: format!("{from}->{to}"),
                d: curve(a.x + R, a.y, b.x - R, b.y),
                fill: fill(departed(status(from)), Some(target)),
            });
        }
    }
    for leaf in &plan.leaves {
        if let Some(s) = plan.find(leaf) {
            tracks.push(Track {
                key: format!("{leaf}->end"),
                d: curve(s.x + R, s.y, end_x, bar_y),
                fill: fill(departed(status(leaf)), None),
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

    // One loop per step that has been sent back, over the track it came in
    // on. With several, the one from the nearest height: that is the track
    // that runs alongside, so the loop never has to cut across the map.
    struct Revise {
        key: String,
        shape: Loop,
        label: String,
        live: bool,
    }
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
            div { class: "run-head",
                div { class: "run-head-main",
                    div { class: "run-title", title: "{props.repo_label}", "{props.repo_label}" }
                    div { class: "run-sub",
                        span { "{props.flow_label}" }
                        span { class: "run-count",
                            if !started { "not started" }
                            else if finished { "finished · {done} of {total} done" }
                            else { "{done} of {total} done" }
                        }
                        if waiting {
                            span { class: "run-needs", "\u{23f8} needs you" }
                        }
                    }
                }
                div { class: "run-player",
                    // Two buttons, not one that changes its label: the one
                    // in effect stays pressed, so the state reads at a glance.
                    if fresh {
                        // Nothing has run, so there is nothing to play back:
                        // Play starts the flow, and stops at every approval
                        // just as Start in the list view does.
                        button {
                            class: "run-ctl",
                            disabled: !props.can_start,
                            title: if props.can_start {
                                "Start this flow \u{2014} the same as Start in the list view. It stops for you at every approval.".to_string()
                            } else {
                                props.start_note.clone()
                            },
                            onclick: move |_| props.on_start.call(()),
                            "\u{25b6} Play"
                        }
                        button {
                            class: "run-ctl",
                            disabled: true,
                            title: "Nothing is running yet.",
                            "\u{275a}\u{275a} Pause"
                        }
                    } else {
                        // Two buttons, not one that changes its label: the one
                        // in effect stays pressed, so the state reads at a glance.
                        button {
                            class: if *playing.read() { "run-ctl run-ctl-on" } else { "run-ctl" },
                            title: "Carry on from here, catching up with the run step by step.",
                            onclick: move |_| playing.set(true),
                            "\u{25b6} Play"
                        }
                        button {
                            class: if *playing.read() { "run-ctl" } else { "run-ctl run-ctl-on" },
                            title: "Freeze the map here. The run itself carries on.",
                            onclick: move |_| {
                                if !*playing.peek() {
                                    return;
                                }
                                // Pausing while live pins the view to now, so the
                                // run moving on underneath does not move the map.
                                if cursor.peek().is_none() {
                                    cursor.set(Some((this_run, len)));
                                }
                                playing.set(false);
                            },
                            "\u{275a}\u{275a} Pause"
                        }
                    }
                    button {
                        class: "run-restart",
                        disabled: len == 0,
                        title: if len == 0 {
                            "Nothing to replay yet \u{2014} start the flow first."
                        } else {
                            "Replay this run from the beginning"
                        },
                        onclick: move |_| {
                            *epoch.write() += 1;
                            cursor.set(Some((this_run, 0)));
                            playing.set(true);
                        },
                        "\u{21bb} Restart"
                    }
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
                    span { class: if live && !fresh { "run-mode run-mode-live" } else { "run-mode" },
                        if fresh { "not started" }
                        else if live && *playing.read() { "live" }
                        else if let Some(i) = at { "replay {i} / {len}" }
                        else { "paused" }
                    }
                }
                div { class: "run-legend",
                    span { class: "run-key run-key-moving", "in progress" }
                    span { class: "run-key run-key-back", "sent back" }
                    span { class: "run-key run-key-arrived", "done" }
                }
            }

            // Above the body, so a step's panel never covers the flow tabs.
            {props.flows}

            div { class: "run-body",
            div {
                class: if *playing.read() { "run-canvas" } else { "run-canvas run-paused" },
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
                    svg {
                        class: "run-svg",
                        style: "transform: translateX(-{slide}px);",
                        width: "{plan.width}",
                        height: "{plan.height}",
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
                        if let Some(g) = props.gates.clone() {
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
                                                                props.on_select.call(id);
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
                                            onclick: move |_| props.on_select.call(on_hub.clone()),
                                            circle { class: "run-hub-ring", cx: "{cx}", cy: "{cy}", r: "{HUB_R}" }
                                            text { class: "run-hub-text", x: "{cx}", y: "{cy + 5.5}", "Review" }
                                        }
                                    }
                                }
                            }
                        }

                        for s in plan.stations.iter() {
                            if let Some(spec) = props.graph.get(&s.id) {
                                {
                                    let st = status(&s.id);
                                    let id = s.id.clone();
                                    let on = s.id == props.selected;
                                    let class = format!(
                                        "run-station run-station-{}{}",
                                        st.css(),
                                        if on { " run-station-on" } else { "" },
                                    );
                                    let kind = if spec.kind == NodeKind::Model { "model" } else { "code" };
                                    let name = wrap(&spec.title, name_chars, 2);
                                    let meta = wrap(&spec.subtitle, meta_chars, 2);
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
                                            onclick: move |_| props.on_select.call(id.clone()),
                                            title { "{spec.title} — {spec.subtitle}\nClick to open this step." }
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

            // Every step as a numbered pill, in the order the map reads, so
            // where the run is shows at a glance even when the map scrolls.
            div { class: "run-foot",
                div { class: "run-stages",
                    for (n, s) in plan.stations.iter().enumerate() {
                        if let Some(spec) = props.graph.get(&s.id) {
                            {
                                let st = status(&s.id);
                                let id = s.id.clone();
                                let class = format!(
                                    "run-stage run-stage-{}{}",
                                    st.css(),
                                    if s.id == props.selected { " run-stage-on" } else { "" },
                                );
                                rsx! {
                                    button {
                                        key: "{s.id}",
                                        class: "{class}",
                                        title: "{spec.title}: {st.label()}. Click to open this step.",
                                        onclick: move |_| props.on_select.call(id.clone()),
                                        span { class: "run-stage-num", "{n + 1}" }
                                        span { class: "run-stage-name", "{spec.title}" }
                                    }
                                }
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
            {props.children}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::flowdef::FlowBook;

    fn commit() -> Graph {
        FlowBook::defaults()
            .get("commit_and_pr")
            .unwrap()
            .to_graph()
    }

    #[test]
    fn a_track_stays_empty_until_its_start_is_done() {
        assert_eq!(fill(false, Some(NodeStatus::Running)), None);
        assert_eq!(fill(true, Some(NodeStatus::Running)), Some(Fill::Moving));
    }

    #[test]
    fn a_track_turns_green_when_both_ends_are_done() {
        assert_eq!(fill(true, Some(NodeStatus::Done)), Some(Fill::Arrived));
        assert_eq!(fill(true, None), Some(Fill::Arrived));
    }

    #[test]
    fn a_rejection_is_drawn_as_sent_back() {
        assert_eq!(fill(true, Some(NodeStatus::Rejected)), Some(Fill::SentBack));
    }

    #[test]
    fn a_track_waiting_short_of_its_step_finishes_from_where_it_stopped() {
        assert_eq!(Phase::of(Fill::Moving, false), Phase::Approach);
        assert_eq!(Phase::of(Fill::Arrived, true), Phase::Finish);
        // Opening the view on a run already past this point draws it whole.
        assert_eq!(Phase::of(Fill::Arrived, false), Phase::Draw);
    }

    #[test]
    fn the_head_runs_into_the_station_only_when_the_step_is_done() {
        assert_eq!(
            Phase::Finish.head(Fill::Arrived),
            Some("run-dot run-dot-finish")
        );
        assert_eq!(
            Phase::Finish.head(Fill::SentBack),
            Some("run-dot run-dot-stop")
        );
        assert_eq!(Phase::Draw.head(Fill::Failed), None);
        assert!(Phase::Approach.head(Fill::Moving).is_some());
    }

    #[test]
    fn a_bypassed_step_still_lets_the_run_through() {
        assert!(departed(NodeStatus::Bypassed));
        assert!(!departed(NodeStatus::Skipped));
    }

    #[test]
    fn the_line_runs_left_to_right() {
        let graph = commit();
        let plan = map(&graph, None, false, false, 0.0, false);
        for (from, to) in &plan.edges {
            assert!(
                plan.find(from).unwrap().x < plan.find(to).unwrap().x,
                "{from} -> {to}"
            );
        }
    }

    #[test]
    fn steps_that_run_side_by_side_share_a_column() {
        // commit and draft_pr both leave draft_commit, and neither waits for
        // the other.
        let plan = map(&commit(), None, false, false, 0.0, false);
        let a = plan.find("commit").unwrap();
        let b = plan.find("draft_pr").unwrap();
        assert_eq!(a.x, b.x);
        assert!((a.y - b.y).abs() >= ROW_MIN);
    }

    #[test]
    fn every_station_fits_on_the_canvas() {
        for area in [None, Some((600.0, 300.0)), Some((2400.0, 1200.0))] {
            let plan = map(&commit(), area, false, false, 0.0, false);
            for s in &plan.stations {
                assert!(s.x > plan.start_x && s.x < plan.end_x);
                assert!(plan.end_x < plan.width);
                assert!(s.y - R >= 0.0 && s.y + R <= plan.height);
            }
        }
    }

    #[test]
    fn the_map_spreads_to_fill_a_bigger_window() {
        let small = map(&commit(), None, false, false, 0.0, false);
        let big = map(&commit(), Some((2400.0, 1000.0)), false, false, 0.0, false);
        assert!(big.col_w > small.col_w);
        assert_eq!((big.width, big.height), (2400.0, 1000.0));
        let gap = |m: &Map| (m.find("commit").unwrap().y - m.find("draft_pr").unwrap().y).abs();
        assert!(gap(&big) > gap(&small));
    }

    #[test]
    fn a_window_too_small_scrolls_rather_than_squeezing() {
        let plan = map(&commit(), Some((300.0, 200.0)), false, false, 0.0, false);
        assert_eq!(plan.col_w, COL_MIN);
        assert!(plan.width > 300.0);
    }

    #[test]
    fn a_long_label_wraps_between_words_and_ends_in_an_ellipsis() {
        assert_eq!(
            wrap("Draft commit message", 14, 2),
            vec!["Draft commit", "message"]
        );
        let lines = wrap("Remote, forge, credentials, and everything else", 16, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('\u{2026}'));
        assert!(lines.iter().all(|l| l.chars().count() <= 16));
    }

    #[test]
    fn a_step_following_only_the_upper_parent_sits_above_one_following_both() {
        // draft_pr follows draft_commit alone; commit follows draft_commit and
        // test. Stacking commit on top crosses their tracks for no reason.
        let plan = map(&commit(), None, false, false, 0.0, false);
        assert!(plan.find("draft_pr").unwrap().y < plan.find("commit").unwrap().y);
    }

    #[test]
    fn a_revise_loop_rises_above_both_of_its_stations() {
        let l = revise_loop((500.0, 300.0), (300.0, 300.0), None);
        assert!(l.peak.1 < 300.0 - R - LOOP_MIN + 1.0);
        assert!(l.peak.0 > 300.0 && l.peak.0 < 500.0);
        assert!(l.end.0 < 500.0, "it lands back at the earlier station");
    }

    #[test]
    fn a_revise_loop_stays_clear_of_the_labels_above_it() {
        let free = revise_loop((500.0, 400.0), (300.0, 400.0), None);
        let tight = revise_loop((500.0, 400.0), (300.0, 400.0), Some(200.0));
        assert!(tight.peak.1 >= free.peak.1);
    }

    #[test]
    fn the_loop_label_counts_rounds() {
        assert_eq!(loop_label(NodeStatus::Rejected, 1), "sent back");
        assert_eq!(
            loop_label(NodeStatus::Rejected, 2),
            "sent back \u{00b7} round 2"
        );
        assert_eq!(loop_label(NodeStatus::AwaitingApproval, 2), "round 3");
        assert_eq!(loop_label(NodeStatus::Done, 2), "approved in round 3");
    }

    #[test]
    fn a_replay_steps_through_the_history_then_goes_live() {
        assert_eq!(advance(Some((7, 0)), 3), Some((7, 1)));
        assert_eq!(advance(Some((7, 1)), 3), Some((7, 2)));
        assert_eq!(advance(Some((7, 2)), 3), None, "caught up");
        assert_eq!(advance(None, 3), None, "live stays live");
    }

    fn brief(number: &str, url: &str, checks: Checks) -> PrBrief {
        PrBrief {
            number: number.into(),
            title: "t".into(),
            url: url.into(),
            checks,
            files: 1,
            additions: 1,
            deletions: 0,
            commits: 1,
        }
    }

    #[test]
    fn a_flow_with_no_pull_request_has_no_gates() {
        let mut graph = commit();
        graph
            .nodes
            .retain(|n| n.id != "open_pr" && n.id != "draft_pr");
        assert_eq!(
            Gates::for_run(&graph, &RunState::default(), "", &[], None),
            None
        );
    }

    #[test]
    fn before_a_pull_request_exists_neither_gate_is_ticked() {
        let g = Gates::for_run(&commit(), &RunState::default(), "", &[], None).unwrap();
        assert_eq!(
            g,
            Gates {
                pr: None,
                checks: Checks::Unknown
            }
        );
    }

    #[test]
    fn the_pull_request_the_run_opened_is_the_one_whose_checks_count() {
        let mut state = RunState::default();
        state
            .artifacts
            .insert("pr_url".into(), "https://x/pull/7".into());
        let prs = [
            brief("5", "https://x/pull/5", Checks::Failing),
            brief("7", "https://x/pull/7", Checks::Passing),
        ];
        let g = Gates::for_run(&commit(), &state, "", &prs, None).unwrap();
        assert_eq!(
            g,
            Gates {
                pr: Some("#7".into()),
                checks: Checks::Passing
            }
        );
    }

    #[test]
    fn a_pull_request_just_opened_is_ready_before_its_checks_are_known() {
        let mut state = RunState::default();
        state
            .artifacts
            .insert("pr_url".into(), "https://x/pull/12".into());
        let g = Gates::for_run(&commit(), &state, "", &[], None).unwrap();
        assert_eq!(
            g,
            Gates {
                pr: Some("#12".into()),
                checks: Checks::Unknown
            }
        );
    }

    #[test]
    fn a_picked_review_names_its_pull_request() {
        let prs = [brief("3", "u3", Checks::Pending)];
        let g = Gates::for_run(&commit(), &RunState::default(), "3", &prs, None).unwrap();
        assert_eq!(
            g,
            Gates {
                pr: Some("#3".into()),
                checks: Checks::Pending
            }
        );
    }

    fn review() -> Graph {
        FlowBook::defaults()
            .get("review_and_merge")
            .unwrap()
            .to_graph()
    }

    #[test]
    fn a_flow_that_reviews_nothing_has_no_review_circle() {
        assert_eq!(
            review_hub(&commit(), &RunState::default(), &Lenses::default()),
            None
        );
    }

    #[test]
    fn the_review_circle_has_you_the_model_and_ci() {
        let hub = review_hub(&review(), &RunState::default(), &Lenses::default()).unwrap();
        let names: Vec<&str> = hub.spokes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "You",
                "Model",
                "Second model",
                "Alignment",
                "Security",
                "Architecture",
                "CI"
            ]
        );
        assert_eq!(hub.tone, Tone::Idle);
    }

    #[test]
    fn each_reviewer_reports_what_its_step_found() {
        let g = review();
        let mut s = RunState::fresh(&g);
        s.started = true;
        s.set_status("pr_diff", NodeStatus::Done);
        s.set_status("analyse", NodeStatus::Done);
        s.artifacts.insert("verdict".into(), "worth_a_look".into());
        s.artifacts.insert("finding_count".into(), "1".into());
        s.set_status("pr_status", NodeStatus::Done);
        s.artifacts.insert("checks_state".into(), "failing".into());
        let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
        let note = |n: &str| {
            hub.spokes
                .iter()
                .find(|s| s.name == n)
                .unwrap()
                .note
                .clone()
        };
        assert_eq!(note("You"), "diff read");
        assert_eq!(note("Model"), "worth a look \u{00b7} 1 finding");
        assert_eq!(note("CI"), "checks failing");
        assert_eq!(
            hub.tone,
            Tone::Bad,
            "the circle takes its most urgent reviewer"
        );
    }

    #[test]
    fn the_second_opinion_is_named_after_its_model() {
        let g = review();
        let mut s = RunState::fresh(&g);
        s.started = true;
        s.set_status("second_opinion", NodeStatus::Done);
        s.artifacts
            .insert("second_model".into(), "deepseek-chat".into());
        s.artifacts
            .insert("second_verdict".into(), "looks_safe".into());
        let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
        let second = hub
            .spokes
            .iter()
            .find(|s| s.name == "deepseek-chat")
            .unwrap();
        assert_eq!(second.note, "second opinion: looks safe");
        assert_eq!(second.tone, Tone::Good);
    }

    #[test]
    fn a_second_opinion_nobody_chose_says_so_and_alarms_nobody() {
        let g = review();
        let mut s = RunState::fresh(&g);
        s.started = true;
        s.set_status("second_opinion", NodeStatus::Bypassed);
        let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
        let second = hub
            .spokes
            .iter()
            .find(|s| s.name == "Second model")
            .unwrap();
        assert_eq!(second.note, "none chosen in Settings");
        assert_eq!(second.tone, Tone::Idle);
    }

    #[test]
    fn a_focused_review_that_is_off_says_where_to_turn_it_on() {
        let hub = review_hub(&review(), &RunState::default(), &Lenses::default()).unwrap();
        let security = hub.spokes.iter().find(|s| s.name == "Security").unwrap();
        assert_eq!(security.note, "off \u{2014} turn on in Settings");
        assert_eq!(security.tone, Tone::Idle);
    }

    #[test]
    fn each_focused_review_that_ran_reports_its_own_verdict() {
        let g = review();
        let mut s = RunState::fresh(&g);
        s.started = true;
        s.set_status("lenses", NodeStatus::Done);
        s.artifacts
            .insert("lenses_run".into(), "security,architecture".into());
        s.artifacts
            .insert("security_verdict".into(), "risky".into());
        s.artifacts
            .insert("security_finding_count".into(), "2".into());
        s.artifacts
            .insert("architecture_verdict".into(), "looks_safe".into());
        let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
        let note = |n: &str| {
            hub.spokes
                .iter()
                .find(|s| s.name == n)
                .unwrap()
                .note
                .clone()
        };
        assert_eq!(note("Security"), "risky \u{00b7} 2 findings");
        assert_eq!(note("Architecture"), "looks safe");
        assert_eq!(note("Alignment"), "off \u{2014} turn on in Settings");
    }

    #[test]
    fn a_review_sent_back_grows_a_fix_spoke() {
        let g = review();
        let mut s = RunState::fresh(&g);
        s.started = true;
        s.reject("merge", &g);
        let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
        let fix = hub.spokes.last().unwrap();
        assert_eq!(
            (fix.name.as_str(), fix.note.as_str(), fix.tone),
            ("Fix", "then review again", Tone::Back)
        );
        s.retry_from("merge", &g);
        let after = review_hub(&g, &s, &Lenses::default()).unwrap();
        assert_eq!(after.spokes.last().unwrap().note, "fixed, reviewed again");
    }

    #[test]
    fn spokes_go_down_the_left_then_the_right_never_straight_up() {
        let angles: Vec<f64> = (0..7).map(|i| spoke_angle(i, 7)).collect();
        for (i, a) in angles.iter().enumerate() {
            let a = a.to_radians();
            assert!(a.sin().abs() < 0.97, "spoke {i} points too near vertical");
            assert_eq!(a.cos() < 0.0, i < 4, "spoke {i} on the wrong side");
        }
        // Down each side in order, so labels stack top to bottom.
        let y = |i: usize| spoke_angle(i, 7).to_radians().sin();
        assert!(y(0) < y(1) && y(1) < y(2) && y(2) < y(3));
        assert!(y(4) < y(5) && y(5) < y(6));
    }

    #[test]
    fn the_merge_checks_need_room_above_only_on_a_one_row_line() {
        let mut line = commit();
        line.nodes.retain(|n| n.id != "test" && n.id != "draft_pr");
        for n in &mut line.nodes {
            n.deps.retain(|d| d != "test" && d != "draft_pr");
        }
        let one_row = map(&line, None, false, true, 0.0, false).height
            - map(&line, None, false, false, 0.0, false).height;
        assert_eq!(one_row, LOOP_ROOM);
        let forked = map(&commit(), None, false, true, 0.0, false).height
            - map(&commit(), None, false, false, 0.0, false).height;
        assert_eq!(forked, 0.0);
    }

    #[test]
    fn the_review_circle_gets_room_under_the_map() {
        let plan = map(&review(), None, false, false, 0.0, true);
        let lowest = plan.stations.iter().map(|s| s.y).fold(f64::MIN, f64::max);
        assert!(plan.height - lowest >= PAD_BOTTOM + HUB_ROOM);
    }

    #[test]
    fn a_step_under_the_panel_slides_out_just_far_enough() {
        let area = 1200.0;
        let visible = area - panel_width(area, false);
        let x = 1000.0;
        let slide = slide_for(x, 200.0, area, false);
        assert!(slide > 0.0);
        // Its labels end right at the panel's edge.
        assert_eq!(x - slide + 100.0 + 16.0, visible);
    }

    #[test]
    fn a_step_already_clear_of_the_panel_does_not_move_the_map() {
        assert_eq!(slide_for(150.0, 200.0, 1200.0, false), 0.0);
    }

    #[test]
    fn a_folded_panel_gives_the_map_almost_all_its_room_back() {
        let open = slide_for(1000.0, 200.0, 1200.0, false);
        let folded = slide_for(1000.0, 200.0, 1200.0, true);
        assert!(folded < open);
        // A step clear of the strip does not move at all.
        assert_eq!(slide_for(900.0, 200.0, 1200.0, true), 0.0);
    }

    #[test]
    fn the_panel_never_takes_more_than_its_cap() {
        assert_eq!(panel_width(3000.0, false), 620.0);
        assert_eq!(panel_width(1000.0, false), 580.0);
        assert_eq!(panel_width(1000.0, true), FOLDED_PANEL);
    }

    #[test]
    fn the_line_starts_at_the_roots_and_ends_at_the_leaves() {
        let plan = map(&commit(), None, false, false, 0.0, false);
        assert_eq!(plan.roots, vec!["preflight".to_string()]);
        assert!(plan.leaves.contains(&"open_pr".to_string()));
    }
}
