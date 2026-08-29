//! Which hosting platform a repository lives on, and what it needs from you
//! before a run can finish.
//!
//! Pull requests are the one part of the flow that is not plain git: GitHub
//! wants `gh pr create`, Azure DevOps wants `az repos pr create`, and each has
//! its own idea of authentication. Detecting the forge from the remote URL up
//! front means the run fails in the first node with an actionable message,
//! rather than in the last one after it has already committed and pushed.

use super::git;
use super::graph::Remedy;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Forge {
    GitHub,
    AzureDevOps,
    /// A remote we can push to but cannot open a pull request on.
    Unsupported(String),
    /// No `origin` at all.
    None,
}

impl Forge {
    pub fn label(&self) -> String {
        match self {
            Forge::GitHub => "GitHub".into(),
            Forge::AzureDevOps => "Azure DevOps".into(),
            Forge::Unsupported(host) => format!("unsupported ({host})"),
            Forge::None => "no remote".into(),
        }
    }

    pub fn as_key(&self) -> String {
        match self {
            Forge::GitHub => "github".into(),
            Forge::AzureDevOps => "azure".into(),
            Forge::Unsupported(h) => format!("unsupported:{h}"),
            Forge::None => "none".into(),
        }
    }

    pub fn from_key(key: &str) -> Forge {
        match key {
            "github" => Forge::GitHub,
            "azure" => Forge::AzureDevOps,
            "none" => Forge::None,
            other => Forge::Unsupported(other.trim_start_matches("unsupported:").to_string()),
        }
    }
}

/// Classifies a remote URL. Handles both SSH (`git@host:path`,
/// `ssh://git@host/path`) and HTTPS forms.
pub fn detect(remote_url: &str) -> Forge {
    let url = remote_url.trim();
    if url.is_empty() {
        return Forge::None;
    }
    let lower = url.to_lowercase();

    if lower.contains("github.com") {
        return Forge::GitHub;
    }
    // Azure DevOps has three live URL shapes: the modern dev.azure.com, the
    // legacy {org}.visualstudio.com, and the SSH host ssh.dev.azure.com.
    if lower.contains("dev.azure.com") || lower.contains("visualstudio.com") {
        return Forge::AzureDevOps;
    }

    Forge::Unsupported(host_of(&lower))
}

/// Best-effort host extraction, for the "unsupported" message only.
fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    // SSH short form: git@host:org/repo
    let after_user = after_scheme
        .split_once('@')
        .map(|(_, rest)| rest)
        .unwrap_or(after_scheme);
    after_user
        .split(['/', ':'])
        .next()
        .unwrap_or("unknown")
        .to_string()
}

#[derive(Clone, PartialEq, Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// Present only when the failure has an exact, non-interactive fix.
    pub fix: Option<Remedy>,
}

impl Check {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
            fix: None,
        }
    }
    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            fix: None,
        }
    }

    /// A failure the app can repair itself, given a button press.
    fn fixable(name: &str, detail: impl Into<String>, fix: Remedy) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            fix: Some(fix),
        }
    }
}

/// Everything that must be true before a run can reach `open_pr`.
pub async fn check_credentials(forge: &Forge, repo: &str) -> Vec<Check> {
    match forge {
        Forge::GitHub => {
            if !git::has_gh().await {
                return vec![Check::fixable(
                    "gh CLI",
                    "not found on PATH — install it, or check it is on the PATH \
                     this app inherits",
                    Remedy::new("Install gh", "brew", &["install", "gh"]),
                )];
            }
            match git::run(".", "gh", &["auth", "status"]).await {
                Ok(out) => vec![Check::pass(
                    "gh auth",
                    out.lines()
                        .find(|l| l.contains("Logged in"))
                        .unwrap_or("authenticated")
                        .trim(),
                )],
                // Deliberately not fixable in-app: `gh auth login` needs a
                // terminal to show the device code and take the answer.
                Err(_) => vec![Check::fail(
                    "gh auth",
                    "not authenticated — run `gh auth login`, or set GH_TOKEN",
                )],
            }
        }
        Forge::AzureDevOps => {
            let mut checks = vec![];
            match git::run(".", "az", &["version"]).await {
                Ok(_) => checks.push(Check::pass("az CLI", "installed")),
                Err(_) => {
                    checks.push(Check::fail(
                        "az CLI",
                        "not found on PATH — install it, or check it is on the PATH \
                         this app inherits",
                    ));
                    return checks;
                }
            }
            match git::run(".", "az", &["extension", "show", "--name", "azure-devops"]).await {
                Ok(_) => checks.push(Check::pass("azure-devops extension", "installed")),
                Err(_) => checks.push(Check::fixable(
                    "azure-devops extension",
                    "missing",
                    Remedy::new(
                        "Add the azure-devops extension",
                        "az",
                        &["extension", "add", "--name", "azure-devops"],
                    ),
                )),
            }
            match git::run(
                ".",
                "az",
                &["account", "show", "--query", "user.name", "-o", "tsv"],
            )
            .await
            {
                Ok(user) => checks.push(Check::pass("az login", user.trim())),
                Err(_) => checks.push(Check::fail(
                    "az login",
                    "not signed in — run `az login`, or set AZURE_DEVOPS_EXT_PAT",
                )),
            }

            // `az account show` only proves an AAD sign-in. Azure DevOps is a
            // separate credential, and `az repos pr create` fails on it at the
            // very end of a run — after a branch and a push. This exercises the
            // same path (including org/project detection from the remote), so
            // the failure lands here instead.
            match git::run(repo, "az", &["repos", "list", "--output", "none"]).await {
                Ok(_) => checks.push(Check::pass("azure devops auth", "can reach the project")),
                Err(e) if e.contains("you need to run the login command") => {
                    checks.push(Check::fail(
                        "azure devops auth",
                        "not signed in to Azure DevOps — run `az login` again, or \
                         `az devops login` with a PAT. Neither can be done from here: \
                         both need a terminal.",
                    ))
                }
                Err(e) => checks.push(Check::fail(
                    "azure devops auth",
                    e.lines()
                        .next()
                        .unwrap_or("could not reach the project")
                        .to_string(),
                )),
            }
            checks
        }
        Forge::Unsupported(host) => vec![Check::fail(
            "forge",
            format!(
                "{host} is not supported — GitHub and Azure DevOps only. \
                     Everything up to and including the push still works."
            ),
        )],
        Forge::None => vec![Check::fail(
            "remote",
            "this repository has no `origin` remote",
        )],
    }
}

