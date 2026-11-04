use super::*;
use crate::services::graph::{NodeKind, ProposalItem, Remedy, Step};

fn render(app: fn() -> Element) -> String {
    let mut dom = VirtualDom::new(app);
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

fn item(key: &str, included: bool) -> ProposalItem {
    ProposalItem {
        key: key.into(),
        label: key.into(),
        note: "modified".into(),
        included,
    }
}

fn remedy(once_resolved: bool) -> Remedy {
    Remedy {
        label: "Finish the merge".into(),
        display: "git commit --no-edit".into(),
        program: "git".into(),
        args: vec![],
        running: false,
        output: String::new(),
        done: false,
        retry_after: true,
        abandons: false,
        sets: vec![],
        failed: false,
        once_resolved,
    }
}

#[test]
fn the_contract_marks_an_empty_side_with_a_dash() {
    let html = render(|| {
        let spec = NodeSpec {
            id: "scan".into(),
            title: "Scan".into(),
            subtitle: String::new(),
            step: Step::ScanChanges,
            kind: NodeKind::Deterministic,
            deps: vec![],
            reads: vec!["diff".into()],
            writes: vec![],
            requires_approval: false,
            config: Default::default(),
            bind: Default::default(),
        };
        contract(&spec)
    });
    assert!(
        html.contains(r#"class="chip""#) && html.contains("diff"),
        "{html}"
    );
    assert_eq!(html.matches("contract-none").count(), 1, "{html}");
}

#[test]
fn an_approval_counts_the_files_and_locks_them_when_read_only() {
    let html = render(|| {
        let run = NodeRun {
            proposal: "git commit".into(),
            items: vec![item("a.rs", true), item("b.rs", false)],
            ..Default::default()
        };
        approval(
            &run,
            "commit".into(),
            1,
            true,
            true,
            EventHandler::new(|_| {}),
            use_signal(|| true),
        )
    });
    assert!(
        html.contains("This will run:") && html.contains("git commit"),
        "{html}"
    );
    assert!(html.contains("1 of 2 selected"), "{html}");
    assert_eq!(html.matches("disabled").count(), 2, "{html}");
    assert!(html.contains("item item-off"), "{html}");
}

#[test]
fn a_fix_that_needs_the_conflicts_settled_waits_for_them() {
    let html = render(|| {
        let run = NodeRun {
            remedies: vec![remedy(true)],
            ..Default::default()
        };
        remedies(
            &run,
            "merge".into(),
            EventHandler::new(|_| {}),
            use_signal(|| vec!["src/a.rs".to_string()]),
            use_signal(|| 0),
        )
    });
    assert!(html.contains("Git is still waiting on src/a.rs"), "{html}");
    assert!(html.contains("Check again"), "{html}");
    assert!(html.contains("disabled"), "{html}");
}

#[test]
fn no_fix_on_offer_says_so() {
    let html = render(|| {
        remedies(
            &NodeRun::default(),
            "x".into(),
            EventHandler::new(|_| {}),
            use_signal(Vec::new),
            use_signal(|| 0),
        )
    });
    assert!(html.contains("No automatic fix"), "{html}");
}

#[test]
fn a_fix_output_is_folded_so_the_resolver_stays_in_sight() {
    let html = render(|| {
        let mut fix = remedy(false);
        fix.failed = true;
        fix.output = "CONFLICT (content): Merge conflict in Cargo.toml".into();
        remedies(
            &NodeRun {
                remedies: vec![fix],
                ..NodeRun::default()
            },
            "x".into(),
            EventHandler::new(|_| {}),
            use_signal(Vec::new),
            use_signal(|| 0),
        )
    });
    assert!(html.contains("<details"), "{html}");
    assert!(!html.contains("<details open"), "{html}");
    assert!(html.contains("That stopped before finishing"), "{html}");
}
