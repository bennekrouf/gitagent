use super::*;
use crate::services::flowdef::FlowBook;

fn commit() -> Graph {
    FlowBook::defaults()
        .get("commit_and_pr")
        .unwrap()
        .to_graph()
}

#[test]
fn a_track_stays_empty_until_its_start_is_done() {
    assert_eq!(fill(false, Some(NodeStatus::Running), None), None);
    assert_eq!(
        fill(true, Some(NodeStatus::Running), None),
        Some(Fill::Moving)
    );
}

#[test]
fn a_track_turns_green_when_both_ends_are_done() {
    assert_eq!(
        fill(true, Some(NodeStatus::Done), None),
        Some(Fill::Arrived)
    );
    assert_eq!(fill(true, None, None), Some(Fill::Arrived));
}

#[test]
fn a_rejection_is_drawn_as_sent_back() {
    assert_eq!(
        fill(true, Some(NodeStatus::Rejected), None),
        Some(Fill::SentBack)
    );
}

#[test]
fn a_track_waiting_short_of_its_step_finishes_from_where_it_stopped() {
    assert_eq!(Phase::of(Fill::Moving, false), Phase::Approach);
    assert_eq!(Phase::of(Fill::Arrived, true), Phase::Finish);
    // Opening the view on a run already past this point draws it whole.
    assert_eq!(Phase::of(Fill::Arrived, false), Phase::Draw);
}

#[test]
fn the_head_runs_into_the_station_only_when_the_step_is_done() {
    assert_eq!(
        Phase::Finish.head(Fill::Arrived),
        Some("run-dot run-dot-finish")
    );
    assert_eq!(
        Phase::Finish.head(Fill::SentBack),
        Some("run-dot run-dot-stop")
    );
    assert_eq!(Phase::Draw.head(Fill::Failed), None);
    assert!(Phase::Approach.head(Fill::Moving).is_some());
}

