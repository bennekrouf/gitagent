use super::*;

#[test]
fn a_missing_cli_is_named_rather_than_reported_as_a_fault() {
    let e = std::io::Error::from(std::io::ErrorKind::NotFound);
    let msg = spawn_error("az", &e);
    assert!(
        msg.starts_with("the Azure CLI (`az`) is not installed"),
        "got {msg}"
    );
    assert!(!msg.contains("program not found"));
}

#[test]
fn other_spawn_failures_keep_the_os_reason() {
    let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    assert!(spawn_error("az", &e).starts_with("could not run `az`:"));
}

#[cfg(unix)]
#[tokio::test]
async fn a_program_that_does_not_exist_says_so_plainly() {
    let err = run(".", "gitagent-no-such-tool", &[]).await.unwrap_err();
    assert!(err.contains("is not installed"), "got {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_that_never_reads_stdin_does_not_deadlock_the_writer() {
    // Well past a 64 KB pipe buffer, handed to a command that reads none
    // of it and writes output of its own. Writing stdin inline, before
    // the readers start, wedges both sides here forever.
    let payload = "x".repeat(256 * 1024);
    let (ok, out) = run_streaming(
        "sh",
        &["-c".to_string(), "echo started; exit 0".to_string()],
        None,
        &payload,
        &mut |_| {},
    )
    .await;
    assert!(ok);
    assert!(out.contains("started"), "got {out:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_backgrounded_grandchild_holding_the_pipe_does_not_hang_the_run() {
    // The daemon inherits stdout, so the pipe stays open after the shell
    // exits. Draining until the pipes close would never return.
    let (ok, _) = run_streaming(
        "sh",
        &[
            "-c".to_string(),
            "sleep 120 & echo done; exit 0".to_string(),
        ],
        None,
        "",
        &mut |_| {},
    )
    .await;
    assert!(ok, "the command itself succeeded");
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_reaches_the_child_and_the_exit_code_is_reported() {
    let (ok, out) = run_streaming(
        "sh",
        &[
            "-c".to_string(),
            "read answer; echo got:$answer".to_string(),
        ],
        None,
        "yes",
        &mut |_| {},
    )
    .await;
    assert!(ok);
    assert!(out.contains("got:yes"), "got {out:?}");

    let (ok, _) = run_streaming(
        "sh",
        &["-c".to_string(), "exit 3".to_string()],
        None,
        "",
        &mut |_| {},
    )
    .await;
    assert!(!ok, "a non-zero exit is a failure");
}

#[cfg(unix)]
#[tokio::test]
async fn both_streams_reach_the_callback_in_arrival_order() {
    let mut seen = vec![];
    let (ok, _) = run_streaming(
        "sh",
        &[
            "-c".to_string(),
            "echo one; echo two >&2; echo three".to_string(),
        ],
        None,
        "",
        &mut |line| seen.push(line.to_string()),
    )
    .await;
    assert!(ok);
    assert_eq!(
        seen.len(),
        3,
        "stderr is interleaved, not appended: {seen:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_real_repository_reports_awkward_filenames_git_add_can_use() {
    // The blocker end to end, against real git rather than a fixture:
    // text porcelain hands back `"has space.rs"` with the quotes, and
    // `git add --` on that fails with "did not match any files".
    let dir = std::env::temp_dir().join(format!("gitagent-status-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let repo = dir.to_str().unwrap();

    assert!(run(repo, "git", &["init", "-q", "."]).await.is_ok());
    for name in ["has space.rs", "na\u{ef}ve.rs", "plain.rs"] {
        std::fs::write(dir.join(name), "fn main() {}\n").unwrap();
    }

    let changes = status(repo).await.unwrap();
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    assert!(paths.contains(&"has space.rs"), "got {paths:?}");
    assert!(paths.contains(&"na\u{ef}ve.rs"), "got {paths:?}");
    assert!(
        !paths.iter().any(|p| p.contains('"')),
        "a quoted path reached the caller: {paths:?}"
    );

    // The whole point: these paths go straight to `git add`.
    let wanted: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    assert_eq!(needs_staging(&changes, &wanted).len(), 3);
    add(repo, &wanted).await.expect("git add rejected a path");
    assert_eq!(staged_paths(&status(repo).await.unwrap()).len(), 3);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn left_right_counts_read_as_ahead_then_behind() {
    assert_eq!(parse_left_right("2\t5\n"), Some((2, 5)));
    assert_eq!(parse_left_right(""), None);
}

#[test]
fn a_path_with_a_space_survives_parsing_intact() {
    // The text form quotes this as `"has space.rs"`, quotes included, and
    // the quoted string then fails `git add` and stops matching the real
    // path in `needs_staging`. Verified against real git output.
    let out = "?? has space.rs\0 M src/lib.rs\0";
    let changes = parse_status_z(out);
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].path, "has space.rs");
    assert_eq!(changes[0].code, "??");
    assert_eq!(changes[1].path, "src/lib.rs");
}

#[test]
fn a_non_ascii_path_is_not_escaped() {
    // Text porcelain renders this as `"na\303\257ve.rs"`.
    let changes = parse_status_z(" M na\u{ef}ve.rs\0");
    assert_eq!(changes[0].path, "na\u{ef}ve.rs");
}

#[test]
fn a_rename_reports_the_new_path_and_drops_the_old_one() {
    // `-z` spends two records on a rename: new path, then origin path.
    // Verified against real git output — the order is not the arrow's.
    let changes = parse_status_z("R  new name.txt\0old name.txt\0?? after.rs\0");
    assert_eq!(changes.len(), 2, "the origin record is not its own change");
    assert_eq!(changes[0].path, "new name.txt");
    assert_eq!(changes[0].note(), "renamed");
    assert_eq!(changes[1].path, "after.rs");
}

#[test]
fn a_copy_drops_its_origin_record_the_same_way() {
    let changes = parse_status_z("C  copy.rs\0source.rs\0");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "copy.rs");
}

#[test]
fn a_path_holding_the_rename_arrow_is_not_split_on_it() {
    let changes = parse_status_z(" M weird -> name.rs\0");
    assert_eq!(changes[0].path, "weird -> name.rs");
}

#[test]
fn empty_and_truncated_records_are_dropped_rather_than_panicking() {
    assert!(parse_status_z("").is_empty());
    assert!(parse_status_z("\0\0").is_empty());
    assert!(parse_status_z("??\0").is_empty(), "no path");
    assert!(parse_status_z("?? \0").is_empty(), "empty path");
}

#[test]
fn porcelain_codes_separate_untracked_from_modified() {
    let modified = FileChange {
        code: " M".into(),
        path: "src/lib.rs".into(),
    };
    let untracked = FileChange {
        code: "??".into(),
        path: "notes.txt".into(),
    };
    assert!(!modified.is_untracked());
    assert!(untracked.is_untracked());
}

#[test]
fn the_fingerprint_moves_with_a_commit_but_not_with_a_status() {
    let dir = std::env::temp_dir().join(format!("gitagent-fp-{}", std::process::id()));
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
            .unwrap();
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a"), "1").unwrap();
    git(&["add", "a"]);
    git(&["commit", "-qm", "one"]);
    let repo = dir.to_string_lossy().to_string();
    let before = change_fingerprint(&repo);
    std::fs::write(dir.join("a"), "2").unwrap();
    git(&["status", "--porcelain"]);
    assert_eq!(change_fingerprint(&repo), before, "looking changes nothing");
    // Timestamps can be coarse: make the commit land on a later tick.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    git(&["commit", "-qam", "two"]);
    assert_ne!(change_fingerprint(&repo), before, "a commit is noticed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn edits_inside_a_nested_repository_are_not_changes() {
    let dir = std::env::temp_dir().join(format!("gitagent-sub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inner")).unwrap();
    let git = |at: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(at)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
    };
    let inner = dir.join("inner");
    git(&inner, &["init", "-q", "-b", "main"]);
    std::fs::write(inner.join("a"), "1").unwrap();
    git(&inner, &["add", "a"]);
    git(&inner, &["commit", "-qm", "one"]);
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "inner"]);
    git(&dir, &["commit", "-qm", "with a nested repository"]);
    std::fs::write(inner.join("a"), "2").unwrap();
    std::fs::write(inner.join("b"), "new").unwrap();
    let repo = dir.to_string_lossy().to_string();
    assert!(
        status(&repo).await.unwrap().is_empty(),
        "work inside it is its own"
    );
    git(&inner, &["commit", "-qam", "two"]);
    let changes = status(&repo).await.unwrap();
    assert_eq!(changes.len(), 1, "a new commit in it can be committed here");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capping_leaves_a_short_diff_alone() {
    assert_eq!(cap("small"), "small");
}

fn file(path: &str, lines: usize) -> String {
    format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{}",
        "+line\n".repeat(lines)
    )
}

#[test]
fn a_diff_is_cut_whole_files_at_a_time_noise_first() {
    let diff = [
        file("src/a.rs", 50),
        file("Cargo.lock", 400),
        file("src/big.rs", 300),
        file("src/b.rs", 50),
    ]
    .concat();
    let fitted = fit(&diff, 1500);
    assert!(fitted.text.len() <= 1500, "{}", fitted.text.len());
    assert_eq!(fitted.left_out, vec!["Cargo.lock", "src/big.rs"]);
    assert!(fitted
        .text
        .starts_with("[Left out to fit: Cargo.lock, src/big.rs]"));
    let a = fitted.text.find("src/a.rs").unwrap();
    let b = fitted.text.find("src/b.rs").unwrap();
    assert!(a < b, "what is kept stays in order");
    assert!(fitted.note().contains("src/big.rs"));
}

#[test]
fn docs_and_ci_go_before_any_code() {
    let diff = [
        file("src/flow.rs", 300),
        file("CHANGELOG.md", 200),
        file(".github/workflows/release.yml", 100),
        file("src/small.rs", 20),
    ]
    .concat();
    let fitted = fit(&diff, 2500);
    assert_eq!(
        fitted.left_out,
        vec!["CHANGELOG.md", ".github/workflows/release.yml"]
    );
    assert!(
        fitted.text.contains("b/src/flow.rs"),
        "the logic is what gets reviewed"
    );
}

#[test]
fn a_diff_that_fits_is_left_alone() {
    let diff = file("src/a.rs", 5);
    let fitted = fit(&diff, 10_000);
    assert_eq!(fitted.text, diff);
    assert!(fitted.left_out.is_empty() && fitted.note().is_empty());
}

#[test]
fn one_file_too_big_on_its_own_is_cut_not_dropped() {
    let diff = [file("src/huge.rs", 1000), file("Cargo.lock", 10)].concat();
    let fitted = fit(&diff, 1000);
    assert!(fitted.text.len() <= 1000, "{}", fitted.text.len());
    assert!(fitted.text.contains("diff --git a/src/huge.rs"));
    assert!(fitted.text.contains("truncated"));
    assert_eq!(fitted.left_out, vec!["Cargo.lock"]);
}

#[test]
fn capping_a_long_diff_says_so() {
    let long = "x".repeat(DIFF_CAP + 500);
    let out = cap(&long);
    assert!(out.len() < long.len());
    assert!(out.contains("truncated"));
}

/// No built-in step may ever discard local work. `git checkout` and
/// `git pull --ff-only` already refuse rather than overwrite, and that is
/// the only shape a step is allowed to take — this pins it down so a
/// future step cannot quietly reach for a hard reset, a forced clean, a
/// forced checkout, a forced push, or force-deleting a branch instead.
///
/// This only covers steps gitagent itself runs. A "Run a script" or
/// "Run on a server" step executes whatever command the user configured,
/// which can be anything — that is its whole point, and no scan of this
/// crate can constrain it.
///
/// The banned flags are assembled at runtime, not written out literally —
/// otherwise this test's own source would flag itself when it scans
/// `git.rs`. Every file the three modules are split into is scanned.
#[test]
fn no_builtin_step_ever_runs_a_destructive_git_command() {
    let dash = "-";
    let banned: Vec<String> = vec![
        format!("reset {dash}{dash}hard"),
        format!("clean {dash}f"),
        format!("clean {dash}x"),
        format!("checkout {dash}f"),
        format!("checkout {dash}{dash}force"),
        format!("push {dash}{dash}force"),
        format!("push {dash}f"),
        format!("branch {dash}D"),
        format!("branch {dash}{dash}delete {dash}{dash}force"),
    ];
    for (path, source) in [
        ("src/services/git.rs", include_str!("../git.rs")),
        ("src/services/git/remote.rs", include_str!("remote.rs")),
        ("src/services/git/status.rs", include_str!("status.rs")),
        (
            "src/services/git/streaming.rs",
            include_str!("streaming.rs"),
        ),
        ("src/services/flow.rs", include_str!("../flow.rs")),
        (
            "src/services/flow/checks.rs",
            include_str!("../flow/checks.rs"),
        ),
        (
            "src/services/flow/commit.rs",
            include_str!("../flow/commit.rs"),
        ),
        (
            "src/services/flow/publish.rs",
            include_str!("../flow/publish.rs"),
        ),
        ("src/services/review.rs", include_str!("../review.rs")),
        ("src/services/review/pr.rs", include_str!("../review/pr.rs")),
        (
            "src/services/review/sync.rs",
            include_str!("../review/sync.rs"),
        ),
    ] {
        for flag in &banned {
            assert!(
                !source.contains(flag.as_str()),
                "{path} contains `{flag}` — a built-in step must never discard local work"
            );
        }
    }
}

fn change(code: &str, path: &str) -> FileChange {
    FileChange {
        code: code.into(),
        path: path.into(),
    }
}

#[test]
fn a_conflict_is_recognised_in_every_shape_porcelain_reports_it() {
    let changes = vec![
        change("UU", "both-modified.rs"),
        change("AA", "both-added.rs"),
        change("DD", "both-deleted.rs"),
        change("AU", "added-by-us.rs"),
        change("UD", "deleted-by-them.rs"),
        change(" M", "just-modified.rs"),
        change("??", "new.rs"),
    ];
    let stuck = conflicted(&changes);
    assert_eq!(stuck.len(), 5);
    assert!(!stuck.contains(&"just-modified.rs".to_string()));
    assert!(!stuck.contains(&"new.rs".to_string()));
}

#[test]
fn a_rebase_is_finished_with_continue_and_a_merge_with_a_commit() {
    // `git merge --continue` exists but a plain merge is finished by
    // committing; getting this wrong is exactly the confusion to avoid.
    assert_eq!(
        InProgress::Rebase.finish_args(),
        vec!["rebase", "--continue"]
    );
    assert_eq!(InProgress::Merge.finish_args(), vec!["commit", "--no-edit"]);
    assert_eq!(
        InProgress::CherryPick.finish_args(),
        vec!["cherry-pick", "--continue"]
    );
}

#[test]
fn every_interrupted_operation_can_be_backed_out() {
    for state in [
        InProgress::Rebase,
        InProgress::Merge,
        InProgress::CherryPick,
        InProgress::Revert,
    ] {
        assert_eq!(state.abort_args()[1], "--abort", "{state:?}");
    }
}

#[test]
fn an_already_staged_deletion_is_not_added_again() {
    // ais_tom_platform: `D ` plus a .gitignore rule made `git add` fail.
    let changes = vec![change("D ", "logic_apps/local.settings.json")];
    let wanted = vec!["logic_apps/local.settings.json".to_string()];
    assert!(needs_staging(&changes, &wanted).is_empty());
}

#[test]
fn a_long_argument_is_elided_rather_than_echoed_whole() {
    // A pull request body is a legitimate argument; printing it verbatim
    // buries the line that says what went wrong.
    let body = "x".repeat(800);
    let args = vec!["repos", "pr", "create", "--description", body.as_str()];
    let short = short_command(&args);
    assert!(short.len() < 120, "got {} chars", short.len());
    assert!(short.contains("(800 chars)"));
    assert!(short.starts_with("repos pr create --description"));
}

#[test]
fn short_arguments_survive_untouched() {
    let args = vec!["push", "-u", "origin", "main"];
    assert_eq!(short_command(&args), "push -u origin main");
}

#[test]
fn eliding_counts_characters_not_bytes() {
    let arg = "é".repeat(100);
    let args = vec![arg.as_str()];
    assert!(short_command(&args).contains("(100 chars)"));
}

#[test]
fn untracked_files_are_not_in_the_index() {
    let changes = vec![change("??", "new.rs"), change("A ", "added.rs")];
    assert_eq!(staged_paths(&changes), vec!["added.rs".to_string()]);
}

#[test]
fn a_staged_file_nobody_approved_is_singled_out() {
    // Unchecking a file that was already staged has to actually exclude it.
    let changes = vec![change("M ", "wanted.rs"), change("M ", "sneaky.rs")];
    let approved = vec!["wanted.rs".to_string()];
    assert_eq!(
        staged_but_unapproved(&changes, &approved),
        vec!["sneaky.rs".to_string()]
    );
}

#[test]
fn nothing_is_unstaged_when_the_index_already_matches() {
    let changes = vec![change("M ", "a.rs"), change(" M", "b.rs")];
    let approved = vec!["a.rs".to_string(), "b.rs".to_string()];
    assert!(staged_but_unapproved(&changes, &approved).is_empty());
}

#[test]
fn a_staged_deletion_counts_as_staged() {
    let changes = vec![change("D ", "logic_apps/local.settings.json")];
    assert_eq!(staged_paths(&changes).len(), 1);
}

#[test]
fn an_unstaged_deletion_still_needs_staging() {
    let changes = vec![change(" D", "gone.rs")];
    let wanted = vec!["gone.rs".to_string()];
    assert_eq!(needs_staging(&changes, &wanted), wanted);
}

#[test]
fn untracked_and_modified_files_need_staging() {
    let changes = vec![change("??", "new.rs"), change(" M", "edited.rs")];
    let wanted = vec!["new.rs".to_string(), "edited.rs".to_string()];
    assert_eq!(needs_staging(&changes, &wanted), wanted);
}

#[test]
fn a_fully_staged_change_needs_nothing_further() {
    let changes = vec![change("M ", "done.rs"), change("A ", "added.rs")];
    let wanted = vec!["done.rs".to_string(), "added.rs".to_string()];
    assert!(needs_staging(&changes, &wanted).is_empty());
}

#[test]
fn a_partly_staged_file_is_staged_the_rest_of_the_way() {
    let changes = vec![change("MM", "half.rs")];
    let wanted = vec!["half.rs".to_string()];
    assert_eq!(needs_staging(&changes, &wanted), wanted);
}

#[test]
fn a_path_git_did_not_report_is_attempted_anyway() {
    assert_eq!(
        needs_staging(&[], &["mystery.rs".to_string()]),
        vec!["mystery.rs".to_string()]
    );
}

#[test]
fn porcelain_codes_read_as_words_in_the_approval_list() {
    let cases = [
        ("??", "new"),
        (" M", "modified"),
        ("A ", "added"),
        (" D", "deleted"),
        ("R ", "renamed"),
    ];
    for (code, expected) in cases {
        let change = FileChange {
            code: code.into(),
            path: "f".into(),
        };
        assert_eq!(change.note(), expected, "code {code:?}");
    }
}

#[test]
fn the_default_branches_are_all_protected_from_direct_commits() {
    for name in ["master", "main", "trunk", "develop"] {
        assert!(is_protected(name));
    }
}

#[test]
fn protection_ignores_case_and_stray_whitespace() {
    assert!(is_protected("  Master "));
    assert!(is_protected("MAIN"));
}

#[test]
fn a_topic_branch_is_not_protected() {
    assert!(!is_protected("fix/stale-lockfile"));
    assert!(!is_protected("mainline-refactor"));
}

#[test]
fn capping_never_splits_a_multibyte_character() {
    // é is two bytes, so the cap lands mid-character for some inputs.
    let long = "é".repeat(DIFF_CAP);
    let out = cap(&long);
    assert!(out.contains("truncated"));
}

#[test]
fn the_worktree_holding_a_branch_is_found_but_never_this_one() {
    let list = "worktree /code/mayorana\nHEAD 94318ce\nbranch refs/heads/master\n\n\
                    worktree /code/mayorana-funnel\nHEAD 68dc94b\nbranch refs/heads/feat/per-app-funnel\n";
    assert_eq!(
        holder_in(list, "master", "/code/mayorana-funnel").as_deref(),
        Some("/code/mayorana")
    );
    assert_eq!(holder_in(list, "master", "/code/mayorana"), None);
    assert_eq!(holder_in(list, "main", "/code/mayorana-funnel"), None);
    // `master` must not match a branch that merely starts with it.
    let list = "worktree /code/a\nHEAD 1\nbranch refs/heads/master-old\n";
    assert_eq!(holder_in(list, "master", "/code/b"), None);
}

#[tokio::test]
async fn access_is_checked_against_origin_without_prompting() {
    let dir = std::env::temp_dir().join(format!("gitagent-access-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let repo = dir.to_string_lossy().to_string();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
    };
    git(&["init", "-q"]);
    // No remote: nothing to be refused by.
    assert_eq!(check_access(&repo).await, Ok(()));

    // A remote that is not there fails, and says why, rather than waiting.
    let missing = dir.join("no-such-remote");
    git(&["remote", "add", "origin", &missing.to_string_lossy()]);
    let err = check_access(&repo).await.unwrap_err();
    assert!(!err.trim().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
