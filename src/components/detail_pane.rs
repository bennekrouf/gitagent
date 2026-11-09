//! The right-hand pane: one node's contract, its output, and — when the node
//! is gated — exactly what it will do and the two buttons that decide.

use dioxus::prelude::*;

use crate::components::conflict_view::ConflictView;
use crate::components::diff_view::DiffView;
use crate::services::graph::{NodeRun, NodeSpec, NodeStatus};

mod parts;

#[derive(Props, Clone, PartialEq)]
pub struct DetailPaneProps {
    pub spec: Option<NodeSpec>,
    pub run: NodeRun,
    /// The repository the step runs in — where a stuck merge is resolved.
    pub repo: String,
    /// The node's own diff artifact (`diff` or `pr_diff`), when it wrote
    /// one — shown as a highlighted diff instead of the plain output log.
    #[props(default)]
    pub diff: Option<String>,
    pub is_light: bool,
    /// Whether this node's run has actually begun. A node in a flow nobody has
    /// started is `Pending` too, and offering to skip it there would write to
    /// state that `Start` throws away.
    pub run_started: bool,
    pub on_approve: EventHandler<String>,
    pub on_reject: EventHandler<String>,
    /// `(node id, item key)` — flips one item in and out of the approval.
    pub on_toggle: EventHandler<(String, String)>,
    /// `(node id, remedy index)` — runs one offered fix.
    pub on_remedy: EventHandler<(String, usize)>,
    /// Re-queues a settled node and everything blocked behind it.
    pub on_retry: EventHandler<String>,
    /// Marks a node bypassed and lets the run carry on without it. Offered at
    /// an approval and after a failure, because both are places a step can
    /// stand between you and work that does not actually depend on it.
    pub on_skip: EventHandler<String>,
    /// Abandons the whole run for this repo+flow — for a failure retrying
    /// can never fix (the branch this was about already merged, the PR
    /// already exists under a different run). Puts the flow back to never
    /// having started, the same state a fresh repository would show.
    pub on_cancel: EventHandler<()>,
    /// Set when the step belongs to a run started elsewhere — by Claude
    /// through the MCP server, or in a terminal — and says where it is
    /// answered. The step is then only read here: no button decides it.
    #[props(default)]
    pub elsewhere: Option<String>,
    /// With `elsewhere`: the run is Claude's, and its approval may still be
    /// answered here — Approve and Reject show beside the note.
    #[props(default)]
    pub answerable: bool,
}