#[test]
fn a_long_branch_name_breaks_at_its_slashes_and_dashes_rather_than_being_cut() {
    let lines = wrap("on feat/run-view-branches", 20, 3);
    assert_eq!(lines, vec!["on feat/run-view-", "branches"]);
    // With nowhere to break, it is cut at the width — still all there.
    assert_eq!(pieces("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    // Short words are left alone.
    assert_eq!(pieces("main", 20), vec!["main"]);
}

#[test]
fn a_track_into_a_step_you_skipped_runs_on_blue_to_the_next_one() {
    // The run is under way past it: through, not idle, and drawn whole.
    let f = fill(true, Some(NodeStatus::Bypassed), Some(NodeStatus::Running)).unwrap();
    assert_eq!(f, Fill::Through);
    assert_eq!(f.css(), "run-fill run-fill-moving");
    assert_eq!(Phase::of(f, false), Phase::Draw, "no stopping short of it");
    assert!(Phase::Draw.head(f).is_some(), "the dot runs through");
    // Done beyond it: green. Nothing beyond: the end, reached.
    assert_eq!(
        fill(true, Some(NodeStatus::Bypassed), Some(NodeStatus::Done)),
        Some(Fill::Arrived)
    );
    assert_eq!(
        fill(true, Some(NodeStatus::Bypassed), None),
        Some(Fill::Arrived)
    );
}

#[test]
fn beyond_a_skipped_step_is_the_next_one_not_skipped() {
    let edges = vec![
        ("a".to_string(), "b".to_string()),
        ("b".to_string(), "c".to_string()),
        ("c".to_string(), "d".to_string()),
    ];
    let status = |id: &str| match id {
        "b" | "c" => NodeStatus::Bypassed,
        "d" => NodeStatus::Running,
        _ => NodeStatus::Done,
    };
    // b and c were both skipped: past b, the run is at d.
    assert_eq!(
        beyond_skipped("b", &edges, &status),
        Some(NodeStatus::Running)
    );
    assert_eq!(beyond_skipped("d", &edges, &status), None);
}

#[test]
fn a_bypassed_step_still_lets_the_run_through() {
    assert!(departed(NodeStatus::Bypassed));
    assert!(!departed(NodeStatus::Skipped));
}

#[test]
fn the_line_runs_left_to_right() {
    let graph = commit();
    let plan = map(&graph, None, false, false, 0.0, false);
    for (from, to) in &plan.edges {
        assert!(
            plan.find(from).unwrap().x < plan.find(to).unwrap().x,
            "{from} -> {to}"
        );
    }
}

#[test]
fn steps_that_run_side_by_side_share_a_column() {
    // push and draft_pr both leave commit, and neither waits for the
    // other.
    let plan = map(&commit(), None, false, false, 0.0, false);
    let a = plan.find("push").unwrap();
    let b = plan.find("draft_pr").unwrap();
    assert_eq!(a.x, b.x);
    assert!((a.y - b.y).abs() >= ROW_MIN);
}

#[test]
fn every_station_fits_on_the_canvas() {
    for area in [None, Some((600.0, 300.0)), Some((2400.0, 1200.0))] {
        let plan = map(&commit(), area, false, false, 0.0, false);
        for s in &plan.stations {
            assert!(s.x > plan.start_x && s.x < plan.end_x);
            assert!(plan.end_x < plan.width);
            assert!(s.y - R >= 0.0 && s.y + R <= plan.height);
        }
    }
}

#[test]
fn the_map_spreads_to_fill_a_bigger_window() {
    let small = map(&commit(), None, false, false, 0.0, false);
    let big = map(&commit(), Some((2400.0, 1000.0)), false, false, 0.0, false);
    assert!(big.col_w > small.col_w);
    assert_eq!((big.width, big.height), (2400.0, 1000.0));
    let gap = |m: &Map| (m.find("push").unwrap().y - m.find("draft_pr").unwrap().y).abs();
    assert!(gap(&big) > gap(&small));
}

#[test]
fn a_window_too_small_scrolls_rather_than_squeezing() {
    let plan = map(&commit(), Some((300.0, 200.0)), false, false, 0.0, false);
    assert_eq!(plan.col_w, COL_MIN);
    assert!(plan.width > 300.0);
}

#[test]
fn a_long_label_wraps_between_words_and_ends_in_an_ellipsis() {
    assert_eq!(
        wrap("Draft commit message", 14, 2),
        vec!["Draft commit", "message"]
    );
    let lines = wrap("Remote, forge, credentials, and everything else", 16, 2);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].ends_with('\u{2026}'));
    assert!(lines.iter().all(|l| l.chars().count() <= 16));
}

#[test]
fn a_step_following_only_the_upper_parent_sits_above_one_following_both() {
    // The shape the commit flow shipped with before draft_pr waited for
    // the commit, and one a flow of your own can still have: draft_pr
    // follows draft_commit alone; commit follows draft_commit and test.
    // Stacking commit on top crosses their tracks for no reason.
    let mut graph = commit();
    for n in graph.nodes.iter_mut().filter(|n| n.id == "draft_pr") {
        n.deps = vec!["draft_commit".into()];
    }
    let plan = map(&graph, None, false, false, 0.0, false);
    assert!(plan.find("draft_pr").unwrap().y < plan.find("commit").unwrap().y);
}

#[test]
fn a_revise_loop_rises_above_both_of_its_stations() {
    let l = revise_loop((500.0, 300.0), (300.0, 300.0), None);
    assert!(l.peak.1 < 300.0 - R - LOOP_MIN + 1.0);
    assert!(l.peak.0 > 300.0 && l.peak.0 < 500.0);
    assert!(l.end.0 < 500.0, "it lands back at the earlier station");
}

#[test]
fn a_revise_loop_stays_clear_of_the_labels_above_it() {
    let free = revise_loop((500.0, 400.0), (300.0, 400.0), None);
    let tight = revise_loop((500.0, 400.0), (300.0, 400.0), Some(200.0));
    assert!(tight.peak.1 >= free.peak.1);
}

