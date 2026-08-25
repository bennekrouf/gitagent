//! The workspace's repositories, and where each one's run has got to.
//!
//! The dot is the point: with several repositories open, the thing you need to
//! see without clicking is which one is waiting on you.

use dioxus::prelude::*;

use crate::components::forge_icon::ForgeIcon;
use crate::services::forge::Forge;
use crate::services::graph::{NodeStatus, RunState};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Running,
    NeedsApproval,
    Done,
    Failed,
}

impl Phase {
    pub fn css(self) -> &'static str {
        match self {
            Phase::Idle => "pending",
            Phase::Running => "running",
            Phase::NeedsApproval => "awaiting",
            Phase::Done => "done",
            Phase::Failed => "failed",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            Phase::Idle => "",
            Phase::Running => "running",
            Phase::NeedsApproval => "needs you",
            Phase::Done => "done",
            Phase::Failed => "failed",
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
    } else if statuses
        .iter()
        .any(|s| matches!(s, NodeStatus::Failed | NodeStatus::Rejected))
    {
        Phase::Failed
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
    /// Uncommitted file count, or `None` while it is still being counted.
    pub changes: Option<usize>,
    /// `None` until the remote has been read.
    pub forge: Option<Forge>,
    pub phase: Phase,
}

#[derive(Props, Clone, PartialEq)]
pub struct RepoSidebarProps {
    pub entries: Vec<RepoEntry>,
    pub selected: Option<String>,
    pub workspace: String,
    pub on_select: EventHandler<String>,
    pub on_change_workspace: EventHandler<()>,
}

#[component]
pub fn RepoSidebar(props: RepoSidebarProps) -> Element {
    let selected = props.selected.clone();

    rsx! {
        div { class: "sidebar",
            div { class: "sidebar-head",
                div { class: "sidebar-title", "Repositories" }
                div { class: "sidebar-actions",
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
                            onclick: {
                                let path = entry.path.clone();
                                move |_| props.on_select.call(path.clone())
                            },
                            span { class: "dot dot-{entry.phase.css()}" }
                            match entry.forge.clone() {
                                Some(forge) => rsx! { ForgeIcon { forge } },
                                None => rsx! { span { class: "forge-icon-gap" } },
                            }
                            span { class: "sidebar-label", "{entry.label}" }
                            if !entry.phase.note().is_empty() {
                                span { class: "sidebar-note status-{entry.phase.css()}", "{entry.phase.note()}" }
                            } else {
                                match entry.changes {
                                    Some(0) => rsx! { span { class: "sidebar-clean", "clean" } },
                                    Some(n) => rsx! { span { class: "sidebar-changes", "{n}" } },
                                    None => rsx! {},
                                }
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
    fn a_rejected_node_reads_as_failed_not_as_done() {
        let mut s = started();
        for node in commit_and_pr_flow().nodes {
            s.set_status(&node.id, NodeStatus::Done);
        }
        s.set_status("push", NodeStatus::Rejected);
        assert_eq!(phase_of(&s), Phase::Failed);
    }
}
