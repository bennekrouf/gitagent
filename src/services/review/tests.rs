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
fn the_update_remedy_targets_the_pull_requests_own_base() {
    let failure = merge_failure("1", "develop", "not mergeable".to_string());
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
    let next = conflict_followup("66", "master", out).unwrap();
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
    assert!(conflict_followup("66", "master", "fatal: not a git repository").is_none());
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
    let failure = merge_failure("8", "master", "gh: authentication required".to_string());
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
         git checkout -q main && echo main > f && echo main > g && git commit -qam main && git push -q origin main && \
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
