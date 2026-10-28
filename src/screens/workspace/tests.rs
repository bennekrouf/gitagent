use super::*;

#[test]
fn it_opens_on_the_first_repository_in_the_list_with_work_left() {
    let list = vec![
        ("a".to_string(), probe::Wants::Nothing),
        ("b".to_string(), probe::Wants::Release),
        ("c".to_string(), probe::Wants::Resolve),
    ];
    // c is more urgent, but b comes first in the list.
    assert_eq!(first_with_work(list).map(|(p, _)| p), Some("b".to_string()));
}

#[test]
fn checks_still_running_is_not_work_left() {
    let list = vec![
        ("a".to_string(), probe::Wants::Wait),
        ("b".to_string(), probe::Wants::Commit),
    ];
    assert_eq!(first_with_work(list).map(|(p, _)| p), Some("b".to_string()));
    assert_eq!(
        first_with_work(vec![("a".to_string(), probe::Wants::Nothing)]),
        None
    );
}

fn states_with(flow: &str, pr: &str, state: RunState) -> States {
    let mut states = States::new();
    states.insert(("repo".into(), flow.into(), pr.into()), state);
    states
}

fn waiting_at_push() -> RunState {
    let mut s = RunState::fresh(&commit_and_pr().to_graph());
    s.started = true;
    s.set_status("push", NodeStatus::AwaitingApproval);
    s.runs.get_mut("push").unwrap().proposal = "git push -u origin feat/x".into();
    s
}

#[test]
fn a_step_waiting_on_a_plain_yes_is_offered_in_the_list() {
    let states = states_with("commit_and_pr", "", waiting_at_push());
    let approval = approval_for(&states, &FlowBook::defaults(), "repo").unwrap();
    assert_eq!(approval.node, "push");
    assert_eq!(
        approval.run,
        ("repo".into(), "commit_and_pr".into(), String::new())
    );
    assert!(approval.tooltip().contains("git push -u origin feat/x"));
    // Only for its own repository.
    assert!(approval_for(&states, &FlowBook::defaults(), "other").is_none());
}

#[test]
fn a_step_a_trusted_run_held_back_is_offered_with_its_reason() {
    let mut held = waiting_at_push();
    held.runs.get_mut("push").unwrap().held = "CI is failing".into();
    let approval = approval_for(
        &states_with("commit_and_pr", "", held),
        &FlowBook::defaults(),
        "repo",
    )
    .unwrap();
    let tooltip = approval.tooltip();
    assert!(tooltip.starts_with("CI is failing"));
    assert!(tooltip.contains("git push -u origin feat/x"));
}

#[test]
fn an_approval_with_nothing_to_approve_is_not_offered_in_the_list() {
    let book = FlowBook::defaults();

    let mut empty = waiting_at_push();
    empty.runs.get_mut("push").unwrap().items = vec![crate::services::graph::ProposalItem {
        key: "a.rs".into(),
        label: "a.rs".into(),
        note: "modified".into(),
        included: false,
    }];
    assert!(approval_for(&states_with("commit_and_pr", "", empty), &book, "repo").is_none());

    // Already answered, and the run has not caught up yet: a second click
    // has nothing to approve.
    let mut answered = waiting_at_push();
    answered.decisions.insert("push".into(), true);
    assert!(approval_for(&states_with("commit_and_pr", "", answered), &book, "repo").is_none());
}

