//! What the workspace keeps an eye on in the background: the licence, runs
//! and flows changed by GitAgent's MCP server, repositories changed outside
//! the window, and the settings files the MCP server writes. Each is a hook,
//! called once from `Workspace` in the same order on every render.

use super::*;
use crate::services::live::{self, LiveRun};
use crate::services::store::{Repo, RepoFlows};

/// The licence is asked about at start and then daily; between checks,
/// and offline, the last answer stands (for a week, for Pro).
pub(super) fn use_licence_check(mut licence_status: Signal<licence::Status>) {
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
}

/// Runs GitAgent's MCP server is driving for a client such as Claude, read
/// from the file it writes after every step — see `services::live`. Read
/// when the file changes, and now and then regardless, so a run whose
/// server went away drops off.
///
/// The flows, too, can change from outside: the MCP server's Setup tools
/// write the same files Setup does. Picked up here so a flow Claude made
/// shows as a tab without reopening the window — but not while Setup is
/// open, whose draft would then be compared against a moved target.
pub(super) fn use_live_runs(
    mut book: Signal<FlowBook>,
    mut repo_flows: Signal<RepoFlows>,
    setup_open: Signal<bool>,
) -> Signal<Vec<LiveRun>> {
    let mut live_runs = use_signal(live::load);
    let flow_files_changed_at = || {
        let dir = store::data_dir();
        ["flows.toml", "repo_flows.json"].map(|f| {
            std::fs::metadata(dir.join(f))
                .and_then(|m| m.modified())
                .ok()
        })
    };
    use_future(move || async move {
        let mut stamp = live::changed_at();
        let mut flows_stamp = flow_files_changed_at();
        let mut ticks = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            ticks = ticks.wrapping_add(1);
            let now = live::changed_at();
            if now != stamp || ticks.is_multiple_of(15) {
                stamp = now;
                let runs = live::load();
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
    live_runs
}

/// Something outside this window changed a repository — a commit from a
/// terminal, a push, GitAgent's own MCP server running a flow — and its row
/// and tabs should say so without a Refresh. A run of this window's own is
/// left alone: it re-reads the repository when it settles.
pub(super) fn use_repo_changes(
    repos: Signal<Vec<Repo>>,
    running: Signal<BTreeSet<Key>>,
    statuses: Signal<BTreeMap<String, RepoStatus>>,
) {
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
}

/// The signals a Refresh asked for from outside goes through: the same ones
/// the workspace's own Refresh does.
#[derive(Clone, Copy)]
pub(super) struct Probe {
    pub repos: Signal<Vec<Repo>>,
    pub statuses: Signal<BTreeMap<String, RepoStatus>>,
    pub probing: Signal<usize>,
    pub picked: Signal<bool>,
    pub selected_repo: Signal<Option<String>>,
    pub selected_flow: Signal<String>,
    pub book: Signal<FlowBook>,
}

impl Probe {
    /// Probes every repository in `folder`, `fresh` or not — see
    /// `refresh_all`.
    pub fn all(self, folder: &str, fresh: bool) {
        refresh_all(
            folder,
            self.repos,
            self.statuses,
            self.probing,
            self.picked,
            self.selected_repo,
            self.selected_flow,
            self.book,
            fresh,
        );
    }
}

/// What the MCP server changes for a client such as Claude, besides runs
/// and flows: Trust all turned off, the model's region switched, and a
/// Refresh asked for. Each is a file in the data directory, picked up when
/// it changes.
#[allow(clippy::too_many_arguments)]
pub(super) fn use_outside_settings(
    folder: String,
    probe: Probe,
    mut states: Signal<States>,
    running: Signal<BTreeSet<Key>>,
    mut global_trust: Signal<bool>,
    mut model: Signal<LlmConfig>,
    settings_open: Signal<bool>,
) {
    use_future(move || {
        let folder = folder.clone();
        async move {
            let changed_at = |f: &str| {
                std::fs::metadata(store::data_dir().join(f))
                    .and_then(|m| m.modified())
                    .ok()
            };
            let model_files = || {
                (
                    changed_at("settings.json"),
                    changed_at("folder_models.json"),
                )
            };
            let mut trust_stamp = changed_at("trust.json");
            let mut model_stamp = model_files();
            let mut refresh_stamp = changed_at(store::REFRESH_FILE);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                // The top bar says Trust all is off at once, rather than
                // "Trusting everything" over runs that now ask.
                let now = changed_at("trust.json");
                if now != trust_stamp {
                    trust_stamp = now;
                    let all = store::trust_all();
                    if *global_trust.peek() != all {
                        global_trust.set(all);
                    }
                }
                // Not while Settings is open: its draft would be saved over the
                // region just chosen, or shown beside a top bar that disagrees.
                let now = model_files();
                if now != model_stamp && !*settings_open.peek() {
                    model_stamp = now;
                    let cfg = store::load_settings_for(Some(&folder));
                    if *model.peek() != cfg {
                        model.set(cfg);
                    }
                }
                let now = changed_at(store::REFRESH_FILE);
                if now != refresh_stamp {
                    refresh_stamp = now;
                    match store::refresh_request().scope_for(&folder) {
                        Some(store::RefreshScope::Folder) => {
                            forget_finished(&mut states.write(), &running.peek(), None);
                            probe.all(&folder, true);
                        }
                        Some(store::RefreshScope::Repo(path)) => {
                            let found = probe
                                .repos
                                .peek()
                                .iter()
                                .find(|r| store::same_path(&r.path, &path))
                                .map(|r| r.path.clone());
                            if let Some(path) = found {
                                forget_finished(&mut states.write(), &running.peek(), Some(&path));
                                reprobe(path, probe.statuses);
                            }
                        }
                        None => {}
                    }
                }
            }
        }
    });
}
