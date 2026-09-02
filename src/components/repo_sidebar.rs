//! The workspace's repositories, and where each one's run has got to.
//!
//! The dot is the point: with several repositories open, the thing you need to
//! see without clicking is which one is waiting on you.

use dioxus::prelude::*;

use crate::components::forge_icon::ForgeIcon;
use crate::services::forge::Forge;
use crate::services::graph::{NodeStatus, RunState};
use crate::services::probe::Wants;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Running,
    NeedsApproval,
    Done,
    /// Ran and found nothing to do.
    Nothing,
    /// Stopped because the person declined an approval. Deliberate, so it is
    /// not a failure and must not be reported as one.
    Declined,
    Failed,
}

impl Phase {
    pub fn css(self) -> &'static str {
        match self {
            Phase::Idle => "pending",
            Phase::Running => "running",
            Phase::NeedsApproval => "awaiting",
            Phase::Done => "done",
            Phase::Nothing => "skipped",
            Phase::Declined => "rejected",
            Phase::Failed => "failed",
        }
    }

    /// Whether this phase is about a run happening *now*.
    ///
    /// Only those are worth showing instead of what the repository needs. A
    /// finished run says "done" forever otherwise, hiding the release that
    /// became due the moment it landed.
    ///
    /// `Failed` is deliberately not one of them, though it used to be. A run
    /// that failed ten minutes ago is a fact about that run, not an answer to
    /// "what does this repository need now" — and leaving FAILED sitting in
    /// the one slot that answers that question is how a repository with a
    /// release due read as broken until something else was run. The failure is
    /// still shown, as a mark beside the name; see `left_a_failure`.
    pub fn is_live(self) -> bool {
        matches!(self, Phase::Running | Phase::NeedsApproval)
    }

    /// Whether the last run here ended badly and nothing has been run since.
    /// Worth a mark, never worth the whole slot.
    pub fn left_a_failure(self) -> bool {
        matches!(self, Phase::Failed)
    }

    pub fn note(self) -> &'static str {
        match self {
            Phase::Idle => "",
            Phase::Running => "running",
            Phase::NeedsApproval => "needs you",
            Phase::Done => "done",
            Phase::Nothing => "nothing to do",
            Phase::Declined => "declined",
            Phase::Failed => "failed",
        }
    }

    /// A glyph for the live phases, so the eye can sort a column of rows
    /// without reading any of them.
    pub fn icon(self) -> &'static str {
        match self {
            Phase::NeedsApproval => "\u{25c6}",
            Phase::Running => "\u{25b8}",
            _ => "",
        }
    }

    /// Lower sorts first. Used to pick the single phase to show for a
    /// repository that has several PR-scoped runs going at once — a person
    /// being waited on always wins, the same precedence `phase_of` already
    /// applies within one run.
    pub fn priority(self) -> u8 {
        match self {
            Phase::NeedsApproval => 0,
            Phase::Failed => 1,
            Phase::Running => 2,
            Phase::Nothing => 3,
            Phase::Declined => 4,
            Phase::Done => 5,
            Phase::Idle => 6,
        }
    }
}

