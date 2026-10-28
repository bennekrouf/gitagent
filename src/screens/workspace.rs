//! The workspace screen: repositories on the left, the selected flow in the
//! middle, the selected node on the right.
//!
//! Run state is keyed by `(repository, flow, pull request)`. That is what
//! lets you leave one repository parked at an approval, look at another, come
//! back and find it where it was; what lets a repository hold a half-finished
//! commit flow and a review flow at the same time without them treading on
//! each other; and what lets two different pull requests on the same
//! repository each sit mid-review independently, rather than the second
//! overwriting the first's progress. The PR slot is empty for anything that
//! isn't PR-scoped review.

use dioxus::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

use crate::components::branches_panel::BranchesPanel;
use crate::components::detail_pane::DetailPane;
use crate::components::diff_view::DiffView;
use crate::components::forge_icon::ForgeIcon;
use crate::components::licence_panel::LicencePanel;
use crate::components::mcp_panel::McpPanel;
use crate::components::node_card::NodeCard;
use crate::components::pr_card::PrCard;
use crate::components::repo_sidebar::{phase_of, Approval, Phase, RepoEntry, RepoSidebar};
use crate::components::run_view::{Gates, RunView};
use crate::components::settings_panel::SettingsPanel;
use crate::screens::setup::Setup;
use crate::services::awake;
use crate::services::flow;
use crate::services::flowdef::{self, FlowBook};
use crate::services::graph::{
    live_subtitle, Graph, NodeRun, NodeSpec, NodeStatus, Remedy, RunState, Step,
};
use crate::services::licence;
use crate::services::llm::LlmConfig;
use crate::services::notify;
use crate::services::probe::{self, Need, RepoStatus, Wants};
use crate::services::release;
use crate::services::review;
use crate::services::runner;
use crate::services::store::Layout;
use crate::services::trusted;
use crate::services::{forge, git, store};
use crate::telemetry;
mod run;
use run::*;
mod detail;
use detail::*;
mod actions;
mod context;
mod dialogs;
mod flows;
mod middle;
mod sidebar;
mod strip;
mod topbar;
use flows::*;

/// One run per repository, per flow, per pull request — the third slot is
/// empty for anything that isn't PR-scoped review. That is what lets you
/// review PR #7 and PR #5 on the same repository at once without one
/// overwriting the other's progress, the same way two different repositories
/// already don't tread on each other.
type Key = (String, String, String);
type States = BTreeMap<Key, RunState>;
/// Each pull request's diff by (repository, number): `None` while it loads.
type PrDiffs = BTreeMap<(String, String), Option<Result<(String, String), String>>>;

#[derive(Props, Clone, PartialEq)]
pub struct WorkspaceProps {
    pub workspace: String,
    pub llm_config: Signal<LlmConfig>,
    pub is_light: Signal<bool>,
    pub theme_overridden: Signal<bool>,
    pub on_change_workspace: EventHandler<()>,
}