/// Opens the pull request on whichever forge this repository lives on.
/// Returns the PR URL.
/// Whether a create failed only because the pull request is already there.
///
/// Both platforms say so in their own words; neither is an error worth
/// stopping a run over, because the outcome the step wanted already holds.
pub fn already_exists(error: &str) -> bool {
    let e = error.to_lowercase();
    e.contains("already exists")
        // Azure DevOps: "TF401179: An active pull request for the source and
        // target branch already exists."
        || e.contains("tf401179")
}

/// The pull request URL out of a "already exists" message, when it carries
/// one. `gh` does; `az` does not, so the caller falls back to looking it up.
pub fn url_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|word| word.starts_with("https://") && word.contains("/pull"))
        .map(|w| w.trim_end_matches(['.', ',', ')']).to_string())
}

pub async fn create_pr(
    forge: &Forge,
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    match forge {
        Forge::GitHub => match git::gh_pr_create(repo, base, head, title, body).await {
            Ok(url) => Ok(url),
            // Re-running a flow whose pull request already landed is not a
            // failure; adopt the existing one and carry on.
            Err(e) if already_exists(&e) => match url_in(&e) {
                Some(url) => Ok(url),
                None => git::run(
                    repo,
                    "gh",
                    &["pr", "view", head, "--json", "url", "-q", ".url"],
                )
                .await
                .map(|out| out.trim().to_string())
                .map_err(|_| e),
            },
            Err(e) => Err(e),
        },
        Forge::AzureDevOps => {
            // `--detect true` is the default: az reads the org, project and
            // repository straight off the git remote.
            let out = git::run(
                repo,
                "az",
                &[
                    "repos",
                    "pr",
                    "create",
                    "--source-branch",
                    head,
                    "--target-branch",
                    base,
                    "--title",
                    title,
                    "--description",
                    body,
                    "--output",
                    "json",
                ],
            )
            .await;

            let out = match out {
                Ok(out) => out,
                Err(e) if already_exists(&e) => {
                    // az does not name the pull request in the error, so ask.
                    let listed = git::run(
                        repo,
                        "az",
                        &[
                            "repos",
                            "pr",
                            "list",
                            "--source-branch",
                            head,
                            "--status",
                            "active",
                            "--output",
                            "json",
                        ],
                    )
                    .await
                    .map_err(|_| e.clone())?;

                    let first = serde_json::from_str::<serde_json::Value>(&listed)
                        .ok()
                        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
                        .ok_or(e)?;
                    return Ok(azure_pr_url(&first.to_string())
                        .unwrap_or_else(|| "pull request already open".into()));
                }
                Err(e) => return Err(e),
            };
            Ok(azure_pr_url(&out).unwrap_or_else(|| out.trim().to_string()))
        }
        Forge::Unsupported(host) => Err(format!(
            "cannot open a pull request on {host}. The branch is pushed — open it by hand."
        )),
        Forge::None => Err("no `origin` remote to open a pull request against".into()),
    }
}

/// `az repos pr create` returns the PR object; the browsable URL has to be
/// assembled from the repository web URL and the PR id.
fn azure_pr_url(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let id = value.get("pullRequestId")?.as_i64()?;
    let web = value.get("repository")?.get("webUrl")?.as_str()?;
    Some(format!("{web}/pullrequest/{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
