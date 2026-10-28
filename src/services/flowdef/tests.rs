mod validation;
#[test]
fn a_flow_id_reads_as_its_name() {
    assert_eq!(id_from_label("Deploy api0 landing"), "deploy_api0_landing");
    assert_eq!(id_from_label("Deploy VPS"), "deploy_vps");
    assert_eq!(id_from_label("Commit → PR"), "commit_pr");
    assert_eq!(id_from_label("  Release!  "), "release");
    assert_eq!(id_from_label("→"), "flow");
}

#[test]
fn flows_you_made_take_their_names_and_the_shipped_ones_keep_theirs() {
    let mut book = FlowBook::defaults();
    let copy = |id: &str, label: &str| FlowDef {
        id: id.into(),
        label: label.into(),
        handles: vec![],
        nodes: vec![NodeDef::from_catalogue("preflight", "preflight")],
    };
    book.flows.push(copy("review_and_merge_copy", "Release"));
    book.flows
        .push(copy("review_and_merge_copy_copy", "Deploy cvenom"));
    book.flows
        .push(copy("review_and_merge_copy_copy_2", "Deploy cvenom"));
    let renames = book.ids_from_labels();
    assert_eq!(
        renames,
        vec![
            ("review_and_merge_copy".to_string(), "release".to_string()),
            (
                "review_and_merge_copy_copy".to_string(),
                "deploy_cvenom".to_string()
            ),
            (
                "review_and_merge_copy_copy_2".to_string(),
                "deploy_cvenom_2".to_string()
            ),
        ]
    );
    assert!(book.get("commit_and_pr").is_some() && book.get("review_and_merge").is_some());
    assert!(
        book.ids_from_labels().is_empty(),
        "a second pass changes nothing"
    );
    assert!(
        book.adopted.iter().any(|a| a == ADOPTED_IDS_FROM_LABELS),
        "a new book starts adopted"
    );
}

fn review_without_second_opinion() -> FlowDef {
    let mut flow = FlowBook::defaults()
        .get("review_and_merge")
        .unwrap()
        .clone();
    flow.remove_node("second_opinion");
    flow
}

#[test]
fn a_saved_review_flow_gets_the_focused_reviews_beside_its_analysis() {
    let mut flow = FlowBook::defaults()
        .get("review_and_merge")
        .unwrap()
        .clone();
    flow.remove_node("lenses");
    assert!(flow.insert_lenses());
    let lenses = flow.nodes.iter().find(|n| n.step == "lenses").unwrap();
    assert_eq!(lenses.deps, vec!["pr_diff".to_string()]);
    let merge = flow.nodes.iter().find(|n| n.id == "merge").unwrap();
    assert!(merge.deps.contains(&"lenses".to_string()));
    assert!(validate(&flow).is_empty(), "{:?}", validate(&flow));
    assert!(!flow.insert_lenses(), "only once");
}

#[test]
fn the_focused_reviews_are_added_once_ever() {
    let mut book = FlowBook::defaults();
    book.flows[1].remove_node("lenses");
    assert!(book.adopt_lenses());
    book.flows[1].remove_node("lenses");
    assert!(!book.adopt_lenses());
    assert!(!book.flows[1].nodes.iter().any(|n| n.step == "lenses"));
}

#[test]
fn a_saved_review_flow_gets_the_second_opinion_beside_its_analysis() {
    let mut flow = review_without_second_opinion();
    assert!(flow.insert_second_opinion());
    let second = flow
        .nodes
        .iter()
        .find(|n| n.step == "second_opinion")
        .unwrap();
    assert_eq!(second.deps, vec!["pr_diff".to_string()]);
    let merge = flow.nodes.iter().find(|n| n.id == "merge").unwrap();
    assert!(merge.deps.contains(&"analyse".to_string()));
    assert!(merge.deps.contains(&"second_opinion".to_string()));
    assert!(validate(&flow).is_empty(), "{:?}", validate(&flow));
}

#[test]
fn the_second_opinion_is_added_once_ever() {
    let mut book = FlowBook::defaults();
    book.flows[1] = review_without_second_opinion();
    assert!(book.adopt_second_opinion());
    assert!(!book.adopt_second_opinion());
    // Deleted afterwards, it stays deleted.
    book.flows[1].remove_node("second_opinion");
    assert!(!book.adopt_second_opinion());
    assert!(!book.flows[1]
        .nodes
        .iter()
        .any(|n| n.step == "second_opinion"));
}

