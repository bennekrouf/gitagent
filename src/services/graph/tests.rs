#[test]
fn a_step_names_the_real_branch_once_the_run_knows_it() {
    let mut state = RunState::default();
    assert_eq!(
        live_subtitle(Step::Push, "git push -u origin <branch>", &state),
        "git push -u origin <branch>",
        "the catalogue's words until then"
    );
    state
        .artifacts
        .insert("branch_name".into(), "feat/run-diff".into());
    state.artifacts.insert("base".into(), "main".into());
    assert_eq!(
        live_subtitle(Step::Push, "git push -u origin <branch>", &state),
        "git push -u origin feat/run-diff"
    );
    assert_eq!(
        live_subtitle(Step::OpenPr, "gh pr create", &state),
        "feat/run-diff \u{2192} main"
    );
    // Once committed, the branch it really went on wins over the plan.
    state
        .artifacts
        .insert("work_branch".into(), "feat/run-diff-2".into());
    assert_eq!(
        live_subtitle(Step::Commit, "Branch if needed", &state),
        "on feat/run-diff-2"
    );
}

#[test]
fn a_review_names_its_pull_request_and_branches() {
    let mut state = RunState::default();
    for (k, v) in [
        ("pr_number", "104"),
        ("pr_base", "main"),
        ("pr_head", "feat/x"),
    ] {
        state.artifacts.insert(k.into(), v.into());
    }
    assert_eq!(
        live_subtitle(Step::FindPr, "", &state),
        "#104 \u{00b7} feat/x"
    );
    assert_eq!(
        live_subtitle(Step::PrDiff, "", &state),
        "git diff main...feat/x"
    );
    assert_eq!(
        live_subtitle(Step::Merge, "", &state),
        "squash #104 into main"
    );
    assert_eq!(
        live_subtitle(Step::Sync, "", &state),
        "checkout main and pull"
    );
    // A step these names say nothing about keeps its own words.
    assert_eq!(
        live_subtitle(Step::Analyse, "Model call", &state),
        "Model call"
    );
}

use super::*;

fn node(id: &str, deps: &[&str]) -> NodeSpec {
    NodeSpec {
        id: id.into(),
        title: id.into(),
        subtitle: String::new(),
        step: Step::ScanChanges,
        kind: NodeKind::Deterministic,
        deps: deps.iter().map(|d| d.to_string()).collect(),
        reads: vec![],
        writes: vec![],
        requires_approval: false,
        config: Default::default(),
        bind: Default::default(),
    }
}

/// a -> b -> d, a -> c -> d: the diamond. `c` must be reachable without
/// waiting on `b`, which is the whole point of declaring deps instead of
/// a sequence.
fn diamond() -> Graph {
    Graph {
        nodes: vec![
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["a"]),
            node("d", &["b", "c"]),
        ],
    }
}

fn item(key: &str, included: bool) -> ProposalItem {
    ProposalItem {
        key: key.into(),
        label: key.into(),
        note: String::new(),
        included,
    }
}

#[test]
fn only_the_included_items_are_handed_to_the_step() {
    let run = NodeRun {
        items: vec![item("a", true), item("b", false), item("c", true)],
        ..Default::default()
    };
    assert_eq!(run.included_keys(), vec!["a", "c"]);
}

#[test]
fn unchecking_everything_is_not_approvable() {
    let run = NodeRun {
        items: vec![item("a", false)],
        ..Default::default()
    };
    assert!(run.has_nothing_selected());
}

#[test]
fn a_node_that_offers_no_choice_is_always_approvable() {
    assert!(!NodeRun::default().has_nothing_selected());
}

#[test]
fn only_the_root_is_ready_at_the_start() {
    let g = diamond();
    let s = RunState::fresh(&g);
    assert_eq!(s.next_ready(&g).unwrap().id, "a");
}