#[test]
fn a_refresh_clears_runs_that_ended_and_keeps_the_rest() {
    let graph = commit_and_pr().to_graph();
    let mut done = RunState::fresh(&graph);
    done.started = true;
    for n in &graph.nodes {
        done.set_status(&n.id, NodeStatus::Done);
    }
    let mut failed = RunState::fresh(&graph);
    failed.started = true;
    failed.set_status("push", NodeStatus::Failed);
    let mut waiting = RunState::fresh(&graph);
    waiting.started = true;
    waiting.set_status("push", NodeStatus::AwaitingApproval);

    let key = |repo: &str, flow: &str| -> Key { (repo.into(), flow.into(), String::new()) };
    let mut states = States::new();
    states.insert(key("a", "commit_and_pr"), done.clone());
    states.insert(key("a", "review_and_merge"), failed);
    states.insert(key("a", "release"), waiting);
    states.insert(key("b", "commit_and_pr"), done);

    forget_finished(&mut states, &BTreeSet::new(), Some("a"));
    // a's finished run is gone; its failed and waiting ones stay, and
    // b — not refreshed — is untouched.
    assert!(!states.contains_key(&key("a", "commit_and_pr")));
    assert!(states.contains_key(&key("a", "review_and_merge")));
    assert!(states.contains_key(&key("a", "release")));
    assert!(states.contains_key(&key("b", "commit_and_pr")));

    forget_finished(&mut states, &BTreeSet::new(), None);
    assert!(!states.contains_key(&key("b", "commit_and_pr")));
}

#[test]
fn a_flow_that_has_not_run_says_nothing() {
    let graph = commit_and_pr().to_graph();
    assert_eq!(
        flow_run(&States::new(), "repo", "commit_and_pr", &graph),
        None
    );
}

#[test]
fn a_running_flow_says_how_far_it_has_got() {
    let graph = commit_and_pr().to_graph();
    let mut s = RunState::fresh(&graph);
    s.started = true;
    s.set_status("preflight", NodeStatus::Done);
    s.set_status("scan", NodeStatus::Done);
    s.set_status("draft_commit", NodeStatus::Running);
    let run = flow_run(
        &states_with("commit_and_pr", "", s),
        "repo",
        "commit_and_pr",
        &graph,
    )
    .unwrap();
    assert_eq!(run.phase, Phase::Running);
    assert_eq!(
        run.note,
        format!("running \u{00b7} 2/{}", graph.nodes.len())
    );
}

#[test]
fn a_flow_waiting_for_you_names_the_step_and_its_pull_request() {
    let graph = commit_and_pr().to_graph();
    let mut s = RunState::fresh(&graph);
    s.started = true;
    s.set_status("open_pr", NodeStatus::AwaitingApproval);
    let run = flow_run(
        &states_with("commit_and_pr", "7", s),
        "repo",
        "commit_and_pr",
        &graph,
    )
    .unwrap();
    assert_eq!(run.phase, Phase::NeedsApproval);
    assert_eq!(run.step, "open_pr");
    assert_eq!(run.pr, "7");
    assert!(run.note.starts_with("needs you \u{00b7} "));
}

#[test]
fn of_two_runs_of_one_flow_the_one_that_needs_you_wins() {
    let graph = commit_and_pr().to_graph();
    let mut done = RunState::fresh(&graph);
    done.started = true;
    for n in &graph.nodes {
        done.set_status(&n.id, NodeStatus::Done);
    }
    let mut waiting = RunState::fresh(&graph);
    waiting.started = true;
    waiting.set_status("push", NodeStatus::AwaitingApproval);
    let mut states = states_with("commit_and_pr", "5", done);
    states.insert(("repo".into(), "commit_and_pr".into(), "6".into()), waiting);
    let run = flow_run(&states, "repo", "commit_and_pr", &graph).unwrap();
    assert_eq!((run.phase, run.pr.as_str()), (Phase::NeedsApproval, "6"));
}

#[test]
fn play_and_start_run_the_flow_on_screen_whatever_is_trusted() {
    let on_screen = ("review_and_merge".to_string(), "7".to_string());
    let needed = Some(("commit_and_pr".to_string(), String::new()));
    // Trust all on or off, a plain Start or Play is not a request to
    // choose: the Review tab reviews.
    assert_eq!(
        starting_flow(false, needed.clone(), on_screen.clone()),
        Some(on_screen.clone())
    );
    // Only the Trusted run button goes where the repository needs.
    assert_eq!(
        starting_flow(true, needed, on_screen.clone()),
        Some(("commit_and_pr".to_string(), String::new()))
    );
    // And with nothing in particular needed, it too runs what is on screen.
    assert_eq!(
        starting_flow(true, None, on_screen.clone()),
        Some(on_screen)
    );
}

