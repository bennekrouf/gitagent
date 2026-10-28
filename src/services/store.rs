//! On-disk state: the repository registry and the model settings.
//!
//! Same posture as ais-monitor's caches — one small JSON file per concern under
//! the platform data directory, best-effort writes, and a sane default when the
//! file is missing or unreadable.
//!
//! API keys are deliberately **not** in here. It is read from the
//! environment at call time so that nothing ever writes a credential to disk.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::llm::{LlmConfig, ModelChoice};

const REPOS_FILE: &str = "repos.json";
const SETTINGS_FILE: &str = "settings.json";
const REPO_FLOWS_FILE: &str = "repo_flows.json";
const REPO_BASES_FILE: &str = "repo_bases.json";
const FOLDER_MODELS_FILE: &str = "folder_models.json";

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Repo {
    pub path: String,
    pub label: String,
}

impl Repo {
    /// Directory name is a better default label than the full path.
    pub fn from_path(path: &str) -> Self {
        let label = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        Self {
            path: path.to_string(),
            label,
        }
    }
}

/// A workspace is one folder containing repositories — `~/code`, typically.
/// Only the list of recently opened folders is persisted; the repositories
/// inside one are rediscovered on every open, so adding a clone needs no
/// bookkeeping.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub recent: Vec<String>,
}

impl Registry {
    /// Most recently opened first, capped — this is a convenience list, not
    /// history worth keeping.
    pub fn remember(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_string());
        self.recent.truncate(8);
    }

    pub fn forget(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
    }
}

/// Every immediate child of `folder` that is a git repository, plus `folder`
/// itself if it is one.
///
/// Depth one on purpose: it matches how the repositories are actually laid out
/// and it keeps opening a workspace instant. A recursive walk would wander into
/// `target/` and vendored checkouts.
pub fn discover_repos(folder: &str) -> Vec<Repo> {
    let mut found = vec![];

    if is_repo_dir(std::path::Path::new(folder)) {
        found.push(Repo::from_path(folder));
    }

    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    let mut children: Vec<Repo> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && is_repo_dir(p))
        .map(|p| Repo::from_path(&p.to_string_lossy()))
        .collect();

    children.sort_by_key(|r| r.label.to_lowercase());
    found.extend(children);
    found
}

/// `.git` is a directory in a normal clone and a file in a worktree or
/// submodule; both are repositories as far as this app is concerned.
fn is_repo_dir(path: &std::path::Path) -> bool {
    path.join(".git").exists()
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gitagent")
}

fn read<T: Default + for<'de> Deserialize<'de>>(file: &str) -> T {
    let content = std::fs::read_to_string(data_dir().join(file)).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

fn write<T: Serialize>(file: &str, value: &T) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(value) {
        let _ = write_atomic(&dir.join(file), json.as_bytes());
    }
}

/// Writes through a temporary file in the same directory, then renames it
/// into place.
///
/// A plain `fs::write` truncates the file before it writes, so losing power
/// — or being killed — inside that window leaves a zero-length file behind.
/// `read` above turns any unparseable file into `Default::default()` without
/// a word, so the next launch would come up with an empty repository list and
/// the shipped flows, and nothing on screen to say the real ones had been
/// lost. A rename within one filesystem is atomic: a reader sees the old
/// file or the new one, never a half-written one.
///
/// The temporary name carries the process id because two GitAgent windows
/// are two `Signal` copies of the same state writing the same files, and
/// they must not collide on the scratch file even when they race on the
/// real one.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".into());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));

    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        // Durable before the rename, or a crash can leave the rename visible
        // while the contents behind it are not.
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn load_registry() -> Registry {
    read(REPOS_FILE)
}

pub fn save_registry(registry: &Registry) {
    write(REPOS_FILE, registry);
}

/// Pane widths, in pixels. Persisted so a layout you set once survives a
/// restart rather than snapping back every launch.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Layout {
    pub sidebar: f64,
    /// The middle column: the flow list in the workspace, the editor in Setup.
    pub middle: f64,
    /// Whether the run view's step panel is folded to a strip. Kept between
    /// openings and across restarts: someone who folds it wants the map, and
    /// should not have to fold it again for every step.
    #[serde(default)]
    pub panel_folded: bool,
    /// The run view's step panel, dragged wider or narrower by its left
    /// edge. `None` until it is first dragged: the panel then sizes itself
    /// to the window.
    #[serde(default)]
    pub panel: Option<f64>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: 224.0,
            middle: 330.0,
            panel_folded: false,
            panel: None,
        }
    }
}

