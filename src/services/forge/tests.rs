use super::*;

#[test]
fn signed_out_of_azure_offers_az_login() {
    let err = "ERROR: Please run 'az login' to setup account.\n\nwhile running: `az repos pr list`";
    let fix = pr_list_remedy(&Forge::AzureDevOps, err).unwrap();
    assert_eq!(fix.display, "az login");
}

#[test]
fn a_missing_azure_devops_extension_offers_to_add_it() {
    let err = "ERROR: 'repos' is misspelled or not recognized by the system.";
    let fix = pr_list_remedy(&Forge::AzureDevOps, err).unwrap();
    assert_eq!(fix.display, "az extension add --name azure-devops");
}

#[test]
fn a_missing_cli_offers_its_install_where_the_platform_has_one() {
    let err = "the Azure CLI (`az`) is not installed, or not on the PATH GitAgent can see.";
    let fix = pr_list_remedy(&Forge::AzureDevOps, err);
    assert_eq!(fix.is_some(), git::install_command("az").is_some());
}

/// `gh auth login` needs a terminal to show its code, so no button.
#[test]
fn signed_out_of_github_stays_an_instruction() {
    let err = "To get started with GitHub CLI, please run:  gh auth login";
    assert!(pr_list_remedy(&Forge::GitHub, err).is_none());
}

#[test]
fn an_unrecognised_failure_offers_nothing() {
    assert!(pr_list_remedy(&Forge::AzureDevOps, "TF401019: not found").is_none());
}

#[test]
fn github_ssh_and_https_are_both_recognised() {
    assert_eq!(
        detect("git@github.com:bennekrouf/ais-runner.git"),
        Forge::GitHub
    );
    assert_eq!(
        detect("https://github.com/bennekrouf/ais-runner.git"),
        Forge::GitHub
    );
}

#[test]
fn all_three_azure_url_shapes_are_recognised() {
    assert_eq!(
        detect("https://dev.azure.com/org/project/_git/repo"),
        Forge::AzureDevOps
    );
    assert_eq!(
        detect("git@ssh.dev.azure.com:v3/org/project/repo"),
        Forge::AzureDevOps
    );
    assert_eq!(
        detect("https://org.visualstudio.com/project/_git/repo"),
        Forge::AzureDevOps
    );
}

#[test]
fn an_unknown_host_is_named_in_the_error_not_silently_treated_as_github() {
    assert_eq!(
        detect("git@gitlab.com:group/repo.git"),
        Forge::Unsupported("gitlab.com".into())
    );
}

#[test]
fn a_local_folder_remote_is_not_read_as_a_host() {
    for url in [
        r"C:\Users\me\remotes\demo.git",
        "C:/Users/me/remotes/demo.git",
        r"\\server\share\demo.git",
        "/srv/git/demo.git",
        "../demo.git",
        "file:///srv/git/demo.git",
    ] {
        assert_eq!(detect(url), Forge::Unsupported(LOCAL.into()), "{url}");
    }
    // scp-style with a one-letter host still reads as a host.
    assert_eq!(detect("git@c:org/repo.git"), Forge::Unsupported("c".into()));
}

#[test]
fn an_empty_remote_is_none_not_unsupported() {
    assert_eq!(detect(""), Forge::None);
    assert_eq!(detect("   "), Forge::None);
}

#[test]
fn the_forge_survives_a_round_trip_through_the_artifact_map() {
    for forge in [Forge::GitHub, Forge::AzureDevOps, Forge::None] {
        assert_eq!(Forge::from_key(&forge.as_key()), forge);
    }
}

#[test]
fn signing_gh_in_is_spelled_out_with_what_gh_said() {
    let help = gh_login_steps(
        "You are not logged into any GitHub hosts.\n\nwhile running: `gh auth status`",
    );
    assert!(help.contains("gh auth login"));
    assert!(help.contains("GH_TOKEN"));
    assert!(help.contains("You are not logged into any GitHub hosts."));
    assert!(!help.contains("while running"));
    assert!(!help.contains("stale one"));
}

#[test]
fn a_stale_second_account_is_told_apart_from_never_signing_in() {
    let help = gh_login_steps(
        "github.com\n  ✓ Logged in to github.com account a\n  X Failed to log in to github.com account b",
    );
    assert!(help.contains("stale one"));
}

#[test]
fn the_azure_organization_is_read_from_every_remote_shape() {
    assert_eq!(
        azure_org_url(
            "git@ssh.dev.azure.com:v3/CGI-Switzerland-SA/CGI%20Augmented%20Support/kit\n"
        ),
        "https://dev.azure.com/CGI-Switzerland-SA"
    );
    assert_eq!(
        azure_org_url("https://org@dev.azure.com/org/project/_git/repo"),
        "https://dev.azure.com/org"
    );
    assert_eq!(
        azure_org_url("https://org.visualstudio.com/project/_git/repo"),
        "https://dev.azure.com/org"
    );
    assert_eq!(azure_org_url("git@github.com:a/b.git"), "");
}

