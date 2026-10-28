use super::*;
use crate::services::graph::{Graph, NodeKind};

fn node(step: Step) -> NodeSpec {
    NodeSpec {
        id: "n".into(),
        title: "n".into(),
        subtitle: String::new(),
        step,
        kind: NodeKind::Deterministic,
        deps: vec![],
        reads: vec![],
        writes: vec![],
        requires_approval: true,
        config: Default::default(),
        bind: Default::default(),
    }
}

/// A run state carrying just the artifacts the merge rules read.
fn state(verdict: &str, findings: &str, checks: &str) -> RunState {
    let mut s = RunState::fresh(&Graph { nodes: vec![] });
    s.artifacts.insert("verdict".into(), verdict.into());
    s.artifacts.insert("finding_count".into(), findings.into());
    s.artifacts.insert("checks_state".into(), checks.into());
    s
}

use crate::services::flowdef::FlowBook;
use crate::services::forge::Forge;
use crate::services::probe::{Checks, PrBrief};
use crate::services::release::ReleaseState;

/// A minimal flow declaring it answers the release need.
fn release_flow() -> crate::services::flowdef::FlowDef {
    crate::services::flowdef::FlowDef {
        id: "release".into(),
        label: "Release".into(),
        handles: vec![Need::Release.key().to_string()],
        nodes: vec![crate::services::flowdef::NodeDef::from_catalogue(
            "preflight",
            "preflight",
        )],
    }
}

/// A repository in whatever state the arguments describe.
fn repo(changes: usize, unmerged: usize, pr: Option<Checks>, release: bool) -> RepoStatus {
    let brief = |checks| PrBrief {
        number: "7".into(),
        title: "t".into(),
        url: "u".into(),
        checks,
        files: 1,
        additions: 1,
        deletions: 0,
        commits: 1,
        base: String::new(),
        head: String::new(),
        failed: vec![],
    };
    RepoStatus {
        branch: "feat/x".into(),
        changes,
        forge: Forge::GitHub,
        pr: pr.map(brief),
        prs: pr.map(|c| vec![brief(c)]).unwrap_or_default(),
        prs_error: None,
        ahead: 0,
        behind: 0,
        unmerged,
        release: if release {
            ReleaseState {
                last_tag: Some("v1".into()),
                commits: 2,
                prs: vec![],
                releases: true,
                since_shipped: false,
            }
        } else {
            ReleaseState::default()
        },
        in_progress: None,
    }
}

#[test]
fn a_trusted_run_will_not_publish_release_notes_nobody_read() {
    let verdict = decide(&node(Step::WriteNotes), &RunState::default());
    assert!(matches!(verdict, Verdict::Hold(_)));
}

#[test]
fn a_trusted_run_will_not_click_through_a_gated_shell_step() {
    // The command is whatever was typed into Setup; nothing here can
    // tell `cargo build` from `rm -rf`.
    for step in [Step::RunScript, Step::RunRemote] {
        let verdict = decide(&node(step), &RunState::default());
        assert!(
            matches!(verdict, Verdict::Hold(_)),
            "{step:?} was approved automatically"
        );
        assert!(
            verdict.reason().unwrap().contains("Setup"),
            "the hold must say how to change it"
        );
    }
}

#[test]
fn this_apps_own_steps_are_still_clicked_through() {
    for step in [Step::Commit, Step::Push, Step::OpenPr, Step::ScanChanges] {
        assert_eq!(decide(&node(step), &RunState::default()), Verdict::Approve);
    }
}

#[test]
fn the_chain_walks_commit_then_review_then_release() {
    // The whole point of chaining: each flow's output is the next one's
    // input, and stopping after the first leaves the obvious work undone.
    let book = FlowBook::defaults();

    let dirty = repo(3, 0, None, false);
    assert_eq!(
        next_flow(&book, &dirty, &[]),
        Some(("commit_and_pr".into(), String::new()))
    );

    let has_pr = repo(0, 0, Some(Checks::Passing), false);
    assert_eq!(
        next_flow(&book, &has_pr, &[]),
        Some(("review_and_merge".into(), "7".into())),
        "and it names the pull request to review"
    );
}

