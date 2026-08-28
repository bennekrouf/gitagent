//! Native OS notifications — Notification Center on macOS, toasts on Windows,
//! libnotify on Linux — for the moments a run stops and waits for a person.
//!
//! Same approach as ais-runner's `services::notifications`: `notify-rust`, with
//! the blocking OS call moved off the async runtime so a slow notification
//! daemon can never stall the executor that raised it.
//!
//! The rule about *when* to raise one is the interesting part, and it lives in
//! `should_notify`: a gated node parking, or a run failing, is only worth
//! interrupting someone over when they are not already looking at it.

use crate::services::graph::NodeStatus;

/// Whether a status change is worth a notification.
///
/// Two conditions, both necessary. The status has to be one that stops the run
/// and waits for a person — progress is not news. And the window has to be out
/// of sight: a notification for something already on screen is pure noise, and
/// the approval is right there.
pub fn should_notify(status: NodeStatus, focused: bool) -> bool {
    if focused {
        return false;
    }
    matches!(status, NodeStatus::AwaitingApproval | NodeStatus::Failed)
}

/// The words for a status. Kept separate from the OS call so it can be tested.
pub fn message(status: NodeStatus, repo: &str, node: &str, detail: &str) -> (String, String) {
    let detail = first_line(detail);
    match status {
        NodeStatus::AwaitingApproval => (
            format!("{repo} needs you"),
            if detail.is_empty() {
                format!("{node} is waiting for approval")
            } else {
                format!("{node} is waiting for approval — {detail}")
            },
        ),
        NodeStatus::Failed => (
            format!("{repo} failed"),
            if detail.is_empty() {
                format!("{node} did not finish")
            } else {
                format!("{node}: {detail}")
            },
        ),
        _ => (repo.to_string(), node.to_string()),
    }
}

/// Notification bodies are one or two lines on every platform, so a multi-line
/// error is trimmed to the line that says what happened.
fn first_line(text: &str) -> String {
    const CAP: usize = 120;
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() <= CAP {
        return line.to_string();
    }
    let head: String = line.chars().take(CAP).collect();
    format!("{head}…")
}

/// Whether the app is currently in front of the person using it.
///
/// Treated as focused when it cannot be determined: a missed notification is a
/// smaller failure than one that fires while you are looking at the thing.
pub fn window_focused() -> bool {
    let window = dioxus::desktop::window();
    let visible = window.is_visible() && !window.is_minimized();
    window.is_focused() && visible
}

/// Fire-and-forget. Never blocks the caller, never fails loudly — a machine
/// with notifications turned off should still run every flow.
pub fn raise(summary: String, body: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = notify_rust::Notification::new()
            .summary(&summary)
            .body(&body)
            .show()
        {
            eprintln!("desktop notification failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_behind_your_back_is_worth_interrupting_for() {
        assert!(should_notify(NodeStatus::AwaitingApproval, false));
        assert!(should_notify(NodeStatus::Failed, false));
    }

    #[test]
    fn nothing_fires_while_you_are_looking_at_it() {
        // The approval is on screen; a notification would be pure noise.
        assert!(!should_notify(NodeStatus::AwaitingApproval, true));
        assert!(!should_notify(NodeStatus::Failed, true));
    }

    #[test]
    fn progress_is_not_news() {
        for status in [
            NodeStatus::Pending,
            NodeStatus::Running,
            NodeStatus::Done,
            NodeStatus::Skipped,
            NodeStatus::Blocked,
        ] {
            assert!(!should_notify(status, false), "{status:?}");
        }
    }

    #[test]
    fn a_rejection_is_your_own_doing_and_needs_no_telling() {
        assert!(!should_notify(NodeStatus::Rejected, false));
    }

    #[test]
    fn an_approval_names_the_repository_and_the_step() {
        let (summary, body) = message(
            NodeStatus::AwaitingApproval,
            "ais-runner",
            "Commit",
            "git commit -m \"fix: thing\"",
        );
        assert_eq!(summary, "ais-runner needs you");
        assert!(body.contains("Commit is waiting for approval"));
    }

    #[test]
    fn a_failure_leads_with_the_reason() {
        let (summary, body) = message(
            NodeStatus::Failed,
            "ais-monitor",
            "Release",
            "The following paths are ignored by one of your .gitignore files:\nCargo.lock",
        );
        assert_eq!(summary, "ais-monitor failed");
        assert!(body.starts_with("Release: The following paths are ignored"));
        assert!(!body.contains('\n'), "one line only");
    }

    #[test]
    fn a_long_reason_is_cut_rather_than_dropped() {
        let (_, body) = message(NodeStatus::Failed, "r", "n", &"x".repeat(400));
        assert!(
            body.chars().count() < 140,
            "got {} chars",
            body.chars().count()
        );
        assert!(body.ends_with('…'));
    }

    #[test]
    fn an_empty_detail_still_says_something_useful() {
        let (_, body) = message(NodeStatus::AwaitingApproval, "r", "Push", "");
        assert_eq!(body, "Push is waiting for approval");
    }

    #[test]
    fn leading_blank_lines_do_not_become_the_message() {
        assert_eq!(first_line("\n\n  real reason  \nmore"), "real reason");
    }
}