impl Layout {
    /// Keeps a drag inside sane bounds — a pane dragged to nothing is a pane
    /// you cannot grab again.
    pub fn clamp_sidebar(width: f64) -> f64 {
        width.clamp(150.0, 480.0)
    }

    pub fn clamp_middle(width: f64) -> f64 {
        width.clamp(220.0, 640.0)
    }

    /// The step panel may grow until it covers the whole run view — a diff
    /// read full width — but never narrower than its edge can be found and
    /// its content read.
    pub fn clamp_panel(width: f64, room: f64) -> f64 {
        width.min(room).max(PANEL_MIN.min(room))
    }
}

/// The narrowest the step panel can be dragged.
pub const PANEL_MIN: f64 = 360.0;

const LAYOUT_FILE: &str = "layout.json";

/// Whether the repository list is folded to a strip. Its own file rather than
/// a field of `Layout`, so folding never races a divider drag saving widths.
#[derive(Default, Serialize, Deserialize)]
struct SidebarPref {
    #[serde(default)]
    folded: bool,
}

const SIDEBAR_FILE: &str = "sidebar.json";

/// "Trust all", kept on disk rather than in one window's memory: GitAgent's
/// MCP server is another process, and a run it drives for Claude has to
/// answer approvals the way the window would.
#[derive(Default, Serialize, Deserialize)]
struct TrustPref {
    #[serde(default)]
    all: bool,
}

const TRUST_FILE: &str = "trust.json";

pub fn trust_all() -> bool {
    read::<TrustPref>(TRUST_FILE).all
}

pub fn save_trust_all(all: bool) {
    write(TRUST_FILE, &TrustPref { all });
}

pub fn sidebar_folded() -> bool {
    read::<SidebarPref>(SIDEBAR_FILE).folded
}

pub fn save_sidebar_folded(folded: bool) {
    write(SIDEBAR_FILE, &SidebarPref { folded });
}

pub fn load_layout() -> Layout {
    let content = std::fs::read_to_string(data_dir().join(LAYOUT_FILE)).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_layout(layout: &Layout) {
    write(LAYOUT_FILE, layout);
}

/// Whether this install has ever been set up.
///
/// The settings file existing is the marker: `load_settings` falls back to a
/// default that looks identical to a real choice, so nothing else can tell a
/// fresh install from one that deliberately runs on the defaults.
pub fn is_configured() -> bool {
    data_dir().join(SETTINGS_FILE).exists()
}

pub fn load_settings() -> LlmConfig {
    let content = std::fs::read_to_string(data_dir().join(SETTINGS_FILE)).unwrap_or_default();
    let cfg: LlmConfig = serde_json::from_str(&content).unwrap_or_default();
    cfg.with_region_filled()
}

pub fn save_settings(cfg: &LlmConfig) {
    write(SETTINGS_FILE, cfg);
}

/// The model each folder has chosen for itself, keyed by the folder's path.
///
/// A window is one open folder, so this is what lets two windows run
/// different models at the same time. A folder with no entry runs whatever
/// `settings.json` says — which is also what a folder inherits the first
/// time it is opened, so nothing has to be chosen twice.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct FolderModels {
    #[serde(default)]
    pub by_folder: BTreeMap<String, ModelChoice>,
}

