use super::*;
use crate::services::graph::RunState;

fn run(trusted: bool) -> Run {
    let graph = FlowBook::defaults()
        .get("commit_and_pr")
        .expect("shipped")
        .to_graph();
    let state = RunState::fresh(&graph);
    Run::new("/r".into(), "commit_and_pr".into(), graph, state, trusted)
}

#[test]
fn a_trusted_run_stops_trusting_at_its_first_hold() {
    // Trust all is given rather than read: with it on in this machine's
    // settings, a hold rightly changes nothing, and the test failed there.
    let mut r = run(true);
    assert!(r.trusting_with(false));
    r.held = Some("CI is still running".into());
    assert!(
        !r.trusting_with(false),
        "from the hold on, the run is the person's"
    );
    assert!(r.trusting_with(true), "but Trust all does not ask again");
    let out = report(&r);
    assert_eq!(out["held"], "CI is still running");
    assert_eq!(out["trusted"], false);
}

#[test]
fn an_ordinary_run_reports_nothing_about_trust() {
    // Trust all lives on disk; this only holds while it is off.
    if store::trust_all() {
        return;
    }
    let out = report(&run(false));
    assert!(out.get("trusted").is_none() && out.get("approved_automatically").is_none());
}

#[test]
fn a_flow_stored_under_another_id_says_which() {
    let note = id_note(Some("wintest_tmp"), "win_test_tmp").expect("ids differ");
    assert!(note.starts_with("stored as win_test_tmp, not wintest_tmp"));
    assert_eq!(id_note(Some("win_test_tmp"), "win_test_tmp"), None);
    assert_eq!(
        id_note(None, "win_test_tmp"),
        None,
        "no id given, nothing to correct"
    );
}

#[test]
fn trust_all_is_only_read_unless_turned_off() {
    let before = store::trust_all();
    let out = trust_all(false);
    assert_eq!(out["trust_all"], before);
    assert_eq!(store::trust_all(), before, "reading it changes nothing");
}

#[test]
fn no_tool_turns_trust_all_on() {
    let tools = tools();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "trust_all")
        .expect("listed");
    let props = tool["inputSchema"]["properties"].as_object().unwrap();
    assert_eq!(props.keys().collect::<Vec<_>>(), ["turn_off"]);
}

#[tokio::test]
async fn stop_asking_on_a_run_with_nothing_to_approve_trusts_nothing() {
    let mut runs = BTreeMap::new();
    let r = run(false);
    let id = r.state.run;
    runs.insert(id, r);
    assert!(approve(id, &[], true, &mut runs).await.is_err());
    assert!(
        !runs[&id].trusted,
        "refused, so the run is still the person's"
    );
    assert!(approve(id + 1000, &[], true, &mut runs).await.is_err());
}

#[test]
fn regions_go_by_the_names_people_use() {
    assert_eq!(region_named("EU").map(|r| r.id), Some("europe"));
    assert_eq!(region_named("cn").map(|r| r.id), Some("china"));
    assert_eq!(region_named(" United States ").map(|r| r.id), Some("usa"));
    assert_eq!(region_named("usa").map(|r| r.id), Some("usa"));
    assert!(region_named("mars").is_none());
}

#[tokio::test]
async fn an_unknown_region_changes_nothing() {
    let before = store::load_settings();
    let err = model_region("mars", "").await.unwrap_err();
    assert!(err.contains("europe, china, usa"), "{err}");
    assert_eq!(store::load_settings(), before);
}

#[test]
fn refresh_refuses_a_path_that_is_not_a_folder() {
    assert!(refresh("/no/such/folder/anywhere").is_err());
}

#[test]
fn an_earlier_flow_is_told_in_one_line_per_step_that_did_something() {
    let out = brief(&json!({
        "run_id": 7, "flow": "commit_and_pr", "phase": "finished",
        "steps": [
            { "id": "a", "title": "Commit", "summary": "3 files" },
            { "id": "b", "title": "Push", "summary": null },
        ],
        "awaiting": { "diff": "long" },
        "produced": { "branch": "x" },
    }));
    assert_eq!(out["steps"], json!(["Commit: 3 files"]));
    assert!(out.get("awaiting").is_none());
    assert_eq!(out["produced"]["branch"], "x");
}

#[test]
fn a_report_says_nothing_twice() {
    let mut r = run(false);
    let art = |k: &str, v: &str| (k.to_string(), v.to_string());
    r.state.artifacts.extend([
        art("file_notes", "a.rs\tmodified"),
        art("commit_paths", "a.rs"),
        art("untracked", "c.txt"),
        art("branch_name", "fix/x"),
        art("work_branch", "fix/x"),
        art("commit_sha", "abc1234"),
    ]);
    let first = r.graph.nodes[0].id.clone();
    r.state.runs.entry(first).or_default().summary = "abc1234".into();
    let out = report(&r);
    let produced = out["produced"].as_object().unwrap();
    assert_eq!(
        produced.keys().collect::<Vec<_>>(),
        ["file_notes", "work_branch"],
        "the sha is a step's summary, the paths are in file_notes, the branch didn't change"
    );
    let steps = out["steps"].as_array().unwrap();
    assert!(
        steps[1..].iter().all(|s| s.get("summary").is_none()),
        "no empty summaries"
    );
}

