//! The head of a run: its title, its progress, and the player buttons.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn run_head(
    mut cursor: Signal<Option<(u64, usize)>>,
    mut playing: Signal<bool>,
    mut epoch: Signal<u32>,
    len: usize,
    this_run: u64,
    at: Option<usize>,
    live: bool,
    fresh: bool,
    startable: bool,
    started: bool,
    total: usize,
    done: usize,
    waiting: bool,
    finished: bool,
    props: RunViewProps,
) -> Element {
    rsx! {
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
            div { class: "run-actions", {props.actions} }
            div { class: "run-player",
                // Two buttons, not one that changes its label: the one
                // in effect stays pressed, so the state reads at a glance.
                if startable {
                    // Nothing has run, or all of it has, so there is
                    // nothing to play back: Play starts the flow, and
                    // stops at every approval just as Start in the list
                    // view does.
                    button {
                        class: "run-ctl",
                        disabled: !props.can_start,
                        title: match (props.can_start, fresh) {
                            (false, _) => props.start_note.clone(),
                            (true, true) => "Start this flow \u{2014} the same as Start in the list view. It stops for you at every approval.".to_string(),
                            (true, false) => "Run this flow again from the start \u{2014} the same as Start in the list view. Restart replays the run that finished.".to_string(),
                        },
                        onclick: move |_| props.on_start.call(()),
                        // Plain "Play" says nothing about what is about to
                        // happen; the list view's words do.
                        if props.start_label.is_empty() {
                            "\u{25b6} Play"
                        } else {
                            "\u{25b6} {props.start_label}"
                        }
                    }
                    button {
                        class: "run-ctl",
                        disabled: true,
                        title: if fresh { "Nothing is running yet." } else { "Nothing is running: the run has finished." },
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
    }
}