#[test]
fn a_pull_request_whose_checks_are_still_running_does_not_end_the_chain() {
    // The reported bug: a trusted run never got past the commit flow. A
    // pull request opened seconds ago always has pending checks, which is
    // `Wants::Wait` — nothing a *person* is expected to do, so the old
    // `needs_a_person` gate returned `None` and the chain stopped every
    // single time, on every repository. Reviewing is still the right next
    // flow; the merge rules below are what refuse to merge on pending CI.
    let just_pushed = repo(0, 0, Some(Checks::Pending), false);
    assert_eq!(just_pushed.wants(), crate::services::probe::Wants::Wait);
    assert!(
        !just_pushed.wants().needs_a_person(),
        "the state the old gate rejected"
    );
    assert_eq!(
        next_flow(&FlowBook::defaults(), &just_pushed, &[]),
        Some(("review_and_merge".into(), "7".into()))
    );

    // And it stops at the merge rather than merging on pending checks, so
    // moving the chain on has not weakened anything.
    let pending = state("looks_safe", "0", "pending");
    assert!(decide(&node(Step::Merge), &pending).reason().is_some());
}

#[test]
fn the_review_is_scoped_to_the_checked_out_branchs_own_pull_request() {
    // `prs` is every open pull request on the repository, in whatever
    // order the forge listed them — reviewing its first entry would review
    // somebody else's branch instead of the one just pushed.
    let mut many = repo(0, 0, Some(Checks::Passing), false);
    let mut other = many.prs[0].clone();
    other.number = "3".into();
    many.prs.insert(0, other);
    assert_eq!(
        next_flow(&FlowBook::defaults(), &many, &[]),
        Some(("review_and_merge".into(), "7".into()))
    );
}

#[test]
fn a_flow_hidden_on_this_repository_is_never_what_the_chain_takes_on_next() {
    // The reported bug. A repository whose release flow has been hidden
    // and replaced by others still had the chain start the hidden one:
    // the lookup went through `runnable()`, which knows about validity
    // and nothing about the repository it is choosing for.
    let mut book = FlowBook::defaults();
    book.flows.push(release_flow());
    let waiting = repo(0, 0, None, true);
    assert_eq!(waiting.wants(), crate::services::probe::Wants::Release);

    assert_eq!(
        next_flow(&book, &waiting, &[]),
        Some(("release".into(), String::new())),
        "with nothing hidden it is still the right answer"
    );
    assert_eq!(
        next_flow(&book, &waiting, &["release".to_string()]),
        None,
        "hidden here, so a trusted run must not start it"
    );
}

#[test]
fn hiding_one_flow_leaves_another_answering_the_same_need_available() {
    // Hiding is how a repository swaps one flow for another, so the
    // replacement has to be picked up rather than the chain simply dying.
    let mut book = FlowBook::defaults();
    book.flows.push(release_flow());
    let mut replacement = release_flow();
    replacement.id = "deploy_vps".into();
    replacement.label = "Deploy VPS".into();
    book.flows.push(replacement);

    let waiting = repo(0, 0, None, true);
    assert_eq!(
        next_flow(&book, &waiting, &["release".to_string()]),
        Some(("deploy_vps".into(), String::new())),
        "the flow that replaced it answers the same need"
    );
}

#[test]
fn a_hidden_answer_is_explained_as_hidden_rather_than_as_missing() {
    // "No flow says it handles that" about a flow sitting right there,
    // hidden, is how someone concludes the chain is ignoring them.
    let mut book = FlowBook::defaults();
    book.flows.push(release_flow());
    let waiting = repo(0, 0, None, true);

    let why = why_stopped(&book, &waiting, &["release".to_string()])
        .expect("a chain that stopped for this reason has to say so");
    assert!(why.contains("hidden"), "got {why:?}");
    assert!(why.contains("Release"), "it should name the flow: {why:?}");

    assert_eq!(
        why_stopped(&book, &waiting, &[]),
        None,
        "nothing to explain when it is not hidden"
    );
}

