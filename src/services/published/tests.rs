use super::*;

fn view(status: &str, conclusion: &str, jobs: &[(&str, &str, &str)]) -> RunView {
    RunView {
        status: status.into(),
        conclusion: conclusion.into(),
        jobs: jobs
            .iter()
            .map(|(name, status, conclusion)| Job {
                name: (*name).into(),
                status: (*status).into(),
                conclusion: (*conclusion).into(),
                url: format!("https://github.com/o/r/actions/runs/1/job/{name}"),
            })
            .collect(),
    }
}

fn run() -> RunRef {
    RunRef {
        id: "41".into(),
        workflow: "Release".into(),
        url: "https://github.com/o/r/actions/runs/41".into(),
    }
}

#[test]
fn the_newest_run_is_the_one_followed() {
    let json = r#"[{"databaseId":41,"workflowName":"Release","url":"https://github.com/o/r/actions/runs/41"},
                   {"databaseId":40,"workflowName":"Release","url":"u"}]"#;
    assert_eq!(run_from_list(json), Some(run()));
    assert_eq!(run_from_list("[]"), None);
    assert_eq!(run_from_list("not json"), None);
}

#[test]
fn jobs_are_read_from_gh_run_view() {
    let json = r#"{"status":"in_progress","conclusion":"","jobs":[
        {"name":"build-mac","status":"in_progress","conclusion":"","url":"m"},
        {"name":"build-linux","status":"completed","conclusion":"success","url":"l"},
        {"name":"build-windows","status":"queued","conclusion":"","url":"w"}]}"#;
    let view = view_from_json(json).expect("parses");
    let states: Vec<&str> = view.jobs.iter().map(Job::state).collect();
    assert_eq!(states, ["running", "done", "waiting for a runner"]);
}

#[test]
fn a_green_run_is_published_and_names_what_it_built() {
    let done = view(
        "completed",
        "success",
        &[
            ("prepare", "completed", "skipped"),
            ("build-mac", "completed", "success"),
            ("release", "completed", "success"),
        ],
    );
    let outcome = finished(&run(), "v0.1.58", &done, None).expect("published");
    assert_eq!(outcome.summary, "v0.1.58 published — build-mac, release");
    assert!(!outcome.nothing_to_do);
}

#[test]
fn a_failed_job_is_named_with_a_rerun_of_only_the_failures() {
    let done = view(
        "completed",
        "failure",
        &[
            ("build-mac", "completed", "success"),
            ("build-windows", "completed", "failure"),
            ("release", "completed", "success"),
        ],
    );
    let failure = finished(&run(), "v0.1.47", &done, None).expect_err("not published");
    assert!(
        failure.message.contains("1 job failed"),
        "{}",
        failure.message
    );
    assert!(
        failure.message.contains("build-windows — failed"),
        "{}",
        failure.message
    );
    assert!(
        !failure.message.contains("build-mac —"),
        "{}",
        failure.message
    );
    assert_eq!(failure.remedies.len(), 1);
    assert_eq!(failure.remedies[0].display, "gh run rerun 41 --failed");
}

#[test]
fn a_job_that_never_got_a_runner_counts_as_failed() {
    let done = view(
        "completed",
        "cancelled",
        &[
            ("build-mac", "completed", "cancelled"),
            ("build-linux", "completed", "success"),
        ],
    );
    let failure = finished(&run(), "v1.0.0", &done, None).expect_err("cancelled");
    assert!(
        failure.message.contains("build-mac — cancelled"),
        "{}",
        failure.message
    );
}

#[test]
fn versions_order_by_number_not_by_text() {
    assert_eq!(
        version_order("v0.1.10", "v0.1.9"),
        std::cmp::Ordering::Greater
    );
    assert_eq!(version_order("v1.0.0", "v1.0.0"), std::cmp::Ordering::Equal);
}

#[test]
fn an_apple_agreement_to_accept_is_said_instead_of_try_again() {
    let done = view(
        "completed",
        "failure",
        &[
            ("build-mac", "completed", "failure"),
            ("build-windows", "completed", "success"),
        ],
    );
    let log = "build-mac\tBuild .app + .dmg\t2026-11-14T10:25:33Z Error: HTTP status code: 403. \
               A required agreement is missing or has expired. This request requires an \
               in-effect agreement that has not been signed or has expired.";
    let failure = finished(&run(), "v0.1.65", &done, Some(log)).expect_err("not published");
    assert!(
        failure.message.contains("developer.apple.com/account"),
        "{}",
        failure.message
    );
    assert!(
        !failure.message.contains("second try"),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("build-mac"), "{}", failure.message);
    assert_eq!(
        failure.remedies[0].label,
        "Re-run once the agreement is accepted"
    );
    assert_eq!(failure.remedies[0].args, ["run", "rerun", "41", "--failed"]);
}

#[test]
fn an_unrecognised_failure_still_suggests_a_second_try() {
    let done = view(
        "completed",
        "failure",
        &[("build-mac", "completed", "failure")],
    );
    let failure = finished(
        &run(),
        "v0.1.65",
        &done,
        Some("error: linker `cc` not found"),
    )
    .expect_err("not published");
    assert!(
        failure.message.contains("second try"),
        "{}",
        failure.message
    );
    assert_eq!(failure.remedies[0].label, "Re-run the failed jobs");
    assert!(known_cause("security: CSSMERR_TP_CERT_EXPIRED").is_some());
}