#[test]
fn a_join_waits_for_every_branch() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Done);
    s.set_status("b", NodeStatus::Done);
    // c is still pending, so d must not be offered.
    assert_eq!(s.next_ready(&g).unwrap().id, "c");
    s.set_status("c", NodeStatus::Done);
    assert_eq!(s.next_ready(&g).unwrap().id, "d");
}

#[test]
fn a_failure_blocks_only_what_depends_on_it() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Done);
    s.set_status("b", NodeStatus::Failed);
    s.propagate_block(&g);
    assert_eq!(s.status("c"), NodeStatus::Pending, "c does not depend on b");
    assert_eq!(s.status("d"), NodeStatus::Blocked, "d joins on b");
}

#[test]
fn rejection_blocks_downstream_the_same_way_failure_does() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Rejected);
    s.propagate_block(&g);
    assert!(s.is_finished(&g));
}

#[test]
fn a_step_sent_back_and_retried_remembers_its_round() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.reject("a", &g);
    s.retry_from("a", &g);
    s.reject("a", &g);
    s.retry_from("a", &g);
    assert_eq!(s.status("a"), NodeStatus::Pending);
    assert_eq!(s.rejections.get("a"), Some(&2));
    assert_eq!(s.rejections.get("b"), None);
}

#[test]
fn every_status_change_is_recorded_once_in_order() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Running);
    s.set_status("a", NodeStatus::Running);
    s.set_status("a", NodeStatus::Done);
    s.reject("b", &g);
    s.retry_from("b", &g);
    let seen: Vec<(&str, NodeStatus)> = s
        .history
        .iter()
        .map(|(id, st)| (id.as_str(), *st))
        .collect();
    assert_eq!(
        &seen[..4],
        &[
            ("a", NodeStatus::Running),
            ("a", NodeStatus::Done),
            ("b", NodeStatus::Rejected),
            ("d", NodeStatus::Blocked),
        ]
    );
    assert!(seen.contains(&("b", NodeStatus::Pending)));
}

#[test]
fn the_newest_approval_is_the_latest_step_still_waiting() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    assert_eq!(s.newest_for_a_person(), None);
    s.set_status("b", NodeStatus::AwaitingApproval);
    s.set_status("c", NodeStatus::AwaitingApproval);
    assert_eq!(s.newest_for_a_person(), Some(("c".to_string(), 1)));
    // Answered, it no longer counts; the one still waiting does.
    s.set_status("c", NodeStatus::Running);
    assert_eq!(s.newest_for_a_person(), Some(("b".to_string(), 0)));
    // Asked again after a retry: a new approval, at a new place.
    s.set_status("b", NodeStatus::Rejected);
    s.set_status("b", NodeStatus::Pending);
    s.set_status("b", NodeStatus::AwaitingApproval);
    assert_eq!(s.newest_for_a_person(), Some(("b".to_string(), 5)));
}

#[test]
fn a_step_that_failed_needs_a_person_until_it_is_retried() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::AwaitingApproval);
    s.set_status("a", NodeStatus::Running);
    s.set_status("a", NodeStatus::Failed);
    assert_eq!(s.newest_for_a_person(), Some(("a".to_string(), 2)));
    s.retry_from("a", &g);
    assert_eq!(s.newest_for_a_person(), None);
}

#[test]
fn replaying_part_of_a_run_rewinds_statuses_and_rounds() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Done);
    s.reject("b", &g);
    s.retry_from("b", &g);
    s.set_status("b", NodeStatus::Done);
    let start = s.replayed(0);
    assert_eq!(start.status("a"), NodeStatus::Pending);
    assert!(start.rejections.is_empty());
    let after_rejection = s.replayed(2);
    assert_eq!(after_rejection.status("b"), NodeStatus::Rejected);
    assert_eq!(after_rejection.rejections.get("b"), Some(&1));
    assert_eq!(s.replayed(s.history.len()).status("b"), NodeStatus::Done);
}

