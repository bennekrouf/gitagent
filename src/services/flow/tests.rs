use super::*;
use crate::services::graph::NodeStatus;
mod commit;

const NO_NOTES_OUTPUT: &str =
    "  ❌ Release notes: nothing under [Unreleased] in CHANGELOG.md\n     \
    Add the entry (## [Unreleased] + ### Added/Changed/Fixed/Removed),\n     \
    or pass --no-notes for a build-only release.";

#[test]
fn a_first_commit_stays_on_its_branch_and_opens_no_pull_request() {
    let mut s = RunState::default();
    s.artifacts.insert("branch".into(), "main".into());
    s.artifacts.insert("base".into(), "master".into());
    s.artifacts.insert("work_branch".into(), "main".into());
    assert!(
        must_branch(&s),
        "an ordinary commit on main branches off it"
    );

    s.artifacts.insert("first_commit".into(), "true".into());
    assert!(!must_branch(&s), "nothing exists yet to branch from");
    assert!(proposal(&spec(Step::Commit), &s).contains("first commit"));
    assert!(
        nothing_to_approve(&spec(Step::OpenPr), &s),
        "no approval for a pull request that cannot exist"
    );
    assert!(first_commit_note(&s).contains("main"));
}

#[test]
fn writing_notes_asks_only_when_there_is_a_draft_to_read() {
    let mut s = RunState::default();
    let write = spec(Step::WriteNotes);
    assert!(nothing_to_approve(&write, &s), "notes already written");
    s.artifacts
        .insert("release_notes".into(), "### Fixed\n\n- A fix.".into());
    assert!(!nothing_to_approve(&write, &s));
    assert!(proposal(&write, &s).contains("- A fix."));
    assert!(!nothing_to_approve(
        &spec(Step::Commit),
        &RunState::default()
    ));
}

#[test]
fn a_release_missing_its_notes_offers_a_build_only_release() {
    let failure = script_failure(
        "./scripts/release.sh --patch",
        NO_NOTES_OUTPUT.into(),
        &ReleaseFindings::default(),
    );
    assert!(failure.message.contains("Nothing was bumped"));
    assert_eq!(failure.remedies.len(), 1);
    let fix = &failure.remedies[0];
    assert_eq!(fix.args, ["-c", "./scripts/release.sh --patch --no-notes"]);
    assert!(
        !fix.retry_after,
        "the fix is the release; running it again would bump twice"
    );
    assert!(!fix.abandons, "releasing is not giving up");
}

#[test]
fn any_other_script_failure_offers_nothing() {
    assert!(script_failure(
        "./scripts/release.sh",
        "❌ Tag v1 already exists".into(),
        &ReleaseFindings::default()
    )
    .remedies
    .is_empty());
    assert!(script_failure(
        "make deploy",
        NO_NOTES_OUTPUT.into(),
        &ReleaseFindings::default()
    )
    .remedies
    .is_empty());
    assert!(
        script_failure(
            "./scripts/release.sh --no-notes",
            NO_NOTES_OUTPUT.into(),
            &ReleaseFindings::default()
        )
        .remedies
        .is_empty(),
        "already build-only: offering it again would loop"
    );
}

fn behind(n: usize) -> ReleaseFindings {
    ReleaseFindings {
        upstream: git::Upstream::Behind {
            tracking: "origin/master".into(),
            behind: n,
            ahead: 0,
        },
        unpushed_tag: None,
    }
}

const REJECTED_OUTPUT: &str = "[master 8b87904] chore: release v0.3.41
To github.com:o/r.git
 ! [rejected]        HEAD -> master (non-fast-forward)
error: failed to push some refs to 'github.com:o/r.git'
hint: Updates were rejected because the tip of your current branch is behind";