#[test]
fn a_flow_with_no_analysis_gets_no_second_opinion() {
    let mut flow = FlowBook::defaults().get("commit_and_pr").unwrap().clone();
    assert!(!flow.insert_second_opinion());
}

#[test]
fn the_shipped_review_flow_is_valid_with_its_second_opinion() {
    let book = FlowBook::defaults();
    let flow = book.get("review_and_merge").unwrap();
    assert!(flow.nodes.iter().any(|n| n.step == "second_opinion"));
    assert!(validate(flow).is_empty(), "{:?}", validate(flow));
}

use super::*;

fn node(id: &str, step: &str, deps: &[&str]) -> NodeDef {
    NodeDef {
        id: id.into(),
        step: step.into(),
        title: String::new(),
        subtitle: String::new(),
        deps: deps.iter().map(|d| d.to_string()).collect(),
        gated: None,
        config: BTreeMap::new(),
        bind: BTreeMap::new(),
    }
}

fn commit_flow() -> FlowDef {
    FlowDef {
        id: "commit_and_pr".into(),
        label: "Commit → PR".into(),
        handles: vec![],
        nodes: vec![
            node("preflight", "preflight", &[]),
            node("scan", "scan_changes", &["preflight"]),
            node("draft_commit", "draft_commit", &["scan"]),
            node("commit", "commit", &["draft_commit"]),
            node("draft_pr", "draft_pr", &["draft_commit"]),
            node("push", "push", &["commit"]),
            node("open_pr", "open_pr", &["push", "draft_pr"]),
        ],
    }
}

#[test]
fn a_well_formed_flow_has_no_problems() {
    assert_eq!(validate(&commit_flow()), vec![]);
}

#[test]
fn a_book_saved_before_declarations_existed_gets_them_back() {
    // The real flows.toml on disk predates `handles`, so both shipped
    // flows loaded declaring nothing and selection always fell back.
    let mut book = FlowBook::defaults();
    for flow in &mut book.flows {
        flow.handles.clear();
    }

    book.adopt_missing_declarations();
    assert_eq!(
        book.get("commit_and_pr").unwrap().handles,
        vec!["uncommitted".to_string(), "unpushed_branch".to_string()]
    );
    assert_eq!(
        book.get("review_and_merge").unwrap().handles,
        vec!["open_pull_request".to_string()]
    );
}

#[test]
fn a_deliberate_choice_to_declare_nothing_survives() {
    // Only an *unmodified* shipped flow is backfilled; a flow of your own
    // that answers nothing keeps answering nothing.
    let mut book = FlowBook::defaults();
    book.flows.push(FlowDef {
        id: "deploy_vps".into(),
        label: "Deploy VPS".into(),
        handles: vec![],
        nodes: vec![],
    });
    book.adopt_missing_declarations();
    assert!(book.get("deploy_vps").unwrap().handles.is_empty());
}

#[test]
fn backfilling_never_overwrites_what_is_already_declared() {
    let mut book = FlowBook::defaults();
    book.flows[0].handles = vec!["release".into()];
    book.adopt_missing_declarations();
    assert_eq!(book.get("commit_and_pr").unwrap().handles, vec!["release"]);
}

#[test]
fn a_flow_answers_a_need_by_declaring_it_not_by_its_name() {
    use crate::services::probe::Need;
    let book = FlowBook::defaults();

    let commit = book.get("commit_and_pr").unwrap();
    assert!(commit.answers(Need::Uncommitted));
    assert!(commit.answers(Need::UnpushedBranch));
    assert!(!commit.answers(Need::OpenPullRequest));

    let review = book.get("review_and_merge").unwrap();
    assert!(review.answers(Need::OpenPullRequest));
    assert!(!review.answers(Need::Release));
}

#[test]
fn a_flow_copied_in_setup_keeps_its_kind() {
    let shipped = FlowBook::defaults();
    let review = shipped.get("review_and_merge").unwrap();
    assert_eq!(review.kind(), FlowKind::Review);
    assert_eq!(
        shipped.get("commit_and_pr").unwrap().kind(),
        FlowKind::Commit
    );

    // A copy under another id, even one that no longer declares what it
    // answers: it still reads and merges a pull request, or commits.
    let mut copy = review.clone();
    copy.id = "review_and_merge_copy".into();
    copy.handles.clear();
    assert_eq!(copy.kind(), FlowKind::Review);

    let mut commit_copy = shipped.get("commit_and_pr").unwrap().clone();
    commit_copy.id = "commit_and_pr_copy".into();
    commit_copy.handles.clear();
    assert_eq!(commit_copy.kind(), FlowKind::Commit);

    // The release copies in the wild share the "review_and_merge_copy"
    // prefix but merge nothing; the name must not decide it.
    let release = FlowDef {
        id: "review_and_merge_copy_2".into(),
        label: "Release".into(),
        handles: vec!["release".into()],
        nodes: vec![],
    };
    assert_eq!(release.kind(), FlowKind::Other);
}

