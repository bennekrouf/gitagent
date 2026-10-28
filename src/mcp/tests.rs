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