#[test]
fn a_trusted_run_answers_its_own_approval() {
    let mut state = RunState::default();
    state.set_status("merge", NodeStatus::AwaitingApproval);
    assert!(answers_itself(&state, "merge", true));
    assert!(!answers_itself(&state, "merge", false));
}

#[test]
fn a_merge_a_trusted_run_held_back_is_yours_to_answer() {
    let mut state = RunState::default();
    state.set_status("merge", NodeStatus::AwaitingApproval);
    state.runs.get_mut("merge").unwrap().held = "The analysis found 2 possible regressions.".into();
    // Still counted as trusted for a moment, or with Trust all on: held
    // is what says a person has to answer it.
    assert!(!answers_itself(&state, "merge", true));
}

#[test]
fn a_failed_step_is_yours_even_in_a_trusted_run() {
    let mut state = RunState::default();
    state.set_status("open_pr", NodeStatus::Failed);
    assert!(!answers_itself(&state, "open_pr", true));
    assert!(needs_a_person(NodeStatus::Failed));
    assert!(!needs_a_person(NodeStatus::Running));
}

fn commit_and_pr() -> flowdef::FlowDef {
    FlowBook::defaults().get("commit_and_pr").unwrap().clone()
}

fn brief(number: &str) -> probe::PrBrief {
    probe::PrBrief {
        number: number.into(),
        title: "t".into(),
        url: "u".into(),
        checks: probe::Checks::Passing,
        files: 1,
        additions: 1,
        deletions: 0,
        commits: 1,
        base: String::new(),
        head: String::new(),
        failed: vec![],
    }
}

#[test]
fn arriving_at_a_review_picks_the_pull_request_too() {
    // Landing on the review flow with nothing selected to review leaves
    // the person to answer a question the app already knows.
    let book = FlowBook::defaults();
    let states = States::new();
    let prs = [brief("7"), brief("9")];

    let (flow_id, _, pr_id) =
        default_selection(&book, &states, "/repo", Some(Wants::Merge), &prs, &[]);
    assert_eq!(flow_id, "review_and_merge");
    assert_eq!(pr_id, "7", "the first one, matching the order shown");
}

#[test]
fn a_run_in_a_repository_holds_every_other_flow_there() {
    let key = |r: &str, f: &str, p: &str| (r.to_string(), f.to_string(), p.to_string());
    let running: BTreeSet<Key> = [key("/a", "commit_and_pr", "")].into();
    // Merging a pull request while a commit is under way is refused…
    let merge = key("/a", "review_and_merge", "7");
    assert_eq!(
        other_run_in(&running, "/a", &merge),
        Some(&key("/a", "commit_and_pr", ""))
    );
    // …the run itself is not in its own way…
    assert_eq!(
        other_run_in(&running, "/a", &key("/a", "commit_and_pr", "")),
        None
    );
    // …and another repository is unaffected.
    assert_eq!(
        other_run_in(&running, "/b", &key("/b", "review_and_merge", "7")),
        None
    );
}

#[test]
fn a_commit_flow_selects_no_pull_request() {
    let book = FlowBook::defaults();
    let states = States::new();
    let (_, _, pr_id) = default_selection(
        &book,
        &states,
        "/repo",
        Some(Wants::Commit),
        &[brief("7")],
        &[],
    );
    assert!(pr_id.is_empty(), "nothing to scope a commit run to");
}

#[test]
fn a_review_with_no_pull_requests_listed_selects_none() {
    let book = FlowBook::defaults();
    let states = States::new();
    let (_, _, pr_id) = default_selection(&book, &states, "/repo", Some(Wants::Merge), &[], &[]);
    assert!(pr_id.is_empty());
}