#[test]
fn a_flow_called_anything_can_answer_the_release_need() {
    use crate::services::probe::Need;
    // mayorana deploys to a VPS rather than tagging a release.
    let flow = FlowDef {
        id: "deploy_vps".into(),
        label: "Deploy VPS".into(),
        handles: vec!["release".into()],
        nodes: vec![],
    };
    assert!(flow.answers(Need::Release));
    assert!(!flow.answers(Need::Uncommitted));
}

#[test]
fn a_flow_declaring_nothing_is_never_auto_selected() {
    use crate::services::probe::Need;
    let flow = FlowDef {
        id: "x".into(),
        label: "x".into(),
        handles: vec![],
        nodes: vec![],
    };
    assert!(Need::ALL.into_iter().all(|need| !flow.answers(need)));
}

#[test]
fn both_shipped_flows_are_valid() {
    for flow in &FlowBook::defaults().flows {
        assert_eq!(validate(flow), vec![], "{} is broken", flow.id);
    }
}

#[test]
fn the_shipped_flows_survive_a_round_trip_through_toml() {
    let book = FlowBook::defaults();
    let text = toml::to_string_pretty(&book).unwrap();
    let back: FlowBook = toml::from_str(&text).unwrap();
    assert_eq!(book, back);
}

#[test]
fn a_corrupt_flow_file_falls_back_to_the_defaults() {
    let book: FlowBook = toml::from_str("this is not toml").unwrap_or_default();
    assert!(book.flows.is_empty());
    // load() turns that into the defaults rather than leaving the app empty.
    assert_eq!(FlowBook::defaults().flows.len(), 2);
}

#[test]
fn the_shipped_flows_keep_the_diamond() {
    let book = FlowBook::defaults();
    let commit = book.get("commit_and_pr").unwrap();
    let draft_pr = commit.nodes.iter().find(|n| n.id == "draft_pr").unwrap();
    assert_eq!(draft_pr.deps, vec!["commit".to_string()]);

    let review = book.get("review_and_merge").unwrap();
    let status = review.nodes.iter().find(|n| n.id == "pr_status").unwrap();
    assert_eq!(status.deps, vec!["find_pr".to_string()]);
}

#[test]
fn an_invalid_flow_is_kept_for_repair_but_not_offered_to_run() {
    let mut book = FlowBook::defaults();
    book.flows[0].nodes[1].deps = vec!["ghost".into()];
    assert_eq!(book.flows.len(), 2);
    assert_eq!(book.runnable().len(), 1);
}

#[test]
fn a_flow_hidden_on_a_repository_is_not_runnable_there() {
    let book = FlowBook::defaults();
    let all = book.runnable().len();
    assert!(all >= 2, "the shipped book has something to hide");

    let first = book.runnable()[0].id.clone();
    let left = book.runnable_for(std::slice::from_ref(&first));
    assert_eq!(left.len(), all - 1);
    assert!(!left.iter().any(|f| f.id == first));
}

#[test]
fn hiding_is_per_repository_so_an_empty_list_changes_nothing() {
    let book = FlowBook::defaults();
    assert_eq!(book.runnable_for(&[]).len(), book.runnable().len());
}

#[test]
fn hiding_an_id_no_flow_uses_is_harmless() {
    // A flow can be deleted in Setup while a repository still lists its id
    // as hidden — that stale entry must not remove anything else.
    let book = FlowBook::defaults();
    assert_eq!(
        book.runnable_for(&["deleted_long_ago".to_string()]).len(),
        book.runnable().len()
    );
}

