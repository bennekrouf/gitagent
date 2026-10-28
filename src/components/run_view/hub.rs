//! The review hub: the lenses around a pull request under review.

use super::*;

/// How a reviewer, or the review as a whole, stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub(super) enum Tone {
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
    pub(super) fn css(self) -> &'static str {
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

/// The review circle: the reviews a pull request gets in this flow, around
/// the steps that do the reviewing. Only what the flow really does — you
/// reading the diff, the model's analysis, a second model's opinion, the CI
/// checks — plus a Fix spoke once anything in the review has been sent back.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct Hub {
    /// The steps that do the reviewing — the analysis, and the second opinion
    /// when there is one. The circle hangs from whichever sits lowest on the
    /// map, so its line never runs down through the other.
    pub(super) anchors: Vec<String>,
    /// Which step the circle itself opens.
    pub(super) anchor: String,
    pub(super) spokes: Vec<Spoke>,
    pub(super) tone: Tone,
}

/// Where spoke `i` of `n` points, in degrees, clockwise from the right.
///
/// Only out to the sides: the first half down the left, the rest down the
/// right, each between 75° above and below the horizontal. Labels then always
/// read outward, one above another, rather than two near the top or bottom
/// colliding — and the top stays clear for the line down from the map.
pub(super) fn spoke_angle(i: usize, n: usize) -> f64 {
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
pub(super) fn review_hub(graph: &Graph, state: &RunState, lenses: &Lenses) -> Option<Hub> {
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
    // "gpt-4o-mini" says more than "second model" does.
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