/// Whether `path` is `folder` or sits somewhere under it.
fn is_within(path: &str, folder: &str) -> bool {
    let path = path.trim_end_matches('/');
    let folder = folder.trim_end_matches('/');
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl FolderModels {
    /// The choice made for `path`, or for the nearest folder above it.
    ///
    /// Walking upwards is what lets a run started from the MCP server, which
    /// knows a repository rather than a folder, use the same model the window
    /// on that folder is set to.
    pub fn choice_for(&self, path: &str) -> Option<&ModelChoice> {
        self.by_folder
            .iter()
            .filter(|(folder, _)| is_within(path, folder))
            .max_by_key(|(folder, _)| folder.trim_end_matches('/').len())
            .map(|(_, choice)| choice)
    }
}

pub fn load_folder_models() -> FolderModels {
    read(FOLDER_MODELS_FILE)
}

pub fn save_folder_models(models: &FolderModels) {
    write(FOLDER_MODELS_FILE, models);
}

/// The settings a window with `folder` open should run on: the shared file,
/// with that folder's own model laid over it once it has chosen one.
///
/// `None` — the welcome screen, the first run, anywhere with no folder to
/// scope to — is the shared settings as they stand.
pub fn load_settings_for(folder: Option<&str>) -> LlmConfig {
    let shared = load_settings();
    let Some(folder) = folder else {
        return shared;
    };
    match load_folder_models().choice_for(folder) {
        Some(choice) => shared.with_model_choice(choice).with_region_filled(),
        None => shared,
    }
}

/// Writes what Settings was closed on.
///
/// With a folder open, the model becomes that folder's and the shared default
/// is left exactly as it was — that is the whole point: picking China here
/// must not move another window off the United States. Everything else in the
/// panel is not per folder, so it still goes to the shared file. Without a
/// folder there is nothing to scope to, and the lot is the shared default.
pub fn save_settings_for(folder: Option<&str>, cfg: &LlmConfig) {
    let Some(folder) = folder else {
        return save_settings(cfg);
    };
    let mut models = load_folder_models();
    models
        .by_folder
        .insert(folder.trim_end_matches('/').to_string(), cfg.model_choice());
    save_folder_models(&models);
    let mut shared = load_settings();
    shared.second = cfg.second.clone();
    shared.lenses = cfg.lenses.clone();
    save_settings(&shared);
}

/// Which flows each repository has chosen not to see, keyed by repository
/// path. Flows are shared by every repository and every window — this is
/// purely a per-repo *filter* on top of that shared list, not a copy of it,
/// so hiding a flow here never touches `flows.toml`. A repository with
/// nothing hidden simply has no entry.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct RepoFlows {
    #[serde(default)]
    pub hidden: BTreeMap<String, Vec<String>>,
    /// Flows shown only on the repositories listed, keyed by flow id. Opt-in
    /// rather than hidden everywhere else: a deploy flow made for one
    /// repository must not turn up on every other one, including those
    /// cloned later, which a hidden list can never name in advance.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub only: BTreeMap<String, Vec<String>>,
}

impl RepoFlows {
    pub fn is_hidden(&self, repo: &str, flow_id: &str) -> bool {
        self.hidden
            .get(repo)
            .is_some_and(|ids| ids.iter().any(|id| id == flow_id))
            || self.elsewhere_only(repo, flow_id)
    }

    /// Shown only on other repositories — what the flow picker offers here
    /// as a suggestion rather than a flow someone chose to hide.
    pub fn elsewhere_only(&self, repo: &str, flow_id: &str) -> bool {
        self.only
            .get(flow_id)
            .is_some_and(|repos| !repos.iter().any(|r| r == repo))
    }

    pub fn hide(&mut self, repo: &str, flow_id: &str) {
        if let Some(repos) = self.only.get_mut(flow_id) {
            repos.retain(|r| r != repo);
            return;
        }
        let ids = self.hidden.entry(repo.to_string()).or_default();
        if !ids.iter().any(|id| id == flow_id) {
            ids.push(flow_id.to_string());
        }
    }

    /// Un-hides one flow. Drops the repo's entry entirely once nothing is
    /// hidden for it, so a repo that has restored everything looks exactly
    /// like one that never hid anything.
    pub fn show(&mut self, repo: &str, flow_id: &str) {
        if let Some(repos) = self.only.get_mut(flow_id) {
            if !repos.iter().any(|r| r == repo) {
                repos.push(repo.to_string());
            }
        }
        let Some(ids) = self.hidden.get_mut(repo) else {
            return;
        };
        ids.retain(|id| id != flow_id);
        if ids.is_empty() {
            self.hidden.remove(repo);
        }
    }

    /// Restricts a flow to one repository, replacing any hidden entries for
    /// it — those were only ever a way of saying the same thing.
    pub fn only_on(&mut self, flow_id: &str, repo: &str) {
        self.only
            .insert(flow_id.to_string(), vec![repo.to_string()]);
        for ids in self.hidden.values_mut() {
            ids.retain(|id| id != flow_id);
        }
        self.hidden.retain(|_, ids| !ids.is_empty());
    }

    pub fn everywhere(&mut self, flow_id: &str) {
        self.only.remove(flow_id);
    }

