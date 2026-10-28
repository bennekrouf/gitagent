//! Laying out a run: where each step sits on the map, how far along it is, and
//! keeping the running step in view.

use super::*;

/// The step the run is at, for following it across a map wider than the
/// screen: the one that most recently started running, stopped for your
/// approval, or failed, and is still in that state — looking only at the
/// first `upto` changes, so a replay follows the replay. `None` once the run
/// has nothing in progress.
pub(super) fn progress_focus(state: &RunState, upto: usize) -> Option<String> {
    state
        .history
        .iter()
        .take(upto)
        .rev()
        .find(|(id, status)| {
            matches!(
                status,
                NodeStatus::Running | NodeStatus::AwaitingApproval | NodeStatus::Failed
            ) && state.status(id) == *status
        })
        .map(|(id, _)| id.clone())
}

/// Scrolls the map to `left`, smoothly unless motion is reduced — and only
/// when there is anywhere to scroll, the map being wider than its window.
pub(super) fn follow_js(left: f64) -> String {
    format!(
        "(() => {{\
           const c = document.querySelector('.run-canvas');\
           if (!c || c.scrollWidth <= c.clientWidth + 1) return;\
           const calm = window.matchMedia('(prefers-reduced-motion: reduce)').matches;\
           c.scrollTo({{ left: {left}, behavior: calm ? 'auto' : 'smooth' }});\
         }})()"
    )
}

/// Listens on the window for ⌘+ / ⌘− / ⌘0 (Ctrl on other systems) and says
/// which was pressed. Only while a run map is on screen: in the list view the
/// keys are left to whatever else wants them. The handler is kept on
/// `window` and replaced on each mount, so two never stack.
pub(super) const ZOOM_KEYS_JS: &str = "\
    if (window._gaZoomKeys) window.removeEventListener('keydown', window._gaZoomKeys);\
    window._gaZoomKeys = (e) => {\
        if (!(e.metaKey || e.ctrlKey) || e.altKey) return;\
        if (!document.querySelector('.run-canvas')) return;\
        const k = e.key;\
        const what = (k === '=' || k === '+') ? 'in' : (k === '-' || k === '_') ? 'out' : (k === '0') ? 'reset' : null;\
        if (!what) return;\
        e.preventDefault();\
        dioxus.send(what);\
    };\
    window.addEventListener('keydown', window._gaZoomKeys);";

/// Keeps the point under the cursor — `at`, in window coordinates — where it
/// is while the map is redrawn at another zoom, or the middle of the visible
/// map when there is no cursor to follow (the − and + buttons).
///
/// Sent before the zoom changes, so it reads the scroll and the map's size as
/// they were; it then waits for the map to be redrawn and scrolls by however
/// much it actually grew or shrank, rather than by the zoom ratio, which the
/// layout does not follow exactly while the map still fits on screen.
pub(super) fn zoom_anchor_js(at: Option<(f64, f64)>) -> String {
    let (x, y) = match at {
        Some((x, y)) => (format!("{x} - r.left"), format!("{y} - r.top")),
        None => ("c.clientWidth / 2".into(), "c.clientHeight / 2".into()),
    };
    format!(
        "(() => {{\
           const c = document.querySelector('.run-canvas');\
           const s = c && c.querySelector('svg.run-svg');\
           if (!c || !s) return;\
           const r = c.getBoundingClientRect();\
           const px = {x}, py = {y};\
           const ox = c.scrollLeft + px, oy = c.scrollTop + py;\
           const b0 = s.getBoundingClientRect();\
           let tries = 0;\
           const settle = () => {{\
             const b1 = s.getBoundingClientRect();\
             if (Math.abs(b1.width - b0.width) < 0.5 && tries++ < 20) {{\
               requestAnimationFrame(settle); return;\
             }}\
             const kx = b0.width ? b1.width / b0.width : 1;\
             const ky = b0.height ? b1.height / b0.height : 1;\
             c.scrollLeft = ox * kx - px;\
             c.scrollTop = oy * ky - py;\
           }};\
           requestAnimationFrame(settle);\
         }})()"
    )
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Map {
    pub(super) stations: Vec<Station>,
    pub(super) edges: Vec<(String, String)>,
    pub(super) roots: Vec<String>,
    pub(super) leaves: Vec<String>,
    /// Distance between columns, which is also how wide a label may be.
    pub(super) col_w: f64,
    pub(super) start_x: f64,
    pub(super) end_x: f64,
    pub(super) width: f64,
    pub(super) height: f64,
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
pub(super) fn map(
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
pub(super) enum Fill {
    /// The step it leads to is waiting or under way.
    Moving,
    /// Both ends done.
    Arrived,
    /// The step it leads to was rejected.
    SentBack,
    Failed,
    /// The step it leads to will not run: skipped or blocked, or bypassed
    /// with nothing beyond it that will.
    Idle,
    /// Into a step you skipped, with the run under way beyond it: the train
    /// passes straight through, blue, and waits at the next real step
    /// instead of at the one it is not stopping at.
    Through,
}

impl Fill {
    /// Just the colour, for the dot riding this fill.
    pub(super) fn css_colour(self) -> &'static str {
        match self {
            Fill::Moving => "moving",
            Fill::Arrived => "arrived",
            Fill::SentBack => "back",
            Fill::Failed => "failed",
            Fill::Idle => "idle",
            Fill::Through => "moving",
        }
    }

    pub(super) fn css(self) -> &'static str {
        match self {
            Fill::Moving => "run-fill run-fill-moving",
            Fill::Arrived => "run-fill run-fill-arrived",
            Fill::SentBack => "run-fill run-fill-back",
            Fill::Failed => "run-fill run-fill-failed",
            Fill::Idle => "run-fill run-fill-idle",
            Fill::Through => "run-fill run-fill-moving",
        }
    }
}

/// How a fill reaches where it is going, which decides its animation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
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
    pub(super) fn of(fill: Fill, was_approaching: bool) -> Self {
        match fill {
            Fill::Moving => Phase::Approach,
            _ if was_approaching => Phase::Finish,
            _ => Phase::Draw,
        }
    }

    pub(super) fn css(self) -> &'static str {
        match self {
            Phase::Approach => "run-phase-approach",
            Phase::Finish => "run-phase-finish",
            Phase::Draw => "run-phase-draw",
        }
    }

    /// The dot at the head of the fill, if there is one. It runs on into the
    /// station only when the step there is done; a step that failed or was
    /// sent back stops it where it stood.
    pub(super) fn head(self, fill: Fill) -> Option<&'static str> {
        match (self, fill) {
            (Phase::Approach, _) => Some("run-dot run-dot-approach"),
            (Phase::Finish, Fill::Arrived | Fill::Through) => Some("run-dot run-dot-finish"),
            (Phase::Draw, Fill::Arrived | Fill::Through) => Some("run-dot run-dot-draw"),
            (Phase::Finish, _) => Some("run-dot run-dot-stop"),
            (Phase::Draw, _) => None,
        }
    }
}