#[test]
fn az_is_told_the_organization_project_and_repository() {
    let expected = |org: &str, project: &str, repo: &str| -> Vec<String> {
        [
            "--organization",
            org,
            "--project",
            project,
            "--repository",
            repo,
            "--detect",
            "false",
        ]
        .map(String::from)
        .to_vec()
    };
    assert_eq!(
        azure_scope_for(
            "git@ssh.dev.azure.com:v3/CGI-Switzerland-SA/CGI%20Augmented%20Support/kit\n",
            Scope::Repository
        ),
        expected(
            "https://dev.azure.com/CGI-Switzerland-SA",
            "CGI Augmented Support",
            "kit"
        )
    );
    assert_eq!(
        azure_scope_for(
            "https://org@dev.azure.com/org/project/_git/repo",
            Scope::Repository
        ),
        expected("https://dev.azure.com/org", "project", "repo")
    );
    assert_eq!(
        azure_scope_for(
            "https://org.visualstudio.com/DefaultCollection/project/_git/repo",
            Scope::Repository
        ),
        expected("https://dev.azure.com/org", "project", "repo")
    );
    assert_eq!(
        azure_scope_for(
            "https://dev.azure.com/org/project/_git/repo",
            Scope::Organization
        ),
        vec![
            "--organization",
            "https://dev.azure.com/org",
            "--detect",
            "false"
        ]
    );
    assert!(azure_scope_for("git@github.com:a/b.git", Scope::Repository).is_empty());
}

#[test]
fn not_authorized_on_azure_explains_the_tenant_and_the_token() {
    let help = azure_access_steps("https://dev.azure.com/oryx");
    assert!(help.contains("https://dev.azure.com/oryx"));
    assert!(help.contains("az login --tenant"));
    assert!(help.contains("AADSTS50079"));
    assert!(help.contains("AZURE_DEVOPS_EXT_PAT"));
}

#[test]
fn a_remedy_renders_the_command_the_user_would_have_typed() {
    let r = Remedy::new(
        "Add it",
        "az",
        &["extension", "add", "--name", "azure-devops"],
    );
    assert_eq!(r.display, "az extension add --name azure-devops");
    assert!(!r.done);
}

#[test]
fn both_platforms_ways_of_saying_it_already_exists_are_recognised() {
    assert!(already_exists(
        "a pull request for branch \"refactor/azure-services\" into branch \"master\" already exists"
    ));
    assert!(already_exists(
        "ERROR: TF401179: An active pull request for the source and target branch already exists."
    ));
    assert!(!already_exists(
        "fatal: could not read Username for 'https://github.com'"
    ));
}

#[test]
fn the_existing_pull_request_is_taken_from_the_message() {
    let msg = "a pull request for branch \"x\" into branch \"master\" already exists: \
               https://github.com/bennekrouf/ais-runner/pull/19";
    assert_eq!(
        url_in(msg).unwrap(),
        "https://github.com/bennekrouf/ais-runner/pull/19"
    );
}

#[test]
fn trailing_punctuation_is_not_part_of_the_url() {
    let msg = "already exists: https://github.com/o/r/pull/7.";
    assert_eq!(url_in(msg).unwrap(), "https://github.com/o/r/pull/7");
}

#[test]
fn a_message_with_no_link_asks_the_caller_to_look_it_up() {
    assert_eq!(url_in("TF401179: already exists"), None);
    // A non-PR link is not mistaken for one.
    assert_eq!(url_in("see https://github.com/o/r/actions"), None);
}

#[test]
fn an_azure_pr_response_yields_a_browsable_url() {
    let json = r#"{
            "pullRequestId": 4211,
            "repository": { "webUrl": "https://dev.azure.com/oryx/energy/_git/pricing" }
    }"#;
    assert_eq!(
        azure_pr_url(json).unwrap(),
        "https://dev.azure.com/oryx/energy/_git/pricing/pullrequest/4211"
    );
}

#[test]
fn a_malformed_azure_response_does_not_panic() {
    assert!(azure_pr_url("not json").is_none());
    assert!(azure_pr_url(r#"{"pullRequestId": 1}"#).is_none());
}

#[tokio::test]
async fn a_repository_without_a_remote_passes_preflight() {
    let checks = check_credentials(&Forge::None, ".").await;
    assert!(
        checks.iter().all(|c| c.ok),
        "got {:?}",
        checks.iter().map(|c| &c.detail).collect::<Vec<_>>()
    );
}
