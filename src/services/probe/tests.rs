use super::*;
use serde_json::json;

#[test]
fn a_pull_request_knows_the_branches_its_diff_is_between() {
    let pr = github_pr_from_json(&json!({
        "number": 104, "title": "t", "url": "u",
        "baseRefName": "main", "headRefName": "feat/run-diff",
    }))
    .unwrap();
    assert_eq!(
        (pr.base.as_str(), pr.head.as_str()),
        ("main", "feat/run-diff")
    );
    assert!(PR_FIELDS.contains("baseRefName") && PR_FIELDS.contains("headRefName"));
}

#[test]
fn azure_names_its_branches_as_refs() {
    assert_eq!(azure_branch(&json!("refs/heads/feature/x")), "feature/x");
    assert_eq!(azure_branch(&json!("main")), "main");
}

#[test]
fn a_check_is_reused_for_two_minutes_and_not_after() {
    let then = Instant::now();
    assert!(still_recent(then, then + Duration::from_secs(5)));
    assert!(still_recent(then, then + RECENT - Duration::from_secs(1)));
    assert!(!still_recent(then, then + RECENT));
}

#[test]
fn the_repository_check_never_asks_github_for_every_commit() {
    // One field here priced each check at ~30 of the hour's 5,000
    // GraphQL points, and ran the account out within a few launches.
    assert!(!PR_FIELDS.split(',').any(|f| f == "commits"));
    // Everything a pull request card and the sidebar do show is still
    // asked for.
    for field in ["number", "title", "url", "state", "statusCheckRollup"] {
        assert!(PR_FIELDS.split(',').any(|f| f == field), "{field}");
    }
}

fn status(changes: usize, pr: Option<Checks>) -> RepoStatus {
    RepoStatus {
        branch: "master".into(),
        changes,
        forge: Forge::GitHub,
        unmerged: 0,
        in_progress: None,
        release: ReleaseState::default(),
        pr: pr.map(|checks| PrBrief {
            number: "2".into(),
            title: "t".into(),
            url: "u".into(),
            checks,
            files: 3,
            additions: 10,
            deletions: 4,
            commits: 1,
            base: String::new(),
            head: String::new(),
            failed: vec![],
        }),
        prs: vec![],
        prs_error: None,
        ahead: 0,
        behind: 0,
    }
}

#[test]
fn a_tab_counts_what_its_flow_would_act_on() {
    let handles = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect::<Vec<_>>();
    let commit = handles(&["uncommitted", "unpushed_branch"]);
    let review = handles(&["open_pull_request"]);
    let release = handles(&["release"]);

    let mut s = status(5, None);
    assert_eq!(s.count_for(&commit).unwrap().0, "5 files");
    assert_eq!(
        s.count_for(&review),
        None,
        "nothing open, nothing on the tab"
    );

    s.changes = 0;
    s.unmerged = 1;
    assert_eq!(s.count_for(&commit).unwrap().0, "1 commit");
    s.pr = status(0, Some(Checks::Pending)).pr;
    assert_eq!(
        s.count_for(&commit),
        None,
        "already proposed: the review tab has it"
    );

    s.prs = vec![s.pr.clone().unwrap(), s.pr.clone().unwrap()];
    assert_eq!(s.count_for(&review).unwrap().0, "2 PRs");

    s.release = ReleaseState {
        last_tag: Some("v1.2.0".into()),
        commits: 3,
        prs: vec![],
        releases: true,
        since_shipped: false,
    };
    let (tab, hover) = s.count_for(&release).unwrap();
    assert_eq!(tab, "3 commits");
    assert!(hover.contains("since v1.2.0"));

    s.release.since_shipped = true;
    let (_, hover) = s.count_for(&release).unwrap();
    assert_eq!(hover, "3 commits since the last deploy, not deployed yet");
    assert_eq!(
        s.count_for(&[]),
        None,
        "a flow declaring nothing counts nothing"
    );
}

#[test]
fn a_green_pull_request_is_the_most_urgent_thing() {
    assert_eq!(status(0, Some(Checks::Passing)).wants(), Wants::Merge);
}

#[test]
fn finishing_beats_starting() {
    // Dirty tree *and* a green PR: the decision comes first.
    assert_eq!(status(5, Some(Checks::Passing)).wants(), Wants::Merge);
}

#[test]
fn a_red_pull_request_asks_for_attention() {
    assert_eq!(status(0, Some(Checks::Failing)).wants(), Wants::Attention);
}