#[test]
fn a_need_no_flow_answers_still_reads_as_missing_not_hidden() {
    // The pre-existing message must survive: hiding something unrelated
    // does not turn "nothing handles this" into "it is hidden".
    let book = FlowBook::defaults();
    let waiting = repo(0, 0, None, true);
    let why =
        why_stopped(&book, &waiting, &["commit_and_pr".to_string()]).expect("still worth saying");
    assert!(!why.contains("hidden"), "got {why:?}");
    assert!(why.contains("Setup"), "got {why:?}");
}

#[test]
fn a_clean_repository_with_a_release_due_still_has_a_next_flow() {
    // The reported case: nothing to commit, so the Commit → PR tab refuses
    // an ordinary Start — but the repository plainly wants a release, and
    // a trusted run is for the repository.
    let mut book = FlowBook::defaults();
    book.flows.push(release_flow());
    let waiting = repo(0, 0, None, true);
    assert_eq!(waiting.wants(), crate::services::probe::Wants::Release);
    assert_eq!(
        next_flow(&book, &waiting, &[]),
        Some(("release".into(), String::new()))
    );
}

#[test]
fn the_chain_survives_the_sync_that_ends_the_commit_flow() {
    // End to end, in the order the app actually produces the states.
    // `sync` is the last node of the shipped commit flow and it checks the
    // base branch back out, so leg two starts from a repository whose HEAD
    // owns no pull request at all. That is where every trusted run stopped.
    let book = FlowBook::defaults();

    let dirty = repo(3, 0, None, false);
    assert_eq!(
        next_flow(&book, &dirty, &[]),
        Some(("commit_and_pr".into(), String::new())),
        "leg 1: commit"
    );

    // The commit flow has run: committed, pushed, opened #7, then synced
    // back to the base branch. `pr` is None because HEAD is on `develop`.
    let mut after_sync = repo(0, 0, Some(Checks::Pending), false);
    after_sync.pr = None;
    assert_eq!(
        next_flow(&book, &after_sync, &[]),
        Some(("review_and_merge".into(), "7".into())),
        "leg 2: review the pull request it just opened"
    );
    assert_eq!(
        why_stopped(&book, &after_sync, &[]),
        None,
        "nothing to explain"
    );

    // The review merged it. Now a release is due and no pull request is
    // open, so the third leg needs a flow that declares it.
    let mut book = book;
    book.flows.push(release_flow());
    let merged = repo(0, 0, None, true);
    assert_eq!(
        next_flow(&book, &merged, &[]),
        Some(("release".into(), String::new())),
        "leg 3: release"
    );

    // And then it is genuinely over.
    assert_eq!(next_flow(&book, &repo(0, 0, None, false), &[]), None);
}

#[test]
fn a_repository_with_nothing_to_do_ends_the_chain() {
    let idle = repo(0, 0, None, false);
    assert_eq!(next_flow(&FlowBook::defaults(), &idle, &[]), None);
}

#[test]
fn a_release_no_flow_declares_says_which_box_to_tick() {
    // The reported case, read straight off the user's own flows.toml: a
    // flow called "Release" exists, runs a script, and has `handles = []`
    // because duplicating a flow copies its nodes and not its purpose. The
    // chain ended there in total silence, which is indistinguishable from
    // the app being broken.
    let waiting = repo(0, 0, None, true);
    let book = FlowBook::defaults();
    assert_eq!(next_flow(&book, &waiting, &[]), None, "nothing answers it");

    let why = why_stopped(&book, &waiting, &[]).expect("and it must say so");
    assert!(why.contains("release due"), "{why}");
    assert!(why.contains(Need::Release.label()), "{why}");
    assert!(why.contains("Setup"), "names where the fix is: {why}");
}

#[test]
fn a_repository_with_nothing_left_is_not_nagged() {
    let idle = repo(0, 0, None, false);
    assert_eq!(why_stopped(&FlowBook::defaults(), &idle, &[]), None);
}

#[test]
fn a_need_a_flow_does_answer_produces_no_complaint() {
    let mut book = FlowBook::defaults();
    book.flows.push(release_flow());
    assert_eq!(why_stopped(&book, &repo(0, 0, None, true), &[]), None);
    assert_eq!(why_stopped(&book, &repo(3, 0, None, false), &[]), None);
}

