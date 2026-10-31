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
    let mut r = run(true);
    assert!(r.trusting());
    r.held = Some("CI is still running".into());
    assert!(!r.trusting(), "from the hold on, the run is the person's");
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

#[test]
fn an_unknown_region_changes_nothing() {
    let before = store::load_settings();
    let err = model_region("mars", "").unwrap_err();
    assert!(err.contains("europe, china, usa"), "{err}");
    assert_eq!(store::load_settings(), before);
}

#[test]
fn refresh_refuses_a_path_that_is_not_a_folder() {
    assert!(refresh("/no/such/folder/anywhere").is_err());
}

#[test]
fn a_tool_result_is_compact_and_has_no_nulls() {
    let text = pretty(&json!({ "a": null, "b": [{ "c": null, "d": 1 }], "e": "x" }));
    assert_eq!(text, r#"{"b":[{"d":1}],"e":"x"}"#);
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
