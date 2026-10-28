#[test]
fn a_layout_saved_before_the_panel_could_fold_still_loads_unfolded() {
    let old: super::Layout =
        serde_json::from_str(r#"{"sidebar": 260.0, "middle": 400.0}"#).unwrap();
    assert_eq!(old.sidebar, 260.0);
    assert!(!old.panel_folded);
}

#[test]
fn the_folded_panel_survives_a_save_and_load() {
    let folded = super::Layout {
        panel_folded: true,
        ..Default::default()
    };
    let text = serde_json::to_string(&folded).unwrap();
    let back: super::Layout = serde_json::from_str(&text).unwrap();
    assert!(back.panel_folded);
}

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
fn a_second_look_at_the_folder_sees_clones_and_deletions_since_the_first() {
    let root = std::env::temp_dir().join(format!("gitagent-discover-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for name in ["alpha", "beta", "not-a-repo"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    std::fs::create_dir_all(root.join("alpha/.git")).unwrap();
    std::fs::create_dir_all(root.join("beta/.git")).unwrap();
    let folder = root.to_string_lossy().to_string();
    let labels =
        |f: &str| -> Vec<String> { discover_repos(f).into_iter().map(|r| r.label).collect() };
    assert_eq!(labels(&folder), ["alpha", "beta"]);

    std::fs::create_dir_all(root.join("gamma/.git")).unwrap();
    std::fs::remove_dir_all(root.join("beta")).unwrap();
    assert_eq!(labels(&folder), ["alpha", "gamma"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_flow_made_for_one_repository_stays_off_the_others_until_added() {
    let mut f = RepoFlows::default();
    f.hide("/other", "deploy_solanize");
    f.only_on("deploy_solanize", "/solanize");
    assert!(f.hidden.is_empty(), "the old hidden entries are folded in");

    assert!(!f.is_hidden("/solanize", "deploy_solanize"));
    assert!(f.is_hidden("/other", "deploy_solanize"));
    assert!(f.is_hidden("/cloned-tomorrow", "deploy_solanize"));
    assert!(f.elsewhere_only("/other", "deploy_solanize"));
    assert_eq!(f.hidden_for("/other"), vec!["deploy_solanize".to_string()]);

    f.show("/other", "deploy_solanize");
    assert!(
        !f.is_hidden("/other", "deploy_solanize"),
        "ticking it adds the repository"
    );
    f.hide("/other", "deploy_solanize");
    assert!(f.is_hidden("/other", "deploy_solanize"));
    assert!(!f.is_hidden("/solanize", "deploy_solanize"));

    f.everywhere("deploy_solanize");
    assert!(!f.is_hidden("/cloned-tomorrow", "deploy_solanize"));
}

#[test]
fn a_renamed_flow_stays_hidden_and_scoped_where_it_was() {
    let mut f = RepoFlows::default();
    f.hide("/a", "review_and_merge_copy");
    f.only_on("review_and_merge_copy_copy", "/cvenom");
    f.rename_flow("review_and_merge_copy", "release");
    f.rename_flow("review_and_merge_copy_copy", "deploy_cvenom");
    assert!(f.is_hidden("/a", "release"));
    assert!(!f.is_hidden("/cvenom", "deploy_cvenom"));
    assert!(f.is_hidden("/a", "deploy_cvenom"));
    assert!(f.shown_only_on("review_and_merge_copy_copy").is_none());
}

#[test]
fn an_existing_repo_flows_file_still_loads() {
    let old = r#"{ "hidden": { "/a": ["x"] } }"#;
    let f: RepoFlows = serde_json::from_str(old).unwrap();
    assert!(f.is_hidden("/a", "x") && f.only.is_empty());
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

fn in_region(id: &str) -> ModelChoice {
    let mut cfg = LlmConfig::default();
    cfg.use_region(id);
    cfg.model_choice()
}

#[test]
fn two_folders_keep_their_own_models() {
    let mut models = FolderModels::default();
    models
        .by_folder
        .insert("/code/api0".into(), in_region("china"));
    models
        .by_folder
        .insert("/code/desktop".into(), in_region("usa"));

    assert_eq!(
        models.choice_for("/code/api0").unwrap().remote_model,
        "china"
    );
    assert_eq!(
        models.choice_for("/code/desktop").unwrap().remote_model,
        "usa"
    );
    // Nothing chosen here: the shared default stands.
    assert!(models.choice_for("/code/elsewhere").is_none());
}

#[test]
fn a_repository_inside_a_folder_uses_that_folders_model() {
    // How a run started from the MCP server, which knows a repository
    // rather than a folder, lands on the same model as the window.
    let mut models = FolderModels::default();
    models
        .by_folder
        .insert("/code/desktop".into(), in_region("usa"));
    assert_eq!(
        models
            .choice_for("/code/desktop/gitagent")
            .unwrap()
            .remote_model,
        "usa"
    );
    // A sibling folder whose name merely starts the same is not inside it.
    assert!(models.choice_for("/code/desktop-old/thing").is_none());
}

#[test]
fn the_nearest_folder_above_a_repository_wins() {
    let mut models = FolderModels::default();
    models.by_folder.insert("/code".into(), in_region("europe"));
    models
        .by_folder
        .insert("/code/desktop".into(), in_region("china"));
    assert_eq!(
        models
            .choice_for("/code/desktop/gitagent")
            .unwrap()
            .remote_model,
        "china"
    );
    assert_eq!(
        models
            .choice_for("/code/api0/gateway")
            .unwrap()
            .remote_model,
        "europe"
    );
}

#[test]
fn a_trailing_slash_is_the_same_folder() {
    let mut models = FolderModels::default();
    models
        .by_folder
        .insert("/code/desktop/".into(), in_region("china"));
    assert_eq!(
        models.choice_for("/code/desktop").unwrap().remote_model,
        "china"
    );
}

#[test]
fn a_folders_model_does_not_carry_the_shared_review_settings() {
    // Picking China for one folder must not turn another window's second
    // opinion or focused reviews on or off.
    let mut shared = LlmConfig::default();
    shared.second.kind = crate::services::llm::ProviderKind::Remote;
    shared.lenses.security = true;

    let laid_over = shared.with_model_choice(&in_region("china"));
    assert_eq!(laid_over.remote_model, "china");
    assert_eq!(laid_over.second, shared.second);
    assert_eq!(laid_over.lenses, shared.lenses);
}