    /// A flow that no longer exists, removed from every list that names it.
    pub fn forget_flow(&mut self, flow_id: &str) {
        self.only.remove(flow_id);
        for ids in self.hidden.values_mut() {
            ids.retain(|id| id != flow_id);
        }
        self.hidden.retain(|_, ids| !ids.is_empty());
    }

    pub fn shown_only_on(&self, flow_id: &str) -> Option<&[String]> {
        self.only.get(flow_id).map(Vec::as_slice)
    }

    /// Follows a flow whose id changed, everywhere this file names it.
    pub fn rename_flow(&mut self, old: &str, new: &str) {
        for ids in self.hidden.values_mut() {
            for id in ids.iter_mut().filter(|id| *id == old) {
                *id = new.to_string();
            }
        }
        if let Some(repos) = self.only.remove(old) {
            self.only.insert(new.to_string(), repos);
        }
    }

    /// Every flow not shown on `repo`, for whatever reason.
    pub fn hidden_for(&self, repo: &str) -> Vec<String> {
        let mut ids: Vec<String> = self.hidden.get(repo).cloned().unwrap_or_default();
        for flow_id in self.only.keys() {
            if self.elsewhere_only(repo, flow_id) && !ids.contains(flow_id) {
                ids.push(flow_id.clone());
            }
        }
        ids
    }
}

pub fn load_repo_flows() -> RepoFlows {
    read(REPO_FLOWS_FILE)
}

pub fn save_repo_flows(flows: &RepoFlows) {
    write(REPO_FLOWS_FILE, flows);
}

/// Which base branch each repository's pull requests should target, keyed
/// by repository path. One setting per repository rather than per flow node
/// — a repo whose PRs go to `develop` means that everywhere in that repo,
/// not "in this one flow if you remembered to set it there too", which is
/// what living inside each flow's Preflight config amounted to.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct RepoBases {
    #[serde(default)]
    pub base: BTreeMap<String, String>,
}

impl RepoBases {
    pub fn get(&self, repo: &str) -> Option<&str> {
        self.base.get(repo).map(String::as_str)
    }

    /// An empty branch name clears the override — same as never having set
    /// one — rather than persisting an empty string that Setup would then
    /// have to treat specially.
    pub fn set(&mut self, repo: &str, branch: &str) {
        let branch = branch.trim();
        if branch.is_empty() {
            self.base.remove(repo);
        } else {
            self.base.insert(repo.to_string(), branch.to_string());
        }
    }
}

pub fn load_repo_bases() -> RepoBases {
    read(REPO_BASES_FILE)
}

pub fn save_repo_bases(bases: &RepoBases) {
    write(REPO_BASES_FILE, bases);
}

/// The base-branch commit each repository last shipped through a flow that
/// handles releases, keyed by repository path.
///
/// A deploy flow ships without tagging, so "commits since the last tag" never
/// goes back to zero for it — backend-cvenom read 172 commits pending, a year
/// of deploys after its last tag. This is the bound such a flow leaves behind.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Shipped {
    #[serde(default)]
    pub commit: BTreeMap<String, String>,
}

const SHIPPED_FILE: &str = "shipped.json";

pub fn shipped_commit(repo: &str) -> Option<String> {
    read::<Shipped>(SHIPPED_FILE).commit.get(repo).cloned()
}

pub fn save_shipped_commit(repo: &str, commit: &str) {
    let mut shipped: Shipped = read(SHIPPED_FILE);
    shipped.commit.insert(repo.to_string(), commit.to_string());
    write(SHIPPED_FILE, &shipped);
}

/// How far the run view's map is zoomed, `1.0` being its own size. Its own
/// file, like the sidebar's fold, so nothing else that saves layout can race
/// it.
#[derive(Serialize, Deserialize)]
struct RunZoomPref {
    zoom: f64,
}

impl Default for RunZoomPref {
    fn default() -> Self {
        Self { zoom: 1.0 }
    }
}

const RUN_ZOOM_FILE: &str = "run_zoom.json";

pub fn run_zoom() -> f64 {
    read::<RunZoomPref>(RUN_ZOOM_FILE).zoom
}

pub fn save_run_zoom(zoom: f64) {
    write(RUN_ZOOM_FILE, &RunZoomPref { zoom });
}

#[cfg(test)]
mod tests;