#[test]
fn while_ci_runs_there_is_still_uncommitted_work_to_do() {
    assert_eq!(status(3, Some(Checks::Pending)).wants(), Wants::Commit);
    assert_eq!(status(0, Some(Checks::Pending)).wants(), Wants::Wait);
}

#[test]
fn no_pull_request_and_a_dirty_tree_wants_a_commit() {
    assert_eq!(status(1, None).wants(), Wants::Commit);
}

#[test]
fn a_clean_repository_wants_nothing() {
    assert_eq!(status(0, None).wants(), Wants::Nothing);
    assert!(!Wants::Nothing.needs_a_person());
    assert!(!Wants::Wait.needs_a_person());
}

#[test]
fn an_unfinished_rebase_outranks_everything_else() {
    // Mid-rebase, every other answer is advice you cannot act on.
    let mut s = status(4, Some(Checks::Passing));
    s.unmerged = 2;
    s.in_progress = Some(git::InProgress::Rebase);
    assert_eq!(s.wants(), Wants::Resolve);
    assert!(s.wants().needs_a_person());
}

#[test]
fn no_flow_claims_the_unfinished_state() {
    // It is settled in preflight, not by picking a flow.
    assert_eq!(Wants::Resolve.need(), None);
}

#[test]
fn a_release_is_not_hidden_behind_a_pull_request_still_building() {
    // The reported bug: "Clean" (or "checks running") while a release was
    // plainly due. A pending PR is not a decision, so it must not mask one.
    let mut s = status(0, Some(Checks::Pending));
    s.release = ReleaseState {
        last_tag: Some("v0.3.17".into()),
        commits: 3,
        prs: vec!["#6".into()],
        releases: true,
        since_shipped: false,
    };
    assert_eq!(s.wants(), Wants::Release);
}

#[test]
fn a_pending_pull_request_with_nothing_else_is_still_a_wait() {
    let s = status(0, Some(Checks::Pending));
    assert_eq!(s.wants(), Wants::Wait);
}

#[test]
fn a_decision_on_a_pull_request_still_outranks_a_release() {
    let mut s = status(0, Some(Checks::Passing));
    s.release = ReleaseState {
        last_tag: Some("v1".into()),
        commits: 9,
        prs: vec![],
        releases: true,
        since_shipped: false,
    };
    assert_eq!(s.wants(), Wants::Merge);
}

#[test]
fn a_red_check_outranks_everything_behind_it() {
    let mut s = status(4, Some(Checks::Failing));
    s.release = ReleaseState {
        last_tag: Some("v1".into()),
        commits: 2,
        prs: vec![],
        releases: true,
        since_shipped: false,
    };
    assert_eq!(s.wants(), Wants::Attention);
}

#[test]
fn a_branch_with_commits_and_no_pull_request_asks_for_one() {
    // ais-tracing: on feat/azure-error-handling, 1 ahead of master, clean
    // tree, no PR. This used to report "nothing to do" from every angle.
    let mut s = status(0, None);
    s.unmerged = 1;
    assert_eq!(s.wants(), Wants::OpenPr);
    assert!(s.wants().needs_a_person());
    assert_eq!(s.summary(), "1 ahead");
}

#[test]
fn that_branch_can_actually_be_acted_on() {
    let mut s = status(0, None);
    s.unmerged = 1;
    let a = affordance(FlowKind::Commit, Some(&s), false, false, "", &[]);
    assert!(a.enabled, "the button must not be dead");
    assert_eq!(a.label, "Push 1 commit & open PR");
}

#[test]
fn uncommitted_work_is_still_the_first_thing_offered() {
    let mut s = status(3, None);
    s.unmerged = 1;
    assert_eq!(s.wants(), Wants::Commit);
    assert_eq!(
        affordance(FlowKind::Commit, Some(&s), false, false, "", &[]).label,
        "Commit 3 files"
    );
}

#[test]
fn a_branch_already_on_the_base_is_still_nothing_to_do() {
    let s = status(0, None);
    assert_eq!(s.wants(), Wants::Nothing);
    assert!(!affordance(FlowKind::Commit, Some(&s), false, false, "", &[]).enabled);
}

#[test]
fn an_open_pull_request_outranks_an_unmerged_branch() {
    let mut s = status(0, Some(Checks::Passing));
    s.unmerged = 2;
    assert_eq!(s.wants(), Wants::Merge);
}