/// `to` is `None` for the track into the end bar, which is reached as soon as
/// the step before it is done. `beyond` is, for a step you skipped, where the
/// run stands past it — see `beyond_skipped` — since the run does not stop
/// there and neither should the track.
pub(super) fn fill(
    departed: bool,
    to: Option<NodeStatus>,
    beyond: Option<NodeStatus>,
) -> Option<Fill> {
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
        Some(NodeStatus::Bypassed) => match beyond {
            Some(NodeStatus::Pending | NodeStatus::Running | NodeStatus::AwaitingApproval) => {
                Fill::Through
            }
            None | Some(NodeStatus::Done) => Fill::Arrived,
            Some(NodeStatus::Failed) => Fill::Failed,
            Some(NodeStatus::Rejected) => Fill::SentBack,
            _ => Fill::Idle,
        },
        Some(NodeStatus::Skipped | NodeStatus::Blocked) => Fill::Idle,
    })
}

/// Where the run stands past `id`, a step you skipped: the status of the
/// next steps that were not skipped too, following `edges` on through any
/// that were. The liveliest of them when it forks — under way, then failed,
/// then sent back, then done — and `None` when nothing follows, the end of
/// the line being reached.
pub(super) fn beyond_skipped(
    id: &str,
    edges: &[(String, String)],
    status: &dyn Fn(&str) -> NodeStatus,
) -> Option<NodeStatus> {
    fn rank(s: NodeStatus) -> u8 {
        match s {
            NodeStatus::Running | NodeStatus::AwaitingApproval | NodeStatus::Pending => 0,
            NodeStatus::Failed => 1,
            NodeStatus::Rejected => 2,
            NodeStatus::Done => 3,
            _ => 4,
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut queue = vec![id.to_string()];
    let mut found: Vec<NodeStatus> = vec![];
    while let Some(at) = queue.pop() {
        if !seen.insert(at.clone()) {
            continue;
        }
        for (_, next) in edges.iter().filter(|(from, _)| *from == at) {
            match status(next) {
                NodeStatus::Bypassed => queue.push(next.clone()),
                other => found.push(other),
            }
        }
    }
    found.into_iter().min_by_key(|s| rank(*s))
}

/// Up to `lines` lines of at most `width` characters, broken between words,
/// with an ellipsis if there was more. A monospace font is what makes a
/// character count a width.
pub(super) fn wrap(text: &str, width: usize, lines: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out: Vec<String> = vec![];
    let mut line = String::new();
    for word in text.split_whitespace().flat_map(|w| pieces(w, width)) {
        let fits = line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width;
        if !fits {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
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

/// A word too long for a line, in pieces that each fit: broken just after a
/// `/` or `-` where there is one — `feat/run-view-` and `branches` — so a
/// branch name is split where it reads naturally, and cut anywhere only when
/// it has nowhere better. Its end is usually what tells one branch from the
/// next, so it is kept rather than lost to an ellipsis.
pub(super) fn pieces(word: &str, width: usize) -> Vec<String> {
    let mut rest: Vec<char> = word.chars().collect();
    let mut out = vec![];
    while rest.len() > width {
        let cut = rest[..width]
            .iter()
            .rposition(|c| *c == '/' || *c == '-')
            .map(|i| i + 1)
            .filter(|i| *i > width / 3)
            .unwrap_or(width);
        out.push(rest.drain(..cut).collect());
    }
    out.push(rest.into_iter().collect());
    out
}

/// `from` is the step sent back, `to` where its track starts. The arc rises
/// as high as the gap allows: it must not reach the labels of a station above
/// that sits between the two ends.
pub(super) fn revise_loop(from: (f64, f64), to: (f64, f64), above: Option<f64>) -> Loop {
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

/// How wide a step's panel is over a map `area_w` wide, open or folded, at
/// the width it was dragged to if it was. Must agree with `.run-drawer`'s
/// width in the stylesheet: this is what the map slides out from under.
pub(super) fn panel_width(area_w: f64, folded: bool, chosen: Option<f64>) -> f64 {
    if folded {
        FOLDED_PANEL
    } else if let Some(w) = chosen {
        store::Layout::clamp_panel(w, area_w)
    } else {
        (area_w * 0.58).min(620.0)
    }
}