#[test]
fn with_nothing_running_the_flow_follows_what_the_repository_needs() {
    // Clicking a repository whose only outstanding work is a release must
    // not land on "Commit → PR" and show nothing to do.
    let book = FlowBook::defaults();
    let states = States::new();

    let (flow_id, node_id, _) =
        default_selection(&book, &states, "/repo", Some(Wants::Merge), &[], &[]);
    assert_eq!(flow_id, "review_and_merge");
    assert_eq!(node_id, book.get("review_and_merge").unwrap().first_node());

    let (flow_id, _, _) = default_selection(&book, &states, "/repo", Some(Wants::Commit), &[], &[]);
    assert_eq!(flow_id, "commit_and_pr");
}

#[test]
fn a_row_offers_the_commit_or_release_its_repository_is_ready_for() {
    use crate::services::release::ReleaseState;
    let status = |changes: usize, release: bool| RepoStatus {
        branch: "main".into(),
        changes,
        forge: crate::services::forge::Forge::GitHub,
        pr: None,
        prs: vec![],
        prs_error: None,
        ahead: 0,
        behind: 0,
        unmerged: 0,
        release: ReleaseState {
            last_tag: Some("v1".into()),
            commits: if release { 2 } else { 0 },
            prs: vec![],
            releases: release,
            since_shipped: false,
        },
        in_progress: None,
    };
    let mut book = FlowBook::defaults();

    let (flow, label, pr) = row_start(&book, &status(3, false), &[]).unwrap();
    assert_eq!(
        (flow.as_str(), label.as_str()),
        ("commit_and_pr", "Commit 3 files")
    );
    assert!(pr.is_empty(), "a commit acts on no pull request");
    assert_eq!(
        row_start(&book, &status(1, false), &[]).unwrap().1,
        "Commit 1 file"
    );
    // Hidden here: nothing to offer, the note stays.
    assert!(row_start(&book, &status(3, false), &["commit_and_pr".into()]).is_none());
    // Clean: nothing at all.
    assert!(row_start(&book, &status(0, false), &[]).is_none());
    // Committed on a branch with no pull request: push and open one.
    let unpushed = RepoStatus {
        branch: "topic".into(),
        unmerged: 1,
        ..status(0, false)
    };
    let (flow, label, _) = row_start(&book, &unpushed, &[]).unwrap();
    assert_eq!(
        (flow.as_str(), label.as_str()),
        ("commit_and_pr", "Push & open PR")
    );

    // A release is only offered once a flow can cut it.
    assert!(row_start(&book, &status(0, true), &[]).is_none());
    let mut release = book.get("commit_and_pr").unwrap().clone();
    release.id = "release".into();
    release.label = "Release".into();
    release.handles = vec![Need::Release.key().to_string()];
    book.flows.push(release);
    // The button says which flow it starts, not "Run release".
    let (flow, label, _) = row_start(&book, &status(0, true), &[]).unwrap();
    assert_eq!((flow.as_str(), label.as_str()), ("release", "Release"));

    book.flows.last_mut().unwrap().label = "Deploy VPS".into();
    assert_eq!(
        row_start(&book, &status(0, true), &[]).unwrap().1,
        "Deploy VPS"
    );
}