#[test]
fn the_ranking_puts_actionable_repositories_first() {
    let mut all = vec![
        Wants::Resolve,
        Wants::Nothing,
        Wants::Wait,
        Wants::Release,
        Wants::OpenPr,
        Wants::Commit,
        Wants::Merge,
        Wants::Attention,
    ];
    all.sort();
    assert_eq!(
        all,
        vec![
            Wants::Resolve,
            Wants::Merge,
            Wants::Attention,
            Wants::Commit,
            Wants::OpenPr,
            Wants::Release,
            Wants::Wait,
            Wants::Nothing
        ]
    );
}

#[test]
fn a_github_pr_json_object_carries_its_size_and_checks() {
    let value = json!({
        "number": 7,
        "title": "Add diff view",
        "url": "https://github.com/o/r/pull/7",
        "changedFiles": 5,
        "additions": 120,
        "deletions": 30,
        "commits": [{}, {}],
        "statusCheckRollup": [{"status": "COMPLETED", "conclusion": "SUCCESS"}],
    });
    let pr = github_pr_from_json(&value).unwrap();
    assert_eq!(pr.number, "7");
    assert_eq!(pr.title, "Add diff view");
    assert_eq!(pr.files, 5);
    assert_eq!(pr.additions, 120);
    assert_eq!(pr.deletions, 30);
    assert_eq!(pr.commits, 2);
    assert_eq!(pr.checks, Checks::Passing);
}

#[test]
fn a_pr_json_object_missing_a_number_is_rejected_rather_than_faked() {
    assert!(github_pr_from_json(&json!({"title": "no number"})).is_none());
}

#[test]
fn ahead_and_behind_are_parsed_in_the_order_git_prints_them() {
    // rev-list prints "<behind>\t<ahead>" for `upstream...HEAD` — behind
    // first, which is easy to swap by accident.
    assert_eq!(parse_ahead_behind("2\t3"), (3, 2));
}

#[test]
fn a_branch_caught_up_with_its_upstream_reports_nothing() {
    assert_eq!(parse_ahead_behind("0\t0\n"), (0, 0));
}

#[test]
fn unparsable_output_defaults_to_nothing_rather_than_panicking() {
    assert_eq!(parse_ahead_behind(""), (0, 0));
    assert_eq!(parse_ahead_behind("garbage"), (0, 0));
}

#[test]
fn every_state_worth_acting_on_names_the_need_behind_it() {
    assert_eq!(Wants::Merge.need(), Some(Need::OpenPullRequest));
    assert_eq!(Wants::Attention.need(), Some(Need::OpenPullRequest));
    assert_eq!(Wants::Wait.need(), Some(Need::OpenPullRequest));
    assert_eq!(Wants::Commit.need(), Some(Need::Uncommitted));
    assert_eq!(Wants::OpenPr.need(), Some(Need::UnpushedBranch));
    assert_eq!(Wants::Release.need(), Some(Need::Release));
    assert_eq!(Wants::Nothing.need(), None);
}

#[test]
fn need_keys_survive_a_round_trip_through_the_flow_file() {
    for need in Need::ALL {
        assert_eq!(Need::from_key(need.key()), Some(need));
    }
    assert_eq!(Need::from_key("deploy_vps"), None);
}

#[test]
fn committing_is_offered_only_when_there_is_something_to_commit() {
    let clean = status(0, None);
    assert!(!affordance(FlowKind::Commit, Some(&clean), false, false, "", &[]).enabled);

    let dirty = status(3, None);
    let a = affordance(FlowKind::Commit, Some(&dirty), false, false, "", &[]);
    assert!(a.enabled);
    assert_eq!(a.label, "Commit 3 files");
}

#[test]
fn the_button_counts_one_file_in_the_singular() {
    let one = status(1, None);
    assert_eq!(
        affordance(FlowKind::Commit, Some(&one), false, false, "", &[]).label,
        "Commit 1 file"
    );
}

#[test]
fn reviewing_is_offered_only_when_a_pull_request_exists() {
    let none = status(0, None);
    let a = affordance(FlowKind::Review, Some(&none), false, false, "", &[]);
    assert!(!a.enabled);
    assert!(a.reason.contains("master"), "says which branch");

    let open = status(0, Some(Checks::Passing));
    let b = affordance(FlowKind::Review, Some(&open), false, false, "", &[]);
    assert!(b.enabled);
    assert_eq!(b.label, "Review #2");
}

#[test]
fn picking_a_pr_from_the_list_enables_review_even_off_its_branch() {
    // The checked-out branch has nothing open (`none`), but a PR was
    // explicitly picked from the sidebar's list — Start must not stay
    // disabled just because HEAD points somewhere else.
    let none = status(0, None);
    let a = affordance(FlowKind::Review, Some(&none), false, false, "9", &[]);
    assert!(a.enabled);
    assert_eq!(a.label, "Review #9");
}

