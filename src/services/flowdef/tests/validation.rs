//! Validating flows: bindings, dependencies, cycles, script steps, and what the
//! graph takes from the catalogue.

use super::*;

#[test]
fn a_flow_with_no_bindings_validates_exactly_as_before() {
    // The whole compatibility claim in one assertion: every shipped flow
    // binds nothing, and nothing about them changes.
    for flow in &FlowBook::defaults().flows {
        assert!(
            validate(flow).is_empty(),
            "{}: {:?}",
            flow.id,
            validate(flow)
        );
        assert!(flow.nodes.iter().all(|n| n.bind.is_empty()));
    }
}

#[test]
fn a_binding_names_which_producer_an_input_comes_from() {
    let mut flow = two_producers_flow();
    let merge = flow.nodes.iter_mut().find(|n| n.id == "find_pr").unwrap();
    merge.bind.insert("forge".into(), "preflight.forge".into());
    assert_eq!(
        flow.nodes
            .iter()
            .find(|n| n.id == "find_pr")
            .unwrap()
            .binding("forge"),
        Some(("preflight", "forge"))
    );
    assert!(validate(&flow).is_empty(), "{:?}", validate(&flow));
}

#[test]
fn a_bare_node_name_binds_the_input_of_the_same_name() {
    // `forge = "preflight"` is the common case and much less to type than
    // `forge = "preflight.forge"`.
    let mut node = NodeDef::from_catalogue("find_pr", "find_pr");
    node.bind.insert("forge".into(), "preflight".into());
    assert_eq!(node.binding("forge"), Some(("preflight", "forge")));
}

#[test]
fn binding_to_something_not_upstream_is_refused() {
    // `find_pr` does not depend on `draft_pr`, so nothing says it has run.
    let mut flow = two_producers_flow();
    flow.nodes
        .iter_mut()
        .find(|n| n.id == "find_pr")
        .unwrap()
        .bind
        .insert("forge".into(), "draft_pr.forge".into());
    let problems = validate(&flow);
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::BindingNotUpstream { .. })),
        "{problems:?}"
    );
    let message = problems
        .iter()
        .find(|p| matches!(p, Problem::BindingNotUpstream { .. }))
        .unwrap()
        .message();
    assert!(message.contains("does not depend on"), "{message}");
}

#[test]
fn binding_to_a_node_that_does_not_write_it_is_refused() {
    let mut flow = two_producers_flow();
    flow.nodes
        .iter_mut()
        .find(|n| n.id == "find_pr")
        .unwrap()
        .bind
        .insert("forge".into(), "preflight.pr_title".into());
    let problems = validate(&flow);
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::BindingNotProduced { .. })),
        "{problems:?}"
    );
}

#[test]
fn binding_an_input_the_step_does_not_have_is_refused() {
    // Dead configuration that looks like it is doing something.
    let mut flow = two_producers_flow();
    flow.nodes
        .iter_mut()
        .find(|n| n.id == "find_pr")
        .unwrap()
        .bind
        .insert("nonsense".into(), "preflight.forge".into());
    let problems = validate(&flow);
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, Problem::BindingUnknownInput { .. })),
        "{problems:?}"
    );
}

#[test]
fn a_binding_reaches_the_graph_fully_qualified() {
    // The executor never sees the shorthand.
    let mut flow = two_producers_flow();
    flow.nodes
        .iter_mut()
        .find(|n| n.id == "find_pr")
        .unwrap()
        .bind
        .insert("forge".into(), "preflight".into());
    let graph = flow.to_graph();
    let node = graph.get("find_pr").unwrap();
    assert_eq!(
        node.bind.get("forge").map(String::as_str),
        Some("preflight.forge")
    );
}

#[test]
fn bindings_survive_a_round_trip_through_the_file() {
    let mut flow = two_producers_flow();
    flow.nodes
        .iter_mut()
        .find(|n| n.id == "find_pr")
        .unwrap()
        .bind
        .insert("forge".into(), "preflight.forge".into());
    let book = FlowBook {
        adopted: vec![],
        flows: vec![flow],
    };
    let text = toml::to_string_pretty(&book).unwrap();
    assert_eq!(toml::from_str::<FlowBook>(&text).unwrap(), book);
    // And a node that binds nothing writes no `bind` table at all, so
    // existing files keep their shape.
    assert_eq!(text.matches("[flow.node.bind]").count(), 1);
}

