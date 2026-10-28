//! The commit step's tests: which files it stages, and the branch name and
//! message it makes.

use super::*;

#[test]
fn a_truncated_branch_name_does_not_end_on_a_separator() {
    // git refuses a refname ending in a dot, and trimming before the
    // 40-character cut let the cut put one back.
    let name = sanitise_branch(&format!("feat/{}.tail", "a".repeat(60)));
    assert!(name.chars().count() <= 40);
    assert!(
        !name.ends_with('.') && !name.ends_with('-') && !name.ends_with('/'),
        "got {name:?}"
    );
}

#[test]
fn refnames_git_rejects_outright_are_not_produced() {
    for raw in [
        "fix/../../etc/passwd",
        "feat//double",
        "chore/thing.lock",
        "---",
        "",
    ] {
        let name = sanitise_branch(raw);
        assert!(!name.is_empty(), "{raw:?}");
        assert!(!name.contains(".."), "{raw:?} -> {name:?}");
        assert!(!name.contains("//"), "{raw:?} -> {name:?}");
        assert!(!name.ends_with(".lock"), "{raw:?} -> {name:?}");
        assert!(!name.starts_with('-'), "{raw:?} -> {name:?}");
    }
}

#[test]
fn a_file_deselected_at_the_scan_stays_deselected_at_the_commit() {
    let mut s = RunState::default();
    s.artifacts
        .insert("commit_paths".into(), "keep.rs\ndrop.rs".into());
    s.runs.entry("scan".into()).or_default().items = vec![
        ProposalItem {
            key: "keep.rs".into(),
            label: "keep.rs".into(),
            note: "modified".into(),
            included: true,
        },
        ProposalItem {
            key: "drop.rs".into(),
            label: "drop.rs".into(),
            note: "modified".into(),
            included: false,
        },
    ];

    let items = proposal_items(&spec(Step::Commit), &s);
    let dropped = items.iter().find(|i| i.key == "drop.rs").unwrap();
    let kept = items.iter().find(|i| i.key == "keep.rs").unwrap();
    assert!(!dropped.included, "the approval must not re-check it");
    assert!(kept.included);
}

#[test]
fn a_commit_node_reads_its_own_checkboxes_whatever_it_is_called() {
    // A flow built in Setup names a second commit step `commit_2`.
    // Reading a hardcoded `"commit"` handed it the first node's answer.
    let s = commit_run("commit_2", &[("keep.rs", true), ("drop.rs", false)]);
    assert_eq!(paths_to_stage("commit_2", &s), vec!["keep.rs".to_string()]);
}

#[test]
fn unchecking_everything_stages_nothing_rather_than_everything() {
    // The fail-open case: an empty selection must not widen back to the
    // full scan, or the approval would stage exactly what was unchecked.
    let s = commit_run("commit", &[("keep.rs", false), ("drop.rs", false)]);
    assert!(paths_to_stage("commit", &s).is_empty());
    assert!(offered_a_choice("commit", &s), "so the caller fails loudly");
}

#[test]
fn a_node_that_never_offered_a_list_falls_back_to_the_scan() {
    // `scan` found the tree clean with the branch ahead of base: there was
    // no list to check, and this must stay a fallback rather than an error.
    let s = commit_run("commit", &[]);
    assert!(!offered_a_choice("commit", &s));
    assert_eq!(
        paths_to_stage("commit", &s),
        vec!["keep.rs".to_string(), "drop.rs".to_string()]
    );
}

#[test]
fn a_deselection_is_found_whatever_the_scanning_step_is_called() {
    // A flow built in Setup can name its scan step anything.
    let mut s = RunState::default();
    s.artifacts.insert("commit_paths".into(), "drop.rs".into());
    s.runs.entry("look_at_things".into()).or_default().items = vec![ProposalItem {
        key: "drop.rs".into(),
        label: "drop.rs".into(),
        note: "modified".into(),
        included: false,
    }];
    assert!(!proposal_items(&spec(Step::Commit), &s)[0].included);
}

#[test]
fn scan_offers_every_change_checked() {
    // Checked by default: the common case is committing everything.
    let mut s = RunState::default();
    s.artifacts
        .insert("commit_paths".into(), "a.rs\nb.rs".into());
    let items = proposal_items(&spec(Step::Commit), &s);
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i.included));
}

#[test]
fn a_new_file_is_offered_for_commit_rather_than_refused() {
    // ais-monitor: the only change was an untracked ci.yml, and the scan
    // used to fail outright.
    let mut s = RunState::default();
    s.artifacts
        .insert("commit_paths".into(), ".github/workflows/ci.yml".into());
    s.artifacts
        .insert("file_notes".into(), ".github/workflows/ci.yml\tnew".into());
    let items = proposal_items(&spec(Step::Commit), &s);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].note, "new");
    assert!(items[0].included, "checked by default");
}