/// Every step of `r` settled as `status`.
fn settle(r: &mut Run, status: NodeStatus) {
    for n in r.graph.nodes.clone() {
        r.state.runs.entry(n.id).or_default().status = status;
    }
}

#[test]
fn a_fix_tells_claude_where_it_goes_next_as_the_window_does() {
    use crate::services::graph::{THEN, THEN_OUTCOME, THEN_STARTED};
    let mut r = run(false);
    assert!(report(&r)["then"].is_null(), "not a fix");

    r.then = Some(("review_and_merge".into(), "145".into()));
    r.state
        .artifacts
        .insert(THEN.into(), "Review → Merge #145".into());
    let out = report(&r);
    assert_eq!(out["then"]["note"], "then Review → Merge #145");
    assert_eq!(out["then"]["starts_by_itself"], true);
    assert!(
        out["produced"].get(THEN).is_none(),
        "said once, in words: {}",
        out["produced"]
    );

    settle(&mut r, NodeStatus::Done);
    r.state
        .artifacts
        .insert(THEN_OUTCOME.into(), THEN_STARTED.into());
    let out = report(&r);
    assert_eq!(out["then"]["note"], "went on to Review → Merge #145");
    assert!(out["produced"].get(THEN_OUTCOME).is_none());
    assert_eq!(brief(&out)["then"], out["then"], "kept under earlier_flows");

    r.state.artifacts.insert(
        THEN_OUTCOME.into(),
        "run 9 (release) is still going in this repository".into(),
    );
    let out = report(&r);
    assert_eq!(out["then"]["note"], "Review → Merge #145 did not start");
    assert_eq!(out["then"]["starts_by_itself"], false);
    assert!(
        out["then"]["why"].as_str().unwrap().contains("still going"),
        "{}",
        out["then"]
    );
}

#[tokio::test]
async fn a_fix_goes_back_to_the_run_it_fixed_untrusted_or_not() {
    let mut r = run(false);
    r.then = Some(("review_and_merge".into(), "145".into()));
    settle(&mut r, NodeStatus::Done);
    match r.next().await {
        Next::Flow(flow, pr) => {
            assert_eq!((flow.as_str(), pr.as_str()), ("review_and_merge", "145"))
        }
        Next::Stop(_) => panic!("a clean fix goes back to the merge"),
    }
    let first = r.graph.nodes[0].id.clone();
    r.state.runs.get_mut(&first).unwrap().status = NodeStatus::Failed;
    assert!(
        matches!(r.next().await, Next::Stop(_)),
        "a failed commit does not go on to merge without it"
    );
}

#[tokio::test]
async fn a_merge_refused_over_edits_says_how_to_run_its_fix_and_goes_back_to_it() {
    let dir = std::env::temp_dir().join(format!("gitagent-mcpfix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let repo = repo_path(dir.to_str().unwrap()).unwrap();
    // The commit flow as this machine's Setup has it; none is a setup this
    // test has nothing to say about.
    let Some(commit) = runner::flow_for(&repo, probe::Need::Uncommitted) else {
        return;
    };

    let graph = FlowBook::defaults()
        .get("review_and_merge")
        .expect("shipped")
        .to_graph();
    let mut state = RunState::fresh(&graph);
    state.artifacts.insert("pr_number".into(), "145".into());
    let mut r = Run::new(repo.clone(), "review_and_merge".into(), graph, state, false);
    let merge = r.graph.nodes.last().unwrap().id.clone();
    let step = r.state.runs.entry(merge).or_default();
    step.status = NodeStatus::Failed;
    step.remedies = vec![crate::services::graph::Remedy::flow(
        "Commit them with Commit → PR, then review and merge #145",
        "Commit → PR",
        probe::Need::Uncommitted,
    )];
    let id = r.state.run;

    let out = report(&r);
    let failed = out["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["status"] == "failed")
        .expect("the merge step");
    let call = &failed["fixes"][0]["run_flow"];
    assert_eq!(call["flow"], commit.as_str());
    assert_eq!(call["fixing"], id);
    assert_eq!(call["repo"], repo.as_str());

    let mut runs = BTreeMap::new();
    runs.insert(id, r);
    assert_eq!(
        going_back_to(id, &repo, &commit, &runs).unwrap(),
        ("review_and_merge".to_string(), "145".to_string())
    );
    assert!(
        going_back_to(id, &repo, "release", &runs).is_err(),
        "only the flow the step offered chains back"
    );
    std::fs::remove_dir_all(&dir).ok();
}
