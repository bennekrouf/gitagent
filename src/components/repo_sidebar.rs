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
            Phase::Failed => "failed",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            Phase::Idle => "",
            Phase::Running => "running",
            Phase::NeedsApproval => "needs you",
            Phase::Done => "done",
            Phase::Nothing => "nothing to do",
            Phase::Failed => "failed",
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
            Phase::Done => 4,
            Phase::Idle => 5,
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
    /// The checked-out branch. Shown only when it is not a default one —
    /// "on master" is the assumption, so it is not worth a line; being parked
    /// on a topic branch is the thing you want to notice.
    pub branch: String,
    pub phase: Phase,
    /// Commits on `branch` not yet on its upstream, and vice versa. Both `0`
    /// when there's nothing to report — no upstream, or fully caught up.
    pub ahead: usize,
    pub behind: usize,
}

impl RepoEntry {
    pub fn shows_branch(&self) -> bool {
        !self.branch.is_empty() && !crate::services::git::is_protected(&self.branch)
    }
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
                            class: if entry.phase == Phase::Idle {
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
                                span { class: "sidebar-label", "{entry.label}" }
                                if entry.shows_branch() || entry.ahead > 0 || entry.behind > 0 {
                                    span { class: "sidebar-branch",
                                        if entry.shows_branch() {
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
                            // A run in progress outranks anything the probe found:
                            // it is more recent, and it is already yours.
                            if !entry.phase.note().is_empty() {
                                span { class: "sidebar-note status-{entry.phase.css()}", "{entry.phase.note()}" }
                            } else {
                                match entry.wants {
                                    Some(wants) => rsx! {
                                        if !entry.detail.is_empty() {
                                            span { class: "sidebar-detail", "{entry.detail}" }
                                        }
                                        span { class: "sidebar-note status-{wants.css()}", "{wants.note()}" }
                                    },
                                    None => rsx! { span { class: "sidebar-clean", "…" } },
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

    #[test]
    fn a_person_being_waited_on_outranks_a_failure_or_a_run_in_progress() {
        assert!(Phase::NeedsApproval.priority() < Phase::Failed.priority());
        assert!(Phase::Failed.priority() < Phase::Running.priority());
        assert!(Phase::Running.priority() < Phase::Idle.priority());
    }

    fn started() -> RunState {
        let mut s = RunState::fresh(&commit_and_pr_flow());
        s.started = true;
        s
    }

    fn entry(branch: &str) -> RepoEntry {
        RepoEntry {
            path: "/p".into(),
            label: "r".into(),
            wants: None,
            detail: String::new(),
            forge: None,
            branch: branch.into(),
            phase: Phase::Idle,
            ahead: 0,
            behind: 0,
        }
    }

    #[test]
    fn a_default_branch_is_not_worth_a_line() {
        for name in ["master", "main", "develop"] {
            assert!(!entry(name).shows_branch(), "{name}");
        }
    }

    #[test]
    fn a_topic_branch_is_worth_noticing() {
        assert!(entry("feat/US-14932-ignite-api").shows_branch());
        assert!(entry("chore/enforce-rustfmt").shows_branch());
    }

    #[test]
    fn an_unprobed_repository_shows_no_branch_at_all() {
        assert!(!entry("").shows_branch());
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
    fn a_rejected_node_reads_as_failed_not_as_done() {
        let mut s = started();
        for node in commit_and_pr_flow().nodes {
            s.set_status(&node.id, NodeStatus::Done);
        }
        s.set_status("push", NodeStatus::Rejected);
        assert_eq!(phase_of(&s), Phase::Failed);
    }
}