#[test]
fn the_loop_label_counts_rounds() {
    assert_eq!(loop_label(NodeStatus::Rejected, 1), "sent back");
    assert_eq!(
        loop_label(NodeStatus::Rejected, 2),
        "sent back \u{00b7} round 2"
    );
    assert_eq!(loop_label(NodeStatus::AwaitingApproval, 2), "round 3");
    assert_eq!(loop_label(NodeStatus::Done, 2), "approved in round 3");
}

#[test]
fn a_replay_steps_through_the_history_then_goes_live() {
    assert_eq!(advance(Some((7, 0)), 3), Some((7, 1)));
    assert_eq!(advance(Some((7, 1)), 3), Some((7, 2)));
    assert_eq!(advance(Some((7, 2)), 3), None, "caught up");
    assert_eq!(advance(None, 3), None, "live stays live");
}

fn brief(number: &str, url: &str, checks: Checks) -> PrBrief {
    PrBrief {
        number: number.into(),
        title: "t".into(),
        url: url.into(),
        checks,
        files: 1,
        additions: 1,
        deletions: 0,
        commits: 1,
        base: String::new(),
        head: String::new(),
        failed: vec![],
    }
}

#[test]
fn a_flow_with_no_pull_request_has_no_gates() {
    let mut graph = commit();
    graph
        .nodes
        .retain(|n| n.id != "open_pr" && n.id != "draft_pr");
    assert_eq!(
        Gates::for_run(&graph, &RunState::default(), "", &[], None),
        None
    );
}

#[test]
fn before_a_pull_request_exists_neither_gate_is_ticked() {
    let g = Gates::for_run(&commit(), &RunState::default(), "", &[], None).unwrap();
    assert_eq!(
        g,
        Gates {
            pr: None,
            checks: Checks::Unknown
        }
    );
}

#[test]
fn the_pull_request_the_run_opened_is_the_one_whose_checks_count() {
    let mut state = RunState::default();
    state
        .artifacts
        .insert("pr_url".into(), "https://x/pull/7".into());
    let prs = [
        brief("5", "https://x/pull/5", Checks::Failing),
        brief("7", "https://x/pull/7", Checks::Passing),
    ];
    let g = Gates::for_run(&commit(), &state, "", &prs, None).unwrap();
    assert_eq!(
        g,
        Gates {
            pr: Some("#7".into()),
            checks: Checks::Passing
        }
    );
}

#[test]
fn a_pull_request_just_opened_is_ready_before_its_checks_are_known() {
    let mut state = RunState::default();
    state
        .artifacts
        .insert("pr_url".into(), "https://x/pull/12".into());
    let g = Gates::for_run(&commit(), &state, "", &[], None).unwrap();
    assert_eq!(
        g,
        Gates {
            pr: Some("#12".into()),
            checks: Checks::Unknown
        }
    );
}

#[test]
fn a_picked_review_names_its_pull_request() {
    let prs = [brief("3", "u3", Checks::Pending)];
    let g = Gates::for_run(&commit(), &RunState::default(), "3", &prs, None).unwrap();
    assert_eq!(
        g,
        Gates {
            pr: Some("#3".into()),
            checks: Checks::Pending
        }
    );
}

fn review() -> Graph {
    FlowBook::defaults()
        .get("review_and_merge")
        .unwrap()
        .to_graph()
}

#[test]
fn a_flow_that_reviews_nothing_has_no_review_circle() {
    assert_eq!(
        review_hub(&commit(), &RunState::default(), &Lenses::default()),
        None
    );
}

#[test]
fn the_review_circle_has_you_the_model_and_ci() {
    let hub = review_hub(&review(), &RunState::default(), &Lenses::default()).unwrap();
    let names: Vec<&str> = hub.spokes.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "You",
            "Model",
            "Second model",
            "Alignment",
            "Security",
            "Architecture",
            "CI"
        ]
    );
    assert_eq!(hub.tone, Tone::Idle);
}