#[test]
fn the_same_repository_can_offer_one_flow_and_refuse_the_other() {
    // Committed and pushed: nothing left to commit, a PR now to review.
    let after = status(0, Some(Checks::Passing));
    assert!(!affordance(FlowKind::Commit, Some(&after), false, false, "", &[]).enabled);
    assert!(affordance(FlowKind::Review, Some(&after), false, false, "", &[]).enabled);

    // And the other way round, before any of that happened.
    let before = status(4, None);
    assert!(affordance(FlowKind::Commit, Some(&before), false, false, "", &[]).enabled);
    assert!(!affordance(FlowKind::Review, Some(&before), false, false, "", &[]).enabled);
}

#[test]
fn a_broken_flow_is_refused_however_much_work_is_waiting() {
    // The tab is now shown rather than hidden, so the button is what has
    // to say no — and it must say no even for a repository that is
    // otherwise perfectly ready to run this flow.
    let dirty = status(3, None);
    let problems = vec!["`push` depends on `commit`, which is not in this flow.".to_string()];
    let a = affordance(FlowKind::Commit, Some(&dirty), false, false, "", &problems);
    assert!(!a.enabled);
    assert_eq!(a.label, "Flow is broken");
    assert!(
        a.reason.contains("`push` depends on `commit`"),
        "says what is wrong"
    );
    assert!(a.reason.contains("Setup"), "says where to fix it");
}

#[test]
fn several_problems_name_the_first_and_count_the_rest() {
    let problems = vec!["One is wrong.".to_string(), "Two is wrong.".to_string()];
    let a = affordance(
        FlowKind::Commit,
        Some(&status(3, None)),
        false,
        false,
        "",
        &problems,
    );
    assert_eq!(a.reason, "One is wrong. And 1 more. Fix them in Setup.");
}

#[test]
fn a_broken_flow_outranks_even_a_pull_request_waiting_to_be_reviewed() {
    let open = status(0, Some(Checks::Passing));
    let problems = vec!["This flow has no steps.".to_string()];
    assert!(!affordance(FlowKind::Review, Some(&open), false, false, "7", &problems).enabled);
    // And without the problems it is offered as before.
    assert!(affordance(FlowKind::Review, Some(&open), false, false, "7", &[]).enabled);
}

#[test]
fn a_second_run_says_again() {
    let dirty = status(2, None);
    assert_eq!(
        affordance(FlowKind::Commit, Some(&dirty), false, true, "", &[]).label,
        "Commit 2 files again"
    );
}

#[test]
fn a_review_that_has_run_is_never_offered_again() {
    // #2 is the checked-out branch's own, just reviewed and declined.
    let mut s = status(0, Some(Checks::Passing));
    let a = affordance(FlowKind::Review, Some(&s), false, true, "", &[]);
    assert!(!a.enabled);
    assert_eq!(a.label, "Nothing else to review");

    // Another is open: that is where to go, not round again.
    let mut other = s.pr.clone().unwrap();
    other.number = "5".into();
    s.prs = vec![s.pr.clone().unwrap(), other];
    assert_eq!(s.next_to_review("2").as_deref(), Some("5"));
    let b = affordance(FlowKind::Review, Some(&s), false, true, "2", &[]);
    assert!(!b.enabled);
    assert!(b.reason.contains("#5"), "{}", b.reason);
    assert!(!b.label.contains("again"));
}

#[test]
fn the_next_review_skips_the_one_just_done_and_prefers_green() {
    let mut s = status(0, None);
    let base = status(0, Some(Checks::Passing)).pr.unwrap();
    s.prs = [
        ("3", Checks::Pending),
        ("4", Checks::Failing),
        ("7", Checks::Passing),
    ]
    .into_iter()
    .map(|(n, checks)| PrBrief {
        number: n.into(),
        checks,
        ..base.clone()
    })
    .collect();
    assert_eq!(s.next_to_review("3").as_deref(), Some("7"));
    assert_eq!(
        s.next_to_review("7").as_deref(),
        Some("4"),
        "red before running"
    );
    s.prs.retain(|p| p.number == "7");
    assert_eq!(s.next_to_review("7"), None);
}

#[test]
fn a_flow_built_in_setup_is_never_second_guessed() {
    let clean = status(0, None);
    let a = affordance(FlowKind::Other, Some(&clean), false, false, "", &[]);
    assert!(
        a.enabled,
        "only its author knows when a custom flow applies"
    );
}

