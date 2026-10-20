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
    /// What to do about a failure the app cannot fix itself, step by step.
    /// Empty when the detail line already says it all.
    pub help: String,
}

impl Check {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: true,
            detail: detail.into(),
            fix: None,
            help: String::new(),
        }
    }
    fn fail(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            fix: None,
            help: String::new(),
        }
    }

    /// A failure the app can repair itself, given a button press.
    fn fixable(name: &str, detail: impl Into<String>, fix: Remedy) -> Self {
        Self {
            name: name.into(),
            ok: false,
            detail: detail.into(),
            fix: Some(fix),
            help: String::new(),
        }
    }

    fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = help.into();
        self
    }
}

/// A forge CLI that could not be started, with an install button where the
/// platform has a standard one-line install.
fn missing_cli(name: &str, program: &str) -> Check {
    let detail = "not installed, or not on the PATH GitAgent can see — install it, then refresh";
    match install_remedy(program) {
        Some(fix) => Check::fixable(name, detail, fix),
        None => Check::fail(name, detail),
    }
}

fn install_remedy(program: &str) -> Option<Remedy> {
    let (cmd, args) = git::install_command(program)?;
    Some(Remedy::new(&format!("Install {program}"), cmd, &args))
}

/// `az login` opens the browser to sign in and returns once that is done, so
/// unlike `gh auth login` (which has to show a code and take an answer) it
/// can run from a button. Without a browser it prints a device-code link
/// instead, which the remedy's output shows.
fn az_login_remedy() -> Remedy {
    Remedy::new("Sign in with az login", "az", &["login"])
}

/// A fix for why the open pull requests could not be listed, when the app
/// has one: installing the CLI, signing in to Azure, or adding the
/// azure-devops extension. Matched on the error text, since that is all a
/// failed CLI call leaves.
pub fn pr_list_remedy(forge: &Forge, err: &str) -> Option<Remedy> {
    let program = match forge {
        Forge::GitHub => "gh",
        Forge::AzureDevOps => "az",
        _ => return None,
    };
    if err.contains("not on the PATH GitAgent can see") {
        return install_remedy(program);
    }
    if program != "az" {
        return None;
    }
    let lower = err.to_lowercase();
    if lower.contains("az login") || lower.contains("login command") || lower.contains("aadsts") {
        return Some(az_login_remedy());
    }
    if lower.contains("'repos' is misspelled") || lower.contains("azure-devops") {
        return Some(Remedy::new(
            "Add the azure-devops extension",
            "az",
            &["extension", "add", "--name", "azure-devops"],
        ));
    }
    None
}

/// How to sign the GitHub CLI in, for someone who has never done it. `said`
/// is what `gh auth status` answered: it fails when *any* account it knows
/// has a stale token, even with another one signed in, and only its own
/// words tell those two cases apart.
fn gh_login_steps(said: &str) -> String {
    let said = said
        .split("\n\nwhile running:")
        .next()
        .unwrap_or_default()
        .trim();
    let mut steps = String::from(
        "To sign the GitHub CLI in:\n\
         1. Open a terminal and run:  gh auth login\n\
         2. Answer its questions: GitHub.com; SSH if your remote starts with\n\
         \x20  git@, HTTPS otherwise; then \"Login with a web browser\".\n\
         3. Copy the one-time code it shows, press Enter, paste the code in the\n\
         \x20  browser page that opens and authorize GitHub CLI.\n\
         4. Check it with:  gh auth status  — it should say \"Logged in to\n\
         \x20  github.com\" and report no errors.\n\
         5. Run the flow again. No restart needed.\n\
         \n\
         Or, instead: create a personal access token with the repo scope at\n\
         github.com/settings/tokens, export GH_TOKEN=<token> in your shell\n\
         profile (~/.zshrc), and restart GitAgent so it reads it.",
    );
    if said.contains("Logged in") {
        steps.push_str(
            "\n\nOne of your accounts is signed in, but gh reports a problem with\n\
             another. Run  gh auth logout  for the stale one, or\n\
             gh auth refresh, until  gh auth status  shows no errors.",
        );
    }
    if !said.is_empty() {
        steps.push_str(&format!("\n\ngh auth status said:\n{said}"));
    }
    steps
}