#[component]
pub fn DetailPane(props: DetailPaneProps) -> Element {
    let Some(spec) = props.spec.clone() else {
        return rsx! {
            div { class: "detail detail-empty", "Select a node to see its contract and output." }
        };
    };

    let run = props.run.clone();
    let awaiting = run.status == NodeStatus::AwaitingApproval;
    let approve_id = spec.id.clone();
    let reject_id = spec.id.clone();
    let toggle_id = spec.id.clone();
    let remedy_id = spec.id.clone();
    let retry_id = spec.id.clone();
    let skip_id = spec.id.clone();
    let skip_failed_id = spec.id.clone();
    let early_skip_id = spec.id.clone();
    let diff_id = spec.id.clone();
    // What this node would have written, named at the button, because those
    // are exactly the artifacts every later step will read as empty.
    let skip_cost = if spec.writes.is_empty() {
        "Nothing downstream reads anything from this step.".to_string()
    } else {
        format!("Later steps will read {} as empty.", spec.writes.join(", "))
    };
    let failed = matches!(run.status, NodeStatus::Failed | NodeStatus::Rejected);
    // Skip is not only for a step that has stopped to ask or has fallen over.
    // An ungated step that simply takes too long — a model call against a diff
    // it cannot chew through — never reaches either of those states, so
    // without this the one step most worth skipping is the one you cannot.
    let skippable_early =
        props.run_started && matches!(run.status, NodeStatus::Pending | NodeStatus::Running);
    let running_now = run.status == NodeStatus::Running;
    let nothing_selected = run.has_nothing_selected();
    let chosen = run.items.iter().filter(|i| i.included).count();
    let elsewhere = props.elsewhere.clone();
    let read_only = elsewhere.is_some();
    // Collapsed by default — the count already says "all N selected" at a
    // glance, and most approvals want everything committed. The list is one
    // click away for the times a subset needs picking.
    // Open. The file list only exists on a node that has stopped to ask
    // whether to commit these files, so folding away the one thing the
    // question is about — behind an unlabelled "…" — meant the answer to "how
    // do I choose the files" was a control nobody could find.
    let show_files = use_signal(|| true);
    // The files git is still waiting on, as the resolver last read them —
    // what holds back a fix that finishes the merge.
    let mut unresolved = use_signal(Vec::<String>::new);
    // Bumped by "Check again", for a file settled outside the app.
    let rechecks = use_signal(|| 0u32);
    // Anything a fix or a retry changes, so the conflict list is read again.
    let stamp = format!(
        "{:?}{}{}",
        run.status,
        rechecks(),
        run.remedies
            .iter()
            .map(|r| format!("{}{}{}", r.running, r.done, r.failed))
            .collect::<String>()
    );

    rsx! {
        div { class: "detail",
            // Everything but the buttons scrolls; the buttons sit below it.
            div { class: "detail-body",
            div { class: "detail-head",
                span { class: "dot dot-{run.status.css()}" }
                span { class: "detail-title", "{spec.title}" }
                span { class: "detail-status status-{run.status.css()}", "{run.status.label()}" }
            }

            if !run.approved_by.is_empty() {
                div { class: "approval-auto",
                    "Approved automatically by {run.approved_by}: you were not asked before this ran."
                }
            }

            {parts::contract(&spec)}

            // A failure over files git is still waiting on is settled here,
            // above the message about it. Shows nothing while nothing is stuck.
            if failed {
                ConflictView {
                    repo: props.repo.clone(),
                    stamp,
                    on_left: move |left: Vec<String>| unresolved.set(left),
                }
            }

            if awaiting {
                {parts::approval(&run, toggle_id.clone(), chosen, read_only, props.is_light, props.on_toggle, show_files)}
            }

            if let Some(diff) = props.diff.clone().filter(|d| !d.trim().is_empty()) {
                {
                    // A step that offered files (scan does) lets them be
                    // dropped here, against the diff — which is the only place
                    // the contents are visible to judge by.
                    let selectable = !run.items.is_empty();
                    let excluded: Vec<String> = run
                        .items
                        .iter()
                        .filter(|i| !i.included)
                        .map(|i| i.key.clone())
                        .collect();
                    let node = diff_id.clone();
                    rsx! {
                        div { class: "log-head",
                            span { "Diff" }
                            if selectable {
                                span { class: "items-count",
                                    "{chosen} of {run.items.len()} files selected"
                                }
                            }
                        }
                        div { class: "log-wrap log-wrap-diff",
                            DiffView {
                                diff: diff.clone(),
                                is_light: props.is_light,
                                excluded,
                                on_toggle: (selectable && !read_only).then(|| {
                                    let on_toggle = props.on_toggle;
                                    EventHandler::new(move |path: String| {
                                        on_toggle.call((node.clone(), path))
                                    })
                                }),
                            }
                            CopyButton { text: diff }
                        }
                    }
                }
            } else if !run.log.is_empty() {
                div { class: "log-head", if run.status == NodeStatus::Failed { "Error" } else { "Output" } }
                LogBox { log: run.log.clone(), failed: run.status == NodeStatus::Failed }
            }
            }

            // The buttons that decide a step, below the scrolling body rather
            // than inside it: always on screen however long the diff or log,
            // with no `position: sticky` for a webview to get wrong.
            if let (Some(note), true, true) = (elsewhere.clone(), awaiting, props.answerable) {
                div { class: "detail-actions",
                    span { class: "detail-elsewhere", "{note}" }
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| props.on_approve.call(approve_id.clone()),
                        "Approve"
                    }
                    button {
                        class: "btn btn-danger",
                        onclick: move |_| props.on_reject.call(reject_id.clone()),
                        "Reject"
                    }
                }
            } else if let Some(note) = elsewhere {
                if awaiting || failed || skippable_early {
                    div { class: "detail-actions detail-elsewhere", "{note}" }
                }
            } else if awaiting {
                div { class: "detail-actions",
                    button {
                        class: "btn btn-primary",
                        disabled: nothing_selected,
                        onclick: move |_| props.on_approve.call(approve_id.clone()),
                        if nothing_selected { "Nothing selected" } else { "Approve" }
                    }
                    button {
                        class: "btn btn-danger",
                        onclick: move |_| props.on_reject.call(reject_id.clone()),
                        "Reject"
                    }
                    button {
                        class: "btn btn-ghost",
                        title: "Don't run this step, and carry on anyway. Unlike Reject, \
                                which stops everything behind it. {skip_cost}",
                        onclick: move |_| props.on_skip.call(skip_id.clone()),
                        "Skip"
                    }
                }
            } else if failed {
                // The fixes on offer, then the step's own choices — all of it
                // in the panel's footer, where every decision on a step is.
                div { class: "detail-foot",
                    {parts::remedies(&run, remedy_id.clone(), props.on_remedy, unresolved, rechecks)}

                div { class: "detail-actions",
                    button {
                        class: "btn",
                        onclick: move |_| props.on_retry.call(retry_id.clone()),
                        "Retry this step"
                    }
                    button {
                        class: "btn",
                        title: "Carry on without this step — for one that is failing on \
                                something the rest of the run does not need. {skip_cost}",
                        onclick: move |_| props.on_skip.call(skip_failed_id.clone()),
                        "Skip this step"
                    }
                    button {
                        class: "btn btn-ghost",
                        title: "Give up on this run — for a failure retrying can't fix \
                                 (already merged, already open elsewhere). Puts this repo's \
                                 flow back to not-started.",
                        onclick: move |_| props.on_cancel.call(()),
                        "Cancel run"
                    }
                }
                }
            } else if skippable_early {
                div { class: "detail-actions",
                    button {
                        class: "btn",
                        title: if running_now {
                            "Stop waiting for this step and carry on without it. Whatever it \
                             has already started is not undone. {skip_cost}"
                        } else {
                            "Don't run this step when the run reaches it. {skip_cost}"
                        },
                        onclick: move |_| props.on_skip.call(early_skip_id.clone()),
                        if running_now { "Skip this step" } else { "Skip when reached" }
                    }
                }
            }
        }
    }
}