#[test]
fn each_reviewer_reports_what_its_step_found() {
    let g = review();
    let mut s = RunState::fresh(&g);
    s.started = true;
    s.set_status("pr_diff", NodeStatus::Done);
    s.set_status("analyse", NodeStatus::Done);
    s.artifacts.insert("verdict".into(), "worth_a_look".into());
    s.artifacts.insert("finding_count".into(), "1".into());
    s.set_status("pr_status", NodeStatus::Done);
    s.artifacts.insert("checks_state".into(), "failing".into());
    let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
    let note = |n: &str| {
        hub.spokes
            .iter()
            .find(|s| s.name == n)
            .unwrap()
            .note
            .clone()
    };
    assert_eq!(note("You"), "diff read");
    assert_eq!(note("Model"), "worth a look \u{00b7} 1 finding");
    assert_eq!(note("CI"), "checks failing");
    assert_eq!(
        hub.tone,
        Tone::Bad,
        "the circle takes its most urgent reviewer"
    );
}

#[test]
fn the_second_opinion_is_named_after_its_model() {
    let g = review();
    let mut s = RunState::fresh(&g);
    s.started = true;
    s.set_status("second_opinion", NodeStatus::Done);
    s.artifacts
        .insert("second_model".into(), "gpt-4o-mini".into());
    s.artifacts
        .insert("second_verdict".into(), "looks_safe".into());
    let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
    let second = hub.spokes.iter().find(|s| s.name == "gpt-4o-mini").unwrap();
    assert_eq!(second.note, "second opinion: looks safe");
    assert_eq!(second.tone, Tone::Good);
}

#[test]
fn a_second_opinion_nobody_chose_says_so_and_alarms_nobody() {
    let g = review();
    let mut s = RunState::fresh(&g);
    s.started = true;
    s.set_status("second_opinion", NodeStatus::Bypassed);
    let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
    let second = hub
        .spokes
        .iter()
        .find(|s| s.name == "Second model")
        .unwrap();
    assert_eq!(second.note, "none chosen in Settings");
    assert_eq!(second.tone, Tone::Idle);
}

#[test]
fn a_focused_review_that_is_off_says_where_to_turn_it_on() {
    let hub = review_hub(&review(), &RunState::default(), &Lenses::default()).unwrap();
    let security = hub.spokes.iter().find(|s| s.name == "Security").unwrap();
    assert_eq!(security.note, "off \u{2014} turn on in Settings");
    assert_eq!(security.tone, Tone::Idle);
}

#[test]
fn each_focused_review_that_ran_reports_its_own_verdict() {
    let g = review();
    let mut s = RunState::fresh(&g);
    s.started = true;
    s.set_status("lenses", NodeStatus::Done);
    s.artifacts
        .insert("lenses_run".into(), "security,architecture".into());
    s.artifacts
        .insert("security_verdict".into(), "risky".into());
    s.artifacts
        .insert("security_finding_count".into(), "2".into());
    s.artifacts
        .insert("architecture_verdict".into(), "looks_safe".into());
    let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
    let note = |n: &str| {
        hub.spokes
            .iter()
            .find(|s| s.name == n)
            .unwrap()
            .note
            .clone()
    };
    assert_eq!(note("Security"), "risky \u{00b7} 2 findings");
    assert_eq!(note("Architecture"), "looks safe");
    assert_eq!(note("Alignment"), "off \u{2014} turn on in Settings");
}

#[test]
fn a_review_sent_back_grows_a_fix_spoke() {
    let g = review();
    let mut s = RunState::fresh(&g);
    s.started = true;
    s.reject("merge", &g);
    let hub = review_hub(&g, &s, &Lenses::default()).unwrap();
    let fix = hub.spokes.last().unwrap();
    assert_eq!(
        (fix.name.as_str(), fix.note.as_str(), fix.tone),
        ("Fix", "then review again", Tone::Back)
    );
    s.retry_from("merge", &g);
    let after = review_hub(&g, &s, &Lenses::default()).unwrap();
    assert_eq!(after.spokes.last().unwrap().note, "fixed, reviewed again");
}