/// The organization's address, `https://dev.azure.com/{org}`, from any of
/// the three remote shapes. Empty for a remote that is none of them.
fn azure_org_url(remote: &str) -> String {
    let remote = remote.trim();
    if let Some(rest) = remote.split("ssh.dev.azure.com:v3/").nth(1) {
        return format!("https://dev.azure.com/{}", rest.split('/').next().unwrap_or_default());
    }
    if let Some(rest) = remote.split("dev.azure.com/").nth(1) {
        return format!("https://dev.azure.com/{}", rest.split('/').next().unwrap_or_default());
    }
    if let Some(host) = remote
        .split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .filter(|h| h.ends_with(".visualstudio.com"))
    {
        let org = host.trim_end_matches(".visualstudio.com");
        let org = org.rsplit('@').next().unwrap_or(org);
        return format!("https://dev.azure.com/{org}");
    }
    String::new()
}

/// What to do when `az login` is fine but Azure DevOps turns it away. The
/// usual reason is the tenant: an account in several directories signs in
/// to whichever `az` picked as default, often a customer's, and the
/// organization does not know you there. The tenant that is right may
/// demand multi-factor authentication first, which `az login` reports
/// only as AADSTS50079.
fn azure_access_steps(org: &str) -> String {
    let org = if org.is_empty() { "https://dev.azure.com/<organization>" } else { org };
    format!(
        "You are signed in to Azure, but Azure DevOps does not accept that sign-in\n\
         for this organization. Usually az signed in to another tenant (directory)\n\
         than the one the organization is connected to.\n\
         \n\
         1. Open {org} in your browser with the same account. If you cannot\n\
         \x20  see the project, ask an Azure DevOps admin to add you; nothing\n\
         \x20  else will help until they do.\n\
         2. Find the organization's tenant: Organization settings, then\n\
         \x20  Microsoft Entra.\n\
         3. In a terminal, run:\n\
         \x20  az login --tenant <that tenant ID> --allow-no-subscriptions\n\
         \x20  If it says AADSTS50079, that tenant wants multi-factor\n\
         \x20  authentication: enroll at https://aka.ms/mfasetup, or ask your\n\
         \x20  IT if it will not let you, then run az login again.\n\
         4. Check it from the repository folder with:  az repos list -o table\n\
         \x20  When it lists the repositories, run the flow again.\n\
         \n\
         Or, instead: create a personal access token with the Code (Read &\n\
         Write) scope under User settings, Personal access tokens, in\n\
         {org}. Export AZURE_DEVOPS_EXT_PAT=<token> in your shell profile\n\
         (~/.zshrc) and restart GitAgent so it reads it. A token does not\n\
         depend on the tenant az signed in to."
    )
}

/// Everything that must be true before a run can reach `open_pr`.
pub async fn check_credentials(forge: &Forge, repo: &str) -> Vec<Check> {
    match forge {
        Forge::GitHub => {
            if !git::has_gh().await {
                return vec![missing_cli("gh CLI", "gh")];
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
                // terminal to show the device code and take the answer. So
                // the steps are spelled out instead.
                Err(e) => vec![Check::fail(
                    "gh auth",
                    "not authenticated — run `gh auth login`, or set GH_TOKEN",
                )
                .with_help(gh_login_steps(&e))],
            }
        }
        Forge::AzureDevOps => {
            let mut checks = vec![];
            match git::run(".", "az", &["version"]).await {
                Ok(_) => checks.push(Check::pass("az CLI", "installed")),
                Err(_) => {
                    checks.push(missing_cli("az CLI", "az"));
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
                Err(_) => checks.push(Check::fixable(
                    "az login",
                    "not signed in — sign in with `az login`, or set AZURE_DEVOPS_EXT_PAT",
                    az_login_remedy(),
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
                    checks.push(Check::fixable(
                        "azure devops auth",
                        "not signed in to Azure DevOps — sign in again with `az login`, \
                         or run `az devops login` with a PAT in a terminal",
                        az_login_remedy(),
                    ))
                }
                Err(e) => {
                    let remote = git::run(repo, "git", &["remote", "get-url", "origin"])
                        .await
                        .unwrap_or_default();
                    let check = Check::fail(
                        "azure devops auth",
                        e.lines()
                            .next()
                            .unwrap_or("could not reach the project")
                            .to_string(),
                    );
                    // Not authorized, with `az login` passing just above: the
                    // sign-in is real, but for a tenant this organization is
                    // not connected to, or one it does not know you in.
                    if e.contains("TF400813") || e.contains("not authorized") {
                        checks.push(check.with_help(azure_access_steps(&azure_org_url(&remote))))
                    } else {
                        checks.push(check)
                    }
                }
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
    fn signed_out_of_azure_offers_az_login() {
        let err =
            "ERROR: Please run 'az login' to setup account.\n\nwhile running: `az repos pr list`";
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
            azure_org_url("git@ssh.dev.azure.com:v3/CGI-Switzerland-SA/CGI%20Augmented%20Support/kit\n"),
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
}