#[test]
fn a_new_flow_id_never_collides() {
    let book = FlowBook::defaults();
    assert_eq!(book.free_flow_id("commit_and_pr"), "commit_and_pr_2");
    assert_eq!(book.free_flow_id("release"), "release");
}

#[test]
fn duplicating_a_flow_copies_its_nodes_under_a_free_id() {
    let mut book = FlowBook::defaults();
    let new_id = book.duplicate("commit_and_pr").unwrap();
    assert_eq!(new_id, "commit_and_pr_copy");
    let copy = book.get(&new_id).unwrap();
    assert_eq!(copy.nodes, book.get("commit_and_pr").unwrap().nodes);

    assert_eq!(
        book.duplicate("commit_and_pr").unwrap(),
        "commit_and_pr_copy_2"
    );
}

#[test]
fn default_deps_attaches_to_the_selected_node_or_else_every_leaf() {
    let f = commit_flow();
    // open_pr is the only leaf.
    assert_eq!(default_deps(&f, ""), vec!["open_pr".to_string()]);
    assert_eq!(default_deps(&f, "scan"), vec!["scan".to_string()]);
}

#[test]
fn a_cycle_is_reported_rather_than_hanging_the_executor() {
    let mut f = commit_flow();
    f.nodes[0].deps = vec!["open_pr".into()];
    let problems = validate(&f);
    assert!(problems.iter().any(|p| matches!(p, Problem::Cycle(_))));
    assert!(problems.iter().any(|p| matches!(p, Problem::NoRoot)));
}

#[test]
fn a_dependency_on_a_step_that_is_not_there_is_caught() {
    let mut f = commit_flow();
    f.nodes[1].deps = vec!["ghost".into()];
    assert!(validate(&f)
        .iter()
        .any(|p| matches!(p, Problem::UnknownDep { dep, .. } if dep == "ghost")));
}

#[test]
fn a_step_that_depends_on_itself_is_caught() {
    let mut f = commit_flow();
    f.nodes[1].deps = vec!["scan".into()];
    assert!(validate(&f)
        .iter()
        .any(|p| matches!(p, Problem::SelfDep(_))));
}

#[test]
fn reordering_so_an_input_is_no_longer_upstream_is_caught() {
    // draft_commit reads `diff`, which only scan_changes writes.
    let mut f = commit_flow();
    f.nodes[2].deps = vec!["preflight".into()];
    assert!(validate(&f).iter().any(
            |p| matches!(p, Problem::MissingInput { node, key } if node == "draft_commit" && key == "diff")
        ));
}

#[test]
fn a_sibling_writing_the_key_does_not_satisfy_it() {
    // Two roots: the second reads what the first writes, but does not
    // depend on it, so ordering is not guaranteed.
    let f = FlowDef {
        id: "x".into(),
        label: "x".into(),
        handles: vec![],
        nodes: vec![
            node("preflight", "preflight", &[]),
            node("scan", "scan_changes", &[]),
        ],
    };
    assert!(validate(&f).iter().any(
        |p| matches!(p, Problem::MissingInput { node, key } if node == "scan" && key == "base")
    ));
}

#[test]
fn duplicate_ids_are_caught() {
    let mut f = commit_flow();
    f.nodes[1].id = "preflight".into();
    assert!(validate(&f)
        .iter()
        .any(|p| matches!(p, Problem::DuplicateId(id) if id == "preflight")));
}

#[test]
fn an_unknown_step_key_is_reported_and_dropped_from_the_graph() {
    let mut f = commit_flow();
    f.nodes[3].step = "teleport".into();
    assert!(validate(&f)
        .iter()
        .any(|p| matches!(p, Problem::UnknownStep { key, .. } if key == "teleport")));
    assert_eq!(f.to_graph().nodes.len(), 6, "the rest of the flow survives");
}

#[test]
fn deleting_a_step_takes_its_dependencies_with_it() {
    let mut f = commit_flow();
    f.remove_node("push");
    assert!(!f.nodes.iter().any(|n| n.id == "push"));
    assert!(!f.nodes.iter().any(|n| n.deps.contains(&"push".to_string())));
    assert!(!validate(&f)
        .iter()
        .any(|p| matches!(p, Problem::UnknownDep { .. })));
}