#[test]
fn only_the_commit_node_offers_a_file_list() {
    let s = RunState::default();
    for step in [Step::Push, Step::OpenPr, Step::Preflight] {
        assert!(proposal_items(&spec(step), &s).is_empty());
    }
}

#[test]
fn a_path_with_no_recorded_status_still_appears() {
    let mut s = RunState::default();
    s.artifacts
        .insert("commit_paths".into(), "src/lib.rs".into());
    let items = proposal_items(&spec(Step::Commit), &s);
    assert_eq!(items[0].note, "modified");
}

#[test]
fn branch_names_from_a_model_are_scrubbed_before_they_reach_a_shell() {
    assert_eq!(sanitise_branch("Feat/Add Thing"), "feat/add-thing");
    assert_eq!(sanitise_branch("fix: `rm -rf /`"), "fix-rm--rf");
    assert_eq!(sanitise_branch("  --weird--  "), "weird");
    assert_eq!(sanitise_branch("$(whoami)"), "whoami");
    assert_eq!(sanitise_branch(""), "gitagent/change");
}

#[test]
fn a_long_branch_name_is_capped() {
    assert!(sanitise_branch(&"a".repeat(200)).len() <= 40);
}

#[test]
fn without_a_model_the_commit_still_gets_a_real_message() {
    // The reason skipped model steps still write: `commit` reads
    // `commit_subject`, and a blank one commits with no message at all.
    let node = spec(Step::DraftCommit);
    let mut state = RunState::default();
    state.artifacts.insert(
        "stat".into(),
        " src/services/azure.rs  | 12 ++++\n src/services/store.rs  |  4 +-\n".into(),
    );

    let out = without_model(&node, &state);
    let map: std::collections::BTreeMap<_, _> = out.artifacts.into_iter().collect();
    assert_eq!(
        map["commit_subject"],
        "chore: update 2 files in src/services"
    );
    assert_eq!(map["branch_name"], "work/chore-update-2-files-in-src");
    assert!(map["commit_body"].contains("azure.rs"));
    assert!(out.summary.contains("without AI"));
}

#[test]
fn one_changed_file_is_named_rather_than_counted() {
    let node = spec(Step::DraftCommit);
    let mut state = RunState::default();
    state
        .artifacts
        .insert("stat".into(), " README.md | 2 +-\n".into());
    let map: std::collections::BTreeMap<_, _> =
        without_model(&node, &state).artifacts.into_iter().collect();
    assert_eq!(map["commit_subject"], "chore: update README.md");
}

#[test]
fn files_with_no_shared_directory_are_only_counted() {
    // Claiming a directory they do not share would be worse than vague.
    let node = spec(Step::DraftCommit);
    let mut state = RunState::default();
    state.artifacts.insert(
        "stat".into(),
        " src/main.rs | 1 +\n assets/main.css | 2 +-\n".into(),
    );
    let map: std::collections::BTreeMap<_, _> =
        without_model(&node, &state).artifacts.into_iter().collect();
    assert_eq!(map["commit_subject"], "chore: update 2 files");
}

#[test]
fn the_pr_reuses_the_commit_subject_rather_than_inventing_another() {
    let node = spec(Step::DraftPr);
    let mut state = RunState::default();
    state
        .artifacts
        .insert("commit_subject".into(), "fix: the thing".into());
    state
        .artifacts
        .insert("stat".into(), " a.rs | 1 +\n".into());
    let map: std::collections::BTreeMap<_, _> =
        without_model(&node, &state).artifacts.into_iter().collect();
    assert_eq!(map["pr_title"], "fix: the thing");
}

#[test]
fn nothing_stands_in_for_reading_a_diff_for_regressions() {
    // An empty verdict is the honest answer, and it is the one the
    // trusted-run rules already read as "nobody looked".
    let node = spec(Step::Analyse);
    let out = without_model(&node, &RunState::default());
    assert!(out.artifacts.is_empty());
    assert!(!out.nothing_to_do, "not an error, and not a dead branch");
}

#[test]
fn an_empty_diffstat_still_produces_something_commitable() {
    let node = spec(Step::DraftCommit);
    let map: std::collections::BTreeMap<_, _> = without_model(&node, &RunState::default())
        .artifacts
        .into_iter()
        .collect();
    assert_eq!(map["commit_subject"], "chore: update");
    assert!(!map["branch_name"].is_empty());
}