#[test]
fn a_step_with_nothing_to_do_stops_the_run_without_failing_it() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Skipped);
    s.propagate_block(&g);
    assert!(s.is_finished(&g));
    assert_eq!(s.status("b"), NodeStatus::Blocked);
    assert_ne!(s.status("a"), NodeStatus::Failed, "not an error");
}

#[test]
fn retrying_a_failed_node_frees_everything_it_had_blocked() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Done);
    s.set_status("b", NodeStatus::Failed);
    s.propagate_block(&g);
    assert_eq!(s.status("d"), NodeStatus::Blocked);

    s.retry_from("b", &g);
    assert_eq!(s.status("b"), NodeStatus::Pending);
    assert_eq!(s.status("d"), NodeStatus::Pending);
    assert_eq!(s.status("a"), NodeStatus::Done, "work already done is kept");
    assert_eq!(s.next_ready(&g).unwrap().id, "b");
}

#[test]
fn retrying_one_failure_leaves_the_other_failures_block_standing() {
    // a -> b -> d and a -> c -> d. Both b and c fail, so d is blocked
    // twice over. Retrying b must not free d: c is still failed, and a
    // `Pending` d would never run and never let the run finish.
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Done);
    s.set_status("b", NodeStatus::Failed);
    s.set_status("c", NodeStatus::Failed);
    s.propagate_block(&g);
    assert_eq!(s.status("d"), NodeStatus::Blocked);

    s.retry_from("b", &g);
    assert_eq!(s.status("b"), NodeStatus::Pending, "the retried node runs");
    assert_eq!(
        s.status("d"),
        NodeStatus::Blocked,
        "d still joins on c, which is still failed"
    );
    assert_eq!(s.next_ready(&g).unwrap().id, "b");
}

#[test]
fn retrying_the_last_failure_frees_the_join_behind_both() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.set_status("a", NodeStatus::Done);
    s.set_status("b", NodeStatus::Done);
    s.set_status("c", NodeStatus::Failed);
    s.propagate_block(&g);

    s.retry_from("c", &g);
    assert_eq!(s.status("c"), NodeStatus::Pending);
    assert_eq!(s.status("d"), NodeStatus::Pending, "nothing blocks it now");
}

#[test]
fn retrying_clears_a_stale_approval_so_it_is_asked_again() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    s.decisions.insert("b".into(), false);
    s.set_status("b", NodeStatus::Rejected);
    s.retry_from("b", &g);
    assert!(!s.decisions.contains_key("b"));
}

#[test]
fn a_run_is_finished_only_when_every_node_settled() {
    let g = diamond();
    let mut s = RunState::fresh(&g);
    assert!(!s.is_finished(&g));
    for id in ["a", "b", "c", "d"] {
        s.set_status(id, NodeStatus::Done);
    }
    assert!(s.is_finished(&g));
}

#[test]
fn a_bypassed_node_lets_the_run_carry_on_past_it() {
    // What Skip is for: `analyse` will not answer, and `merge` does not
    // actually need it to — the person reads the diff themselves.
    let graph = Graph {
        nodes: vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])],
    };
    let mut s = RunState::fresh(&graph);
    s.set_status("a", NodeStatus::Done);
    s.set_status("b", NodeStatus::Failed);
    s.propagate_block(&graph);
    assert_eq!(s.status("c"), NodeStatus::Blocked);

    s.bypass("b", &graph);
    assert_eq!(s.status("b"), NodeStatus::Bypassed);
    assert_eq!(
        s.status("c"),
        NodeStatus::Pending,
        "freed, not left blocked"
    );
    assert_eq!(
        s.next_ready(&graph).map(|n| n.id),
        Some("c".into()),
        "a bypassed dependency counts as satisfied"
    );
}