#[test]
fn an_invalid_flow_is_still_listed_so_it_can_be_seen_and_marked() {
    // `runnable` dropping it is what the executor needs; the tab strip
    // needs the opposite, or a flow one edit from working looks deleted.
    let mut book = FlowBook::defaults();
    book.flows[0].nodes[1].deps = vec!["ghost".into()];

    let listed = book.listed();
    assert_eq!(listed.len(), 2, "every flow is listed, broken or not");

    let (broken, problems) = &listed[0];
    assert_eq!(broken.id, "commit_and_pr");
    assert!(!problems.is_empty(), "and carries what is wrong with it");
    assert!(problems[0].message().contains("ghost"));

    assert!(listed[1].1.is_empty(), "the healthy one has nothing to say");
}

/// A commit flow the way one actually looks after a while in use: a base
/// pinned on preflight, two extra nodes on the end, and no test step.
fn a_used_commit_flow() -> FlowDef {
    let mut flow = FlowBook::defaults().get("commit_and_pr").unwrap().clone();
    flow.remove_node("test");
    flow.nodes[0].config.insert("base".into(), "main".into());
    flow.nodes.push(dep(
        NodeDef::from_catalogue("find_pr", "find_pr"),
        &["open_pr"],
    ));
    flow.nodes
        .push(dep(NodeDef::from_catalogue("sync", "sync"), &["find_pr"]));
    flow
}

#[test]
fn a_commit_flow_saved_before_the_test_step_picks_it_up() {
    // Everyone with an existing flows.toml would otherwise never see the
    // step, because defaults only apply when the file is absent.
    let mut book = FlowBook {
        adopted: vec![],
        flows: vec![a_used_commit_flow()],
    };
    assert!(book.adopt_missing_test_step());

    let flow = &book.flows[0];
    let test = flow.nodes.iter().find(|n| n.step == "run_tests").unwrap();
    assert_eq!(test.deps, vec!["scan".to_string()]);
    let commit = flow.nodes.iter().find(|n| n.id == "commit").unwrap();
    assert!(commit.deps.contains(&"test".to_string()));
    assert!(validate(flow).is_empty(), "{:?}", validate(flow));
}

#[test]
fn a_customised_commit_flow_keeps_everything_it_already_had() {
    let mut book = FlowBook {
        adopted: vec![],
        flows: vec![a_used_commit_flow()],
    };
    book.adopt_missing_test_step();

    let flow = &book.flows[0];
    assert_eq!(
        flow.nodes[0].setting("base"),
        "main",
        "the pinned base survives"
    );
    assert!(
        flow.nodes.iter().any(|n| n.id == "sync"),
        "so do added nodes"
    );
}

#[test]
fn a_flow_that_does_not_commit_is_left_alone() {
    // The "Release" flow: preflight and a script. Nothing to test between.
    let mut book = FlowBook {
        adopted: vec![],
        flows: vec![FlowDef {
            id: "release".into(),
            label: "Release".into(),
            handles: vec![],
            nodes: vec![
                NodeDef::from_catalogue("preflight", "preflight"),
                dep(
                    NodeDef::from_catalogue("run_script", "run_script"),
                    &["preflight"],
                ),
            ],
        }],
    };
    book.adopt_missing_test_step();
    assert!(!book.flows[0].nodes.iter().any(|n| n.step == "run_tests"));
}

fn a_release_flow(command: &str) -> FlowBook {
    let mut script = dep(
        NodeDef::from_catalogue("run_script", "run_script"),
        &["preflight"],
    );
    script.config.insert("command".into(), command.into());
    FlowBook {
        adopted: vec![],
        flows: vec![FlowDef {
            id: "release".into(),
            label: "Release".into(),
            handles: vec!["release".into()],
            nodes: vec![NodeDef::from_catalogue("preflight", "preflight"), script],
        }],
    }
}

#[test]
fn a_release_flow_gets_its_notes_drafted_before_the_script_runs() {
    let mut book = a_release_flow("./scripts/release.sh --patch");
    assert!(book.adopt_notes_steps());

    let flow = &book.flows[0];
    let deps = |id: &str| flow.nodes.iter().find(|n| n.id == id).unwrap().deps.clone();
    assert_eq!(deps("draft_notes"), vec!["preflight"]);
    assert_eq!(deps("write_notes"), vec!["draft_notes"]);
    assert_eq!(deps("run_script"), vec!["write_notes"]);
    assert_eq!(validate(flow), vec![], "the result must still run");
}

#[test]
fn a_script_that_is_not_a_release_is_left_alone() {
    let mut book = a_release_flow("make deploy");
    book.adopt_notes_steps();
    assert_eq!(book.flows[0].nodes.len(), 2);
}