#[test]
fn spokes_go_down_the_left_then_the_right_never_straight_up() {
    let angles: Vec<f64> = (0..7).map(|i| spoke_angle(i, 7)).collect();
    for (i, a) in angles.iter().enumerate() {
        let a = a.to_radians();
        assert!(a.sin().abs() < 0.97, "spoke {i} points too near vertical");
        assert_eq!(a.cos() < 0.0, i < 4, "spoke {i} on the wrong side");
    }
    // Down each side in order, so labels stack top to bottom.
    let y = |i: usize| spoke_angle(i, 7).to_radians().sin();
    assert!(y(0) < y(1) && y(1) < y(2) && y(2) < y(3));
    assert!(y(4) < y(5) && y(5) < y(6));
}

#[test]
fn the_merge_checks_need_room_above_only_on_a_one_row_line() {
    let mut line = commit();
    line.nodes.retain(|n| n.id != "test" && n.id != "draft_pr");
    for n in &mut line.nodes {
        n.deps.retain(|d| d != "test" && d != "draft_pr");
    }
    let one_row = map(&line, None, false, true, 0.0, false).height
        - map(&line, None, false, false, 0.0, false).height;
    assert_eq!(one_row, LOOP_ROOM);
    let forked = map(&commit(), None, false, true, 0.0, false).height
        - map(&commit(), None, false, false, 0.0, false).height;
    assert_eq!(forked, 0.0);
}

#[test]
fn the_review_circle_gets_room_under_the_map() {
    let plan = map(&review(), None, false, false, 0.0, true);
    let lowest = plan.stations.iter().map(|s| s.y).fold(f64::MIN, f64::max);
    assert!(plan.height - lowest >= PAD_BOTTOM + HUB_ROOM);
}

#[test]
fn the_zoom_buttons_move_in_tenths_within_bounds() {
    assert_eq!(zoom_step(1.0, true), 1.1);
    assert_eq!(zoom_step(1.0, false), 0.9);
    // A level reached by scrolling snaps back to round numbers.
    assert_eq!(zoom_step(1.04, true), 1.1);
    assert_eq!(zoom_step(ZOOM_MAX, true), ZOOM_MAX);
    assert_eq!(zoom_step(ZOOM_MIN, false), ZOOM_MIN);
}

#[test]
fn the_run_is_followed_to_the_step_it_is_at() {
    let graph = FlowBook::defaults()
        .get("commit_and_pr")
        .unwrap()
        .to_graph();
    let mut s = RunState::fresh(&graph);
    s.started = true;
    assert_eq!(progress_focus(&s, s.history.len()), None);
    s.set_status("preflight", NodeStatus::Running);
    s.set_status("preflight", NodeStatus::Done);
    s.set_status("scan", NodeStatus::Running);
    assert_eq!(progress_focus(&s, s.history.len()).as_deref(), Some("scan"));
    s.set_status("scan", NodeStatus::AwaitingApproval);
    assert_eq!(progress_focus(&s, s.history.len()).as_deref(), Some("scan"));
    // A replay one change in follows what was going on then: the view
    // hands it the run rewound to that point.
    assert_eq!(
        progress_focus(&s.replayed(1), 1).as_deref(),
        Some("preflight")
    );
    // Nothing in progress, nothing to follow.
    s.set_status("scan", NodeStatus::Done);
    assert_eq!(progress_focus(&s, s.history.len()), None);
}

#[test]
fn following_only_scrolls_a_map_wider_than_its_window() {
    let js = follow_js(420.0);
    assert!(js.contains("scrollWidth <= c.clientWidth"));
    assert!(js.contains("left: 420"));
}

#[test]
fn the_browser_zoom_keys_step_the_map_and_command_zero_resets_it() {
    assert_eq!(zoom_key(1.0, "in"), 1.1);
    assert_eq!(zoom_key(1.0, "out"), 0.9);
    assert_eq!(zoom_key(1.7, "reset"), 1.0);
    assert_eq!(zoom_key(ZOOM_MAX, "in"), ZOOM_MAX);
    assert_eq!(zoom_key(1.3, "anything else"), 1.3);
}