#[test]
fn an_unfinished_rebase_is_explained_rather_than_blamed_on_the_flows() {
    // No flow answers `Resolve` on purpose, so the "tick a box in Setup"
    // message would send someone hunting for a setting that should not
    // exist.
    let mut stuck = repo(3, 0, None, false);
    stuck.in_progress = Some(crate::services::git::InProgress::Rebase);
    let why = why_stopped(&FlowBook::defaults(), &stuck, &[]).expect("still worth saying");
    assert!(why.contains("Preflight"), "{why}");
    assert!(
        !why.contains("Setup"),
        "not a flow-declaration problem: {why}"
    );
}

#[test]
fn a_need_no_flow_answers_ends_the_chain() {
    // A release is due but no flow says it handles one: there is nothing
    // for a trusted run to start, and inventing one would be worse.
    let waiting = repo(0, 0, None, true);
    assert_eq!(next_flow(&FlowBook::defaults(), &waiting, &[]), None);
}

#[test]
fn an_unfinished_rebase_ends_the_chain_even_though_it_needs_a_person() {
    // `Resolve` is deliberately answered by no flow — it is settled in
    // preflight — so a trusted run must stop rather than pick something.
    let mut stuck = repo(3, 0, None, false);
    stuck.in_progress = Some(crate::services::git::InProgress::Rebase);
    assert!(stuck.wants().needs_a_person());
    assert_eq!(next_flow(&FlowBook::defaults(), &stuck, &[]), None);
}

#[test]
fn a_failed_or_rejected_run_stops_the_chain() {
    let graph = FlowBook::defaults()
        .get("commit_and_pr")
        .unwrap()
        .to_graph();

    let mut done = RunState::fresh(&graph);
    for node in &graph.nodes {
        done.set_status(&node.id, NodeStatus::Done);
    }
    assert!(may_continue(&done, &graph));

    let mut failed = done.clone();
    failed.set_status("push", NodeStatus::Failed);
    assert!(!may_continue(&failed, &graph));

    let mut rejected = done.clone();
    rejected.set_status("commit", NodeStatus::Rejected);
    assert!(!may_continue(&rejected, &graph));
}

#[test]
fn skipping_the_analysis_does_not_buy_a_trusted_merge() {
    // Skip is a deliberate button, and pressing it says "I will judge this
    // myself" — not "merge it for me with nothing having looked".
    let mut skipped = state("looks_safe", "0", "passing");
    skipped.artifacts.remove("verdict");
    skipped.artifacts.remove("finding_count");
    let held = decide(&node(Step::Merge), &skipped);
    assert!(held.reason().unwrap().contains("Nothing analysed"));
}

#[test]
fn a_bypassed_step_does_not_stop_the_chain() {
    // Unlike a failure: the person chose to carry on without it, so the
    // trusted run carrying on is doing what they said.
    let graph = FlowBook::defaults()
        .get("commit_and_pr")
        .unwrap()
        .to_graph();
    let mut state = RunState::fresh(&graph);
    for node in &graph.nodes {
        state.set_status(&node.id, NodeStatus::Done);
    }
    state.set_status("test", NodeStatus::Bypassed);
    assert!(may_continue(&state, &graph));
}

#[test]
fn a_step_with_nothing_to_do_is_not_trouble() {
    // `scan` finding an empty tree skips, blocking everything behind it.
    // That is a flow reporting no work, not a flow going wrong.
    let graph = FlowBook::defaults()
        .get("commit_and_pr")
        .unwrap()
        .to_graph();
    let mut state = RunState::fresh(&graph);
    state.set_status("preflight", NodeStatus::Done);
    state.set_status("scan", NodeStatus::Skipped);
    state.propagate_block(&graph);
    assert!(may_continue(&state, &graph));
}

#[test]
fn ordinary_work_is_clicked_through() {
    for step in [Step::Commit, Step::Push, Step::OpenPr, Step::ScanChanges] {
        let clean = state("looks_safe", "0", "passing");
        assert_eq!(decide(&node(step), &clean), Verdict::Approve);
    }
}

#[test]
fn a_clean_merge_is_clicked_through() {
    let clean = state("looks_safe", "0", "passing");
    assert_eq!(decide(&node(Step::Merge), &clean), Verdict::Approve);
}