#[test]
fn a_bypassed_node_is_terminal_but_a_skipped_one_still_blocks() {
    // The two must not be confused: `Skipped` is a step reporting no work,
    // and everything behind it genuinely cannot run.
    assert!(NodeStatus::Bypassed.is_terminal());
    let graph = Graph {
        nodes: vec![node("a", &[]), node("b", &["a"])],
    };
    let mut s = RunState::fresh(&graph);
    s.set_status("a", NodeStatus::Skipped);
    s.propagate_block(&graph);
    assert_eq!(s.status("b"), NodeStatus::Blocked);
}

#[test]
fn bypassing_keeps_the_failure_it_replaced() {
    // The log is the only account of why the step did not run.
    let graph = Graph {
        nodes: vec![node("a", &[])],
    };
    let mut s = RunState::fresh(&graph);
    s.set_status("a", NodeStatus::Failed);
    s.runs.entry("a".into()).or_default().log = "ollama timed out".into();
    s.bypass("a", &graph);
    assert_eq!(s.runs["a"].log, "ollama timed out");
    assert!(s.runs["a"].summary.contains("carry on without it"));
}

#[test]
fn a_bound_input_reads_the_producer_it_names() {
    // `pr_url` written by two nodes: without a binding the map holds
    // whichever ran last, and with one the reader gets the one it asked
    // for — while still asking by its own literal key.
    let mut node = node("merge", &["open_pr", "find_pr"]);
    node.bind
        .insert("pr_url".into(), qualified("open_pr", "pr_url"));

    let mut s = RunState::default();
    s.artifacts
        .insert(qualified("open_pr", "pr_url"), "from-open".into());
    s.artifacts
        .insert(qualified("find_pr", "pr_url"), "from-find".into());
    s.artifacts.insert("pr_url".into(), "from-find".into());

    assert_eq!(s.artifact("pr_url"), "from-find", "bare key is last-write");
    assert_eq!(
        s.resolved_for(&node).artifact("pr_url"),
        "from-open",
        "the binding decides"
    );
}

#[test]
fn a_node_that_binds_nothing_sees_the_state_unchanged() {
    // The compatibility guarantee, at the one place it is enforced.
    let node = node("a", &[]);
    let mut s = RunState::default();
    s.artifacts.insert("diff".into(), "d".into());
    assert_eq!(s.resolved_for(&node), s);
}

#[test]
fn a_binding_whose_source_produced_nothing_falls_back() {
    // `validate` refuses this flow, so it should not arrive — and if it
    // does, the behaviour is the one that existed before bindings did,
    // not a silently blanked input.
    let mut node = node("b", &["a"]);
    node.bind.insert("diff".into(), qualified("a", "diff"));
    let mut s = RunState::default();
    s.artifacts.insert("diff".into(), "bare".into());
    assert_eq!(s.resolved_for(&node).artifact("diff"), "bare");
}

#[test]
fn a_pending_node_can_be_skipped_before_the_run_reaches_it() {
    // The case Skip could not reach: an ungated step that neither stops to
    // ask nor fails, so it has no other control on it.
    let graph = Graph {
        nodes: vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])],
    };
    let mut s = RunState::fresh(&graph);
    s.set_status("a", NodeStatus::Done);
    assert_eq!(s.status("b"), NodeStatus::Pending);

    s.bypass("b", &graph);

    assert_eq!(s.status("b"), NodeStatus::Bypassed);
    assert_eq!(
        s.next_ready(&graph).map(|n| n.id),
        Some("c".into()),
        "the run goes straight to what was behind it"
    );
}

#[test]
fn skipping_a_running_node_leaves_the_log_it_had_written() {
    // The partial output is how you see how far it got before you gave up
    // on it.
    let graph = Graph {
        nodes: vec![node("a", &[])],
    };
    let mut s = RunState::fresh(&graph);
    s.set_status("a", NodeStatus::Running);
    s.runs.entry("a".into()).or_default().log = "$ cargo test\nrunning 40 tests".into();

    s.bypass("a", &graph);

    assert_eq!(s.status("a"), NodeStatus::Bypassed);
    assert!(s.runs["a"].log.contains("running 40 tests"));
}