#[test]
fn missing_notes_on_a_stale_copy_offer_a_pull_not_a_build_only_release() {
    // The incident: a pull request merged on GitHub carried the notes, the
    // local copy had not pulled it, and the only button shipped without them.
    let failure = script_failure("./scripts/release.sh", NO_NOTES_OUTPUT.into(), &behind(1));
    assert!(failure.message.contains("origin/master has 1 commit"));
    assert_eq!(failure.remedies.len(), 1);
    let fix = &failure.remedies[0];
    assert_eq!(fix.args, ["pull", "--rebase"]);
    assert!(fix.retry_after, "the release runs again once pulled");
    assert!(
        !failure
            .remedies
            .iter()
            .any(|r| r.display.contains("--no-notes")),
        "build-only would ship the pulled change with no notes"
    );
}

#[test]
fn a_release_whose_push_was_rejected_offers_to_undo_it_and_go_again() {
    let found = ReleaseFindings {
        unpushed_tag: Some("v0.3.41".into()),
        ..behind(1)
    };
    let failure = script_failure("./scripts/release.sh", REJECTED_OUTPUT.into(), &found);
    assert!(failure
        .message
        .contains("v0.3.41 exists only on this machine"));
    assert_eq!(failure.remedies.len(), 1);
    let fix = &failure.remedies[0];
    assert_eq!(
        fix.args,
        [
            "-c",
            "git tag -d v0.3.41 && git reset --keep HEAD~1 && git pull --rebase"
        ]
    );
    assert!(fix.retry_after);
}

#[test]
fn a_rejected_release_push_with_nothing_tagged_offers_the_plain_pull() {
    let failure = script_failure("./scripts/release.sh", REJECTED_OUTPUT.into(), &behind(1));
    assert_eq!(failure.remedies.len(), 1);
    assert_eq!(failure.remedies[0].args, ["pull", "--rebase"]);
}