#[test]
fn a_second_opinion_with_findings_stops_the_merge_too() {
    let mut s = state("looks_safe", "0", "passing");
    assert_eq!(decide(&node(Step::Merge), &s), Verdict::Approve);
    s.artifacts
        .insert("second_verdict".into(), "worth_a_look".into());
    s.artifacts
        .insert("second_finding_count".into(), "2".into());
    s.artifacts
        .insert("second_model".into(), "gpt-4o-mini".into());
    let held = decide(&node(Step::Merge), &s);
    assert!(held.reason().unwrap().contains("gpt-4o-mini"));
}

#[test]
fn a_focused_review_with_findings_stops_the_merge() {
    let mut s = state("looks_safe", "0", "passing");
    s.artifacts
        .insert("security_verdict".into(), "worth_a_look".into());
    s.artifacts
        .insert("security_finding_count".into(), "1".into());
    let held = decide(&node(Step::Merge), &s);
    assert!(held
        .reason()
        .unwrap()
        .contains("security review found 1 problem"));
}

#[test]
fn no_second_opinion_is_no_reason_to_stop() {
    let s = state("looks_safe", "0", "passing");
    assert_eq!(decide(&node(Step::Merge), &s), Verdict::Approve);
}

#[test]
fn a_risky_verdict_stops_the_merge() {
    let risky = state("risky", "0", "passing");
    let held = decide(&node(Step::Merge), &risky);
    assert!(held.reason().unwrap().contains("risky"));
}

#[test]
fn a_single_finding_stops_the_merge_even_when_ci_is_green() {
    // The whole point: green CI is not the same as nobody having spotted
    // anything, and the analysis is the half that reads the diff.
    let found = state("worth_a_look", "1", "passing");
    let held = decide(&node(Step::Merge), &found);
    let reason = held.reason().unwrap();
    assert!(reason.contains("1 possible regression"), "{reason}");
    assert!(!reason.contains("regressions"), "singular: {reason}");
}

#[test]
fn several_findings_are_counted_in_the_plural() {
    let found = state("worth_a_look", "3", "passing");
    let reason = decide(&node(Step::Merge), &found)
        .reason()
        .unwrap()
        .to_string();
    assert!(reason.contains("3 possible regressions"), "{reason}");
    assert!(reason.contains("read them"), "{reason}");
}

#[test]
fn red_or_still_running_ci_stops_the_merge() {
    for checks in ["failing", "pending"] {
        let s = state("looks_safe", "0", checks);
        assert!(
            decide(&node(Step::Merge), &s).reason().is_some(),
            "{checks} must not merge on its own"
        );
    }
}

#[test]
fn a_merge_with_nothing_vouching_for_it_stops() {
    // Azure reports no checks, and a flow can simply not have a CI step.
    // Neither is evidence of safety.
    let unknown = state("looks_safe", "0", "unknown");
    assert!(decide(&node(Step::Merge), &unknown)
        .reason()
        .unwrap()
        .contains("checked CI"));

    let absent = RunState::fresh(&Graph { nodes: vec![] });
    assert!(decide(&node(Step::Merge), &absent).reason().is_some());
}

#[test]
fn the_shipped_review_flow_produces_everything_the_merge_rules_read() {
    // The rules are only as good as the artifacts reaching them: a review
    // flow that stopped writing `finding_count` would silently downgrade
    // "no regressions found" to "nobody looked", and this says so here
    // rather than at a merge.
    let review = crate::services::flowdef::FlowBook::defaults()
        .get("review_and_merge")
        .expect("the shipped book always contains review_and_merge")
        .to_graph();

    let written: Vec<&str> = review
        .nodes
        .iter()
        .flat_map(|n| n.writes.iter().map(|w| w.as_str()))
        .collect();
    for key in ["verdict", "finding_count", "checks_state"] {
        assert!(written.contains(&key), "the flow must produce `{key}`");
    }
}

#[test]
fn an_unparsable_finding_count_is_not_read_as_zero_findings() {
    // Defaulting a broken count to 0 would be the one direction that
    // silently merges; the CI rule behind it still has to agree.
    let s = state("looks_safe", "not a number", "failing");
    assert!(decide(&node(Step::Merge), &s).reason().is_some());
}