#[test]
fn zooming_holds_the_point_under_the_cursor_or_the_middle_of_the_view() {
    let at_cursor = zoom_anchor_js(Some((380.0, 260.0)));
    assert!(at_cursor.contains("380 - r.left") && at_cursor.contains("260 - r.top"));
    let at_middle = zoom_anchor_js(None);
    assert!(at_middle.contains("c.clientWidth / 2") && at_middle.contains("c.clientHeight / 2"));
    // It scrolls by how much the map really changed, measured after the
    // redraw, not by the zoom ratio it was asked for.
    assert!(at_cursor.contains("b1.width / b0.width"));
}

#[test]
fn scrolling_up_with_the_command_key_zooms_in_and_stays_in_bounds() {
    assert!(zoom_wheel(1.0, -100.0) > 1.0);
    assert!(zoom_wheel(1.0, 100.0) < 1.0);
    assert_eq!(zoom_wheel(1.9, -10_000.0), ZOOM_MAX);
    assert_eq!(zoom_wheel(0.6, 10_000.0), ZOOM_MIN);
}

#[test]
fn zoomed_out_the_same_flow_is_laid_out_for_more_room() {
    // The map is laid out for the room it would have at the zoom, so at
    // half size it spreads over twice the room, and drawn at half size
    // fills the screen it is on.
    let screen = (1000.0, 600.0);
    let half = map(
        &commit(),
        Some((screen.0 / 0.5, screen.1 / 0.5)),
        false,
        false,
        0.0,
        false,
    );
    assert_eq!(half.width * 0.5, screen.0);
}

#[test]
fn a_step_under_the_panel_slides_out_just_far_enough() {
    let area = 1200.0;
    let visible = area - panel_width(area, false, None);
    let x = 1000.0;
    let slide = slide_for(x, 200.0, area, false, None);
    assert!(slide > 0.0);
    // Its labels end right at the panel's edge.
    assert_eq!(x - slide + 100.0 + 16.0, visible);
}

#[test]
fn a_step_already_clear_of_the_panel_does_not_move_the_map() {
    assert_eq!(slide_for(150.0, 200.0, 1200.0, false, None), 0.0);
}

#[test]
fn a_folded_panel_gives_the_map_almost_all_its_room_back() {
    let open = slide_for(1000.0, 200.0, 1200.0, false, None);
    let folded = slide_for(1000.0, 200.0, 1200.0, true, None);
    assert!(folded < open);
    // A step clear of the strip does not move at all.
    assert_eq!(slide_for(900.0, 200.0, 1200.0, true, None), 0.0);
}

#[test]
fn the_panel_never_takes_more_than_its_cap() {
    assert_eq!(panel_width(3000.0, false, None), 620.0);
    assert_eq!(panel_width(1000.0, false, None), 580.0);
    assert_eq!(panel_width(1000.0, true, None), FOLDED_PANEL);
}

#[test]
fn a_dragged_panel_can_cover_the_view_but_not_shrink_away() {
    // As wide as the whole view, and no wider.
    assert_eq!(panel_width(1200.0, false, Some(5000.0)), 1200.0);
    assert_eq!(panel_width(1200.0, false, Some(800.0)), 800.0);
    // Never below the minimum, unless the view itself is narrower.
    assert_eq!(panel_width(1200.0, false, Some(10.0)), store::PANEL_MIN);
    assert_eq!(panel_width(300.0, false, Some(10.0)), 300.0);
    // Folded is folded, whatever width it was dragged to.
    assert_eq!(panel_width(1200.0, true, Some(800.0)), FOLDED_PANEL);
}

#[test]
fn the_line_starts_at_the_roots_and_ends_at_the_leaves() {
    let plan = map(&commit(), None, false, false, 0.0, false);
    assert_eq!(plan.roots, vec!["preflight".to_string()]);
    assert!(plan.leaves.contains(&"open_pr".to_string()));
}