#[cfg(unix)]
#[tokio::test]
async fn only_gitagents_own_notes_commit_is_put_back_on_top_by_itself() {
    let root = std::env::temp_dir().join(format!("gitagent-notes-rebase-{}", std::process::id()));
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
    let notes = super::super::notes::COMMIT_MESSAGE;
    sh("git init -q --bare -b main origin.git");
    sh(&format!(
        "git clone -q origin.git here && cd here && {ident} && echo a > CHANGELOG.md && \
         git add . && git commit -q -m init && git push -q -u origin main"
    ));
    sh(&format!(
        "git clone -q origin.git forge && cd forge && {ident}"
    ));
    let here = root.join("here");
    let repo = here.to_str().unwrap();
    let subjects = || {
        let out = std::process::Command::new("git")
            .args(["log", "--format=%s", "-3"])
            .current_dir(&here)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    // The notes commit here; a pull request merged on the forge meanwhile.
    sh(&format!(
        "cd here && echo notes >> CHANGELOG.md && git commit -q -am '{notes}'"
    ));
    sh("cd forge && git pull -q && echo x > other.rs && git add . && git commit -q -m 'fix: x (#9)' && git push -q");
    let note = catch_up(repo).await.expect("ours, and it applies cleanly");
    assert!(note.unwrap().contains("put back on top"));
    assert_eq!(
        subjects()[..2],
        [notes.to_string(), "fix: x (#9)".to_string()]
    );

    // Both edited the changelog: undone, nothing changed, the button offered.
    sh("cd here && git push -q");
    sh(&format!(
        "cd here && echo mine >> CHANGELOG.md && git commit -q -am '{notes}'"
    ));
    sh("cd forge && git pull -q && echo theirs >> CHANGELOG.md && git commit -q -am 'docs: theirs (#10)' && git push -q");
    let before = subjects();
    let failure = catch_up(repo).await.expect_err("the replay clashes");
    assert!(failure.message.contains("clashed"), "{}", failure.message);
    assert_eq!(subjects(), before, "nothing changed");
    assert!(
        !here.join(".git/rebase-merge").exists(),
        "no rebase left half-done"
    );
    assert_eq!(failure.remedies[0].args, ["pull", "--rebase"]);

    // A commit of the person's own is never moved without them.
    sh("cd here && git reset -q --hard origin/main");
    sh("cd here && echo y > mine.rs && git add . && git commit -q -m 'feat: mine'");
    sh("cd forge && git pull -q && echo z > z.rs && git add . && git commit -q -m 'fix: z (#11)' && git push -q");
    let failure = catch_up(repo).await.expect_err("not ours to replay");
    assert!(!failure.message.contains("put back"));
    assert_eq!(subjects()[0], "feat: mine");

    // The notes already went in with a later pull request: the local copy
    // is dropped, not replayed into a clash with itself.
    sh("cd here && git reset -q --hard origin/main");
    sh(&format!(
        "cd here && printf '\\n- Carried along.\\n' >> CHANGELOG.md && git commit -q -am '{notes}'"
    ));
    sh("cd forge && git pull -q && printf '\\n- Carried along.\\n' >> CHANGELOG.md && echo w > w.rs && git add . && git commit -q -m 'feat: w (#12)' && git push -q");
    let note = catch_up(repo)
        .await
        .expect("already there, nothing to bring");
    assert!(
        note.unwrap().contains("already on"),
        "dropped, not replayed"
    );
    assert_eq!(subjects()[0], "feat: w (#12)");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_release_catches_up_with_a_merge_made_on_the_forge() {
    // Real git end to end: a bare origin, this copy, and a second clone
    // standing in for the forge merging a pull request.
    let root = std::env::temp_dir().join(format!("gitagent-catchup-{}", std::process::id()));
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
    sh("git init -q --bare -b master origin.git");
    sh(&format!(
        "git clone -q origin.git here && cd here && {ident} && \
         git commit -q --allow-empty -m init && git push -q -u origin master"
    ));
    sh(&format!(
        "git clone -q origin.git forge && cd forge && {ident}"
    ));
    let merge_on_forge = |msg: &str| {
        sh(&format!(
            "cd forge && git pull -q && git commit -q --allow-empty -m '{msg}' && git push -q"
        ))
    };
    let here = root.join("here");
    let repo = here.to_str().unwrap();
    let head = || {
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD", "origin/master"])
            .current_dir(&here)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap()
    };

    assert_eq!(git::upstream(repo).await, git::Upstream::UpToDate);

    // Merged on the forge, never pulled here: the release must pull it.
    merge_on_forge("feat: a change (#1)");
    assert!(matches!(
        git::upstream(repo).await,
        git::Upstream::Behind {
            behind: 1,
            ahead: 0,
            ..
        }
    ));
    let note = catch_up(repo).await.expect("a fast-forward needs no fix");
    assert!(note.unwrap().contains("Pulled 1 commit"));
    let refs = head();
    let mut lines = refs.lines();
    assert_eq!(lines.next(), lines.next(), "HEAD is now origin/master");

    // A release committed and tagged here while the forge moved on: the
    // state a rejected `release.sh` push leaves behind.
    sh("cd here && git commit -q --allow-empty -m 'chore: release v0.1.1' && git tag v0.1.1");
    merge_on_forge("feat: another (#2)");
    assert_eq!(
        git::unpushed_release_tag(repo).await.as_deref(),
        Some("v0.1.1")
    );
    let failure = catch_up(repo)
        .await
        .expect_err("diverged with a stray release");
    let fix = &failure.remedies[0];
    assert!(fix.label.contains("v0.1.1"));

    // And the fix leaves this copy exactly where origin is, tag gone.
    sh(&format!("cd here && {}", fix.args[1]));
    let refs = head();
    let mut lines = refs.lines();
    assert_eq!(lines.next(), lines.next());
    assert_eq!(git::unpushed_release_tag(repo).await, None);
    assert_eq!(git::upstream(repo).await, git::Upstream::UpToDate);

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn diff_preview_is_none_for_anything_but_scan_changes_and_pr_diff() {
    // Short-circuits before touching git, so an unused repo path is fine.
    let node = NodeSpec {
        id: "commit".into(),
        title: String::new(),
        subtitle: String::new(),
        step: Step::Commit,
        kind: crate::services::graph::NodeKind::Deterministic,
        deps: vec![],
        reads: vec![],
        writes: vec![],
        requires_approval: true,
        config: Default::default(),
        bind: Default::default(),
    };
    let state = RunState::fresh(&commit_and_pr_flow());
    assert_eq!(diff_preview(&node, "/does/not/matter", &state).await, None);
}

#[tokio::test]
async fn a_clean_base_branch_has_nothing_already_committed_to_find() {
    // Short-circuits before touching git — no `base` artifact means
    // there is nothing to compare the branch against.
    let state = RunState::fresh(&commit_and_pr_flow());
    let out = already_committed("/does/not/matter", "main", &state)
        .await
        .unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn sitting_on_base_itself_has_nothing_already_committed_to_find() {
    // A checkout of `base` compared against itself is always empty —
    // short-circuits without needing to touch git.
    let mut state = RunState::fresh(&commit_and_pr_flow());
    state.artifacts.insert("base".into(), "main".into());
    let out = already_committed("/does/not/matter", "main", &state)
        .await
        .unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn pr_diff_preview_is_none_without_a_base_and_head_to_compare() {
    let node = NodeSpec {
        id: "pr_diff".into(),
        title: String::new(),
        subtitle: String::new(),
        step: Step::PrDiff,
        kind: crate::services::graph::NodeKind::Deterministic,
        deps: vec![],
        reads: vec![],
        writes: vec![],
        requires_approval: true,
        config: Default::default(),
        bind: Default::default(),
    };
    let state = RunState::fresh(&commit_and_pr_flow());
    assert_eq!(diff_preview(&node, "/does/not/matter", &state).await, None);
}

#[test]
fn every_dependency_names_a_node_that_exists() {
    let g = commit_and_pr_flow();
    for n in &g.nodes {
        for d in &n.deps {
            assert!(g.get(d).is_some(), "{} depends on unknown node {d}", n.id);
        }
    }
}

#[test]
fn every_artifact_a_node_reads_is_written_upstream() {
    let g = commit_and_pr_flow();
    for n in &g.nodes {
        for key in &n.reads {
            let produced = g
                .nodes
                .iter()
                .any(|other| other.id != n.id && other.writes.contains(key));
            assert!(produced, "{} reads {key}, which nothing writes", n.id);
        }
    }
}

#[test]
fn drafting_the_pr_does_not_wait_on_the_push() {
    // The diamond: if this edge ever appears, the flow has silently become
    // a chain and the PR text cannot be prepared while the push runs. It
    // does wait for the commit, so a stopped commit costs no model call.
    let g = commit_and_pr_flow();
    let draft_pr = g.get("draft_pr").unwrap();
    assert!(!draft_pr.deps.contains(&"push".to_string()));
    assert!(draft_pr.deps.contains(&"commit".to_string()));
}

#[test]
fn every_node_that_touches_git_history_or_the_remote_asks_first() {
    let g = commit_and_pr_flow();
    for id in ["commit", "push", "open_pr"] {
        assert!(g.get(id).unwrap().requires_approval, "{id} must be gated");
    }
    for id in ["preflight", "draft_commit", "draft_pr"] {
        assert!(!g.get(id).unwrap().requires_approval, "{id} is read-only");
    }
}

#[test]
fn scan_is_gated_so_the_diff_it_found_is_reviewed_before_a_commit_drafts_from_it() {
    // scan_changes touches no git history or remote, but everything
    // downstream works from the diff it produces — worth a look before
    // the flow keeps going, not just before the commit itself.
    let g = commit_and_pr_flow();
    assert!(g.get("scan").unwrap().requires_approval);
}

#[test]
fn credentials_are_checked_before_anything_is_written() {
    // The bug this guards: gh auth failing at open_pr, after the commit and
    // the push had already happened.
    let g = commit_and_pr_flow();
    assert!(
        g.get("preflight").unwrap().deps.is_empty(),
        "preflight is the root"
    );
    for id in ["scan", "commit", "push", "open_pr"] {
        let mut seen = vec![id.to_string()];
        let mut i = 0;
        while i < seen.len() {
            let node = g.get(&seen[i]).unwrap().clone();
            for d in node.deps {
                if !seen.contains(&d) {
                    seen.push(d);
                }
            }
            i += 1;
        }
        assert!(
            seen.contains(&"preflight".to_string()),
            "{id} must sit downstream of preflight"
        );
    }
}

#[test]
fn a_protected_branch_forces_a_topic_branch_even_when_base_disagrees() {
    // ais-analytics: a stale local `main`, a working branch of `master`.
    // Comparing branch to base alone said "already on a topic branch" and
    // committed straight to master.
    let mut s = RunState::default();
    s.artifacts.insert("branch".into(), "master".into());
    s.artifacts.insert("base".into(), "main".into());
    assert!(must_branch(&s));
}

#[test]
fn sitting_on_the_base_branch_forces_a_topic_branch() {
    let mut s = RunState::default();
    s.artifacts.insert("branch".into(), "master".into());
    s.artifacts.insert("base".into(), "master".into());
    assert!(must_branch(&s));
}

#[test]
fn a_real_topic_branch_is_committed_to_directly() {
    let mut s = RunState::default();
    s.artifacts
        .insert("branch".into(), "fix/stale-lockfile".into());
    s.artifacts.insert("base".into(), "master".into());
    assert!(!must_branch(&s));
}

#[test]
fn the_flow_reaches_the_pr_when_every_node_succeeds() {
    let g = commit_and_pr_flow();
    let mut s = RunState::fresh(&g);
    let mut order = vec![];
    while let Some(n) = s.next_ready(&g) {
        order.push(n.id.clone());
        s.set_status(&n.id, NodeStatus::Done);
    }
    assert!(s.is_finished(&g));
    assert_eq!(order.last().unwrap(), "open_pr");
    assert_eq!(order.first().unwrap(), "preflight");
}

#[test]
fn rejecting_the_commit_blocks_the_push_and_the_pr() {
    let g = commit_and_pr_flow();
    let mut s = RunState::fresh(&g);
    for id in ["preflight", "scan", "draft_commit", "draft_pr", "test"] {
        s.set_status(id, NodeStatus::Done);
    }
    s.set_status("commit", NodeStatus::Rejected);
    s.propagate_block(&g);
    assert_eq!(s.status("push"), NodeStatus::Blocked);
    assert_eq!(s.status("open_pr"), NodeStatus::Blocked);
    assert!(s.is_finished(&g));
}

#[test]
fn a_failing_test_suite_stops_the_flow_before_anything_is_committed() {
    // The reason the node sits where it does: catching a red test after
    // the push is worth much less than catching it before the commit.
    let g = commit_and_pr_flow();
    let mut s = RunState::fresh(&g);
    for id in ["preflight", "scan", "draft_commit", "draft_pr"] {
        s.set_status(id, NodeStatus::Done);
    }
    s.set_status("test", NodeStatus::Failed);
    s.propagate_block(&g);

    assert_eq!(s.status("commit"), NodeStatus::Blocked);
    assert_eq!(s.status("push"), NodeStatus::Blocked);
    assert_eq!(s.status("open_pr"), NodeStatus::Blocked);
}

#[test]
fn the_tests_run_alongside_the_model_not_after_it() {
    // Both hang off `scan`, so neither waits on the other — the point of
    // the graph being a graph.
    let g = commit_and_pr_flow();
    let test = g.get("test").expect("the shipped flow runs the tests");
    assert_eq!(test.deps, vec!["scan".to_string()]);
    assert_eq!(test.step, Step::RunTests);

    let commit = g.get("commit").unwrap();
    assert!(commit.deps.contains(&"test".to_string()));
    assert!(commit.deps.contains(&"draft_commit".to_string()));

    // With the model call still in flight, the suite is already runnable.
    let mut s = RunState::fresh(&g);
    for id in ["preflight", "scan"] {
        s.set_status(id, NodeStatus::Done);
    }
    s.set_status("draft_commit", NodeStatus::Running);
    assert_eq!(s.next_ready(&g).map(|n| n.id), Some("test".to_string()));
}

#[test]
fn running_the_tests_does_not_stop_to_be_approved() {
    // Nothing it does can be undone-by-approval: no history, no remote.
    let g = commit_and_pr_flow();
    assert!(!g.get("test").unwrap().requires_approval);
}

#[test]
fn every_step_in_the_catalogue_has_an_implementation() {
    // A step the executor cannot run has no business being offered in the
    // editor. This catches a catalogue entry added without wiring.
    for entry in crate::services::catalogue::CATALOGUE {
        let _ = entry.step;
    }
    // Not every catalogue entry maps to a distinct `Step` — a gated and
    // an ungated variant can share one execution — so this only pins
    // down that the catalogue has not shrunk, not an exact 1:1 count.
    assert!(crate::services::catalogue::CATALOGUE.len() >= 15);
}

/// A bare node carrying just a step, for the pure functions that only
/// look at one.
fn spec(step: Step) -> NodeSpec {
    NodeSpec {
        id: "n".into(),
        title: String::new(),
        subtitle: String::new(),
        step,
        kind: crate::services::graph::NodeKind::Deterministic,
        deps: vec![],
        reads: vec![],
        writes: vec![],
        requires_approval: false,
        config: Default::default(),
        bind: Default::default(),
    }
}

#[test]
fn a_rejected_push_offers_the_pull_git_itself_suggests() {
    let output = "\
 ! [rejected]        HEAD -> main (non-fast-forward)
error: failed to push some refs to 'github.com:bennekrouf/gitagent.git'
hint: Updates were rejected because the tip of your current branch is behind";
    let remedies = pull_remedy(output);
    assert_eq!(remedies.len(), 1);
    assert_eq!(remedies[0].program, "git");
    assert_eq!(remedies[0].args, vec!["pull", "--rebase"]);
    assert!(
        remedies[0].retry_after,
        "once the branch is caught up the step is worth running again"
    );
}

#[test]
fn renaming_a_leftover_branch_renames_it_for_the_rest_of_the_run() {
    let fix = rename_remedy("feat/release-notes", "feat/release-notes-2");
    assert_eq!(
        fix.args,
        ["branch", "-m", "feat/release-notes", "feat/release-notes-2"]
    );
    assert!(fix.retry_after, "the push is retried under the new name");
    assert_eq!(
        fix.sets,
        vec![(
            "work_branch".to_string(),
            "feat/release-notes-2".to_string()
        )],
        "push and open_pr read work_branch, so it has to change with git"
    );
}

#[test]
fn a_push_that_failed_for_another_reason_offers_nothing() {
    assert!(pull_remedy("Permission denied (publickey).").is_empty());
    assert!(pull_remedy("").is_empty());
}

fn commit_run(node_id: &str, items: &[(&str, bool)]) -> RunState {
    let mut s = RunState::default();
    s.artifacts
        .insert("commit_paths".into(), "keep.rs\ndrop.rs".into());
    s.runs.entry(node_id.into()).or_default().items = items
        .iter()
        .map(|(key, included)| ProposalItem {
            key: (*key).into(),
            label: (*key).into(),
            note: "modified".into(),
            included: *included,
        })
        .collect();
    s
}

#[cfg(unix)]
#[tokio::test]
async fn a_release_with_nothing_since_its_tag_is_already_released() {
    let root = std::env::temp_dir().join(format!(
        "gitagent-released-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let sh = |cmd: &str| {
        let out = std::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(out.status.success(), "{cmd}");
    };
    let repo = root.to_str().unwrap();
    sh(
        "git init -q -b main && git config user.name t && git config user.email t@t && \
        echo a > f && git add . && git commit -q -m 'chore: release v0.1.35' && git tag v0.1.35",
    );
    assert_eq!(released_already(repo).await.as_deref(), Some("v0.1.35"));

    sh("echo b > f && git commit -qam 'fix: something'");
    assert_eq!(released_already(repo).await, None, "one commit to ship");
    let _ = std::fs::remove_dir_all(&root);
}