#[test]
fn steps_added_later_in_setup_are_listed_where_they_run() {
    // Deploy mayorana: the deploy step existed first, the notes steps were
    // added in front of it afterwards, so the file has them last.
    let f = FlowDef {
        id: "deploy".into(),
        label: "Deploy".into(),
        handles: vec![],
        nodes: vec![
            node("preflight", "preflight", &[]),
            node("run_remote", "run_remote", &["preflight", "write_notes"]),
            node("write_notes", "write_notes", &["draft_notes"]),
            node("draft_notes", "draft_notes", &["preflight"]),
        ],
    };
    let ids: Vec<String> = f.to_graph().nodes.into_iter().map(|n| n.id).collect();
    assert_eq!(
        ids,
        ["preflight", "draft_notes", "write_notes", "run_remote"]
    );
}

#[test]
fn steps_on_the_same_level_keep_their_file_order() {
    let ids: Vec<String> = FlowBook::defaults()
        .get("commit_and_pr")
        .unwrap()
        .to_graph()
        .nodes
        .into_iter()
        .map(|n| n.id)
        .collect();
    let at = |id: &str| ids.iter().position(|i| i == id).unwrap();
    assert!(
        at("draft_commit") < at("test"),
        "declared first, listed first"
    );
    assert!(at("commit") < at("draft_pr"));
}

#[test]
fn a_script_step_with_no_command_is_reported_rather_than_run() {
    let mut f = commit_flow();
    f.nodes.push(node("release", "run_script", &["open_pr"]));
    assert!(validate(&f)
        .iter()
        .any(|p| matches!(p, Problem::MissingSetting { node, .. } if node == "release")));

    f.nodes
        .last_mut()
        .unwrap()
        .config
        .insert("command".into(), "./scripts/release.sh --patch".into());
    assert_eq!(validate(&f), vec![]);
}

#[test]
fn two_script_steps_in_one_flow_do_not_collide() {
    let mut f = commit_flow();
    for id in ["release", "smoke"] {
        let mut n = node(id, "run_script", &["open_pr"]);
        n.config.insert("command".into(), "true".into());
        f.nodes.push(n);
    }
    assert_eq!(validate(&f), vec![]);
    let g = f.to_graph();
    assert_eq!(
        g.get("release").unwrap().writes,
        vec!["release_output", "release_exit"]
    );
    assert_eq!(
        g.get("smoke").unwrap().writes,
        vec!["smoke_output", "smoke_exit"]
    );
}

#[test]
fn a_new_id_never_collides() {
    let f = commit_flow();
    assert_eq!(f.free_id("commit"), "commit_2");
    assert_eq!(f.free_id("merge"), "merge");
}

#[test]
fn the_editor_refuses_a_dependency_that_would_close_a_loop() {
    let f = commit_flow();
    assert!(
        !can_depend_on(&f, "preflight", "open_pr"),
        "would be a cycle"
    );
    assert!(!can_depend_on(&f, "scan", "scan"), "self");
    assert!(
        can_depend_on(&f, "open_pr", "scan"),
        "already upstream, harmless"
    );
    assert!(can_depend_on(&f, "draft_pr", "commit"));
}

#[test]
fn the_graph_takes_titles_from_the_catalogue_unless_overridden() {
    let mut f = commit_flow();
    f.nodes[0].title = "Check everything".into();
    let g = f.to_graph();
    assert_eq!(g.get("preflight").unwrap().title, "Check everything");
    assert_eq!(g.get("scan").unwrap().title, "Scan changes");
}

#[test]
fn gating_follows_the_catalogue_until_it_is_overridden() {
    let mut f = commit_flow();
    assert!(f.to_graph().get("push").unwrap().requires_approval);
    f.nodes[5].gated = Some(false);
    assert!(!f.to_graph().get("push").unwrap().requires_approval);
}

#[test]
fn the_contract_always_comes_from_the_catalogue() {
    // A flow file cannot claim a step reads or writes something it does not.
    let f = commit_flow();
    let g = f.to_graph();
    assert_eq!(
        g.get("commit").unwrap().writes,
        vec!["work_branch".to_string(), "commit_sha".to_string()]
    );
}