#[component]
pub fn Workspace(props: WorkspaceProps) -> Element {
    let workspace = props.workspace.clone();
    let repos = use_signal(|| store::discover_repos(&workspace));
    let statuses = use_signal(BTreeMap::<String, RepoStatus>::new);
    let probing = use_signal(|| 0usize);
    // Auto-selection happens once, on the first probe: after that the choice is
    // yours and a refresh must not move it.
    let picked = use_signal(|| false);
    let states = use_signal(States::new);
    let selected_repo = use_signal(|| Option::<String>::None);
    // Loaded once per mount, so returning from Setup picks up any edits.
    let mut book = use_signal(FlowBook::load);
    // A runnable flow by preference — never open on a broken one while a
    // working one exists. But if every flow is broken, select the first anyway:
    // an empty column explains nothing, whereas the selected tab's banner says
    // exactly what to fix.
    //
    // Repo-blind on purpose, unlike every other flow choice in this file: no
    // repository is selected yet, so there is nothing for "hidden here" to be
    // relative to. Picking one replaces this via `default_selection`.
    let first_flow = {
        let book = book.read();
        book.runnable()
            .first()
            .or(book.flows.first().as_ref())
            .map(|f| f.id.clone())
            .unwrap_or_default()
    };
    let mut selected_flow = use_signal(|| first_flow);
    // Which open PR a review run is scoped to. Empty means "whatever the
    // checked-out branch has open" — the same default behaviour as before
    // this existed. Set by picking a specific PR from the sidebar's list.
    let selected_pr = use_signal(String::new);
    let mut selected_node = use_signal(String::new);
    let running = use_signal(BTreeSet::<Key>::new);
    // A fix being run for "couldn't list pull requests", by repository:
    // whether it is still running, and what it printed.
    let pr_fix = use_signal(BTreeMap::<String, (bool, String)>::new);
    // Runs whose approvals are being clicked through for us. A key is in here
    // only while that is wanted: taking it out mid-run hands the next approval
    // straight back to the person, without disturbing the run itself.
    let trusted = use_signal(BTreeSet::<Key>::new);
    // Every repository, every flow, no asking. Set from the "Trust all" button
    // in the top bar rather than per repository — for someone who wants
    // GitAgent to just run, not for the default. Turning it on also trusts
    // whatever is already running, the same way adopting a single run does;
    // turning it off does not untrust anything already in flight, so a run
    // that is mid-chain still finishes the leg it is on before the next
    // approval asks a person again.
    // Remembered, and shared with the MCP server, which reads it to answer a
    // run's approvals the way this window would.
    let global_trust = use_signal(store::trust_all);
    // Why the last trusted run stopped, when the reason was not "there is
    // nothing left". Cleared when the next one starts.
    let chain_note = use_signal(String::new);
    // The run map instead of the step list and detail pane: one switch for
    // the whole window, so it stays on while you move between repositories.
    // On by default — where a run is is the first thing worth seeing; the
    // list is one click away for approving and reading logs.
    let run_view = use_signal(|| true);
    // The step whose panel is open over the run map, if any.
    let mut drawer = use_signal(|| Option::<String>::None);
    // The step the panel last showed, kept after it closes so the panel can
    // slide out with its content still in it instead of going blank first.
    let mut panel_step = use_signal(|| Option::<String>::None);
    // Folded to a strip at the right edge: the map gets its room back and the
    // step stays one click away. Remembered — between openings and across
    // restarts — so a panel folded once stays folded for the next step, and a
    // step that comes to need you shows up on the strip rather than taking
    // the map back. Only the strip itself unfolds it.
    let mut panel_folded = use_signal(|| store::load_layout().panel_folded);
    let set_folded = move |folded: bool| {
        panel_folded.set(folded);
        store::save_layout(&Layout {
            panel_folded: folded,
            ..store::load_layout()
        });
    };
    // A pull request whose diff is open in the run view's panel, by number,
    // instead of a step. And the diffs fetched for it, by repository and
    // number: `None` while git is still fetching.
    let mut diff_panel = use_signal(|| Option::<String>::None);
    let pr_diffs = use_signal(PrDiffs::new);
    // A step approved from its panel, kept open on its log while it runs.
    let mut close_when_done = use_signal(|| Option::<(Key, String)>::None);
    use_effect(move || {
        let Some((key, step)) = close_when_done.read().clone() else {
            return;
        };
        // Closed by hand, or moved to another step or repository: the panel
        // is yours again.
        let shown: Key = (
            selected_repo.read().clone().unwrap_or_default(),
            selected_flow.read().clone(),
            selected_pr.read().clone(),
        );
        if shown != key || drawer.read().as_deref() != Some(step.as_str()) {
            close_when_done.set(None);
            return;
        }
        let status = states
            .read()
            .get(&key)
            .map(|state| state.status(&step))
            .unwrap_or_default();
        match status {
            NodeStatus::Done | NodeStatus::Skipped | NodeStatus::Bypassed => {
                close_when_done.set(None);
                drawer.set(None);
            }
            NodeStatus::Failed | NodeStatus::Rejected | NodeStatus::Blocked => {
                close_when_done.set(None);
            }
            NodeStatus::Pending | NodeStatus::Running | NodeStatus::AwaitingApproval => {}
        }
    });
    use_effect(move || {
        if let Some(step) = drawer.read().clone() {
            panel_step.set(Some(step));
            // A step taking the panel — clicked, or come to need you — takes
            // it from a diff being read.
            diff_panel.set(None);
        }
    });
    // Opens a pull request's diff in the run view's panel, fetching it with
    // git: the base and head from origin, compared here, as the review flow
    // does — so it costs none of the forge's API budget. A diff already
    // fetched shows at once while it is fetched again.
    let open_diff = move |repo: String, pr: probe::PrBrief| {
        actions::open_diff(drawer, diff_panel, pr_diffs, repo, pr)
    };
    let mut settings_open = use_signal(|| false);
    let mut setup_open = use_signal(|| false);
    let mut mcp_open = use_signal(|| false);
    // GitAgent Pro: the licence, and whether its window is open.
    let mut licence_status = use_signal(licence::current);
    let mut licence_open = use_signal(|| false);
    // The licence is asked about at start and then daily; between checks,
    // and offline, the last answer stands (for a week, for Pro).
    use_future(move || async move {
        loop {
            let wait = match licence::check().await {
                Ok(now) => {
                    if *licence_status.peek() != now {
                        licence_status.set(now);
                        crate::refresh_window_title();
                    }
                    licence::CHECK_EVERY
                }
                Err(_) => {
                    // The answer on disk may just have aged past the grace.
                    let now = licence::current();
                    if *licence_status.peek() != now {
                        licence_status.set(now);
                        crate::refresh_window_title();
                    }
                    licence::RETRY_EVERY
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
    // Which flows the *current* repository has chosen not to see — a filter
    // on top of the shared flow list, not a copy of it. `flows.toml` never
    // changes when a flow is hidden here.
    let mut repo_flows = use_signal(store::load_repo_flows);
    let confirm_hide = use_signal(|| Option::<(String, String)>::None);
    let branches_open = use_signal(|| Option::<String>::None);
    let branches_data =
        use_signal(|| Option::<Result<Vec<crate::services::branches::BranchInfo>, String>>::None);
    let repo_bases = use_signal(store::load_repo_bases);
    let branches_action_error = use_signal(|| Option::<String>::None);
    // Which repository and branch a delete, clean-up or create-PR is currently
    // running against, so the panel can show that row working instead of
    // looking like the click did nothing. It also holds the repository the
    // way a run does: no flow starts there until it is done, and the other
    // way round.
    let branches_busy = use_signal(|| Option::<(String, String)>::None);
    // While anything in this window is running, hold off App Nap, so a hidden
    // window keeps driving it instead of pausing until it is shown again. See
    // `services::awake`. Per window: closing one never releases another's.
    let mut awake = use_signal(|| Option::<awake::Activity>::None);
    use_effect(move || {
        let busy = !running.read().is_empty() || branches_busy.read().is_some();
        if busy != awake.peek().is_some() {
            awake.set(if busy { awake::Activity::begin() } else { None });
        }
    });
    let base_editor_open = use_signal(|| Option::<String>::None);
    let base_editor_value = use_signal(String::new);
    // Which repository's flow picker is open, rather than a single flag for
    // all of them: opening it on one repository used to leave it open on the
    // next one you selected, which reads as a panel that will not close.
    let picker_open = use_signal(|| Option::<String>::None);

    // A step that stops for you — to ask for approval, or because it failed —
    // opens its panel over the run map on its own, once each time: close it
    // while the step still waits and it stays closed. Not for a step a
    // trusted run is about to answer — the panel would only flash open — but
    // yes for one it held back for you, such as a merge the reviews were
    // unhappy about, even with Trust all on, and yes for any failure. Not
    // away from another step that needs you too, which would have the two
    // fight for it.
    let mut auto_opened = use_signal(|| Option::<(u64, String, usize)>::None);
    use_effect(move || {
        if !*run_view.read() {
            return;
        }
        let Some(repo) = selected_repo.read().clone() else {
            return;
        };
        let key: Key = (
            repo,
            selected_flow.read().clone(),
            selected_pr.read().clone(),
        );
        let states = states.read();
        let Some(state) = states.get(&key) else {
            return;
        };
        let Some((step, at)) = state.newest_for_a_person() else {
            return;
        };
        let this = (state.run, step.clone(), at);
        if auto_opened.peek().as_ref() == Some(&this) {
            return;
        }
        // Read, not peeked: a trusted run that holds the step takes itself out
        // of `trusted`, and that is the moment to open.
        if answers_itself(state, &step, trusted.read().contains(&key)) {
            return;
        }
        auto_opened.set(Some(this));
        let showing_another = drawer
            .peek()
            .as_ref()
            .is_some_and(|open| open != &step && needs_a_person(state.status(open)));
        if !showing_another {
            selected_node.set(step.clone());
            drawer.set(Some(step));
        }
    });

    // Escape closes a step's panel over the run map. Listened for on the
    // window, not the panel, because nothing in the panel has focus after
    // a click on the map. The handler is kept on `window` and replaced on
    // each mount, so a workspace opened twice never stacks two of them.
    use_future(move || async move {
        let mut keys = document::eval(
            "if (window._gaEscape) window.removeEventListener('keydown', window._gaEscape);\
             window._gaEscape = (e) => { if (e.key === 'Escape') dioxus.send(true); };\
             window.addEventListener('keydown', window._gaEscape);",
        );
        while keys.recv::<bool>().await.is_ok() {
            // A dialog in front owns the key: closing the panel hidden
            // behind it would be a change nobody could see happen.
            let dialog_open = *settings_open.peek()
                || *licence_open.peek()
                || branches_open.peek().is_some()
                || base_editor_open.peek().is_some()
                || confirm_hide.peek().is_some()
                || picker_open.peek().is_some();
            if !dialog_open {
                drawer.set(None);
                diff_panel.set(None);
            }
        }
    });

    // Hiding a flow is one operation whether it comes from a tab's × or from
    // the picker's checkbox, including the part that is easy to forget: the
    // graph column must not keep showing a tab the strip no longer does.
    let hide_flow = move |repo: &str, id: &str| {
        actions::hide_flow(
            selected_repo,
            book,
            selected_flow,
            selected_node,
            repo_flows,
            repo,
            id,
        )
    };

    // Pane widths, dragged by the dividers and remembered on disk.
    let saved = use_signal(store::load_layout);
    let mut sidebar_w = use_signal(|| saved.read().sidebar);
    let mut middle_w = use_signal(|| saved.read().middle);
    // The run view's step panel, as its edge was last dragged.
    let panel_w = use_signal(|| saved.read().panel);
    // 0 = not dragging, 1 = the sidebar edge, 2 = the flow-column edge.
    let mut dragging = use_signal(|| 0u8);
    let mut drag_from = use_signal(|| (0.0f64, 0.0f64));

    let is_light = props.is_light;
    let theme_overridden = props.theme_overridden;

    // Probe every repository at once rather than one after another: eight
    // repositories each needing a `gh` round-trip is seconds sequentially and
    // barely one concurrently. Results land as they arrive, so the list fills
    // in rather than appearing all at once.
    let opened = workspace.clone();
    use_coroutine(move |_rx: UnboundedReceiver<()>| {
        let opened = opened.clone();
        async move {
            refresh_all(
                &opened,
                repos,
                statuses,
                probing,
                picked,
                selected_repo,
                selected_flow,
                book,
                false,
            );
        }
    });

    // Runs GitAgent's MCP server is driving for a client such as Claude, read
    // from the file it writes after every step — see `services::live`. Read
    // when the file changes, and now and then regardless, so a run whose
    // server went away drops off.
    let mut live_runs = use_signal(crate::services::live::load);
    // The flows, too, can change from outside: the MCP server's Setup tools
    // write the same files Setup does. Picked up here so a flow Claude made
    // shows as a tab without reopening the window — but not while Setup is
    // open, whose draft would then be compared against a moved target.
    let flow_files_changed_at = || {
        let dir = store::data_dir();
        ["flows.toml", "repo_flows.json"].map(|f| {
            std::fs::metadata(dir.join(f))
                .and_then(|m| m.modified())
                .ok()
        })
    };
    use_future(move || async move {
        let mut stamp = crate::services::live::changed_at();
        let mut flows_stamp = flow_files_changed_at();
        let mut ticks = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            ticks = ticks.wrapping_add(1);
            let now = crate::services::live::changed_at();
            if now != stamp || ticks.is_multiple_of(15) {
                stamp = now;
                let runs = crate::services::live::load();
                if *live_runs.peek() != runs {
                    live_runs.set(runs);
                }
            }
            let flows_now = flow_files_changed_at();
            if flows_now != flows_stamp && !*setup_open.peek() {
                flows_stamp = flows_now;
                book.set(FlowBook::load());
                repo_flows.set(store::load_repo_flows());
            }
        }
    });

    // Something outside this window changed a repository — a commit from a
    // terminal, a push, GitAgent's own MCP server running a flow — and its row
    // and tabs should say so without a Refresh. A run of this window's own is
    // left alone: it re-reads the repository when it settles.
    use_future(move || async move {
        let mut seen: BTreeMap<String, u64> = BTreeMap::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let paths: Vec<String> = repos.read().iter().map(|r| r.path.clone()).collect();
            for path in paths {
                if running.peek().iter().any(|k| k.0 == path) {
                    continue;
                }
                let now = git::change_fingerprint(&path);
                if seen
                    .insert(path.clone(), now)
                    .is_some_and(|before| before != now)
                {
                    reprobe(path, statuses);
                }
            }
        }
    });

    let llm_config = props.llm_config;
    let wiring = Wiring {
        states,
        running,
        selected_node,
        selected_repo,
        selected_flow,
        selected_pr,
        llm_config,
        statuses,
        trusted,
    };
    let repo_list = repos.read().clone();
    let status_map = statuses.read().clone();
    let forge_map: BTreeMap<String, crate::services::forge::Forge> = status_map
        .iter()
        .map(|(path, s)| (path.clone(), s.forge.clone()))
        .collect();
    let states_snapshot = states.read().clone();
    let flows = book.read().clone();
    // Every flow, not just the runnable ones: a broken flow is shown, marked,
    // and refused, rather than quietly disappearing from the strip.
    let listed: Vec<(String, String, Vec<String>)> = flows
        .listed()
        .iter()
        .map(|(f, problems)| {
            (
                f.id.clone(),
                f.label.clone(),
                problems.iter().map(|p| p.message()).collect(),
            )
        })
        .collect();
    let flow_id = selected_flow.read().clone();
    let flow_problems: Vec<String> = listed
        .iter()
        .find(|(id, _, _)| id == &flow_id)
        .map(|(_, _, problems)| problems.clone())
        .unwrap_or_default();
    let current = flows.get(&flow_id).cloned();
    let flow_kind = current
        .as_ref()
        .map(|f| f.kind())
        .unwrap_or(probe::FlowKind::Other);
    let graph: Graph = current
        .as_ref()
        .map(|f| f.to_graph())
        .unwrap_or(Graph { nodes: vec![] });
    let flow_first_node = current.as_ref().map(|f| f.first_node()).unwrap_or_default();

    let entries: Vec<RepoEntry> = repo_list
        .iter()
        .map(|repo| RepoEntry {
            path: repo.path.clone(),
            label: repo.label.clone(),
            wants: status_map.get(&repo.path).map(|s| s.wants()),
            branch: status_map
                .get(&repo.path)
                .map(|s| s.branch.clone())
                .unwrap_or_default(),
            detail: status_map
                .get(&repo.path)
                .map(|s| s.summary())
                .unwrap_or_default(),
            forge: forge_map.get(&repo.path).cloned(),
            // Several PR-scoped runs can be in flight for this repo+flow at
            // once — the sidebar shows whichever most needs a look, the same
            // way `phase_of` already picks the most urgent status within one.
            // Every flow, not just the one on screen: a review parked at an
            // approval must still show while the commit flow is selected.
            phase: states_snapshot
                .iter()
                .filter(|((r, _, _), _)| *r == repo.path)
                .map(|(_, s)| phase_of(s))
                .min_by_key(|p| p.priority())
                .unwrap_or(Phase::Idle),
            ahead: status_map.get(&repo.path).map(|s| s.ahead).unwrap_or(0),
            behind: status_map.get(&repo.path).map(|s| s.behind).unwrap_or(0),
            open_pr_count: status_map.get(&repo.path).map(|s| s.prs.len()).unwrap_or(0),
            prs_error: status_map.get(&repo.path).and_then(|s| s.prs_error.clone()),
            approval: approval_for(&states_snapshot, &flows, &repo.path),
            start: status_map.get(&repo.path).and_then(|s| {
                row_start(&book.read(), s, &repo_flows.read().hidden_for(&repo.path))
                    .map(|(_, label, _)| label)
            }),
        })
        .collect();

    let active = selected_repo.read().clone();
    let cfg = props.llm_config.read().clone();

    let llm_config_mut = props.llm_config;
    let begin = move |trust: bool| {
        actions::begin(
            statuses,
            states,
            selected_repo,
            book,
            selected_flow,
            selected_pr,
            selected_node,
            running,
            trusted,
            global_trust,
            chain_note,
            repo_flows,
            branches_busy,
            llm_config,
            llm_config_mut,
            trust,
        )
    };
    let start = move |_: Event<MouseData>| begin(false);

    // Branches, Base and Trusted run: the repository's own controls. The same
    // three in the list view's header and the run view's, built here once, so
    // switching views never takes one away or shows a different rule.
    let repo_actions = {
        let status_map = status_map.clone();
        let forge_map = forge_map.clone();
        let flows = flows.clone();
        move |repo: &str,
              key: &Key,
              can_run: &probe::Affordance,
              other_run: &Option<String>|
              -> Element {
            actions::repo_actions(
                running,
                trusted,
                repo_flows,
                branches_open,
                branches_data,
                repo_bases,
                branches_action_error,
                base_editor_open,
                base_editor_value,
                status_map.clone(),
                forge_map.clone(),
                flows.clone(),
                begin,
                start,
                repo,
                key,
                can_run,
                other_run,
            )
        }
    };

    // What the flow on screen is about, under its tabs: why it cannot run if
    // it cannot, and the pull request it acts on — every open one to pick from
    // for a review, else the checked-out branch's. Built once, shown in the
    // list view and the run view alike, so the pull request being validated is
    // in sight in both.
    let flow_context: Element = context::flow_context(
        statuses,
        selected_pr,
        selected_node,
        running,
        pr_fix,
        trusted,
        run_view,
        drawer,
        diff_panel,
        open_diff,
        setup_open,
        live_runs,
        status_map.clone(),
        flow_id.clone(),
        flow_problems.clone(),
        flow_kind,
        graph.clone(),
        active.clone(),
    );

    // The flow tabs — pick a flow, hide one, or choose which are shown — for
    // the selected repository. One strip, placed above the list view's steps
    // and under the run view's header alike.
    let flow_strip: Element = strip::flow_strip(
        book,
        selected_flow,
        selected_pr,
        selected_node,
        drawer,
        diff_panel,
        repo_flows,
        confirm_hide,
        picker_open,
        hide_flow,
        repo_list.clone(),
        status_map.clone(),
        states_snapshot.clone(),
        flows.clone(),
        listed.clone(),
        flow_id.clone(),
        graph.clone(),
        active.clone(),
    );

    if *setup_open.read() {
        return rsx! {
            Setup {
                repo: selected_repo.read().clone(),
                folder: Some(props.workspace.clone()),
                llm_config: props.llm_config,
                on_close: move |_| {
                    // Pick up whatever Setup wrote, without disturbing any run.
                    let before = book.read().clone();
                    book.set(FlowBook::load());
                    repo_flows.set(store::load_repo_flows());
                    // A flow renamed in Setup changes id with its name: stay on
                    // it rather than on an id that no longer exists.
                    let shown = selected_flow.read().clone();
                    if book.read().get(&shown).is_none() {
                        let label = before.get(&shown).map(|f| f.label.clone());
                        let now = label
                            .and_then(|l| book.read().flows.iter().find(|f| f.label == l).map(|f| f.id.clone()))
                            .or_else(|| book.read().flows.first().map(|f| f.id.clone()))
                            .unwrap_or_default();
                        selected_flow.set(now);
                    }
                    setup_open.set(false);
                },
            }
        };
    }

    rsx! {
        div { class: "screen",
            {topbar::topbar(workspace.clone(), running, trusted, global_trust, run_view, settings_open, setup_open, mcp_open, licence_status, licence_open, is_light, theme_overridden, cfg.clone(), props.clone())}

            div {
                class: if *dragging.read() > 0 { "body body-dragging" } else { "body" },
                onmousemove: move |e| {
                    let which = *dragging.read();
                    if which == 0 {
                        return;
                    }
                    let (start_x, start_w) = *drag_from.read();
                    let delta = e.client_coordinates().x - start_x;
                    if which == 1 {
                        sidebar_w.set(Layout::clamp_sidebar(start_w + delta));
                    } else {
                        middle_w.set(Layout::clamp_middle(start_w + delta));
                    }
                },
                onmouseup: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *middle_w.read(),
                            panel_folded: *panel_folded.read(),
                            panel: *panel_w.read(),
                        });
                    }
                },
                // A pointer that leaves the window mid-drag would otherwise
                // leave the divider stuck to the cursor.
                onmouseleave: move |_| {
                    if *dragging.read() > 0 {
                        dragging.set(0);
                        store::save_layout(&Layout {
                            sidebar: *sidebar_w.read(),
                            middle: *middle_w.read(),
                            panel_folded: *panel_folded.read(),
                            panel: *panel_w.read(),
                        });
                    }
                },

                {sidebar::sidebar(workspace.clone(), repos, statuses, probing, picked, states, selected_repo, book, selected_flow, selected_pr, selected_node, running, drawer, diff_panel, repo_flows, sidebar_w, flow_id.clone(), entries.clone(), active.clone(), begin, props.clone())}

                div {
                    class: "divider",
                    onmousedown: move |e| {
                        drag_from.set((e.client_coordinates().x, *sidebar_w.read()));
                        dragging.set(1);
                    },
                }

                {middle::middle_pane(probing, selected_pr, selected_node, running, chain_note, run_view, drawer, panel_step, panel_folded, set_folded, diff_panel, pr_diffs, close_when_done, setup_open, branches_busy, middle_w, panel_w, dragging, drag_from, is_light, llm_config, wiring, repo_list.clone(), status_map.clone(), forge_map.clone(), states_snapshot.clone(), flows.clone(), flow_id.clone(), flow_problems.clone(), current.clone(), flow_kind, graph.clone(), flow_first_node.clone(), active.clone(), begin, start, repo_actions, flow_context.clone(), flow_strip.clone(), props.clone())}
            }
        }

        if *licence_open.read() {
            LicencePanel {
                status: licence_status,
                on_close: move |_| {
                    licence_open.set(false);
                    crate::refresh_window_title();
                },
            }
        }

        if *settings_open.read() {
            SettingsPanel {
                llm_config: props.llm_config,
                folder: Some(props.workspace.clone()),
                // api0 asks for a licence where it is needed, rather than
                // sending you to look for the button in the top bar.
                on_licence: move |_| {
                    settings_open.set(false);
                    licence_open.set(true);
                },
                on_close: move |_| settings_open.set(false),
            }
        }

        {dialogs::branches_dialog(repos, selected_repo, book, selected_flow, selected_pr, selected_node, running, repo_flows, branches_open, branches_data, repo_bases, branches_action_error, branches_busy, forge_map.clone(), flows.clone())}

        if *mcp_open.read() {
            McpPanel { on_close: move |_| mcp_open.set(false) }
        }

        {dialogs::base_editor_dialog(repos, repo_bases, base_editor_open, base_editor_value)}

        {dialogs::confirm_hide_dialog(repos, book, confirm_hide, hide_flow)}
    }
}

#[cfg(test)]
mod tests;
