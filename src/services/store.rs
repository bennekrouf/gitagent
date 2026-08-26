//! On-disk state: the repository registry and the model settings.
//!
//! Same posture as ais-monitor's caches — one small JSON file per concern under
//! the platform data directory, best-effort writes, and a sane default when the
//! file is missing or unreadable.
//!
//! The DeepSeek API key is deliberately **not** in here. It is read from the
//! environment at call time so that nothing ever writes a credential to disk.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::llm::LlmConfig;

const REPOS_FILE: &str = "repos.json";
const SETTINGS_FILE: &str = "settings.json";

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
        let _ = std::fs::write(dir.join(file), json);
    }
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

#[cfg(test)]
mod tests {
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