#[test]
fn notes_steps_you_deleted_stay_deleted() {
    let mut book = a_release_flow("./scripts/release.sh");
    assert!(book.adopt_notes_steps());
    book.flows[0].remove_node("draft_notes");
    book.flows[0].remove_node("write_notes");
    assert!(!book.adopt_notes_steps(), "it only ever runs once");
    assert_eq!(book.flows[0].nodes.len(), 2);
}

#[test]
fn a_test_step_you_deleted_stays_deleted() {
    // The marker is the whole point: without it, every launch would put
    // back a step you had just removed, with no way to refuse it.
    let mut book = FlowBook {
        adopted: vec![],
        flows: vec![a_used_commit_flow()],
    };
    assert!(book.adopt_missing_test_step());

    book.flows[0].remove_node("test");
    assert!(!book.adopt_missing_test_step(), "it only ever runs once");
    assert!(!book.flows[0].nodes.iter().any(|n| n.step == "run_tests"));
}

#[test]
fn the_shipped_book_has_nothing_to_adopt() {
    let mut book = FlowBook::defaults();
    let before = book.clone();
    assert!(!book.adopt_missing_test_step());
    assert_eq!(book, before);
}

#[test]
fn adoption_survives_a_round_trip_through_the_file() {
    // The marker is only worth anything if it is written back.
    let mut book = FlowBook {
        adopted: vec![],
        flows: vec![a_used_commit_flow()],
    };
    book.adopt_missing_test_step();

    let text = toml::to_string_pretty(&book).unwrap();
    let read_back: FlowBook = toml::from_str(&text).unwrap();
    assert_eq!(read_back, book);
    let mut read_back = read_back;
    assert!(!read_back.adopt_missing_test_step());
}

#[test]
fn a_real_flows_file_gets_the_step() {
    // Copied from a `flows.toml` that had been in use for a while — the
    // exact file the first version of this migration silently skipped,
    // because it insisted on a flow identical to the shipped one.
    let text = r#"
[[flow]]
id = "commit_and_pr"
label = "Commit → PR"
handles = []

[[flow.node]]
id = "preflight"
step = "preflight"

[flow.node.config]
base = "main"

[[flow.node]]
id = "scan"
step = "scan_changes"
deps = ["preflight"]

[[flow.node]]
id = "draft_commit"
step = "draft_commit"
deps = ["scan"]

[[flow.node]]
id = "commit"
step = "commit"
deps = ["draft_commit"]

[[flow.node]]
id = "draft_pr"
step = "draft_pr"
deps = ["draft_commit"]

[[flow.node]]
id = "push"
step = "push"
deps = ["commit"]

[[flow.node]]
id = "open_pr"
step = "open_pr"
deps = ["push", "draft_pr"]

[[flow.node]]
id = "find_pr"
step = "find_pr"
deps = ["open_pr"]

[[flow.node]]
id = "sync"
step = "sync"
deps = ["find_pr"]
"#;

    let mut book: FlowBook = toml::from_str(text).unwrap();
    assert!(book.adopt_missing_test_step());

    let flow = book.get("commit_and_pr").unwrap();
    let test = flow
        .nodes
        .iter()
        .find(|n| n.step == "run_tests")
        .expect("the step must land in a flow that scans and commits");
    assert_eq!(test.deps, vec!["scan".to_string()]);
    assert!(flow
        .nodes
        .iter()
        .find(|n| n.id == "commit")
        .unwrap()
        .deps
        .contains(&"test".to_string()));
    // And the result is a flow that still runs.
    assert!(validate(flow).is_empty(), "{:?}", validate(flow));
}

/// `open_pr` and `find_pr` both write `pr_url`, and a real commit flow
/// contains both — so "whichever ancestor writes it" has two answers and
/// picks by luck. These are the tests for saying which you meant.
fn two_producers_flow() -> FlowDef {
    FlowDef {
        id: "two".into(),
        label: "Two".into(),
        handles: vec![],
        nodes: vec![
            NodeDef::from_catalogue("preflight", "preflight"),
            dep(
                NodeDef::from_catalogue("scan", "scan_changes"),
                &["preflight"],
            ),
            dep(
                NodeDef::from_catalogue("draft_commit", "draft_commit"),
                &["scan"],
            ),
            dep(
                NodeDef::from_catalogue("draft_pr", "draft_pr"),
                &["draft_commit"],
            ),
            dep(
                NodeDef::from_catalogue("find_pr", "find_pr"),
                &["preflight"],
            ),
        ],
    }
}
