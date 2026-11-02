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
mod panel;
mod rows;
mod sidebar;
mod strip;
mod topbar;
mod watch;
use flows::*;

/// One run per repository, per flow, per pull request — the third slot is
/// empty for anything that isn't PR-scoped review. That is what lets you
/// review PR #7 and PR #5 on the same repository at once without one
/// overwriting the other's progress, the same way two different repositories
/// already don't tread on each other.
type Key = (String, String, String);
type States = BTreeMap<Key, RunState>;

/// A run started outside the window, on the flow on screen, as its graph
/// draws it.
#[derive(Clone, PartialEq)]
pub(super) struct Watched {
    pub state: RunState,
    pub finished: bool,
    /// Where its approvals are answered.
    pub note: String,
    /// The MCP server's process and its run id, when the window may answer
    /// the run's approvals too. `None` for a terminal run.
    pub remote: Option<(u32, u64)>,
}
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
    let selected_node = use_signal(String::new);
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
    let drawer = use_signal(|| Option::<String>::None);
    // The step the panel last showed, kept after it closes so the panel can
    // slide out with its content still in it instead of going blank first.
    let panel_step = use_signal(|| Option::<String>::None);
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
    let diff_panel = use_signal(|| Option::<String>::None);
    let pr_diffs = use_signal(PrDiffs::new);
    // A step approved from its panel, kept open on its log while it runs.
    let close_when_done = use_signal(|| Option::<(Key, String)>::None);
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
    let licence_status = use_signal(licence::current);
    let mut licence_open = use_signal(|| false);
    watch::use_licence_check(licence_status);
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
    // For as long as this window has a folder open, hold off App Nap, so a
    // hidden window keeps working instead of pausing until it is shown again:
    // a run, and also what starts or answers one without a click here — Claude
    // over MCP, a trusted chain, the repositories and pull requests being read
    // again. See `services::awake`. Per window: closing one never releases
    // another's.
    let _awake = use_hook(|| std::rc::Rc::new(awake::Activity::begin()));
    let base_editor_open = use_signal(|| Option::<String>::None);
    let base_editor_value = use_signal(String::new);
    // Which repository's flow picker is open, rather than a single flag for
    // all of them: opening it on one repository used to leave it open on the
    // next one you selected, which reads as a panel that will not close.
    let picker_open = use_signal(|| Option::<String>::None);

    let panel = panel::Panel {
        states,
        selected_repo,
        selected_flow,
        selected_pr,
        selected_node,
        trusted,
        run_view,
        drawer,
        panel_step,
        diff_panel,
        close_when_done,
    };
    panel::use_close_when_done(panel);
    panel::use_panel_follows_drawer(panel);
    panel::use_auto_open(panel);
    panel::use_escape_closes(panel, move || {
        *settings_open.peek()
            || *licence_open.peek()
            || branches_open.peek().is_some()
            || base_editor_open.peek().is_some()
            || confirm_hide.peek().is_some()
            || picker_open.peek().is_some()
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
    let probe = watch::Probe {
        repos,
        statuses,
        probing,
        picked,
        selected_repo,
        selected_flow,
        book,
    };
    let opened = workspace.clone();
    use_coroutine(move |_rx: UnboundedReceiver<()>| {
        let opened = opened.clone();
        async move { probe.all(&opened, false) }
    });
    let live_runs = watch::use_live_runs(book, repo_flows, setup_open);
    watch::use_repo_changes(repos, running, statuses);
    watch::use_outside_settings(
        workspace.clone(),
        probe,
        states,
        running,
        global_trust,
        props.llm_config,
        settings_open,
    );

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

    let entries = rows::repo_entries(
        &repo_list,
        &status_map,
        &states_snapshot,
        &book.read(),
        &repo_flows.read(),
    );

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
    // A run Claude (or a terminal) is driving on the flow on screen: see
    // `rows::watched_run`.
    let watched: Option<Watched> = active.as_ref().and_then(|repo| {
        rows::watched_run(repo, &flow_id, &graph, &running.read(), &live_runs.read())
    });

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
        watched.is_some(),
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
            {topbar::topbar(running, trusted, global_trust, run_view, settings_open, mcp_open, licence_status, licence_open, cfg.clone(), props.clone())}

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

                {middle::middle_pane(middle::Mid { probing, selected_pr, selected_node, running, chain_note, run_view, drawer, panel_step, panel_folded, diff_panel, pr_diffs, close_when_done, setup_open, branches_busy, middle_w, panel_w, dragging, drag_from, is_light, wiring }, middle::Shown { repo_list: repo_list.clone(), status_map: status_map.clone(), forge_map: forge_map.clone(), states_snapshot: states_snapshot.clone(), flows: flows.clone(), flow_id: flow_id.clone(), flow_problems: flow_problems.clone(), current: current.clone(), flow_kind, graph: graph.clone(), flow_first_node: flow_first_node.clone(), active: active.clone(), flow_context: flow_context.clone(), flow_strip: flow_strip.clone(), watched: watched.clone(), props: props.clone() }, set_folded, begin, start, repo_actions)}
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
                on_setup: move |_| {
                    settings_open.set(false);
                    setup_open.set(true);
                },
                theme: desktop_kit::ui::Theme { light: is_light, picked: theme_overridden },
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