#[test]
fn a_row_ready_to_merge_offers_that_pull_request_by_number() {
    use crate::services::probe::{Checks, PrBrief};
    use crate::services::release::ReleaseState;
    let pr = |number: &str, checks: Checks| PrBrief {
        number: number.into(),
        title: "Add a thing".into(),
        url: String::new(),
        checks,
        files: 3,
        additions: 10,
        deletions: 2,
        commits: 1,
        base: "main".into(),
        head: "feat/x".into(),
        failed: vec![],
    };
    let ready = |pr: Option<PrBrief>, others: Vec<PrBrief>| RepoStatus {
        branch: "feat/x".into(),
        changes: 0,
        forge: crate::services::forge::Forge::GitHub,
        pr,
        prs: others,
        prs_error: None,
        ahead: 0,
        behind: 0,
        unmerged: 0,
        release: ReleaseState {
            last_tag: Some("v1".into()),
            commits: 0,
            prs: vec![],
            releases: false,
            since_shipped: false,
        },
        in_progress: None,
    };
    let book = FlowBook::defaults();

    let status = ready(Some(pr("34", Checks::Passing)), vec![]);
    let (flow, label, chosen) = row_start(&book, &status, &[]).unwrap();
    assert_eq!(
        (flow.as_str(), label.as_str(), chosen.as_str()),
        ("review_and_merge", "Merge #34", "34"),
        "the row names the decision, and the run arrives on that PR"
    );

    // Red checks are not a decision waiting: the note stays.
    let failing = ready(Some(pr("34", Checks::Failing)), vec![]);
    assert!(row_start(&book, &failing, &[]).is_none());

    // Back on the base branch, the open one elsewhere is the one offered.
    let elsewhere = ready(None, vec![pr("35", Checks::Passing)]);
    assert_eq!(row_start(&book, &elsewhere, &[]).unwrap().1, "Merge #35");

    // Hidden here: nothing to offer, "ready to merge" stays.
    assert!(row_start(&book, &status, &["review_and_merge".into()]).is_none());
}

#[test]
fn a_hint_naming_a_flow_that_does_not_exist_falls_back() {
    // Wants::Release points at a flow id nobody has built yet.
    let book = FlowBook::defaults();
    let states = States::new();
    let (flow_id, _, _) =
        default_selection(&book, &states, "/repo", Some(Wants::Release), &[], &[]);
    assert_eq!(flow_id, book.runnable().first().unwrap().id);
}

#[test]
fn a_running_task_is_selected_over_an_idle_flow() {
    let book = FlowBook::defaults();
    let flow = commit_and_pr();
    let mut run = RunState::fresh(&flow.to_graph());
    run.started = true;
    run.set_status("preflight", NodeStatus::Done);
    run.set_status("scan", NodeStatus::Running);

    let mut states = States::new();
    states.insert(("/repo".into(), flow.id.clone(), String::new()), run);

    let (flow_id, node_id, pr_id) = default_selection(&book, &states, "/repo", None, &[], &[]);
    assert_eq!(flow_id, flow.id);
    assert_eq!(node_id, "scan");
    assert_eq!(pr_id, "");
}

#[test]
fn a_running_task_only_wins_for_its_own_repository() {
    let book = FlowBook::defaults();
    let flow = commit_and_pr();
    let mut run = RunState::fresh(&flow.to_graph());
    run.started = true;
    run.set_status("scan", NodeStatus::Running);

    let mut states = States::new();
    states.insert(("/other-repo".into(), flow.id.clone(), String::new()), run);

    let (flow_id, node_id, _) = default_selection(&book, &states, "/repo", None, &[], &[]);
    // Nothing running here — falls back to the first runnable flow's
    // first node, same as an untouched repository.
    assert_eq!(flow_id, book.runnable().first().unwrap().id);
    assert_eq!(node_id, book.runnable().first().unwrap().first_node());
}

#[test]
fn someone_awaiting_approval_still_outranks_a_running_task() {
    let book = FlowBook::defaults();
    let flow = commit_and_pr();
    let mut run = RunState::fresh(&flow.to_graph());
    run.started = true;
    run.set_status("scan", NodeStatus::Done);
    run.set_status("draft_commit", NodeStatus::Running);
    run.set_status("commit", NodeStatus::AwaitingApproval);

    let mut states = States::new();
    states.insert(("/repo".into(), flow.id.clone(), String::new()), run);

    let (_, node_id, _) = default_selection(&book, &states, "/repo", None, &[], &[]);
    assert_eq!(node_id, "commit");
}