#[test]
fn an_unprobed_repository_waits_rather_than_guessing() {
    assert!(!affordance(FlowKind::Commit, None, true, false, "", &[]).enabled);
    // But a probe that never returned must not lock the button forever.
    assert!(affordance(FlowKind::Commit, None, false, false, "", &[]).enabled);
}

#[test]
fn the_sidebar_says_how_big_a_pull_request_is() {
    // Four pull requests all reading "ready to merge" are indistinguishable;
    // their size is what tells them apart.
    let s = status(0, Some(Checks::Passing));
    assert_eq!(s.summary(), "#2 · 3f");
}

#[test]
fn a_pull_request_of_unknown_size_still_shows_its_number() {
    let mut s = status(0, Some(Checks::Passing));
    s.pr.as_mut().unwrap().files = 0;
    assert_eq!(s.summary(), "#2");
}

#[test]
fn the_size_line_gets_the_plural_right() {
    let mut pr = PrBrief {
        number: "1".into(),
        title: "t".into(),
        url: "u".into(),
        checks: Checks::Passing,
        files: 1,
        additions: 2,
        deletions: 0,
        commits: 1,
        base: String::new(),
        head: String::new(),
        failed: vec![],
    };
    assert_eq!(pr.size(), "1 file  +2  −0");
    pr.files = 115;
    pr.additions = 9297;
    pr.deletions = 4567;
    assert_eq!(pr.size(), "115 files  +9297  −4567");
}

#[tokio::test]
async fn an_override_decides_the_base_without_asking_git() {
    // ais_tom_platform: PRs target `develop`, but origin/HEAD in that
    // clone still points at `main`, 1536 commits back. Detection would
    // answer `main`; the override must win, and must say it did so
    // preflight can check the branch actually exists on origin.
    let (base, how) = base_branch("/no/such/repo", Some("develop".into())).await;
    assert_eq!(base, "develop");
    assert_eq!(how, OVERRIDDEN);
}

#[test]
fn a_base_that_is_1536_commits_behind_is_what_produced_the_bad_button() {
    // The symptom the override exists to prevent: clean tree, no PR, and
    // an unmerged count measured against the wrong base.
    let mut s = status(0, None);
    s.unmerged = 1536;
    assert_eq!(s.wants(), Wants::OpenPr);
    assert_eq!(
        affordance(FlowKind::Commit, Some(&s), false, false, "", &[]).label,
        "Push 1536 commits & open PR"
    );

    // Measured against the base the repository actually targets, there is
    // nothing to offer.
    s.unmerged = 0;
    assert_eq!(s.wants(), Wants::Nothing);
    assert!(!affordance(FlowKind::Commit, Some(&s), false, false, "", &[]).enabled);
}

#[test]
fn a_merged_pull_request_is_not_offered_for_review() {
    // The exact failure: merge with --delete-branch, stay on the branch,
    // and gh still answers with the merged PR.
    assert!(!is_open(&json!({ "number": 5, "state": "MERGED" })));
    assert!(!is_open(&json!({ "number": 5, "state": "CLOSED" })));
    assert!(is_open(&json!({ "number": 5, "state": "OPEN" })));
}

#[test]
fn a_gh_that_does_not_report_state_is_still_usable() {
    assert!(is_open(&json!({ "number": 5 })));
}

#[test]
fn the_state_check_is_not_case_sensitive() {
    assert!(is_open(&json!({ "state": "open" })));
}

#[test]
fn one_red_check_makes_the_rollup_red() {
    // Their own case: fmt fails on purpose, test passes.
    let value = json!([
        { "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS" },
        { "name": "fmt",  "status": "COMPLETED", "conclusion": "FAILURE" },
    ]);
    assert_eq!(rollup(&value), Checks::Failing);
}

#[test]
fn a_running_check_reads_as_pending() {
    let value = json!([{ "name": "test", "status": "IN_PROGRESS" }]);
    assert_eq!(rollup(&value), Checks::Pending);
}

#[test]
fn skipped_and_neutral_checks_do_not_count_against_a_build() {
    let value = json!([
        { "name": "a", "status": "COMPLETED", "conclusion": "SKIPPED" },
        { "name": "b", "status": "COMPLETED", "conclusion": "NEUTRAL" },
    ]);
    assert_eq!(rollup(&value), Checks::Passing);
}

#[test]
fn no_checks_at_all_is_unknown_not_green() {
    assert_eq!(rollup(&json!([])), Checks::Unknown);
    assert_eq!(rollup(&json!(null)), Checks::Unknown);
}
