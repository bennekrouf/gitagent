use super::*;

const DIRTY_MERGE: &str = "error: Your local changes to the following files would be \
        overwritten by merge:\n\tCHANGELOG.md\n\tassets/main.css\nPlease commit your changes \
        or stash them before you merge.\nAborting\n\nwhile running: `git merge --ff-only \
        origin/main`";

#[test]
fn local_edits_in_the_way_of_sync_offer_an_autostash() {
    let failure = sync_merge_failure("main", DIRTY_MERGE.into());
    assert!(failure.message.contains("The merge is done"));
    assert_eq!(failure.remedies.len(), 1);
    let remedy = &failure.remedies[0];
    assert_eq!(
        remedy.args,
        ["merge", "--ff-only", "--autostash", "origin/main"]
    );
    assert!(remedy.retry_after);
    assert!(!remedy.abandons);
}

#[cfg(unix)]
#[tokio::test]
async fn edits_are_left_alone_and_a_clash_from_before_is_still_named() {
    // Real git: origin changes a file this copy has uncommitted edits to.
    let root = std::env::temp_dir().join(format!("gitagent-autostash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let sh = |cmd: &str| {
        let out = std::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let ident = "git config user.name t && git config user.email t@t";
    sh("git init -q --bare -b main origin.git");
    sh(&format!(
        "git clone -q origin.git here && cd here && {ident} && echo a > f && \
             git add f && git commit -q -m init && git push -q -u origin main"
    ));
    sh(&format!(
        "git clone -q origin.git forge && cd forge && {ident} && echo b > f && \
             git commit -q -am theirs && git push -q"
    ));
    sh("cd here && echo mine > f");
    let repo = root.join("here");
    let repo = repo.to_str().unwrap();
    let mut state = RunState::default();
    state.artifacts.insert("pr_base".into(), "main".into());

    // Edits in the way are left alone, and the step still finishes.
    let kept = sync(repo, &state)
        .await
        .expect("never a failure over edits");
    assert!(
        kept.summary.contains("1 behind; your edits kept"),
        "{}",
        kept.summary
    );
    assert_eq!(
        std::fs::read_to_string(root.join("here/f")).unwrap().trim(),
        "mine"
    );

    // Setting them aside and updating anyway — by hand, or from before —
    // can leave a clash; then the step says which files to resolve.
    sh("cd here && git merge --ff-only --autostash origin/main || true");
    assert_eq!(git::unmerged_paths(repo).await, ["f"], "the edits clashed");

    let done = sync(repo, &state).await.expect("the retry finishes");
    assert!(
        done.summary.contains("1 file to resolve"),
        "{}",
        done.summary
    );
    assert!(done.log.contains("git stash drop"));

    let release = flow::catch_up(repo).await.expect_err("markers in f");
    assert!(release.message.contains("  f"));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_diverged_base_is_offered_a_replay_rather_than_nothing() {
    let failure = sync_diverged(
        "main",
        "fatal: Not possible to fast-forward, aborting.".into(),
        "",
    );
    assert_eq!(failure.remedies[0].args, ["rebase", "origin/main"]);
    assert!(
        failure.remedies[0].retry_after,
        "replayed, Back to base finishes"
    );
    assert!(failure.message.contains("The merge is done"));
}

#[test]
fn other_sync_merge_failures_stay_as_they_were() {
    let failure = sync_merge_failure("main", "fatal: Not possible to fast-forward".into());
    assert!(failure.remedies.is_empty());
    assert_eq!(failure.message, "fatal: Not possible to fast-forward");
}

const REF_RACE: &str = "error: cannot lock ref 'refs/remotes/origin/master': is at \
        841f7d89d678b65ba252d57fe179dd7b6162d07b but expected \
        871b1b5141f913cce8194a1428b2ce4df0baa09f\n \
        ! 871b1b5..841f7d8  master     -> origin/master  (unable to update local ref)";

#[test]
fn a_ref_moved_by_another_fetch_explains_itself_and_offers_a_retry() {
    let failure = sync_fetch_failure("master", REF_RACE.into());
    assert!(failure
        .message
        .contains("Nothing is wrong with the repository"));
    assert_eq!(failure.remedies.len(), 1);
    assert_eq!(failure.remedies[0].args, ["fetch", "origin", "master"]);
    assert!(
        failure.remedies[0].retry_after,
        "the step finishes on retry"
    );
}

#[test]
fn any_other_fetch_failure_is_passed_through_untouched() {
    let failure = sync_fetch_failure("master", "fatal: could not read from remote".into());
    assert!(failure.remedies.is_empty());
    assert!(!git::ref_moved_underneath(&failure.message));
}

#[test]
fn a_job_id_is_read_out_of_the_details_url() {
    assert_eq!(
        job_id("https://github.com/o/r/actions/runs/32857682937/job/97833574009"),
        Some("97833574009")
    );
    assert_eq!(job_id("https://example.com/no-job-here"), None);
    assert_eq!(job_id(""), None);
}

#[test]
fn the_log_is_cut_down_to_the_failure_and_what_led_to_it() {
    let raw = "\
fmt\tUNKNOWN STEP\t2026-08-25T14:08:59.4479744Z Current runner version: '2.336.0'
fmt\tUNKNOWN STEP\t2026-08-25T14:08:59.4505213Z ##[group]Runner Image Provisioner
fmt\tUNKNOWN STEP\t2026-08-25T14:09:12.9480177Z -const RELEASES_URL: &str =
fmt\tUNKNOWN STEP\t2026-08-25T14:09:12.9480622Z +const RELEASES_URL: &str = \"…\";
fmt\tUNKNOWN STEP\t2026-08-25T14:09:12.9490017Z ##[error]Process completed with exit code 1.
fmt\tUNKNOWN STEP\t2026-08-25T14:09:13.0464818Z Post job cleanup.
fmt\tUNKNOWN STEP\t2026-08-25T14:09:13.0508478Z git version 2.55.0";

    let out = distil_log(raw, 3);
    assert!(out.contains("Process completed with exit code 1"));
    assert!(
        out.contains("-const RELEASES_URL"),
        "the rustfmt diff is the point"
    );
    assert!(
        !out.contains("Post job cleanup"),
        "nothing after the failure"
    );
    assert!(!out.contains("##[group]"), "group markers stripped");
    assert!(!out.contains("2026-08-25T"), "timestamps stripped");
    assert!(!out.contains("##[error]"), "the marker itself is noise");
}

#[test]
fn cargo_colours_do_not_reach_the_reader() {
    let raw = concat!(
        "j\ts\t2026-08-28T14:09:12.9480177Z ",
        "\u{1b}[1m\u{1b}[91merror\u{1b}[0m: found call to `str::trim`\n",
        "j\ts\t2026-08-28T14:09:12.9490017Z ",
        "##[error]Process completed with exit code 101."
    );
    let out = distil_log(raw, 3);
    assert!(!out.contains('\u{1b}'), "no escapes survive");
    assert!(!out.contains("[1m"), "and no leftovers either");
    assert!(
        out.contains("error: found call to `str::trim`"),
        "got {out}"
    );
}

#[test]
fn stripping_colour_leaves_ordinary_text_alone() {
    assert_eq!(strip_ansi("plain [not] an escape"), "plain [not] an escape");
}

#[test]
fn a_truncated_escape_does_not_eat_the_rest_of_the_line() {
    assert_eq!(strip_ansi("before\u{1b}"), "before");
}

#[test]
fn only_the_lines_asked_for_are_kept() {
    let raw = (0..100)
        .map(|i| format!("j\ts\t2026-08-25T14:09:12.9480177Z line {i}"))
        .chain(["j\ts\t2026-08-25T14:09:12.9490017Z ##[error]boom".to_string()])
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(distil_log(&raw, 5).lines().count(), 6);
}

#[test]
fn a_log_with_no_error_marker_still_yields_its_tail() {
    let raw = "j\ts\t2026-08-25T14:09:12.9480177Z something went sideways";
    assert!(distil_log(raw, 10).contains("something went sideways"));
}

#[test]
fn an_empty_log_does_not_panic() {
    assert_eq!(distil_log("", 10), "");
}

#[test]
fn the_merge_approval_shows_ci_and_the_model_side_by_side() {
    let mut s = RunState::default();
    s.artifacts.insert("pr_number".into(), "7".into());
    s.artifacts
        .insert("checks_summary".into(), "1 passing, 1 failing".into());
    s.artifacts.insert("merge_state".into(), "UNSTABLE".into());
    s.artifacts.insert("verdict".into(), "worth_a_look".into());
    s.artifacts
        .insert("analysis".into(), "the lockfile moved".into());

    let text = proposal(Step::Merge, &s);
    assert!(text.contains("1 failing"), "CI state must be visible");
    assert!(text.contains("UNSTABLE"));
    assert!(text.contains("worth_a_look"));
    assert!(text.contains("the lockfile moved"));
}

#[test]
fn only_the_merge_step_proposes_anything() {
    let s = RunState::default();
    for step in [
        Step::FindPr,
        Step::PrStatus,
        Step::PrDiff,
        Step::Analyse,
        Step::SecondOpinion,
        Step::Lenses,
        Step::Sync,
    ] {
        assert!(proposal(step, &s).is_empty());
    }
}

#[test]
fn a_merge_conflict_offers_resolving_before_abandoning() {
    let failure = merge_failure(
            "8",
            "master",
            "X Pull request bennekrouf/gitagent#8 is not mergeable: the merge commit cannot be cleanly created.".to_string(),
            true,
        );
    assert_eq!(failure.remedies.len(), 2);

    // Constructive first: closing the pull request should never be the
    // only thing on offer for a conflict.
    let update = &failure.remedies[0];
    assert_eq!(update.program, "sh");
    assert_eq!(
        update.args[3..],
        ["8", "master"],
        "names travel as arguments"
    );
    assert!(
        update.args[1].contains("gh pr checkout \"$1\""),
        "on the PR's own branch"
    );
    assert!(
        update.retry_after,
        "it only succeeds once the merge was clean and pushed, so merging again is next"
    );
    assert!(
        update.args[1].find("git merge").unwrap() < update.args[1].find("git push").unwrap(),
        "nothing is pushed unless the merge went through"
    );
    assert!(!update.abandons, "bringing the base in is not giving up");
    assert!(
        failure.message.contains("Finish the merge"),
        "the steps after it are spelled out"
    );

    let abandon = &failure.remedies[1];
    assert_eq!(abandon.program, "gh");
    assert_eq!(abandon.args[0..3], ["pr", "close", "8"]);
    assert!(!abandon.retry_after);
}

#[test]
fn checks_still_running_are_not_read_as_a_conflict() {
    // #199: clean against main, its required checks still running. GitHub's
    // refusal says "not mergeable" too, and was taken for a conflict — which
    // offered to merge main in (restarting the checks) or close the PR.
    let failure = merge_failure(
        "199",
        "main",
        "X Pull request bennekrouf/gitagent#199 is not mergeable: the base branch policy \
         prohibits the merge. To have the pull request merged after all the requirements \
         have been met, add the `--auto` flag."
            .to_string(),
        true,
    );
    assert!(
        failure.message.contains("no conflicts"),
        "{}",
        failure.message
    );
    assert!(!failure.message.contains("<<<<<<<"));
    // Nothing waits on the checks: GitHub is handed the merge instead.
    assert_eq!(failure.remedies.len(), 1);
    assert!(failure
        .remedies
        .iter()
        .all(|r| !r.args.iter().any(|a| a.contains("gh pr checks"))));

    let auto = &failure.remedies[0];
    assert_eq!(auto.args[0..4], ["pr", "merge", "199", "--auto"]);
    assert!(
        !auto.retry_after,
        "GitHub finishes it; merging again would only fail"
    );
    assert!(!auto.abandons, "it is the merge, done another way");

    assert!(
        !failure
            .remedies
            .iter()
            .any(|r| r.args.iter().any(|a| a == "close")),
        "nothing offers to close a pull request that only has to wait"
    );
}

#[test]
fn the_update_remedy_targets_the_pull_requests_own_base() {
    let failure = merge_failure("1", "develop", "not mergeable".to_string(), true);
    assert_eq!(failure.remedies[0].args[4], "develop");
}

#[test]
fn an_empty_pull_request_offers_closing_it() {
    let failure = empty_pr(&Forge::GitHub, &[], "11", "feat/a", "chore/b", 3);
    assert!(failure.message.contains("#11"));
    assert!(
        failure.message.contains("3 commit(s)"),
        "says why it is empty"
    );
    assert_eq!(failure.remedies.len(), 1);
    let close = &failure.remedies[0];
    assert_eq!(close.args[0..3], ["pr", "close", "11"]);
    assert!(
        !close.args.iter().any(|a| a == "--delete-branch"),
        "history stays reachable"
    );
    assert!(
        !close.retry_after,
        "retrying the diff after closing would be pointless"
    );
}

#[test]
fn an_empty_pull_request_already_in_its_base_says_so() {
    let failure = empty_pr(&Forge::AzureDevOps, &[], "4", "main", "topic", 0);
    assert!(failure.message.contains("already in `main`"));
    assert_eq!(failure.remedies[0].program, "az");
}

#[test]
fn a_conflicted_bring_in_turns_into_finishing_or_abandoning_it() {
    let out = "Switched to branch 'feat/x'\nAuto-merging Cargo.lock\n\
                   CONFLICT (content): Merge conflict in Cargo.lock\n\
                   CONFLICT (content): Merge conflict in src/api/config_upload.rs\n\
                   Automatic merge failed; fix conflicts and then commit the result.";
    let next = conflict_followup("66", "master", out, &[]).unwrap();
    assert!(next
        .message
        .contains("  Cargo.lock\n  src/api/config_upload.rs"));
    assert!(next.message.contains("Nothing has been pushed"));
    let finish = &next.remedies[0];
    assert!(
        finish.retry_after,
        "finishing leads straight back to the merge"
    );
    assert!(
        finish.args[1].contains("<<<<<<<"),
        "refuses while markers remain"
    );
    assert!(finish.args[1].find("git commit").unwrap() < finish.args[1].find("git push").unwrap());
    assert_eq!(next.remedies[1].args, ["merge", "--abort"]);
    assert!(conflict_followup("66", "master", "fatal: not a git repository", &[]).is_none());
}

#[test]
fn a_quote_matches_the_code_whatever_the_diff_markers_and_spacing() {
    let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,2 @@\n-        !self.is_base\n-            && self.pr_state != PrState::Open\n+        self.pr_state != PrState::Open\n";
    assert!(
        quoted_from(diff, "!self.is_base && self.pr_state != PrState::Open"),
        "two removed lines, joined"
    );
    assert!(
        quoted_from(diff, "`self.pr_state != PrState::Open`"),
        "in backticks"
    );
    assert!(
        quoted_from(diff, "-        !self.is_base"),
        "with its marker, exactly"
    );
    assert!(
        !quoted_from(diff, "!self.is_base || true"),
        "not what the diff says"
    );
    assert!(
        !quoted_from(diff, "PrState::Open PrState::Open"),
        "garbled by repetition"
    );
    assert!(!quoted_from(diff, "   "), "nothing quoted");
}

#[test]
fn a_merge_failure_for_any_other_reason_offers_nothing() {
    let failure = merge_failure(
        "8",
        "master",
        "gh: authentication required".to_string(),
        true,
    );
    assert!(failure.remedies.is_empty());
}

const TWO_FILES: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
@@ -1,3 +1,3 @@\n fn keep_me_as_context() {}\n-    let limit = 10;\n+    let limit = 0;\n\
diff --git a/src/b.rs b/src/b.rs\n--- a/src/b.rs\n+++ b/src/b.rs\n@@ -1,2 +1,2 @@\n\
 fn other_context() {}\n+    retry_forever();\n";

#[test]
fn a_finding_stands_only_on_a_changed_line_of_the_file_it_names() {
    assert!(
        backed(TWO_FILES, "src/a.rs", "let limit = 0;"),
        "added line"
    );
    assert!(
        backed(TWO_FILES, "a.rs", "-    let limit = 10;"),
        "removed line, short name"
    );
    assert!(
        !backed(TWO_FILES, "src/a.rs", "fn keep_me_as_context() {}"),
        "true quote of context, but nothing the change did"
    );
    assert!(
        !backed(TWO_FILES, "src/b.rs", "let limit = 0;"),
        "real line, wrong file"
    );
    assert!(
        !backed(TWO_FILES, "src/c.rs", "let limit = 0;"),
        "no such file"
    );
    assert!(
        backed(TWO_FILES, "", "retry_forever();"),
        "no file named: whole diff"
    );
}

#[test]
fn risky_needs_a_high_severity_finding_that_held_up() {
    let (verdict, note) = supported_verdict("risky", 0);
    assert_eq!(verdict, "worth_a_look");
    assert!(note.is_some(), "says why it is not risky");
    assert_eq!(supported_verdict("risky", 1), ("risky".to_string(), None));
    assert_eq!(
        supported_verdict("looks_safe", 0),
        ("looks_safe".to_string(), None)
    );
}

#[test]
fn a_branch_reused_after_a_squash_merge_is_moved_rather_than_merged_into() {
    let failure = replay_after(
        "38",
        "main",
        "feat/recording",
        &("37".into(), "c9fef29aaaaaaaaaaaa".into()),
        "feat/recording-2",
        "0 passing",
    );
    assert!(failure.message.contains("already merged once, in #37"));
    assert!(failure.message.contains("not offered for merging"));
    let replay = &failure.remedies[0];
    assert!(!replay.args[1].contains("--force"), "never force-pushes");
    assert!(replay.args[1].contains("cherry-pick \"$5..origin/$3\""));
    assert_eq!(
        &replay.args[3..],
        [
            "38",
            "main",
            "feat/recording",
            "feat/recording-2",
            "c9fef29aaaaaaaaaaaa",
            "37"
        ]
    );
    assert!(
        failure.remedies.len() > 1,
        "bringing main in stays on offer"
    );
}

/// Runs the Finish script's guard in a merge of `main` into a pull request's
/// branch that conflicted in `f`, settled with `side` (`--ours` keeps the
/// branch, `--theirs` main). Returns whether it let the commit through.
#[cfg(unix)]
fn finish_guard_after(side: &str) -> (bool, String) {
    finish_guard_in(side, "")
}

/// The same, with `extra` run on the branch and main before the merge:
/// whatever else the two sides did to other files.
#[cfg(unix)]
fn finish_guard_in(side: &str, extra: &str) -> (bool, String) {
    let root = std::env::temp_dir().join(format!(
        "gitagent-finish-{side}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let sh = |cmd: &str| {
        std::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&root)
            .output()
            .unwrap()
    };
    let setup = format!(
        "git init -q --bare -b main origin.git && git clone -q origin.git here && cd here && \
         git config user.name t && git config user.email t@t && \
         echo base > f && echo base > g && git add . && git commit -q -m init && git push -q origin main && \
         git checkout -q -b feat && echo pr > f && git commit -qam pr && \
         git checkout -q main && echo main > f && echo main > g && git commit -qam main && \
         {extra} git push -q origin main && \
         git checkout -q feat && (git merge -q origin/main >/dev/null 2>&1; true) && \
         git checkout {side} -- f && git add f"
    );
    let out = sh(&setup);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = std::process::Command::new("sh")
        .args(["-c", KEEPS_THE_CHANGE, "sh", "38", "main"])
        .current_dir(root.join("here"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[cfg(unix)]
#[test]
fn finishing_a_merge_refuses_when_the_pull_requests_change_was_dropped() {
    let (ok, said) = finish_guard_after("--theirs");
    assert!(!ok, "main's side everywhere loses #38's change to f");
    assert!(said.contains("  f\n"), "{said}");
    assert!(!said.contains("  g\n"), "g only changed on main: {said}");

    let (ok, said) = finish_guard_after("--ours");
    assert!(ok, "keeping the branch's side is fine: {said}");
}

#[cfg(unix)]
#[test]
fn a_file_git_merged_to_the_bases_version_is_not_counted_as_lost() {
    // The branch changed h, and main already has that change and more: git
    // merges h to main's version by itself, with no conflict, and nothing of
    // the branch's is lost. Only f conflicted, kept on the branch's side.
    let extra = "git checkout -q feat && echo pr > h && git add h && git commit -qm h && \
                 git checkout -q main && echo pr > h && echo more >> h && git add h && \
                 git commit -qm 'h, updated' &&";
    let (ok, said) = finish_guard_in("--ours", extra);
    assert!(ok, "h was merged by git, not dropped: {said}");
}

#[cfg(unix)]
#[test]
fn a_lock_file_settled_on_the_bases_side_is_not_counted_as_lost() {
    let extra = "git checkout -q feat && echo pr > uv.lock && git add uv.lock && \
                 git commit -qm lock && git checkout -q main && echo main > uv.lock && \
                 git add uv.lock && git commit -qm lock &&";
    let (ok, said) = finish_guard_in("--ours", extra);
    assert!(ok, "{said}");
}

#[test]
fn auto_merge_is_only_offered_where_the_repository_allows_it() {
    let failure = merge_failure(
        "7",
        "main",
        "X Pull request o/r#7 is not mergeable: the base branch policy prohibits the merge."
            .to_string(),
        false,
    );
    assert!(
        failure.remedies.is_empty(),
        "no auto-merge, no admin: nothing to wait on"
    );
    assert!(failure
        .message
        .contains("Merge again once the checks are green"));
}

#[test]
fn checks_that_already_failed_offer_a_rerun_not_a_wait() {
    // publishkit#67: three required checks red. "Wait for the checks" ended
    // at once with gh's table, and nothing moved the merge on.
    let json = r#"[{"bucket":"fail","link":"https://github.com/o/r/actions/runs/41919067445/job/1","name":"ci / test (macos-latest)"},
{"bucket":"fail","link":"https://github.com/o/r/actions/runs/41919067445/job/2","name":"ci / test (windows-latest)"},
{"bucket":"pass","link":"https://github.com/o/r/actions/runs/41919067445/job/3","name":"ci / fmt"}]"#;
    let failed = failed_checks(json);
    assert_eq!(failed.len(), 2);
    let hold = Hold {
        auto_merge: false,
        admin: true,
        failed,
        error: None,
    };
    let failure = policy_guide(
        "67",
        "X Pull request o/r#67 is not mergeable: the base branch policy prohibits the merge.",
        &hold,
    );
    assert!(
        failure.message.contains("2 required checks failed"),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("ci / test (macos-latest)"));
    assert!(!failure.remedies.iter().any(|r| r.label.starts_with("Wait")));

    let rerun = &failure.remedies[0];
    assert!(rerun.label.starts_with("Run the failed checks again"));
    assert_eq!(
        rerun.args[3..],
        ["67", "41919067445"],
        "one rerun per run, as arguments"
    );
    assert!(rerun.args[1].contains("gh run rerun \"$run\" --failed"));
    assert!(
        !rerun.args[1].contains("gh pr checks"),
        "it reruns and returns, it never waits"
    );
    assert!(
        !rerun.retry_after,
        "merging straight after a rerun would only fail"
    );

    let admin = &failure.remedies[1];
    assert_eq!(admin.args[0..4], ["pr", "merge", "67", "--admin"]);
    assert!(!admin.retry_after);
}

#[test]
fn skipping_the_checks_is_only_offered_to_an_admin() {
    let hold = Hold {
        failed: failed_checks(r#"[{"bucket":"fail","link":"","name":"ci"}]"#),
        ..Default::default()
    };
    let failure = policy_guide("1", "policy", &hold);
    assert!(!failure
        .remedies
        .iter()
        .any(|r| r.args.iter().any(|a| a == "--admin")));
}

#[test]
fn a_merge_already_stopped_offers_finish_whatever_was_printed() {
    // #73: every file resolved, yet the step still offered "Bring main in"
    // again: running it mid-merge prints no "Merge conflict in" lines.
    assert!(conflict_followup(
        "73",
        "main",
        "error: You have not concluded your merge",
        &[]
    )
    .is_none());
    let next = merge_stopped(
        "73",
        "main",
        &["CHANGELOG.md", "Cargo.lock", "Cargo.toml"],
        &[],
    )
    .unwrap();
    assert!(next.remedies[0].label.starts_with("Finish the merge"));
    assert!(next.remedies.iter().all(|r| !r.label.starts_with("Bring")));
    assert!(next.message.contains("Cargo.lock"));
    assert!(merge_stopped("73", "main", &[], &[]).is_none());
}

#[test]
fn an_error_in_the_code_is_shown_and_no_rerun_is_offered() {
    // polishcast#72: clippy stopped both test jobs on the same line, three
    // attempts running, and "Run again" was all the step offered.
    let hold = Hold {
        admin: false,
        failed: failed_checks(
            r#"[{"bucket":"fail","link":"https://github.com/o/r/actions/runs/42103719826/job/128199168175","name":"ci / test (windows-latest)"}]"#,
        ),
        error: Some(
            "crates/app/src/takes.rs:92:16 \u{2014} error: used `expect()` on an `Option` value"
                .into(),
        ),
        ..Default::default()
    };
    let failure = policy_guide("72", "policy", &hold);
    assert!(
        failure.message.contains("takes.rs:92:16"),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("won\u{2019}t help"));
    assert!(failure
        .remedies
        .iter()
        .all(|r| !r.label.starts_with("Run the failed checks")));
}

#[test]
fn a_runner_failure_without_a_code_error_still_offers_the_rerun() {
    use crate::services::checkhint::code_error;
    assert!(code_error("##[error]The runner has received a shutdown signal.").is_none());
    assert!(
        code_error("error: used `expect()` on an `Option` value\n  --> src/a.rs:1:1").is_some()
    );
}

#[test]
fn the_merge_approval_shows_the_command_this_forge_will_run() {
    let mut state = RunState::default();
    state.artifacts.insert("pr_number".into(), "12".into());
    state.artifacts.insert("forge".into(), "azure".into());
    let azure = proposal(Step::Merge, &state);
    assert!(
        azure.starts_with("az repos pr update --id 12 --status completed"),
        "{azure}"
    );
    assert!(!azure.contains("gh pr merge"), "{azure}");

    state.artifacts.insert("forge".into(), "github".into());
    assert!(proposal(Step::Merge, &state).starts_with("gh pr merge 12"));
}

#[cfg(unix)]
#[tokio::test]
async fn running_merge_again_mid_merge_offers_finish_not_uncommitted_changes() {
    // Every conflict resolved, then Retry: the staged merge was read as
    // uncommitted changes, with no fix offered, while the conflict panel
    // said "Finish the merge below".
    let dir = std::env::temp_dir().join(format!("gitagent-midmerge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    let repo = dir.to_string_lossy().to_string();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("CHANGELOG.md"), "one\n").unwrap();
    git(&["add", "CHANGELOG.md"]);
    git(&["commit", "-qm", "one"]);
    assert!(
        merge_in_progress(&repo, "7", "main").await.is_none(),
        "no merge open"
    );

    git(&["checkout", "-qb", "feat"]);
    std::fs::write(dir.join("CHANGELOG.md"), "feat\n").unwrap();
    git(&["commit", "-qam", "feat"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(dir.join("CHANGELOG.md"), "main\n").unwrap();
    git(&["commit", "-qam", "main"]);
    git(&["checkout", "-q", "feat"]);
    git(&["merge", "-q", "main"]);
    std::fs::write(dir.join("CHANGELOG.md"), "feat\nmain\n").unwrap();
    git(&["add", "CHANGELOG.md"]);

    let open = merge_in_progress(&repo, "7", "main")
        .await
        .expect("a merge is open");
    assert!(open.remedies[0].label.starts_with("Finish the merge"));
    assert!(open.remedies[0].once_resolved);
    assert!(open.message.contains("CHANGELOG.md"));
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_merge_refused_over_uncommitted_changes_offers_commit_and_pr() {
    // The refusal used to end on "No automatic fix for this", although the
    // fix is a flow the window can start: Commit → PR.
    let dir = std::env::temp_dir().join(format!("gitagent-dirtymerge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    let repo = dir.to_string_lossy().to_string();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("en.json"), "{}\n").unwrap();
    git(&["add", "en.json"]);
    git(&["commit", "-qm", "one"]);
    std::fs::write(dir.join("en.json"), "{\"a\":1}\n").unwrap();

    let mut state = RunState::default();
    state.artifacts.insert("pr_number".into(), "145".into());
    let refused = merge(&repo, &state)
        .await
        .expect_err("never merged with edits in the tree");
    assert!(refused.message.contains("en.json"), "{}", refused.message);
    assert_eq!(refused.remedies.len(), 1);
    let fix = &refused.remedies[0];
    assert_eq!(fix.starts, Some(crate::services::probe::Need::Uncommitted));
    assert_eq!(fix.display, "Commit → PR");
    assert!(
        !fix.retry_after,
        "the merge is run again by hand, after the commit"
    );
    assert!(!fix.abandons);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_admin_is_offered_to_allow_auto_merge_where_it_is_off() {
    let message =
        "X Pull request o/r#7 is not mergeable: the base branch policy prohibits the merge.";
    let admin = Hold {
        admin: true,
        ..Hold::default()
    };
    let failure = policy_guide("7", message, &admin);
    let allow = &failure.remedies[0];
    assert!(
        allow.label.starts_with("Allow auto-merge"),
        "{}",
        allow.label
    );
    assert_eq!(allow.args[3], "7", "the number travels as an argument");
    assert!(allow.args[1].contains("allow_auto_merge=true"));

    let allowed = Hold {
        auto_merge: true,
        admin: true,
        ..Hold::default()
    };
    let failure = policy_guide("7", message, &allowed);
    assert!(
        !failure
            .remedies
            .iter()
            .any(|r| r.label.starts_with("Allow")),
        "already allowed: nothing to turn on"
    );
}

#[test]
fn only_an_admin_is_offered_to_change_the_repository() {
    let failure = policy_guide(
        "7",
        "X Pull request o/r#7 is not mergeable: the base branch policy prohibits the merge.",
        &Hold::default(),
    );
    assert!(failure.remedies.is_empty());
    assert!(failure.message.contains("ask an admin"));
}

#[test]
fn a_stopped_merge_says_two_steps_and_a_way_out() {
    let next = merge_stopped("239", "main", &["CHANGELOG.md", "src/x.rs"], &[]).unwrap();
    assert!(
        next.message
            .starts_with("#239 conflicts with main in 2 files."),
        "{}",
        next.message
    );
    assert!(
        next.message.contains("1. Resolve each above"),
        "{}",
        next.message
    );
    assert!(next.message.contains("2. Finish below"), "{}", next.message);
    assert!(
        next.message.lines().count() <= 10,
        "short: {}",
        next.message
    );
    assert!(
        !next.message.contains("Incoming"),
        "said on the buttons instead"
    );
}

#[test]
fn a_changelog_merged_for_you_is_not_asked_about() {
    let settled = vec!["CHANGELOG.md".to_string()];
    let next = merge_stopped("239", "main", &["CHANGELOG.md", "src/x.rs"], &settled).unwrap();
    assert!(next.message.contains("in 1 file."), "{}", next.message);
    assert!(next.message.contains("  src/x.rs"));
    assert!(next.message.contains("CHANGELOG.md merged for you"));

    let only = merge_stopped("239", "main", &["CHANGELOG.md"], &settled).unwrap();
    assert!(
        only.message.contains("only in CHANGELOG.md merged for you"),
        "{}",
        only.message
    );
    assert!(!only.message.contains("1. Resolve"));
    assert!(only.remedies[0].label.starts_with("Finish the merge"));
}

#[test]
fn finish_checks_the_build_before_it_commits() {
    let next = merge_stopped("239", "main", &["src/x.rs"], &[]).unwrap();
    let script = &next.remedies[0].args[1];
    let check = script
        .find("cargo check")
        .expect("checks a Rust project builds");
    assert!(
        check < script.find("git commit").unwrap(),
        "before the commit"
    );
    assert!(script.contains("go build"));
    assert!(
        script.contains("git add -- Cargo.lock"),
        "an updated lock goes in the merge"
    );
}

#[test]
fn misfiled_notes_stop_the_merge_with_one_fix() {
    let stop = misfiled_guide(
        "239",
        "main",
        "CHANGELOG.md",
        &["- Ours.".to_string()],
        "/tmp/fixed.md",
        "abc123",
    );
    assert!(stop.message.contains("wrong version"), "{}", stop.message);
    assert!(stop.message.contains("  - Ours."));
    assert!(stop.message.contains("Nothing has been merged"));
    let fix = &stop.remedies[0];
    assert!(
        fix.retry_after,
        "the merge runs again once the notes are moved"
    );
    assert_eq!(
        fix.args[2..],
        [
            "sh",
            "239",
            "main",
            "/tmp/fixed.md",
            "CHANGELOG.md",
            "abc123"
        ],
        "everything travels as an argument"
    );
    let script = &fix.args[1];
    assert!(
        script.find("rev-parse").unwrap() < script.find("git merge").unwrap(),
        "a moved base is caught first"
    );
    assert!(script.find("git commit --amend").unwrap() < script.find("git push").unwrap());
}

#[test]
fn closing_from_the_card_runs_each_forges_own_command() {
    let (program, args) = close_command(&Forge::GitHub, &[], "243", false).unwrap();
    assert_eq!(program, "gh");
    assert_eq!(args[0..3], ["pr", "close", "243"]);
    assert_eq!(args[4], "Closing without merging.");
    let (_, args) = close_command(&Forge::GitHub, &[], "243", true).unwrap();
    assert!(args[4].contains("no longer changes any files"));
    // Abandoned, not deleted: Azure keeps it, and the org scope finds it.
    let scope = vec![
        "--org".to_string(),
        "https://dev.azure.com/acme".to_string(),
    ];
    let (program, args) = close_command(&Forge::AzureDevOps, &scope, "243", true).unwrap();
    assert_eq!(program, "az");
    assert_eq!(
        args[0..7],
        [
            "repos",
            "pr",
            "update",
            "--id",
            "243",
            "--status",
            "abandoned"
        ]
    );
    assert_eq!(args[args.len() - 2..], scope[..]);
    assert!(close_command(&Forge::None, &[], "243", false).is_none());
}

#[test]
fn a_completed_azure_pull_request_has_nothing_to_review() {
    let pr = serde_json::json!({
        "pullRequestId": 42,
        "status": "completed",
        "targetRefName": "refs/heads/develop",
        "sourceRefName": "refs/heads/feature",
    });
    let reason = azure_not_active(&pr, "feature").expect("a merged PR is not reviewed");
    assert!(reason.contains("already merged"), "{reason}");
    assert!(reason.contains("develop"), "{reason}");

    let abandoned = serde_json::json!({ "pullRequestId": 7, "status": "abandoned" });
    assert!(azure_not_active(&abandoned, "x")
        .unwrap()
        .contains("abandoned"));

    let active = serde_json::json!({ "pullRequestId": 1, "status": "active" });
    assert!(azure_not_active(&active, "x").is_none());
}

#[test]
fn the_azure_base_is_the_pull_requests_target_never_a_guess() {
    let pr = serde_json::json!({ "targetRefName": "refs/heads/develop" });
    assert_eq!(azure_ref(&pr, "targetRefName").as_deref(), Some("develop"));
    assert_eq!(azure_ref(&serde_json::json!({}), "targetRefName"), None);
}

#[test]
fn a_deleted_head_branch_is_recognised_in_the_fetch_error() {
    let e = "could not fetch develop and feature from origin: fatal: couldn't find remote ref \
             feature\nwhile running: `git fetch origin develop feature`";
    assert!(head_gone(e, "feature"));
    assert!(!head_gone(e, "other"));
    assert!(!head_gone("fatal: unable to access", "feature"));
}

#[test]
fn merge_tree_conflicts_are_read_once_each() {
    let out = "4b825dc\nuv.lock\ntests/test_server.py\n\nAuto-merging uv.lock\n\
               CONFLICT (content): Merge conflict in uv.lock\n\
               CONFLICT (add/add): Merge conflict in version.py\n\
               CONFLICT (add/add): Merge conflict in version.py\n";
    assert_eq!(merge_tree_conflicts(out), vec!["uv.lock", "version.py"]);
    assert!(merge_tree_conflicts("4b825dc\n").is_empty());
}

#[test]
fn an_azure_pull_request_left_active_is_not_taken_as_merged() {
    let pr = |v: serde_json::Value| v;
    assert_eq!(
        azure_completion(&pr(serde_json::json!({ "status": "completed" }))),
        AzureCompletion::Done
    );
    // What Azure answers to `--status completed` on a conflicting pull
    // request: no error, still active.
    assert_eq!(
        azure_completion(&pr(serde_json::json!({
            "status": "active", "mergeStatus": "conflicts"
        }))),
        AzureCompletion::Conflicts
    );
    assert_eq!(
        azure_completion(&pr(serde_json::json!({
            "status": "active", "mergeStatus": "queued"
        }))),
        AzureCompletion::Waiting
    );
    assert_eq!(
        azure_completion(&pr(serde_json::json!({
            "status": "active", "mergeStatus": "rejectedByPolicy"
        }))),
        AzureCompletion::Refused
    );
    assert_eq!(
        azure_completion(&pr(serde_json::json!({
            "status": "active", "mergeStatus": "succeeded",
            "mergeFailureMessage": "Policy: required reviewers"
        }))),
        AzureCompletion::Refused
    );
}

#[test]
fn an_azure_merge_azure_declined_says_so_rather_than_merged() {
    let pr = serde_json::json!({
        "status": "active", "mergeStatus": "succeeded",
        "mergeFailureMessage": "Policy: required reviewers"
    });
    let failure = azure_not_completed("292", &pr);
    assert!(failure.message.contains("did not complete #292"));
    assert!(failure.message.contains("still active"));
    assert!(failure.message.contains("Policy: required reviewers"));
}

#[test]
fn an_azure_conflict_brings_the_base_in_without_gh() {
    let scope: Vec<String> = [
        "--organization",
        "https://dev.azure.com/o",
        "--detect",
        "false",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let failure = azure_conflict_guide(
        "292",
        "develop",
        "feat/x",
        &scope,
        "merge status: conflicts",
    );
    assert!(failure.message.contains("Azure DevOps cannot merge it"));
    assert!(!failure.message.contains("GitHub"));
    let bring_in = &failure.remedies[0];
    assert_eq!(bring_in.program, "sh");
    let script = &bring_in.args[1];
    assert!(!script.contains("gh pr checkout"));
    assert!(script.contains(r#"git checkout "$3""#));
    assert!(script.contains("--ff-only"));
    assert_eq!(&bring_in.args[3..], ["292", "develop", "feat/x"]);
    let abandon = &failure.remedies[1];
    assert_eq!(abandon.program, "az");
    assert!(abandon.args.iter().any(|a| a == "abandoned"));
    assert!(abandon.args.iter().any(|a| a == "https://dev.azure.com/o"));
}

#[test]
fn the_wait_after_a_push_asks_azure_on_an_azure_remote() {
    assert!(WAIT_FOR_FORGE.contains("dev.azure.com"));
    assert!(WAIT_FOR_FORGE.contains("az repos pr show"));
    assert!(WAIT_FOR_FORGE.contains("gh pr view"));
}