/// A step's output, following its tail as lines stream in — unless you have
/// scrolled up to read something, in which case it stays put until you scroll
/// back to the bottom.
#[component]
fn LogBox(log: String, failed: bool) -> Element {
    use_effect(use_reactive!(|log| {
        let _ = log;
        document::eval(
            "const el=document.getElementById('node-log');\
             if(el){if(!el._gaStick){el._gaStick=true;\
             el.addEventListener('scroll',()=>{\
             el.dataset.stick=(el.scrollHeight-el.scrollTop-el.clientHeight<24)?'1':'0';});}\
             if(el.dataset.stick!=='0'){el.scrollTop=el.scrollHeight;}}",
        );
    }));
    rsx! {
        div { class: "log-wrap",
            pre {
                id: "node-log",
                class: if failed { "log log-error" } else { "log" },
                "{log}"
            }
            CopyButton { text: log.clone() }
        }
    }
}

/// Copies a log or diff to the clipboard from the corner of its box, and says so for a
/// moment. `navigator.clipboard` is not granted in every webview, so the old
/// select-and-copy route backs it up.
#[component]
pub fn CopyButton(
    text: String,
    #[props(default = "btn log-copy".to_string())] class: String,
    #[props(default = "Copy".to_string())] label: String,
) -> Element {
    let mut copied = use_signal(|| false);
    rsx! {
        button {
            class: "{class}",
            title: "Copy to clipboard",
            onclick: move |_| {
                let literal = serde_json::to_string(&text).unwrap_or_default();
                document::eval(&format!(
                    "const t={literal};\
                     const fallback=()=>{{const a=document.createElement('textarea');\
                     a.value=t;a.style.position='fixed';a.style.opacity='0';\
                     document.body.appendChild(a);a.select();\
                     document.execCommand('copy');a.remove();}};\
                     if(navigator.clipboard){{navigator.clipboard.writeText(t).catch(fallback);}}\
                     else{{fallback();}}"
                ));
                copied.set(true);
                spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    copied.set(false);
                });
            },
            if copied() { "Copied" } else { "{label}" }
        }
    }
}