/// One repository's run, reduced to the single thing worth showing in a list.
/// Approval outranks everything — it is the only state that is blocked on the
/// person looking at the screen.
pub fn phase_of(state: &RunState) -> Phase {
    if !state.started {
        return Phase::Idle;
    }
    let statuses: Vec<NodeStatus> = state.runs.values().map(|r| r.status).collect();
    if statuses.contains(&NodeStatus::AwaitingApproval) {
        Phase::NeedsApproval
    } else if statuses.contains(&NodeStatus::Running) {
        Phase::Running
    } else if statuses.contains(&NodeStatus::Failed) {
        Phase::Failed
    } else if statuses.contains(&NodeStatus::Rejected) {
        Phase::Declined
    } else if statuses.contains(&NodeStatus::Skipped) {
        Phase::Nothing
    } else if statuses.iter().all(|s| s.is_terminal()) {
        Phase::Done
    } else {
        Phase::Running
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct RepoEntry {
    pub path: String,
    pub label: String,
    /// What the repository is waiting for, or `None` while still probing.
    pub wants: Option<Wants>,
    /// The short qualifier beside it — a PR number, or a file count.
    pub detail: String,
    /// `None` until the remote has been read.
    pub forge: Option<Forge>,
    /// The checked-out branch, shown under the repo name whenever it is
    /// known — which branch a repository is parked on is worth seeing at a
    /// glance, protected or not.
    pub branch: String,
    pub phase: Phase,
    /// Commits on `branch` not yet on its upstream, and vice versa. Both `0`
    /// when there's nothing to report — no upstream, or fully caught up.
    pub ahead: usize,
    pub behind: usize,
    /// Every open PR on the repository, branch-independent — `wants`/`note`
    /// above only ever reflect the checked-out branch's own PR, so a repo
    /// sitting on `main` with three open PRs elsewhere would otherwise show
    /// nothing here at all.
    pub open_pr_count: usize,
    /// Set when checking for open PRs actually failed (rate limit, network,
    /// auth) — shown instead of the count badge, so a check that never
    /// happened never looks identical to a repository that is truly clean.
    pub prs_error: Option<String>,
}

#[derive(Props, Clone, PartialEq)]
pub struct RepoSidebarProps {
    pub entries: Vec<RepoEntry>,
    pub selected: Option<String>,
    pub workspace: String,
    /// How many probes are still outstanding.
    pub probing: usize,
    pub on_select: EventHandler<String>,
    pub on_refresh: EventHandler<()>,
    pub on_change_workspace: EventHandler<()>,
    /// Re-read one repository, without disturbing the others.
    pub on_reprobe: EventHandler<String>,
    #[props(default = 224.0)]
    pub width: f64,
}

#[component]
pub fn RepoSidebar(props: RepoSidebarProps) -> Element {
    let selected = props.selected.clone();

    rsx! {
        div { class: "sidebar", style: "width: {props.width}px;",
            div { class: "sidebar-head",
                div { class: "sidebar-title", "Repositories" }
                div { class: "sidebar-actions",
                    button {
                        class: if props.probing > 0 { "sidebar-switch spinning" } else { "sidebar-switch" },
                        disabled: props.probing > 0,
                        title: "Re-check every repository",
                        onclick: move |_| props.on_refresh.call(()),
                        "⟳"
                    }
                    button {
                        class: "sidebar-switch",
                        title: "Open this folder in a new window",
                        onclick: {
                            let workspace = props.workspace.clone();
                            move |_| crate::open_in_new_window(workspace.clone())
                        },
                        "⧉"
                    }
                    button {
                        class: "sidebar-switch",
                        title: "Open a different folder in this window",
                        onclick: move |_| props.on_change_workspace.call(()),
                        "⇄"
                    }
                }
            }
            div { class: "sidebar-path", "{props.workspace}" }

            div { class: "sidebar-list",
                if props.entries.is_empty() {
                    div { class: "sidebar-empty", "No git repositories in this folder." }
                } else {
                    for entry in props.entries.iter().cloned() {
                        div {
                            key: "{entry.path}",
                            class: if selected.as_deref() == Some(entry.path.as_str()) {
                                "sidebar-row sidebar-row-on"
                            } else {
                                "sidebar-row"
                            },
                            title: "{entry.path}",
                            onclick: {
                                let path = entry.path.clone();
                                move |_| props.on_select.call(path.clone())
                            },
                            span {
                            class: if !entry.phase.is_live() {
                                format!("dot dot-{}", entry.wants.map(|w| w.css()).unwrap_or("pending"))
                            } else {
                                format!("dot dot-{}", entry.phase.css())
                            },
                        }
                            match entry.forge.clone() {
                                Some(forge) => rsx! { ForgeIcon { forge } },
                                None => rsx! { span { class: "forge-icon-gap" } },
                            }
                            span { class: "sidebar-main",
                                span { class: "sidebar-label-row",
                                    span { class: "sidebar-label", "{entry.label}" }
                                    if entry.phase.left_a_failure() {
                                        span {
                                            class: "run-failed-mark",
                                            title: "The last run on this repository failed \u{2014} open it to see where",
                                            "\u{2715}"
                                        }
                                    }
                                    if let Some(err) = entry.prs_error.clone() {
                                        span {
                                            class: "pr-count-badge pr-count-error",
                                            title: "Couldn't check for open pull requests: {err}",
                                            "!"
                                        }
                                    } else if entry.open_pr_count > 0 {
                                        span {
                                            class: "pr-count-badge",
                                            title: "{entry.open_pr_count} open pull request(s) — select this repo, then Review \u{2192} Merge, to pick one",
                                            "{entry.open_pr_count} PR" if entry.open_pr_count != 1 { "s" }
                                        }
                                    }
                                }
                                if !entry.branch.is_empty() || entry.ahead > 0 || entry.behind > 0 {
                                    span { class: "sidebar-branch",
                                        if !entry.branch.is_empty() {
                                            "⑂ {entry.branch} "
                                        }
                                        if entry.ahead > 0 {
                                            span { class: "sync-badge sync-ahead", title: "{entry.ahead} commit(s) not pushed", "↑{entry.ahead}" }
                                        }
                                        if entry.behind > 0 {
                                            span { class: "sync-badge sync-behind", title: "{entry.behind} commit(s) not pulled", "↓{entry.behind}" }
                                        }
                                    }
                                }
                            }
                            // The status slot, and — on hover — the button that
                            // re-reads this one repository instead of all of
                            // them. Re-checking after doing something in a
                            // terminal is the commonest reason to touch this
                            // list at all, and the only way to do it used to be
                            // a refresh of every repository in the folder.
                            span { class: "row-status",
                                // A run in progress outranks anything the probe found:
                                // it is more recent, and it is already yours.
                                if entry.phase.is_live() {
                                    span { class: "sidebar-note status-{entry.phase.css()}",
                                        span { class: "note-icon", "{entry.phase.icon()}" }
                                        "{entry.phase.note()}"
                                    }
                                } else {
                                    match entry.wants {
                                        // Nothing to say is said with nothing. The dot
                                        // still carries "clean" and "checks running".
                                        Some(wants) if wants.is_worth_saying() => rsx! {
                                            if !entry.detail.is_empty() {
                                                span { class: "sidebar-detail", "{entry.detail}" }
                                            }
                                            span { class: "sidebar-note status-{wants.css()}",
                                                span { class: "note-icon", "{wants.icon()}" }
                                                "{wants.note()}"
                                            }
                                        },
                                        Some(_) => rsx! {},
                                        None => rsx! { span { class: "sidebar-clean", "\u{2026}" } },
                                    }
                                }
                            }
                            button {
                                class: "row-reprobe",
                                title: "Re-check this repository",
                                onclick: {
                                    let path = entry.path.clone();
                                    move |evt: Event<MouseData>| {
                                        // Otherwise this also selects the row,
                                        // which is not what a refresh means.
                                        evt.stop_propagation();
                                        props.on_reprobe.call(path.clone());
                                    }
                                },
                                "\u{27f3}"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::flow::commit_and_pr_flow;

    fn started() -> RunState {
        let mut s = RunState::fresh(&commit_and_pr_flow());
        s.started = true;
        s
    }

    #[test]
    fn a_person_being_waited_on_outranks_a_failure_or_a_run_in_progress() {
        assert!(Phase::NeedsApproval.priority() < Phase::Failed.priority());
        assert!(Phase::Failed.priority() < Phase::Running.priority());
        assert!(Phase::Running.priority() < Phase::Idle.priority());
    }

    #[test]
    fn a_run_that_never_started_is_idle() {
        assert_eq!(
            phase_of(&RunState::fresh(&commit_and_pr_flow())),
            Phase::Idle
        );
    }

    #[test]
    fn waiting_on_a_person_outranks_everything_else() {
        let mut s = started();
        s.set_status("preflight", NodeStatus::Done);
        s.set_status("scan", NodeStatus::Running);
        s.set_status("commit", NodeStatus::AwaitingApproval);
        assert_eq!(phase_of(&s), Phase::NeedsApproval);
    }

    #[test]
    fn a_failure_anywhere_marks_the_repository_failed() {
        let mut s = started();
        s.set_status("preflight", NodeStatus::Failed);
        s.propagate_block(&commit_and_pr_flow());
        assert_eq!(phase_of(&s), Phase::Failed);
    }

    #[test]
    fn every_node_done_is_done() {
        let mut s = started();
        for node in commit_and_pr_flow().nodes {
            s.set_status(&node.id, NodeStatus::Done);
        }
        assert_eq!(phase_of(&s), Phase::Done);
    }

    #[test]
    fn a_clean_tree_reads_as_nothing_to_do_not_as_a_failure() {
        let mut s = started();
        s.set_status("preflight", NodeStatus::Done);
        s.set_status("scan", NodeStatus::Skipped);
        s.propagate_block(&commit_and_pr_flow());
        assert_eq!(phase_of(&s), Phase::Nothing);
    }

    #[test]
    fn declining_an_approval_is_not_a_failure() {
        // Saying no is a deliberate stop. Reporting it as "failed" sends you
        // hunting for a broken task that was never broken.
        let mut s = started();
        for node in commit_and_pr_flow().nodes {
            s.set_status(&node.id, NodeStatus::Done);
        }
        s.set_status("push", NodeStatus::Rejected);
        assert_eq!(phase_of(&s), Phase::Declined);
        assert!(
            !phase_of(&s).is_live(),
            "settled, so it never hides what is due"
        );
    }

    #[test]
    fn a_failure_is_marked_but_never_takes_the_status_slot() {
        // It is still a failure, and still shown. What it must not do is sit
        // in the one place that answers "what does this repository need now",
        // where it stayed until something else was run — long after the tree
        // had moved on and a release had come due.
        let mut s = started();
        s.set_status("preflight", NodeStatus::Failed);
        s.propagate_block(&commit_and_pr_flow());
        assert_eq!(phase_of(&s), Phase::Failed);
        assert!(phase_of(&s).left_a_failure());
        assert!(
            !phase_of(&s).is_live(),
            "so the row still shows what is due"
        );
    }

    #[test]
    fn the_two_resting_states_are_said_with_nothing() {
        // A column where every clean repository says CLEAN is one you have to
        // read to find the one that does not.
        use crate::services::probe::Wants;
        assert!(!Wants::Nothing.is_worth_saying());
        assert!(!Wants::Wait.is_worth_saying());
        for wants in [
            Wants::Resolve,
            Wants::Merge,
            Wants::Attention,
            Wants::Commit,
            Wants::OpenPr,
            Wants::Release,
        ] {
            assert!(wants.is_worth_saying(), "{wants:?} is work someone must do");
            assert!(!wants.icon().is_empty(), "{wants:?} needs a glyph");
        }
    }

    #[test]
    fn a_failure_outranks_a_decline_when_both_are_present() {
        let mut s = started();
        s.set_status("commit", NodeStatus::Rejected);
        s.set_status("draft_pr", NodeStatus::Failed);
        assert_eq!(phase_of(&s), Phase::Failed);
    }
}
