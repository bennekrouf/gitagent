//! On-disk state: the repository registry and the model settings.
//!
//! Same posture as ais-monitor's caches — one small JSON file per concern under
//! the platform data directory, best-effort writes, and a sane default when the
//! file is missing or unreadable.
//!
//! The DeepSeek API key is deliberately **not** in here. It is read from the
//! environment at call time so that nothing ever writes a credential to disk.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::llm::LlmConfig;

const REPOS_FILE: &str = "repos.json";
const SETTINGS_FILE: &str = "settings.json";
const REPO_FLOWS_FILE: &str = "repo_flows.json";
const REPO_BASES_FILE: &str = "repo_bases.json";

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
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar: 224.0,
            middle: 330.0,
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
}

const LAYOUT_FILE: &str = "layout.json";

pub fn load_layout() -> Layout {
    let content = std::fs::read_to_string(data_dir().join(LAYOUT_FILE)).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_layout(layout: &Layout) {
    write(LAYOUT_FILE, layout);
}

pub fn load_settings() -> LlmConfig {
    let content = std::fs::read_to_string(data_dir().join(SETTINGS_FILE)).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save_settings(cfg: &LlmConfig) {
    write(SETTINGS_FILE, cfg);
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
}

impl RepoFlows {
    pub fn is_hidden(&self, repo: &str, flow_id: &str) -> bool {
        self.hidden
            .get(repo)
            .is_some_and(|ids| ids.iter().any(|id| id == flow_id))
    }

    pub fn hide(&mut self, repo: &str, flow_id: &str) {
        let ids = self.hidden.entry(repo.to_string()).or_default();
        if !ids.iter().any(|id| id == flow_id) {
            ids.push(flow_id.to_string());
        }
    }

    /// Un-hides one flow. Drops the repo's entry entirely once nothing is
    /// hidden for it, so a repo that has restored everything looks exactly
    /// like one that never hid anything.
    pub fn show(&mut self, repo: &str, flow_id: &str) {
        let Some(ids) = self.hidden.get_mut(repo) else {
            return;
        };
        ids.retain(|id| id != flow_id);
        if ids.is_empty() {
            self.hidden.remove(repo);
        }
    }

    pub fn hidden_for(&self, repo: &str) -> &[String] {
        self.hidden.get(repo).map(Vec::as_slice).unwrap_or(&[])
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

#[cfg(test)]
mod tests {

    #[test]
    fn an_atomic_write_replaces_the_file_without_a_truncated_window() {
        let dir = std::env::temp_dir().join(format!("gitagent-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");

        write_atomic(&path, b"{\"first\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"first\":true}");

        // Overwriting leaves the file whole, and leaves no scratch file
        // behind for `discover_repos` or a later read to trip over.
        write_atomic(&path, b"{\"second\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"second\":true}");
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
    use super::*;

    #[test]
    fn the_label_defaults_to_the_directory_name() {
        assert_eq!(
            Repo::from_path("/Users/mb/code/ais-monitor").label,
            "ais-monitor"
        );
    }

    #[test]
    fn a_trailing_slash_does_not_produce_an_empty_label() {
        assert_eq!(
            Repo::from_path("/Users/mb/code/ais-runner/").label,
            "ais-runner"
        );
    }

    #[test]
    fn opening_a_folder_again_moves_it_to_the_front() {
        let mut r = Registry::default();
        r.remember("/a");
        r.remember("/b");
        r.remember("/a");
        assert_eq!(r.recent, vec!["/a", "/b"]);
    }

    #[test]
    fn nothing_is_hidden_by_default() {
        let flows = RepoFlows::default();
        assert!(!flows.is_hidden("/repo", "commit_and_pr"));
        assert!(flows.hidden_for("/repo").is_empty());
    }

    #[test]
    fn hiding_a_flow_only_affects_the_repo_it_was_hidden_for() {
        let mut flows = RepoFlows::default();
        flows.hide("/repo-a", "commit_and_pr");
        assert!(flows.is_hidden("/repo-a", "commit_and_pr"));
        assert!(!flows.is_hidden("/repo-b", "commit_and_pr"));
    }

    #[test]
    fn hiding_the_same_flow_twice_does_not_duplicate_it() {
        let mut flows = RepoFlows::default();
        flows.hide("/repo", "commit_and_pr");
        flows.hide("/repo", "commit_and_pr");
        assert_eq!(flows.hidden_for("/repo"), &["commit_and_pr".to_string()]);
    }

    #[test]
    fn showing_a_flow_again_undoes_hiding_it() {
        let mut flows = RepoFlows::default();
        flows.hide("/repo", "commit_and_pr");
        flows.show("/repo", "commit_and_pr");
        assert!(!flows.is_hidden("/repo", "commit_and_pr"));
    }

    #[test]
    fn a_repo_with_everything_restored_leaves_no_trace() {
        // Restoring the last hidden flow removes the repo's entry entirely,
        // rather than leaving an empty list sitting in the saved file.
        let mut flows = RepoFlows::default();
        flows.hide("/repo", "commit_and_pr");
        flows.show("/repo", "commit_and_pr");
        assert!(flows.hidden.is_empty());
    }

    #[test]
    fn a_repository_with_no_override_has_none() {
        let bases = RepoBases::default();
        assert_eq!(bases.get("/repo"), None);
    }

    #[test]
    fn setting_a_base_only_affects_the_repo_it_was_set_for() {
        let mut bases = RepoBases::default();
        bases.set("/repo-a", "develop");
        assert_eq!(bases.get("/repo-a"), Some("develop"));
        assert_eq!(bases.get("/repo-b"), None);
    }

    #[test]
    fn clearing_a_base_with_an_empty_string_removes_the_override() {
        let mut bases = RepoBases::default();
        bases.set("/repo", "develop");
        bases.set("/repo", "");
        assert_eq!(bases.get("/repo"), None);
        assert!(bases.base.is_empty(), "no empty-string entry left behind");
    }

    #[test]
    fn setting_a_base_trims_surrounding_whitespace() {
        let mut bases = RepoBases::default();
        bases.set("/repo", "  develop  ");
        assert_eq!(bases.get("/repo"), Some("develop"));
    }

    #[test]
    fn the_recent_list_does_not_grow_without_bound() {
        let mut r = Registry::default();
        for i in 0..20 {
            r.remember(&format!("/w{i}"));
        }
        assert_eq!(r.recent.len(), 8);
        assert_eq!(r.recent[0], "/w19");
    }

    #[test]
    fn forgetting_a_folder_removes_only_that_one() {
        let mut r = Registry::default();
        r.remember("/a");
        r.remember("/b");
        r.forget("/a");
        assert_eq!(r.recent, vec!["/b"]);
    }

    #[test]
    fn discovery_of_a_folder_that_does_not_exist_is_empty_not_a_panic() {
        assert!(discover_repos("/nope/does/not/exist").is_empty());
    }

    #[test]
    fn a_pane_cannot_be_dragged_out_of_existence() {
        assert_eq!(Layout::clamp_sidebar(0.0), 150.0);
        assert_eq!(Layout::clamp_sidebar(-500.0), 150.0);
        assert_eq!(Layout::clamp_middle(10_000.0), 640.0);
    }

    #[test]
    fn a_width_inside_the_bounds_is_left_alone() {
        assert_eq!(Layout::clamp_sidebar(300.0), 300.0);
        assert_eq!(Layout::clamp_middle(400.0), 400.0);
    }

    #[test]
    fn a_corrupt_layout_file_falls_back_to_the_defaults() {
        let layout: Layout = serde_json::from_str("{{ broken").unwrap_or_default();
        assert_eq!(layout, Layout::default());
    }

    #[test]
    fn settings_fall_back_to_defaults_when_the_file_is_corrupt() {
        let cfg: LlmConfig = serde_json::from_str("{ not json").unwrap_or_default();
        assert_eq!(cfg, LlmConfig::default());
    }
}
